//! Language Server Protocol implementation for nessemble's assembly flavor.
//!
//! A synchronous [`lsp-server`](lsp_server) stdio server. It completes the LSP
//! lifecycle (`initialize` → advertise capabilities → `initialized` →
//! `shutdown`/`exit`), tracks open documents via
//! `textDocument/didOpen|didChange|didClose`, and provides:
//!
//! - **Diagnostics** (Phase 1): each open buffer is assembled and its
//!   errors/warnings are published via `textDocument/publishDiagnostics`.
//!   Line-level for now — today's core reports a line but no column, so each
//!   diagnostic covers its whole line.
//! - **Completion** (Phase 2): `textDocument/completion` offers instruction
//!   mnemonics (from `nessemble-isa`), directives, in-scope labels/constants,
//!   and macro names.
//! - **Formatting & highlighting** (Phase 3): `textDocument/formatting` tidies a
//!   buffer (via `nessemble_core::tooling::format`), and
//!   `textDocument/semanticTokens/full` classifies tokens for highlighting, both
//!   built on the lossless tooling lexer.
//!
//! Column-accurate diagnostics and navigation arrive in later phases.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use lsp_server::{Connection, ErrorCode, Message, Notification, Response};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Notification as _,
    PublishDiagnostics,
};
use lsp_types::request::{Completion, Formatting, Request as _, SemanticTokensFullRequest};
use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionOptions, CompletionParams, CompletionResponse,
    Diagnostic, DiagnosticSeverity, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, DocumentFormattingParams, OneOf, Position, PublishDiagnosticsParams,
    Range, SemanticToken, SemanticTokenType, SemanticTokens, SemanticTokensFullOptions,
    SemanticTokensLegend, SemanticTokensOptions, SemanticTokensParams, SemanticTokensResult,
    SemanticTokensServerCapabilities, ServerCapabilities, TextDocumentSyncCapability,
    TextDocumentSyncKind, TextEdit, Url, WorkDoneProgressOptions,
};

use nessemble_core::tooling::{self, LexKind};
use nessemble_core::{assemble_source_as, AssembleError, Diag, Options};
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

/// Per-document state: the current buffer text plus the user-defined symbol
/// names (labels/constants) from the last successful assembly, used for
/// completion.
#[derive(Default)]
struct Document {
    text: String,
    symbols: Vec<String>,
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
                        symbols: analysis.symbols.unwrap_or_default(),
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
                // Refresh the symbol cache only on a successful assembly, so it
                // survives transient errors while editing.
                if let Some(symbols) = analysis.symbols {
                    doc.symbols = symbols;
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
            items.extend(doc.symbols.iter().map(|name| symbol_item(name)));
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

/// The outcome of assembling a buffer for the language server.
struct Analysis {
    diagnostics: Vec<Diagnostic>,
    /// Symbol names (labels/constants) from a successful assembly, or `None` if
    /// assembly failed — letting the caller keep the previously cached set.
    symbols: Option<Vec<String>>,
}

/// Assemble `text` as the buffer at `uri` and translate the result: warnings and
/// the single error become diagnostics, and a successful assembly yields the
/// symbol names for completion. (Collecting *all* errors at once is Phase 4.)
fn analyze(uri: &Url, text: &str) -> Analysis {
    let path = uri
        .to_file_path()
        .unwrap_or_else(|()| PathBuf::from(uri.path()));
    let top_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("stdin")
        .to_string();

    match assemble_source_as(&path, text, &Options::default()) {
        Ok(assembly) => Analysis {
            diagnostics: assembly
                .warnings
                .iter()
                .map(|d| to_diagnostic(d, DiagnosticSeverity::WARNING, &top_name, text))
                .collect(),
            symbols: Some(assembly.symbols.iter().map(|s| s.name.clone()).collect()),
        },
        Err(AssembleError::Diagnostic(d)) => Analysis {
            diagnostics: vec![to_diagnostic(
                &d,
                DiagnosticSeverity::ERROR,
                &top_name,
                text,
            )],
            symbols: None,
        },
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

/// Convert a core [`Diag`] into a whole-line LSP diagnostic. A diagnostic that
/// originates in the top-level buffer maps to its own line; one from an included
/// file (whose lines aren't in this buffer) is anchored at the top with its
/// origin noted in the message.
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
    let end = line_len_utf16(text, line);
    Diagnostic {
        range: Range::new(Position::new(line, 0), Position::new(line, end)),
        severity: Some(severity),
        source: Some("nessemble".to_string()),
        message,
        ..Default::default()
    }
}

/// UTF-16 length of a 0-based line (LSP character offsets are UTF-16), or 0 if
/// the line is past the end of the text.
fn line_len_utf16(text: &str, line: u32) -> u32 {
    text.lines()
        .nth(line as usize)
        .map_or(0, |l| l.encode_utf16().count() as u32)
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

        // shutdown → response → exit
        client
            .sender
            .send(Message::Request(Request {
                id: RequestId::from(3),
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
