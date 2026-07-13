//! Language Server Protocol implementation for nessemble's assembly flavor.
//!
//! A synchronous [`lsp-server`](lsp_server) stdio server. It completes the LSP
//! lifecycle (`initialize` → advertise capabilities → `initialized` →
//! `shutdown`/`exit`), tracks open documents via
//! `textDocument/didOpen|didChange|didClose`, and provides:
//!
//! - **Diagnostics** (Phases 1 & 4): each open buffer is scanned (via
//!   `nessemble_core::diagnose_source_as`, which recovers past errors) and *all*
//!   errors/warnings are published via `textDocument/publishDiagnostics`, each
//!   with a **token-accurate range** (narrowed to the offending token).
//! - **Completion** (Phase 2): `textDocument/completion` offers instruction
//!   mnemonics (from `nessemble-isa`), directives, in-scope labels/constants,
//!   and macro names.
//! - **Formatting & highlighting** (Phase 3): `textDocument/formatting` tidies a
//!   buffer (via `nessemble_core::tooling::format`), and
//!   `textDocument/semanticTokens/full` classifies tokens for highlighting, both
//!   built on the lossless tooling lexer.
//! - **Navigation, symbols & hover** (Phase 5): `textDocument/documentSymbol`
//!   (an outline of labels/constants/macros), `textDocument/definition` and
//!   `textDocument/references` (jump to / list a symbol's occurrences), and
//!   `textDocument/hover` (opcode/addressing details, directive descriptions,
//!   symbol values), all driven by the lossless tooling lexer over the buffer.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use lsp_server::{Connection, ErrorCode, Message, Notification, Response};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Notification as _,
    PublishDiagnostics,
};
use lsp_types::request::{
    Completion, DocumentSymbolRequest, Formatting, GotoDefinition, HoverRequest, References,
    Request as _, SemanticTokensFullRequest,
};
use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionOptions, CompletionParams, CompletionResponse,
    Diagnostic, DiagnosticSeverity, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, DocumentFormattingParams, DocumentSymbol, DocumentSymbolParams,
    DocumentSymbolResponse, GotoDefinitionParams, GotoDefinitionResponse, Hover, HoverContents,
    HoverParams, Location, MarkupContent, MarkupKind, OneOf, Position, PublishDiagnosticsParams,
    Range, ReferenceParams, SemanticToken, SemanticTokenType, SemanticTokens,
    SemanticTokensFullOptions, SemanticTokensLegend, SemanticTokensOptions, SemanticTokensParams,
    SemanticTokensResult, SemanticTokensServerCapabilities, ServerCapabilities, SymbolKind,
    TextDocumentSyncCapability, TextDocumentSyncKind, TextEdit, Url, WorkDoneProgressOptions,
};

use nessemble_core::tooling::{self, LexKind};
use nessemble_core::{diagnose_source_as, Diag, ListSymbol, Options};
use nessemble_isa::{DIRECTIVES, OPCODES};

/// Semantic-token type legend. The index of each entry is the `token_type`
/// referenced by emitted tokens (see [`token_type`]).
const TOKEN_TYPES: [SemanticTokenType; 7] = [
    SemanticTokenType::KEYWORD,  // 0: directive
    SemanticTokenType::FUNCTION, // 1: instruction mnemonic
    SemanticTokenType::VARIABLE, // 2: identifier (label/constant/register)
    SemanticTokenType::NUMBER,   // 3
    SemanticTokenType::STRING,   // 4: string/char literal
    SemanticTokenType::COMMENT,  // 5
    SemanticTokenType::OPERATOR, // 6: punctuation/operator
];
const TT_KEYWORD: u32 = 0;
const TT_FUNCTION: u32 = 1;
const TT_VARIABLE: u32 = 2;
const TT_NUMBER: u32 = 3;
const TT_STRING: u32 = 4;
const TT_COMMENT: u32 = 5;
const TT_OPERATOR: u32 = 6;

/// A boxed, thread-safe error, matching what the stdio transport surfaces.
type LspError = Box<dyn std::error::Error + Sync + Send>;
type LspResult<T> = Result<T, LspError>;

/// Per-document state: the current buffer text plus the user-defined symbols
/// (labels/constants, with their resolved values) from the last successful
/// assembly, used for completion and hover.
#[derive(Default)]
struct Document {
    text: String,
    symbols: Vec<ListSymbol>,
}

/// In-memory server state: every open document, keyed by URI, kept in sync with
/// the editor's buffers (not the on-disk copy).
#[derive(Default)]
pub struct Server {
    documents: HashMap<Url, Document>,
}

impl Server {
    /// Apply a `textDocument/*` notification to the document store, returning the
    /// diagnostics to publish for the affected document (recomputed on
    /// open/change, cleared on close). Unknown notifications and malformed params
    /// are ignored, as LSP servers should, and yield `None`.
    fn apply_notification(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Option<PublishDiagnosticsParams> {
        match method {
            DidOpenTextDocument::METHOD => {
                let p: DidOpenTextDocumentParams = serde_json::from_value(params).ok()?;
                let uri = p.text_document.uri;
                let analysis = analyze(&uri, &p.text_document.text);
                self.documents.insert(
                    uri.clone(),
                    Document {
                        text: p.text_document.text,
                        symbols: analysis.symbols,
                    },
                );
                Some(publish(uri, analysis.diagnostics))
            }
            DidChangeTextDocument::METHOD => {
                let p: DidChangeTextDocumentParams = serde_json::from_value(params).ok()?;
                // Full-sync: the final content change carries the whole document.
                let change = p.content_changes.into_iter().next_back()?;
                let uri = p.text_document.uri;
                let analysis = analyze(&uri, &change.text);
                let doc = self.documents.entry(uri.clone()).or_default();
                doc.text = change.text;
                // Keep the previous symbols when the new scan found none (e.g. a
                // transient syntax error), so completion doesn't blank out.
                if !analysis.symbols.is_empty() {
                    doc.symbols = analysis.symbols;
                }
                Some(publish(uri, analysis.diagnostics))
            }
            DidCloseTextDocument::METHOD => {
                let p: DidCloseTextDocumentParams = serde_json::from_value(params).ok()?;
                let uri = p.text_document.uri;
                self.documents.remove(&uri);
                // Publish an empty set to clear the editor's squiggles.
                Some(publish(uri, Vec::new()))
            }
            _ => None,
        }
    }

    /// Completion candidates for the document at `uri`: instruction mnemonics
    /// and directives (always), plus that document's in-scope labels/constants
    /// and macro names. Filtering by the typed prefix is left to the client.
    fn complete(&self, uri: &Url) -> Vec<CompletionItem> {
        let mut items = mnemonic_items();
        items.extend(directive_items());
        if let Some(doc) = self.documents.get(uri) {
            items.extend(doc.symbols.iter().map(|s| symbol_item(&s.name)));
            items.extend(macro_names(&doc.text).iter().map(|name| macro_item(name)));
        }
        items
    }

    /// Produce a whole-document formatting edit for `uri`, or `None` if the
    /// document is unknown. An already-formatted document yields no edits.
    fn format_document(&self, uri: &Url) -> Option<Vec<TextEdit>> {
        let text = &self.documents.get(uri)?.text;
        let formatted = tooling::format(text);
        if formatted == *text {
            return Some(Vec::new());
        }
        Some(vec![TextEdit {
            range: full_range(text),
            new_text: formatted,
        }])
    }

    /// Full-document semantic tokens for `uri`, or `None` if it is unknown.
    fn semantic_tokens(&self, uri: &Url) -> Option<SemanticTokensResult> {
        let text = &self.documents.get(uri)?.text;
        Some(SemanticTokensResult::Tokens(SemanticTokens {
            result_id: None,
            data: semantic_tokens(text),
        }))
    }

    /// An outline of the document at `uri`: every label, constant, and macro
    /// defined in the buffer, with its name range. `None` if `uri` is unknown.
    fn document_symbols(&self, uri: &Url) -> Option<Vec<DocumentSymbol>> {
        let text = &self.documents.get(uri)?.text;
        Some(
            definitions(text)
                .into_iter()
                .map(|d| document_symbol(&d))
                .collect(),
        )
    }

    /// Resolve go-to-definition for the symbol under `pos` in the document at
    /// `uri`: the location of its defining label/constant/macro, if any.
    fn goto_definition(&self, uri: &Url, pos: Position) -> Option<Location> {
        let text = &self.documents.get(uri)?.text;
        let name = word_at(text, pos)?;
        let def = definitions(text).into_iter().find(|d| d.name == name)?;
        Some(Location::new(uri.clone(), def.range))
    }

    /// All references to the symbol under `pos` in the document at `uri`: every
    /// identifier occurrence with the same name. The definition itself is
    /// included when `include_declaration` is set.
    fn references(
        &self,
        uri: &Url,
        pos: Position,
        include_declaration: bool,
    ) -> Option<Vec<Location>> {
        let text = &self.documents.get(uri)?.text;
        let name = word_at(text, pos)?;
        let defs = definitions(text);
        let locations = located_lexemes(text)
            .into_iter()
            .filter(|t| t.kind == LexKind::Ident && t.text == name)
            .filter(|t| {
                include_declaration || !defs.iter().any(|d| d.name == name && d.range == t.range)
            })
            .map(|t| Location::new(uri.clone(), t.range))
            .collect();
        Some(locations)
    }

    /// Hover information for the token under `pos` in the document at `uri`:
    /// opcode/addressing details for a mnemonic, the description for a directive,
    /// or the resolved value for a defined symbol.
    fn hover(&self, uri: &Url, pos: Position) -> Option<Hover> {
        let doc = self.documents.get(uri)?;
        let token = token_at(&doc.text, pos)?;
        let markdown = match token.kind {
            LexKind::Directive => directive_hover(token.text)?,
            LexKind::Ident => ident_hover(token.text, &doc.text, &doc.symbols)?,
            _ => return None,
        };
        Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: MarkupKind::Markdown,
                value: markdown,
            }),
            range: Some(token.range),
        })
    }

    /// Number of documents currently open.
    #[must_use]
    pub fn document_count(&self) -> usize {
        self.documents.len()
    }

    /// The current text of an open document, if tracked.
    #[must_use]
    pub fn document_text(&self, uri: &Url) -> Option<&str> {
        self.documents.get(uri).map(|d| d.text.as_str())
    }
}

/// The outcome of a diagnostic scan of a buffer for the language server.
struct Analysis {
    diagnostics: Vec<Diagnostic>,
    /// Symbols (labels/constants, with values) from the best-effort assembly,
    /// for completion and hover. Empty when a syntax error blocked semantic
    /// analysis.
    symbols: Vec<ListSymbol>,
}

/// Scan `text` (the buffer at `uri`) for *all* errors and warnings — with
/// recovery, so several problems surface at once — and translate them into LSP
/// diagnostics with token-accurate ranges.
fn analyze(uri: &Url, text: &str) -> Analysis {
    let path = uri
        .to_file_path()
        .unwrap_or_else(|()| PathBuf::from(uri.path()));
    let top_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("stdin")
        .to_string();

    let found = diagnose_source_as(&path, text, &Options::default());
    let mut diagnostics = Vec::with_capacity(found.errors.len() + found.warnings.len());
    for d in &found.errors {
        diagnostics.push(to_diagnostic(d, DiagnosticSeverity::ERROR, &top_name, text));
    }
    for d in &found.warnings {
        diagnostics.push(to_diagnostic(
            d,
            DiagnosticSeverity::WARNING,
            &top_name,
            text,
        ));
    }
    Analysis {
        diagnostics,
        symbols: found.symbols,
    }
}

/// Package diagnostics for a URI (version-less; full-document publish).
fn publish(uri: Url, diagnostics: Vec<Diagnostic>) -> PublishDiagnosticsParams {
    PublishDiagnosticsParams {
        uri,
        diagnostics,
        version: None,
    }
}

/// Convert a core [`Diag`] into an LSP diagnostic with a token-accurate range.
/// A diagnostic that originates in the top-level buffer maps to its own line;
/// one from an included file (whose lines aren't in this buffer) is anchored at
/// the top with its origin noted in the message.
fn to_diagnostic(
    diag: &Diag,
    severity: DiagnosticSeverity,
    top_name: &str,
    text: &str,
) -> Diagnostic {
    let (line, message) = if diag.file == top_name {
        (diag.line.saturating_sub(1), diag.message.clone())
    } else {
        (0, format!("{} [{}:{}]", diag.message, diag.file, diag.line))
    };
    Diagnostic {
        range: diagnostic_range(text, line, &message),
        severity: Some(severity),
        source: Some("nessemble".to_string()),
        message,
        ..Default::default()
    }
}

/// The range to highlight for a diagnostic on `line`: the backtick-quoted
/// subject of the message if it occurs on the line (token-accurate), otherwise
/// the line's significant span (its content with indentation/trailing trimmed).
fn diagnostic_range(text: &str, line: u32, message: &str) -> Range {
    let src = text.lines().nth(line as usize).unwrap_or("");

    // Default: the trimmed content span (byte offsets, always char boundaries
    // since the trimmed bytes are ASCII whitespace).
    let (mut start, mut end) = {
        let trimmed = src.trim();
        if trimmed.is_empty() {
            (0, 0)
        } else {
            let lead = src.len() - src.trim_start().len();
            (lead, lead + trimmed.len())
        }
    };

    // Narrow to a `quoted` subject present on the line.
    if let Some(subject) = quoted_subject(message) {
        if let Some(pos) = src.find(subject) {
            start = pos;
            end = pos + subject.len();
        }
    }

    Range::new(
        Position::new(line, utf16_col(src, start)),
        Position::new(line, utf16_col(src, end)),
    )
}

/// The text between the first pair of backticks in `message`, if any.
fn quoted_subject(message: &str) -> Option<&str> {
    let start = message.find('`')? + 1;
    let end = message[start..].find('`')? + start;
    Some(&message[start..end])
}

/// UTF-16 column of a byte offset within `line` (LSP columns are UTF-16). The
/// offset must fall on a character boundary.
fn utf16_col(line: &str, byte: usize) -> u32 {
    line[..byte].encode_utf16().count() as u32
}

/// UTF-16 code-unit length of a string (LSP measures positions in UTF-16).
fn utf16_len(s: &str) -> u32 {
    s.encode_utf16().count() as u32
}

/// The range covering the entire document, `(0,0)` to the end of the last line.
fn full_range(text: &str) -> Range {
    let last_line = text.split('\n').next_back().unwrap_or("");
    let last_index = text.split('\n').count().saturating_sub(1) as u32;
    Range::new(
        Position::new(0, 0),
        Position::new(last_index, utf16_len(last_line)),
    )
}

/// Build delta-encoded LSP semantic tokens from the lossless lexeme stream.
/// Whitespace and newlines advance the cursor but emit no token.
fn semantic_tokens(text: &str) -> Vec<SemanticToken> {
    // Lower-cased documented+undocumented mnemonics, for classifying idents.
    let mnemonics: HashSet<String> = OPCODES
        .iter()
        .map(|o| o.mnemonic.to_ascii_lowercase())
        .collect();

    let mut data = Vec::new();
    let (mut line, mut col) = (0u32, 0u32);
    let (mut prev_line, mut prev_col) = (0u32, 0u32);
    for lx in tooling::lex(text) {
        let piece = &text[lx.start..lx.end];
        match lx.kind {
            LexKind::Newline => {
                line += 1;
                col = 0;
            }
            LexKind::Whitespace => col += utf16_len(piece),
            kind => {
                let len = utf16_len(piece);
                let delta_line = line - prev_line;
                let delta_start = if delta_line == 0 { col - prev_col } else { col };
                data.push(SemanticToken {
                    delta_line,
                    delta_start,
                    length: len,
                    token_type: token_type(kind, piece, &mnemonics),
                    token_modifiers_bitset: 0,
                });
                (prev_line, prev_col) = (line, col);
                col += len;
            }
        }
    }
    data
}

/// Map a lexeme to its semantic-token type index (see [`TOKEN_TYPES`]).
fn token_type(kind: LexKind, piece: &str, mnemonics: &HashSet<String>) -> u32 {
    match kind {
        LexKind::Directive => TT_KEYWORD,
        LexKind::Ident => {
            if mnemonics.contains(&piece.to_ascii_lowercase()) {
                TT_FUNCTION
            } else {
                TT_VARIABLE
            }
        }
        LexKind::Number => TT_NUMBER,
        LexKind::String | LexKind::Char => TT_STRING,
        LexKind::Comment => TT_COMMENT,
        // Punctuation, and the unreachable Whitespace/Newline (handled above).
        LexKind::Punct | LexKind::Whitespace | LexKind::Newline => TT_OPERATOR,
    }
}

/// A significant lexeme paired with its source [`Range`] (line + UTF-16
/// columns). Whitespace and newlines are consumed for positioning but not
/// emitted, so consecutive entries are the meaningful tokens in source order.
struct Located<'a> {
    kind: LexKind,
    text: &'a str,
    range: Range,
}

/// Walk the lossless lexeme stream, attaching an LSP [`Range`] to every
/// significant (non-trivia) lexeme. Tokens never span a line break, so a single
/// start/end column pair suffices.
fn located_lexemes(source: &str) -> Vec<Located<'_>> {
    let mut out = Vec::new();
    let (mut line, mut col) = (0u32, 0u32);
    for lx in tooling::lex(source) {
        let piece = &source[lx.start..lx.end];
        match lx.kind {
            LexKind::Newline => {
                line += 1;
                col = 0;
            }
            LexKind::Whitespace => col += utf16_len(piece),
            kind => {
                let len = utf16_len(piece);
                out.push(Located {
                    kind,
                    text: piece,
                    range: Range::new(Position::new(line, col), Position::new(line, col + len)),
                });
                col += len;
            }
        }
    }
    out
}

/// The kind of a symbol definition found by scanning a buffer.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DefKind {
    Label,
    Constant,
    Macro,
}

/// A symbol definition located in the buffer: its name and the [`Range`] of the
/// defining identifier.
struct Definition {
    name: String,
    kind: DefKind,
    range: Range,
}

/// Scan `text` for symbol definitions: a line-initial identifier followed by
/// `:` is a label, one followed by `=` is a constant, and the identifier after
/// `.macrodef` is a macro.
fn definitions(text: &str) -> Vec<Definition> {
    let toks = located_lexemes(text);
    let mut defs = Vec::new();
    for (i, t) in toks.iter().enumerate() {
        let first_on_line = i == 0 || toks[i - 1].range.end.line != t.range.start.line;
        let next_same_line = toks
            .get(i + 1)
            .filter(|n| n.range.start.line == t.range.start.line);
        match t.kind {
            LexKind::Directive if t.text.eq_ignore_ascii_case(".macrodef") => {
                if let Some(name) = next_same_line.filter(|n| n.kind == LexKind::Ident) {
                    defs.push(Definition {
                        name: name.text.to_string(),
                        kind: DefKind::Macro,
                        range: name.range,
                    });
                }
            }
            LexKind::Ident if first_on_line => {
                if let Some(next) = next_same_line.filter(|n| n.kind == LexKind::Punct) {
                    let kind = match next.text {
                        ":" => Some(DefKind::Label),
                        "=" => Some(DefKind::Constant),
                        _ => None,
                    };
                    if let Some(kind) = kind {
                        defs.push(Definition {
                            name: t.text.to_string(),
                            kind,
                            range: t.range,
                        });
                    }
                }
            }
            _ => {}
        }
    }
    defs
}

/// Build a document-outline entry for a definition.
fn document_symbol(def: &Definition) -> DocumentSymbol {
    let (kind, detail) = match def.kind {
        DefKind::Label => (SymbolKind::FUNCTION, "label"),
        DefKind::Constant => (SymbolKind::CONSTANT, "constant"),
        DefKind::Macro => (SymbolKind::FUNCTION, "macro"),
    };
    #[allow(deprecated)] // `deprecated` field is required but unused.
    DocumentSymbol {
        name: def.name.clone(),
        detail: Some(detail.to_string()),
        kind,
        tags: None,
        deprecated: None,
        range: def.range,
        selection_range: def.range,
        children: None,
    }
}

/// The identifier text under `pos`, if the token there is an identifier.
fn word_at(text: &str, pos: Position) -> Option<String> {
    let token = token_at(text, pos)?;
    (token.kind == LexKind::Ident).then(|| token.text.to_string())
}

/// The significant token whose range contains `pos` (inclusive of both ends, so
/// a cursor at a token boundary still resolves).
fn token_at(text: &str, pos: Position) -> Option<Located<'_>> {
    located_lexemes(text).into_iter().find(|t| {
        t.range.start.line == pos.line
            && pos.character >= t.range.start.character
            && pos.character <= t.range.end.character
    })
}

/// Hover markdown for a directive: its spelling and the shared catalog
/// description of the group it belongs to.
fn directive_hover(name: &str) -> Option<String> {
    for (group, desc) in DIRECTIVES {
        let listed = group
            .split(['/', ' '])
            .map(str::trim)
            .any(|n| n.eq_ignore_ascii_case(name));
        if listed {
            return Some(format!("**{name}** (directive)\n\n{desc}"));
        }
    }
    None
}

/// Hover markdown for an identifier: opcode/addressing details if it is a
/// mnemonic, the resolved value if it is a defined symbol, or a macro note.
fn ident_hover(name: &str, text: &str, symbols: &[ListSymbol]) -> Option<String> {
    if let Some(md) = mnemonic_hover(name) {
        return Some(md);
    }
    if let Some(sym) = symbols.iter().find(|s| s.name == name) {
        let kind = if sym.label { "label" } else { "constant" };
        return Some(format!(
            "**{}** ({}) = {} (`{}`)",
            sym.name,
            kind,
            sym.value,
            format_hex(sym.value),
        ));
    }
    if macro_names(text).iter().any(|m| m == name) {
        return Some(format!("**{name}** (macro)"));
    }
    None
}

/// Hover markdown for an instruction mnemonic: a table of its addressing modes
/// with opcode byte, length, and cycle count. `None` if `name` is not a
/// mnemonic.
fn mnemonic_hover(name: &str) -> Option<String> {
    use std::fmt::Write as _;

    let rows: Vec<&nessemble_isa::Opcode> = OPCODES
        .iter()
        .filter(|o| o.mnemonic.eq_ignore_ascii_case(name))
        .collect();
    if rows.is_empty() {
        return None;
    }
    let mnemonic = rows[0].mnemonic;
    let mut md = format!("**{mnemonic}** (instruction)\n\n");
    md.push_str("| mode | opcode | bytes | cycles |\n");
    md.push_str("| --- | --- | --- | --- |\n");
    for op in rows {
        let cycles = if op.is_boundary() {
            format!("{}+", op.timing)
        } else {
            op.timing.to_string()
        };
        let note = if op.is_undocumented() { " ⚠︎" } else { "" };
        // Writing to a String is infallible.
        let _ = writeln!(
            md,
            "| {}{} | ${:02X} | {} | {} |",
            op.mode.label(),
            note,
            op.opcode,
            op.length,
            cycles,
        );
    }
    Some(md)
}

/// Format a symbol value as `$`-prefixed hex, sized to a byte or word.
fn format_hex(value: i64) -> String {
    if (0..=0xFF).contains(&value) {
        format!("${value:02X}")
    } else if (0..=0xFFFF).contains(&value) {
        format!("${value:04X}")
    } else {
        format!("${value:X}")
    }
}

/// Completion items for every documented instruction mnemonic (lower-cased to
/// match the usual nessemble style), detailing its addressing modes.
fn mnemonic_items() -> Vec<CompletionItem> {
    let mut modes: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for op in &OPCODES {
        if op.is_undocumented() {
            continue;
        }
        modes.entry(op.mnemonic).or_default().push(op.mode.label());
    }
    modes
        .into_iter()
        .map(|(mnemonic, modes)| CompletionItem {
            label: mnemonic.to_ascii_lowercase(),
            kind: Some(CompletionItemKind::FUNCTION),
            detail: Some(format!("instruction — {}", modes.join(", "))),
            ..Default::default()
        })
        .collect()
}

/// Completion items for every directive spelling in the shared catalog.
fn directive_items() -> Vec<CompletionItem> {
    let mut items = Vec::new();
    for (group, desc) in DIRECTIVES {
        for name in group
            .split(['/', ' '])
            .map(str::trim)
            .filter(|n| n.starts_with('.'))
        {
            items.push(CompletionItem {
                label: name.to_string(),
                kind: Some(CompletionItemKind::KEYWORD),
                detail: Some((*desc).to_string()),
                ..Default::default()
            });
        }
    }
    items
}

fn symbol_item(name: &str) -> CompletionItem {
    CompletionItem {
        label: name.to_string(),
        kind: Some(CompletionItemKind::VARIABLE),
        detail: Some("label / constant".to_string()),
        ..Default::default()
    }
}

fn macro_item(name: &str) -> CompletionItem {
    CompletionItem {
        label: name.to_string(),
        kind: Some(CompletionItemKind::FUNCTION),
        detail: Some("macro".to_string()),
        ..Default::default()
    }
}

/// Macro names defined in the buffer (`.macrodef NAME`), which aren't part of
/// the assembler's symbol table.
fn macro_names(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let mut tokens = line.split_whitespace();
            if tokens.next() == Some(".macrodef") {
                tokens.next().map(str::to_string)
            } else {
                None
            }
        })
        .collect()
}

/// The capabilities advertised at `initialize`: full-text document sync and
/// completion (triggered on `.` for directives, plus normal identifier typing).
/// Diagnostics are pushed (`publishDiagnostics`) and need no capability flag.
fn server_capabilities() -> ServerCapabilities {
    ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec![".".to_string()]),
            ..Default::default()
        }),
        document_formatting_provider: Some(OneOf::Left(true)),
        semantic_tokens_provider: Some(SemanticTokensServerCapabilities::SemanticTokensOptions(
            SemanticTokensOptions {
                work_done_progress_options: WorkDoneProgressOptions::default(),
                legend: SemanticTokensLegend {
                    token_types: TOKEN_TYPES.to_vec(),
                    token_modifiers: Vec::new(),
                },
                range: Some(false),
                full: Some(SemanticTokensFullOptions::Bool(true)),
            },
        )),
        document_symbol_provider: Some(OneOf::Left(true)),
        definition_provider: Some(OneOf::Left(true)),
        references_provider: Some(OneOf::Left(true)),
        hover_provider: Some(lsp_types::HoverProviderCapability::Simple(true)),
        ..Default::default()
    }
}

/// Run the language server over stdio until the client shuts it down.
///
/// # Errors
/// Returns an error if the LSP handshake fails, a message cannot be sent, or the
/// stdio transport threads fail to join.
pub fn run() -> LspResult<()> {
    let (connection, io_threads) = Connection::stdio();
    serve(&connection)?;
    // Drop the connection before joining so the writer thread's channel closes;
    // otherwise `io_threads.join()` blocks forever waiting on a live sender.
    drop(connection);
    io_threads.join()?;
    Ok(())
}

/// Perform the initialize handshake, then process messages until shutdown,
/// returning the final [`Server`] state. The stdio entry point discards it;
/// tests inspect it.
fn serve(connection: &Connection) -> LspResult<Server> {
    let capabilities = serde_json::to_value(server_capabilities())?;
    let _client_params = connection.initialize(capabilities)?;
    main_loop(connection)
}

/// The message loop: answer requests (shutdown, completion) and, for each
/// document notification, update the store and push refreshed diagnostics.
fn main_loop(connection: &Connection) -> LspResult<Server> {
    let mut server = Server::default();
    for msg in &connection.receiver {
        match msg {
            Message::Request(req) => {
                if connection.handle_shutdown(&req)? {
                    return Ok(server);
                }
                let resp = match req.method.as_str() {
                    Completion::METHOD => {
                        let items = serde_json::from_value::<CompletionParams>(req.params)
                            .map(|p| server.complete(&p.text_document_position.text_document.uri))
                            .unwrap_or_default();
                        Response::new_ok(req.id, CompletionResponse::Array(items))
                    }
                    Formatting::METHOD => {
                        let edits = serde_json::from_value::<DocumentFormattingParams>(req.params)
                            .ok()
                            .and_then(|p| server.format_document(&p.text_document.uri))
                            .unwrap_or_default();
                        Response::new_ok(req.id, edits)
                    }
                    SemanticTokensFullRequest::METHOD => {
                        let result = serde_json::from_value::<SemanticTokensParams>(req.params)
                            .ok()
                            .and_then(|p| server.semantic_tokens(&p.text_document.uri));
                        Response::new_ok(req.id, result)
                    }
                    DocumentSymbolRequest::METHOD => {
                        let result = serde_json::from_value::<DocumentSymbolParams>(req.params)
                            .ok()
                            .and_then(|p| server.document_symbols(&p.text_document.uri))
                            .map(DocumentSymbolResponse::Nested);
                        Response::new_ok(req.id, result)
                    }
                    GotoDefinition::METHOD => {
                        let result = serde_json::from_value::<GotoDefinitionParams>(req.params)
                            .ok()
                            .and_then(|p| {
                                let tdp = p.text_document_position_params;
                                server.goto_definition(&tdp.text_document.uri, tdp.position)
                            })
                            .map(GotoDefinitionResponse::Scalar);
                        Response::new_ok(req.id, result)
                    }
                    References::METHOD => {
                        let result = serde_json::from_value::<ReferenceParams>(req.params)
                            .ok()
                            .and_then(|p| {
                                let tdp = p.text_document_position;
                                server.references(
                                    &tdp.text_document.uri,
                                    tdp.position,
                                    p.context.include_declaration,
                                )
                            });
                        Response::new_ok(req.id, result)
                    }
                    HoverRequest::METHOD => {
                        let result = serde_json::from_value::<HoverParams>(req.params)
                            .ok()
                            .and_then(|p| {
                                let tdp = p.text_document_position_params;
                                server.hover(&tdp.text_document.uri, tdp.position)
                            });
                        Response::new_ok(req.id, result)
                    }
                    other => Response::new_err(
                        req.id,
                        ErrorCode::MethodNotFound as i32,
                        format!("unhandled request: {other}"),
                    ),
                };
                connection.sender.send(Message::Response(resp))?;
            }
            Message::Notification(note) => {
                if let Some(params) = server.apply_notification(&note.method, note.params) {
                    connection.sender.send(Message::Notification(Notification {
                        method: PublishDiagnostics::METHOD.to_string(),
                        params: serde_json::to_value(params)?,
                    }))?;
                }
            }
            Message::Response(_) => {}
        }
    }
    Ok(server)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_server::{Request, RequestId};

    fn open_params(uri: &str, text: &str) -> serde_json::Value {
        serde_json::json!({
            "textDocument": {
                "uri": uri, "languageId": "nessemble", "version": 1, "text": text
            }
        })
    }

    /// Receive the next response, skipping any pushed notifications
    /// (e.g. `publishDiagnostics`) that precede it.
    fn recv_response(client: &Connection) -> Response {
        loop {
            if let Message::Response(r) = client.receiver.recv().unwrap() {
                return r;
            }
        }
    }

    fn labels(items: Vec<CompletionItem>) -> Vec<String> {
        items.into_iter().map(|i| i.label).collect()
    }

    #[test]
    fn analyze_flags_an_unknown_opcode() {
        let uri = Url::parse("file:///bad.asm").unwrap();
        let diags = analyze(&uri, "  notareal\n").diagnostics;
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(diags[0].range.start.line, 0);
        assert_eq!(diags[0].source.as_deref(), Some("nessemble"));
    }

    #[test]
    fn analyze_reports_the_correct_line() {
        let uri = Url::parse("file:///multi.asm").unwrap();
        // Two valid lines, then an unknown opcode on line 3 (0-based line 2).
        let diags = analyze(&uri, "  lda #$00\n  nop\n  notareal\n").diagnostics;
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].range.start.line, 2);
    }

    #[test]
    fn analyze_collects_multiple_errors() {
        let uri = Url::parse("file:///m.asm").unwrap();
        let diags = analyze(&uri, "  notareal\n  alsobad\n").diagnostics;
        assert_eq!(diags.len(), 2);
        assert!(diags
            .iter()
            .all(|d| d.severity == Some(DiagnosticSeverity::ERROR)));
    }

    #[test]
    fn diagnostic_range_narrows_to_the_offending_token() {
        let uri = Url::parse("file:///r.asm").unwrap();
        // `foo` is undefined; the range should cover exactly `foo` (cols 6..9),
        // not the whole line.
        let diags = analyze(&uri, "  lda foo\n").diagnostics;
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].range.start.character, 6);
        assert_eq!(diags[0].range.end.character, 9);
    }

    #[test]
    fn analyze_is_clean_for_valid_source() {
        let uri = Url::parse("file:///ok.asm").unwrap();
        assert!(analyze(&uri, "  lda #$00\n  nop\n").diagnostics.is_empty());
    }

    #[test]
    fn completion_offers_mnemonics_directives_symbols_and_macros() {
        let mut server = Server::default();
        let text = ".macrodef greet\n  nop\n.endm\nstart:\n  lda #$00\ncount = 5\n";
        server.apply_notification(
            DidOpenTextDocument::METHOD,
            open_params("file:///c.asm", text),
        );
        let uri = Url::parse("file:///c.asm").unwrap();
        let ls = labels(server.complete(&uri));
        assert!(ls.iter().any(|l| l == "lda"), "missing mnemonic");
        assert!(ls.iter().any(|l| l == ".db"), "missing directive");
        assert!(ls.iter().any(|l| l == "start"), "missing label");
        assert!(ls.iter().any(|l| l == "count"), "missing constant");
        assert!(ls.iter().any(|l| l == "greet"), "missing macro");
    }

    #[test]
    fn formatting_produces_a_whole_document_edit() {
        let mut server = Server::default();
        server.apply_notification(
            DidOpenTextDocument::METHOD,
            open_params("file:///f.asm", "lda #$00\n"),
        );
        let uri = Url::parse("file:///f.asm").unwrap();
        let edits = server.format_document(&uri).expect("known document");
        assert_eq!(edits.len(), 1);
        assert_eq!(edits[0].new_text, "    lda #$00\n");
    }

    #[test]
    fn formatting_an_already_formatted_document_is_a_no_op() {
        let mut server = Server::default();
        server.apply_notification(
            DidOpenTextDocument::METHOD,
            open_params("file:///g.asm", "    lda #$00\n"),
        );
        let uri = Url::parse("file:///g.asm").unwrap();
        assert!(server.format_document(&uri).unwrap().is_empty());
    }

    #[test]
    fn semantic_tokens_classify_a_mnemonic_and_number() {
        let toks = semantic_tokens("lda #$00\n");
        // First token is the mnemonic `lda` at (0,0), length 3.
        assert_eq!(
            (toks[0].delta_line, toks[0].delta_start, toks[0].length),
            (0, 0, 3)
        );
        assert_eq!(toks[0].token_type, TT_FUNCTION);
        assert!(toks.iter().any(|t| t.token_type == TT_NUMBER));
    }

    /// Drive the server through a full lifecycle over an in-memory connection,
    /// confirming it publishes diagnostics and answers completion requests.
    #[test]
    fn serves_diagnostics_and_completion_over_the_lifecycle() {
        let (server_conn, client) = Connection::memory();
        let server = std::thread::spawn(move || serve(&server_conn));

        // initialize → response → initialized
        client
            .sender
            .send(Message::Request(Request {
                id: RequestId::from(1),
                method: "initialize".into(),
                params: serde_json::json!({ "capabilities": {} }),
            }))
            .unwrap();
        let _init = recv_response(&client);
        client
            .sender
            .send(Message::Notification(Notification {
                method: "initialized".into(),
                params: serde_json::json!({}),
            }))
            .unwrap();

        // didOpen an erroring buffer → publishDiagnostics with 1 error.
        client
            .sender
            .send(Message::Notification(Notification {
                method: "textDocument/didOpen".into(),
                params: open_params("file:///a.asm", "  notareal\n"),
            }))
            .unwrap();
        let msg = client.receiver.recv().unwrap();
        let Message::Notification(note) = msg else {
            panic!("expected a publishDiagnostics notification, got {msg:?}");
        };
        assert_eq!(note.method, "textDocument/publishDiagnostics");
        let published: PublishDiagnosticsParams = serde_json::from_value(note.params).unwrap();
        assert_eq!(published.diagnostics.len(), 1);
        assert_eq!(
            published.diagnostics[0].severity,
            Some(DiagnosticSeverity::ERROR)
        );

        // completion request → an array including a known mnemonic.
        client
            .sender
            .send(Message::Request(Request {
                id: RequestId::from(2),
                method: "textDocument/completion".into(),
                params: serde_json::json!({
                    "textDocument": { "uri": "file:///a.asm" },
                    "position": { "line": 0, "character": 2 }
                }),
            }))
            .unwrap();
        let resp = recv_response(&client);
        let value = resp.result.expect("completion result");
        let CompletionResponse::Array(items) = serde_json::from_value(value).unwrap() else {
            panic!("expected a completion array");
        };
        assert!(labels(items).iter().any(|l| l == "lda"));

        // hover request → markup describing the `lda` mnemonic. Exercises the
        // Phase 5 routing and response serialization over the transport.
        client
            .sender
            .send(Message::Request(Request {
                id: RequestId::from(3),
                method: "textDocument/hover".into(),
                params: serde_json::json!({
                    "textDocument": { "uri": "file:///a.asm" },
                    "position": { "line": 0, "character": 2 }
                }),
            }))
            .unwrap();
        // `notareal` isn't a mnemonic, so this hover is null — but the request
        // must still round-trip as a successful (null) response.
        let hover = recv_response(&client);
        assert!(hover.result.is_some());
        assert!(hover.error.is_none());

        // shutdown → response → exit
        client
            .sender
            .send(Message::Request(Request {
                id: RequestId::from(4),
                method: "shutdown".into(),
                params: serde_json::Value::Null,
            }))
            .unwrap();
        let _ = recv_response(&client);
        client
            .sender
            .send(Message::Notification(Notification {
                method: "exit".into(),
                params: serde_json::Value::Null,
            }))
            .unwrap();

        let server = server.join().unwrap().expect("server ran cleanly");
        assert_eq!(server.document_count(), 1);
    }

    fn open(server: &mut Server, uri: &str, text: &str) {
        server.apply_notification(DidOpenTextDocument::METHOD, open_params(uri, text));
    }

    #[test]
    fn document_symbols_outline_labels_constants_and_macros() {
        let mut server = Server::default();
        let text = ".macrodef greet\n  nop\n.endm\nstart:\n  lda #$00\ncount = 5\n";
        open(&mut server, "file:///o.asm", text);
        let uri = Url::parse("file:///o.asm").unwrap();
        let syms = server.document_symbols(&uri).expect("known document");
        let by_name: Vec<(&str, SymbolKind)> =
            syms.iter().map(|s| (s.name.as_str(), s.kind)).collect();
        assert!(by_name.contains(&("greet", SymbolKind::FUNCTION)));
        assert!(by_name.contains(&("start", SymbolKind::FUNCTION)));
        assert!(by_name.contains(&("count", SymbolKind::CONSTANT)));
        // `start` is a label defined on line 3 (0-based).
        let start = syms.iter().find(|s| s.name == "start").unwrap();
        assert_eq!(start.selection_range.start.line, 3);
        assert_eq!(start.detail.as_deref(), Some("label"));
    }

    #[test]
    fn goto_definition_jumps_to_the_label() {
        let mut server = Server::default();
        // A label defined on line 0, referenced by `jmp` on line 1.
        let text = "start:\n  jmp start\n";
        open(&mut server, "file:///d.asm", text);
        let uri = Url::parse("file:///d.asm").unwrap();
        // Cursor on `start` in the `jmp` operand (line 1, within cols 6..11).
        let loc = server
            .goto_definition(&uri, Position::new(1, 7))
            .expect("definition found");
        assert_eq!(loc.uri, uri);
        assert_eq!(loc.range.start, Position::new(0, 0));
        assert_eq!(loc.range.end, Position::new(0, 5));
    }

    #[test]
    fn references_lists_every_occurrence() {
        let mut server = Server::default();
        let text = "start:\n  jmp start\n  jmp start\n";
        open(&mut server, "file:///e.asm", text);
        let uri = Url::parse("file:///e.asm").unwrap();
        // Including the declaration: the label plus two uses.
        let all = server
            .references(&uri, Position::new(1, 7), true)
            .expect("references found");
        assert_eq!(all.len(), 3);
        // Excluding the declaration: only the two uses.
        let uses = server
            .references(&uri, Position::new(1, 7), false)
            .expect("references found");
        assert_eq!(uses.len(), 2);
        assert!(uses.iter().all(|l| l.range.start.line != 0));
    }

    #[test]
    fn hover_shows_opcode_details_for_a_mnemonic() {
        let mut server = Server::default();
        open(&mut server, "file:///h.asm", "  lda #$00\n");
        let uri = Url::parse("file:///h.asm").unwrap();
        let hover = server
            .hover(&uri, Position::new(0, 3))
            .expect("hover on lda");
        let HoverContents::Markup(md) = hover.contents else {
            panic!("expected markup hover");
        };
        assert!(md.value.contains("**LDA**"));
        assert!(md.value.contains("immediate"));
        assert!(md.value.contains("$A9"));
    }

    #[test]
    fn hover_shows_a_directive_description() {
        let mut server = Server::default();
        open(&mut server, "file:///hd.asm", "  .db $00\n");
        let uri = Url::parse("file:///hd.asm").unwrap();
        let hover = server
            .hover(&uri, Position::new(0, 3))
            .expect("hover on .db");
        let HoverContents::Markup(md) = hover.contents else {
            panic!("expected markup hover");
        };
        assert!(md.value.contains("(directive)"));
    }

    #[test]
    fn hover_shows_a_symbol_value() {
        let mut server = Server::default();
        open(&mut server, "file:///hs.asm", "count = 5\n  lda #count\n");
        let uri = Url::parse("file:///hs.asm").unwrap();
        // Cursor on `count` in the operand (line 1).
        let hover = server
            .hover(&uri, Position::new(1, 8))
            .expect("hover on count");
        let HoverContents::Markup(md) = hover.contents else {
            panic!("expected markup hover");
        };
        assert!(md.value.contains("count"));
        assert!(md.value.contains("(constant)"));
        assert!(md.value.contains("$05"));
    }

    #[test]
    fn hover_on_whitespace_is_none() {
        let mut server = Server::default();
        open(&mut server, "file:///hw.asm", "  lda #$00\n");
        let uri = Url::parse("file:///hw.asm").unwrap();
        assert!(server.hover(&uri, Position::new(5, 0)).is_none());
    }

    /// Closing a document removes it from the store and clears its diagnostics.
    #[test]
    fn close_removes_the_document() {
        let mut server = Server::default();
        server.apply_notification(
            DidOpenTextDocument::METHOD,
            open_params("file:///b.asm", "nop\n"),
        );
        assert_eq!(server.document_count(), 1);
        let cleared = server
            .apply_notification(
                DidCloseTextDocument::METHOD,
                serde_json::json!({ "textDocument": { "uri": "file:///b.asm" } }),
            )
            .expect("close yields a clear");
        assert!(cleared.diagnostics.is_empty());
        assert_eq!(server.document_count(), 0);
    }
}
