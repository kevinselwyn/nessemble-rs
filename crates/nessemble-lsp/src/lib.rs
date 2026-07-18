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
//!   symbol values, plus the doc comment from the run of line comments directly
//!   above a symbol's definition), all driven by the lossless tooling lexer over
//!   the buffer.
//! - **Workspace-aware diagnostics** (Phase 7): when a workspace folder is open,
//!   a file is analyzed in the context of the `.include` project it belongs to,
//!   so cross-file symbols aren't flagged as undefined.
//! - **Editing aids** (Phase 8): `textDocument/foldingRange` (macro/conditional
//!   blocks and comment runs), `textDocument/rename` (a symbol across open
//!   buffers), and `textDocument/codeAction` (numeric base conversions).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use lsp_server::{Connection, ErrorCode, Message, Notification, Response};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Notification as _,
    PublishDiagnostics,
};
use lsp_types::request::{
    CodeActionRequest, Completion, DocumentSymbolRequest, FoldingRangeRequest, Formatting,
    GotoDefinition, HoverRequest, References, Rename, Request as _, SemanticTokensFullRequest,
};
use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams,
    CodeActionProviderCapability, CompletionItem, CompletionItemKind, CompletionOptions,
    CompletionParams, CompletionResponse, Diagnostic, DiagnosticSeverity,
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DocumentFormattingParams, DocumentSymbol, DocumentSymbolParams, DocumentSymbolResponse,
    FoldingRange, FoldingRangeKind, FoldingRangeParams, FoldingRangeProviderCapability,
    GotoDefinitionParams, GotoDefinitionResponse, Hover, HoverContents, HoverParams, Location,
    MarkupContent, MarkupKind, OneOf, Position, PublishDiagnosticsParams, Range, ReferenceParams,
    RenameParams, SemanticToken, SemanticTokenType, SemanticTokens, SemanticTokensFullOptions,
    SemanticTokensLegend, SemanticTokensOptions, SemanticTokensParams, SemanticTokensResult,
    SemanticTokensServerCapabilities, ServerCapabilities, SymbolKind, TextDocumentSyncCapability,
    TextDocumentSyncKind, TextEdit, Url, WorkDoneProgressOptions, WorkspaceEdit,
};

use nessemble_core::tooling::{self, LexKind};
use nessemble_core::{
    diagnose_project_with, diagnose_source_with, lenient_custom_resolver, parse_pseudo_mapping,
    Diag, ListSymbol, Options, ProjectDiagnostics,
};
use nessemble_isa::{DIRECTIVES, OPCODES};

/// Semantic-token type legend. Each emitted token's `token_type` is the shared
/// `TokenClass::wire_id`, so this array is ordered by that id: index `i` is the
/// `SemanticTokenType` for the class whose `wire_id()` is `i`.
const TOKEN_TYPES: [SemanticTokenType; 7] = [
    SemanticTokenType::KEYWORD,  // 0: directive
    SemanticTokenType::FUNCTION, // 1: instruction mnemonic
    SemanticTokenType::VARIABLE, // 2: identifier (label/constant/register)
    SemanticTokenType::NUMBER,   // 3
    SemanticTokenType::STRING,   // 4: string/char literal
    SemanticTokenType::COMMENT,  // 5
    SemanticTokenType::OPERATOR, // 6: punctuation/operator
];

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
    /// Workspace folder roots (from `initialize`), scanned to discover the
    /// `.include` entry points a file belongs to. Empty ⇒ single-file analysis.
    workspace_roots: Vec<PathBuf>,
    /// URIs we last published *non-empty* diagnostics to, so a file can be
    /// explicitly cleared when its problems are fixed or it leaves the project.
    published: HashSet<Url>,
}

impl Server {
    /// Apply a `textDocument/*` notification to the document store, returning the
    /// diagnostics to publish — potentially for **several** files, since a
    /// project assembly spreads diagnostics across the include graph. Unknown
    /// notifications and malformed params are ignored and yield no publishes.
    fn apply_notification(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Vec<PublishDiagnosticsParams> {
        match method {
            DidOpenTextDocument::METHOD => {
                let Ok(p) = serde_json::from_value::<DidOpenTextDocumentParams>(params) else {
                    return Vec::new();
                };
                let uri = p.text_document.uri;
                self.documents.insert(
                    uri.clone(),
                    Document {
                        text: p.text_document.text,
                        symbols: Vec::new(),
                    },
                );
                self.analyze_and_publish(&uri)
            }
            DidChangeTextDocument::METHOD => {
                let Ok(p) = serde_json::from_value::<DidChangeTextDocumentParams>(params) else {
                    return Vec::new();
                };
                // Full-sync: the final content change carries the whole document.
                let Some(change) = p.content_changes.into_iter().next_back() else {
                    return Vec::new();
                };
                let uri = p.text_document.uri;
                self.documents.entry(uri.clone()).or_default().text = change.text;
                self.analyze_and_publish(&uri)
            }
            DidCloseTextDocument::METHOD => {
                let Ok(p) = serde_json::from_value::<DidCloseTextDocumentParams>(params) else {
                    return Vec::new();
                };
                let uri = p.text_document.uri;
                self.documents.remove(&uri);
                self.published.remove(&uri);
                // Publish an empty set to clear the editor's squiggles.
                vec![publish(uri, Vec::new())]
            }
            _ => Vec::new(),
        }
    }

    /// Recompute diagnostics for the project the changed document belongs to,
    /// refresh its symbol table, and produce the publishes to send — including
    /// empty sets that clear files whose problems are now gone.
    fn analyze_and_publish(&mut self, changed: &Url) -> Vec<PublishDiagnosticsParams> {
        let results = self.compute_diagnostics(changed);

        // Project-wide symbols enable cross-file completion/hover; keep the
        // previous set when a transient error yielded none, so they don't blink.
        if !results.changed_symbols.is_empty() {
            if let Some(doc) = self.documents.get_mut(changed) {
                doc.symbols = results.changed_symbols;
            }
        }

        let mut out = Vec::new();
        let mut now = HashSet::new();
        for (uri, diags) in results.per_file {
            if !diags.is_empty() {
                now.insert(uri.clone());
            }
            out.push(publish(uri, diags));
        }
        // Clear any file that had diagnostics last time but isn't in this result.
        for uri in &self.published {
            if !out.iter().any(|p| &p.uri == uri) {
                out.push(publish(uri.clone(), Vec::new()));
            }
        }
        self.published = now;
        out
    }

    /// Compute diagnostics for the project the changed document belongs to.
    ///
    /// If the workspace's `.include` graph places the changed file inside one or
    /// more entry roots, each root is assembled (with unsaved buffers overlaid)
    /// and the resulting diagnostics are distributed to every open document that
    /// participates. When a file is reached from several roots, only the
    /// diagnostics common to *all* of them are kept, so a symbol defined under
    /// any root is never flagged. Otherwise it falls back to single-file
    /// analysis of the changed buffer.
    fn compute_diagnostics(&self, changed: &Url) -> DiagResults {
        let Some(text) = self.documents.get(changed).map(|d| d.text.clone()) else {
            return DiagResults::default();
        };
        let changed_path = normalize(&uri_to_path(changed));

        // Custom pseudo-ops declared in the workspace's `--pseudo` mapping files,
        // so they aren't reported as unknown directives.
        let known_custom: HashSet<String> = self.custom_scripts().into_keys().collect();

        if self.workspace_roots.is_empty() {
            return single_file(changed, &text, &known_custom);
        }

        // An overlay + reader backed by the open buffers, falling back to disk.
        let overlay_map: HashMap<PathBuf, String> = self
            .documents
            .iter()
            .map(|(u, d)| (normalize(&uri_to_path(u)), d.text.clone()))
            .collect();
        let overlay = |p: &Path| overlay_map.get(&normalize(p)).cloned();
        let read = |p: &Path| overlay(p).or_else(|| std::fs::read_to_string(p).ok());

        // Discover the entry roots whose include-closure contains the file.
        let candidates = scan_source_files(&self.workspace_roots)
            .into_iter()
            .chain(self.documents.keys().map(uri_to_path));
        let graph = build_include_graph(candidates, &read);
        let roots = graph.entry_roots_for(&changed_path);
        if roots.is_empty() {
            return single_file(changed, &text, &known_custom);
        }

        // Assemble each entry root and normalize its file table once.
        let runs: Vec<Run> = roots
            .iter()
            .map(|root| {
                let root_text = read(root).unwrap_or_default();
                Run::from(diagnose_project_with(
                    root,
                    &root_text,
                    &Options::default(),
                    &overlay,
                    lenient_custom_resolver(known_custom.clone()),
                ))
            })
            .collect();

        // Distribute diagnostics to every open document that participates.
        let mut per_file: HashMap<Url, Vec<Diagnostic>> = HashMap::new();
        for (uri, doc) in &self.documents {
            let dpath = normalize(&uri_to_path(uri));
            let sets: Vec<Vec<(DiagnosticSeverity, Diag)>> = runs
                .iter()
                .filter(|r| r.norm_paths.contains(&dpath))
                .map(|r| r.diags_for(&dpath))
                .collect();
            if sets.is_empty() {
                continue;
            }
            let merged = intersect_diag_sets(&sets);
            let lsp = merged
                .into_iter()
                .map(|(sev, d)| project_diag_to_lsp(&d, sev, &doc.text))
                .collect();
            per_file.insert(uri.clone(), lsp);
        }
        // The changed doc always gets an entry so its stale diagnostics clear
        // even if it dropped out of every closure this round.
        per_file.entry(changed.clone()).or_default();

        // Project-wide symbols (deduped by name) for the changed document.
        let mut symbols: Vec<ListSymbol> = runs.iter().flat_map(|r| r.symbols.clone()).collect();
        symbols.sort_by(|a, b| a.name.cmp(&b.name));
        symbols.dedup_by(|a, b| a.name == b.name);

        DiagResults {
            per_file,
            changed_symbols: symbols,
        }
    }

    /// Custom pseudo-op scripts declared in the workspace's `--pseudo`-style
    /// mapping files: directive name (without the dot) → resolved script path.
    ///
    /// A mapping file is any `*.txt` whose `.name = path` entries point at files
    /// that exist relative to the mapping file's own directory — matching how
    /// the CLI's `--pseudo` mapping resolves. Both the workspace (scanned
    /// recursively) and each open document's own directory are searched, so this
    /// works with or without a workspace folder.
    fn custom_scripts(&self) -> HashMap<String, PathBuf> {
        let mut files = scan_mapping_files(&self.workspace_roots);
        for uri in self.documents.keys() {
            if let Some(dir) = uri_to_path(uri).parent() {
                list_txt_files(dir, &mut files);
            }
        }

        let mut map = HashMap::new();
        for file in files {
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            let base = file.parent().map_or_else(PathBuf::new, Path::to_path_buf);
            for (name, rel) in parse_pseudo_mapping(&text) {
                let script = base.join(&rel);
                if script.is_file() {
                    map.entry(name).or_insert(script);
                }
            }
        }
        map
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

    /// Resolve go-to-definition for the token under `pos` in the document at
    /// `uri`. A custom pseudo-op (`.foo`) jumps to its script file; a symbol
    /// jumps to its defining label/constant/macro — found in the current buffer
    /// first, then (with a workspace open) across the `.include` project, so
    /// cmd/ctrl-click reaches a definition in a sibling or parent file.
    fn goto_definition(&self, uri: &Url, pos: Position) -> Option<Location> {
        let token = token_at(&self.documents.get(uri)?.text, pos)?;
        let name = token.text.to_string();
        match token.kind {
            LexKind::Directive => {
                // A custom pseudo-op resolves to the script that implements it.
                let script = self.custom_scripts().remove(name.trim_start_matches('.'))?;
                let url = Url::from_file_path(&script).ok()?;
                Some(Location::new(
                    url,
                    Range::new(Position::new(0, 0), Position::new(0, 0)),
                ))
            }
            LexKind::Ident => self.definition_location(uri, &name),
            _ => None,
        }
    }

    /// The definition of `name` for the document at `uri`: the local definition
    /// if present, else the first one found in the project's include closure.
    fn definition_location(&self, uri: &Url, name: &str) -> Option<Location> {
        if let Some(doc) = self.documents.get(uri) {
            if let Some(def) = definitions(&doc.text).into_iter().find(|d| d.name == name) {
                return Some(Location::new(uri.clone(), def.range));
            }
        }
        if self.workspace_roots.is_empty() {
            return None;
        }

        // Search the include closure of the roots that contain this file.
        let overlay_map: HashMap<PathBuf, String> = self
            .documents
            .iter()
            .map(|(u, d)| (normalize(&uri_to_path(u)), d.text.clone()))
            .collect();
        let read = |p: &Path| {
            overlay_map
                .get(&normalize(p))
                .cloned()
                .or_else(|| std::fs::read_to_string(p).ok())
        };
        let candidates = scan_source_files(&self.workspace_roots)
            .into_iter()
            .chain(self.documents.keys().map(uri_to_path));
        let graph = build_include_graph(candidates, &read);
        let here = normalize(&uri_to_path(uri));
        let mut project: HashSet<PathBuf> = HashSet::new();
        for root in graph.entry_roots_for(&here) {
            project.extend(graph.closure(&root));
        }
        project.remove(&here); // already searched as the local buffer

        for path in project {
            let Some(text) = read(&path) else {
                continue;
            };
            if let Some(def) = definitions(&text).into_iter().find(|d| d.name == name) {
                if let Ok(url) = Url::from_file_path(&path) {
                    return Some(Location::new(url, def.range));
                }
            }
        }
        None
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

    /// Foldable regions in the document at `uri`: macro and conditional blocks,
    /// and runs of consecutive line comments. `None` if `uri` is unknown.
    fn folding_ranges(&self, uri: &Url) -> Option<Vec<FoldingRange>> {
        Some(folding_ranges(&self.documents.get(uri)?.text))
    }

    /// Rename the symbol under `pos` in the document at `uri` to `new_name`,
    /// across every open document (nessemble symbols share one global scope).
    /// `None` if the cursor isn't on an identifier or `new_name` is not a legal
    /// identifier.
    fn rename(&self, uri: &Url, pos: Position, new_name: &str) -> Option<WorkspaceEdit> {
        let name = word_at(&self.documents.get(uri)?.text, pos)?;
        if !is_identifier(new_name) {
            return None;
        }
        let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
        for (doc_uri, doc) in &self.documents {
            let edits: Vec<TextEdit> = located_lexemes(&doc.text)
                .into_iter()
                .filter(|t| t.kind == LexKind::Ident && t.text == name)
                .map(|t| TextEdit {
                    range: t.range,
                    new_text: new_name.to_string(),
                })
                .collect();
            if !edits.is_empty() {
                changes.insert(doc_uri.clone(), edits);
            }
        }
        (!changes.is_empty()).then(|| WorkspaceEdit {
            changes: Some(changes),
            ..Default::default()
        })
    }

    /// Code actions offered for `range` in the document at `uri`: base
    /// conversions when the cursor is on a numeric literal.
    fn code_actions(&self, uri: &Url, range: Range) -> Vec<CodeActionOrCommand> {
        let Some(doc) = self.documents.get(uri) else {
            return Vec::new();
        };
        let Some(token) = token_at(&doc.text, range.start) else {
            return Vec::new();
        };
        if token.kind != LexKind::Number {
            return Vec::new();
        }
        number_conversions(uri, token.text, token.range)
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
fn analyze(uri: &Url, text: &str, known_custom: &HashSet<String>) -> Analysis {
    let path = uri_to_path(uri);
    let top_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("stdin")
        .to_string();

    let found = diagnose_source_with(
        &path,
        text,
        &Options::default(),
        None,
        lenient_custom_resolver(known_custom.clone()),
    );
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

/// The result of a project-aware diagnostic pass: LSP diagnostics keyed by the
/// document to publish them to, plus project-wide symbols for the changed file.
#[derive(Default)]
struct DiagResults {
    per_file: HashMap<Url, Vec<Diagnostic>>,
    changed_symbols: Vec<ListSymbol>,
}

/// Single-file fallback: analyze just the changed buffer (no project context),
/// publishing only for that file.
fn single_file(changed: &Url, text: &str, known_custom: &HashSet<String>) -> DiagResults {
    let analysis = analyze(changed, text, known_custom);
    let mut per_file = HashMap::new();
    per_file.insert(changed.clone(), analysis.diagnostics);
    DiagResults {
        per_file,
        changed_symbols: analysis.symbols,
    }
}

/// One assembled entry root: its diagnostics, and its flattened file table with
/// paths pre-normalized for matching against open documents.
struct Run {
    errors: Vec<Diag>,
    warnings: Vec<Diag>,
    files: Vec<String>,
    norm_paths: Vec<PathBuf>,
    symbols: Vec<ListSymbol>,
}

impl From<ProjectDiagnostics> for Run {
    fn from(pd: ProjectDiagnostics) -> Self {
        let norm_paths = pd.paths.iter().map(|p| normalize(p)).collect();
        Run {
            errors: pd.errors,
            warnings: pd.warnings,
            files: pd.files,
            norm_paths,
            symbols: pd.symbols,
        }
    }
}

impl Run {
    /// This run's diagnostics for the file at normalized path `dpath`, tagged
    /// with severity. A diagnostic belongs to `dpath` when its file name matches
    /// one whose resolved path is `dpath`.
    fn diags_for(&self, dpath: &Path) -> Vec<(DiagnosticSeverity, Diag)> {
        let names: HashSet<&str> = self
            .files
            .iter()
            .zip(&self.norm_paths)
            .filter(|(_, p)| p.as_path() == dpath)
            .map(|(n, _)| n.as_str())
            .collect();
        let errs = self
            .errors
            .iter()
            .filter(|d| names.contains(d.file.as_str()))
            .map(|d| (DiagnosticSeverity::ERROR, d.clone()));
        let warns = self
            .warnings
            .iter()
            .filter(|d| names.contains(d.file.as_str()))
            .map(|d| (DiagnosticSeverity::WARNING, d.clone()));
        errs.chain(warns).collect()
    }
}

/// Keep only the diagnostics common to *every* set (compared by severity, line,
/// and message). With a single set the input is returned unchanged. This is how
/// a symbol defined under *any* entry root escapes being flagged: its "not
/// defined" diagnostic is absent from that root's set, so the intersection drops
/// it.
fn intersect_diag_sets(
    sets: &[Vec<(DiagnosticSeverity, Diag)>],
) -> Vec<(DiagnosticSeverity, Diag)> {
    let Some((first, rest)) = sets.split_first() else {
        return Vec::new();
    };
    if rest.is_empty() {
        return first.clone();
    }
    first
        .iter()
        .filter(|item| rest.iter().all(|s| s.iter().any(|o| same_diag(o, item))))
        .cloned()
        .collect()
}

fn same_diag(a: &(DiagnosticSeverity, Diag), b: &(DiagnosticSeverity, Diag)) -> bool {
    a.0 == b.0 && a.1.line == b.1.line && a.1.message == b.1.message
}

/// Convert a project [`Diag`] (already attributed to a specific file) into an
/// LSP diagnostic on its own line, with a token-accurate range within `text`.
fn project_diag_to_lsp(d: &Diag, severity: DiagnosticSeverity, text: &str) -> Diagnostic {
    let line = d.line.saturating_sub(1);
    Diagnostic {
        range: diagnostic_range(text, line, &d.message),
        severity: Some(severity),
        source: Some("nessemble".to_string()),
        message: d.message.clone(),
        ..Default::default()
    }
}

/// The filesystem path a `file://` URI refers to (falling back to its raw path
/// for non-file URIs, matching how the assembler names buffers).
fn uri_to_path(uri: &Url) -> PathBuf {
    uri.to_file_path()
        .unwrap_or_else(|()| PathBuf::from(uri.path()))
}

/// Normalize a path for identity comparison: canonicalize when it exists on
/// disk (resolving symlinks and `..`), else clean it lexically so unsaved
/// buffers still compare equal.
fn normalize(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| lexical_normalize(path))
}

/// Lexically remove `.` and `..` components without touching the filesystem.
fn lexical_normalize(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Cap on how many source files a workspace scan will enumerate, so a huge or
/// misconfigured workspace can't stall analysis.
const MAX_SCAN_FILES: usize = 4000;

/// Enumerate `*.asm` / `*.s` files under the workspace roots, skipping hidden
/// directories (including `.git`) and common build output.
fn scan_source_files(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for root in roots {
        walk_files(root, &mut out, &is_source_file);
    }
    out
}

/// Enumerate `*.txt` mapping-file candidates under the workspace roots.
fn scan_mapping_files(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for root in roots {
        walk_files(root, &mut out, &is_mapping_file);
    }
    out
}

/// Append the `*.txt` files directly in `dir` (non-recursively) to `out`.
fn list_txt_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file() && is_mapping_file(&path) {
            out.push(path);
        }
    }
}

/// Recursively collect files under `dir` matching `accept`, skipping hidden
/// directories (including `.git`) and common build output, bounded by
/// [`MAX_SCAN_FILES`].
fn walk_files(dir: &Path, out: &mut Vec<PathBuf>, accept: &dyn Fn(&Path) -> bool) {
    if out.len() >= MAX_SCAN_FILES {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') {
            continue; // hidden files/dirs, including `.git`
        }
        let path = entry.path();
        if path.is_dir() {
            if name == "target" || name == "node_modules" {
                continue;
            }
            walk_files(&path, out, accept);
        } else if accept(&path) {
            out.push(path);
        }
        if out.len() >= MAX_SCAN_FILES {
            return;
        }
    }
}

fn is_source_file(path: &Path) -> bool {
    matches!(path.extension().and_then(|e| e.to_str()), Some("asm" | "s"))
}

fn is_mapping_file(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()) == Some("txt")
}

/// A file's `.include` graph over normalized paths.
struct IncludeGraph {
    /// Each known file → the files it directly includes.
    includes: HashMap<PathBuf, Vec<PathBuf>>,
}

impl IncludeGraph {
    /// Files nothing else includes — the entry points to assemble from.
    fn roots(&self) -> Vec<PathBuf> {
        let included: HashSet<&PathBuf> = self.includes.values().flatten().collect();
        self.includes
            .keys()
            .filter(|p| !included.contains(p))
            .cloned()
            .collect()
    }

    /// Every file reachable from `root` by following includes (including it).
    fn closure(&self, root: &Path) -> HashSet<PathBuf> {
        let mut seen = HashSet::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(p) = stack.pop() {
            if seen.insert(p.clone()) {
                if let Some(children) = self.includes.get(&p) {
                    stack.extend(children.iter().cloned());
                }
            }
        }
        seen
    }

    /// The entry roots whose closure contains `target`.
    fn entry_roots_for(&self, target: &Path) -> Vec<PathBuf> {
        self.roots()
            .into_iter()
            .filter(|r| self.closure(r).contains(target))
            .collect()
    }
}

/// Build the `.include` graph for `candidates`, reading each file's text through
/// `read` (open buffer or disk) and resolving include targets file-relative,
/// matching the assembler.
fn build_include_graph(
    candidates: impl IntoIterator<Item = PathBuf>,
    read: &impl Fn(&Path) -> Option<String>,
) -> IncludeGraph {
    let mut includes: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
    for file in candidates {
        let norm = normalize(&file);
        if includes.contains_key(&norm) {
            continue;
        }
        let text = read(&file).unwrap_or_default();
        let dir = file.parent().map_or_else(PathBuf::new, Path::to_path_buf);
        let targets = include_targets(&text)
            .into_iter()
            .map(|t| normalize(&dir.join(t)))
            .collect();
        includes.insert(norm, targets);
    }
    IncludeGraph { includes }
}

/// The `.include` / `.inestrn` targets in a source file, as written (the raw
/// double-quoted string), for resolving against the file's own directory.
fn include_targets(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed
            .strip_prefix(".include")
            .or_else(|| trimmed.strip_prefix(".inestrn"))
        else {
            continue;
        };
        // Guard against `.includes`-style prefixes: a real directive is followed
        // by whitespace before its argument.
        if !rest.starts_with(char::is_whitespace) {
            continue;
        }
        if let Some(q1) = rest.find('"') {
            if let Some(len) = rest[q1 + 1..].find('"') {
                out.push(rest[q1 + 1..q1 + 1 + len].to_string());
            }
        }
    }
    out
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
                    // Classification and its wire id are shared with the
                    // wasm/editor highlighter (`tooling::classify` +
                    // `TokenClass::wire_id`); the LSP keeps its own delta encoding
                    // and maps that id through `TOKEN_TYPES`.
                    token_type: tooling::classify(kind, piece).wire_id(),
                    token_modifiers_bitset: 0,
                });
                (prev_line, prev_col) = (line, col);
                col += len;
            }
        }
    }
    data
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
        let mut md = format!(
            "**{}** ({}) = {} (`{}`)",
            sym.name,
            kind,
            sym.value,
            format_hex(sym.value),
        );
        // Append the doc comment: the run of line comments directly above the
        // symbol's definition in this buffer, so hovering shows what the author
        // wrote to describe it.
        if let Some(doc) = preceding_doc(text, name) {
            md.push_str("\n\n");
            md.push_str(&doc);
        }
        return Some(md);
    }
    if macro_names(text).iter().any(|m| m == name) {
        return Some(format!("**{name}** (macro)"));
    }
    None
}

/// The documentation for the symbol `name`: the run of line comments
/// immediately preceding its definition in `text`. Contiguous `;`-comment lines
/// directly above the defining line are collected in source order (their `;`
/// and one following space stripped); a blank line or any code breaks the run,
/// so an "errant" comment separated by a gap is excluded. `None` when the
/// symbol isn't defined in this buffer or has no preceding comment.
fn preceding_doc(text: &str, name: &str) -> Option<String> {
    let def = definitions(text)
        .into_iter()
        .find(|d| d.name == name && matches!(d.kind, DefKind::Label | DefKind::Constant))?;
    let def_line = def.range.start.line as usize;
    let lines: Vec<&str> = text.lines().collect();

    let mut collected: Vec<String> = Vec::new();
    for line in lines[..def_line].iter().rev() {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix(';') else {
            break; // a blank line or code ends the contiguous run
        };
        collected.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
    }
    if collected.is_empty() {
        return None;
    }
    collected.reverse();
    Some(collected.join("\n"))
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

/// A block-directive folding tag: a macro body or a conditional block.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BlockTag {
    Macro,
    If,
}

/// Foldable regions: `.macrodef`…`.endm` and `.if*`…`.endif` blocks (nested via
/// a stack), plus runs of two or more consecutive line comments.
fn folding_ranges(text: &str) -> Vec<FoldingRange> {
    let mut ranges = Vec::new();
    let mut stack: Vec<(BlockTag, u32)> = Vec::new();
    let mut comment_start: Option<u32> = None;
    let lines: Vec<&str> = text.lines().collect();

    for (i, line) in lines.iter().enumerate() {
        let i = i as u32;
        let trimmed = line.trim_start();

        // Comment-run folding: close the run at the first non-comment line.
        if trimmed.starts_with(';') {
            comment_start.get_or_insert(i);
        } else if let Some(start) = comment_start.take() {
            if i - 1 > start {
                ranges.push(fold(start, i - 1, FoldingRangeKind::Comment));
            }
        }

        match leading_directive(trimmed).as_deref() {
            Some("macrodef") => stack.push((BlockTag::Macro, i)),
            Some("if" | "ifdef" | "ifndef") => stack.push((BlockTag::If, i)),
            Some("endm") => close_block(&mut stack, BlockTag::Macro, i, &mut ranges),
            Some("endif") => close_block(&mut stack, BlockTag::If, i, &mut ranges),
            _ => {}
        }
    }

    // A comment run extending to the last line.
    if let Some(start) = comment_start {
        let last = lines.len().saturating_sub(1) as u32;
        if last > start {
            ranges.push(fold(start, last, FoldingRangeKind::Comment));
        }
    }
    ranges
}

/// Close the nearest open block of `tag`, emitting its fold.
fn close_block(
    stack: &mut Vec<(BlockTag, u32)>,
    tag: BlockTag,
    end: u32,
    out: &mut Vec<FoldingRange>,
) {
    if let Some(pos) = stack.iter().rposition(|(t, _)| *t == tag) {
        let (_, start) = stack.remove(pos);
        if end > start {
            out.push(fold(start, end, FoldingRangeKind::Region));
        }
    }
}

fn fold(start_line: u32, end_line: u32, kind: FoldingRangeKind) -> FoldingRange {
    FoldingRange {
        start_line,
        end_line,
        kind: Some(kind),
        ..Default::default()
    }
}

/// The directive word on a line (lower-cased, without the leading `.`), e.g.
/// `.ifdef FOO` → `Some("ifdef")`. `None` when the line isn't a directive.
fn leading_directive(trimmed: &str) -> Option<String> {
    let rest = trimmed.strip_prefix('.')?;
    let word: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    (!word.is_empty()).then(|| word.to_ascii_lowercase())
}

/// Whether `s` is a legal nessemble identifier (for validating a rename target).
fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Base-conversion code actions for a numeric literal `text` at `range`: one
/// per base other than the literal's current one.
fn number_conversions(uri: &Url, text: &str, range: Range) -> Vec<CodeActionOrCommand> {
    let Some(value) = parse_number(text) else {
        return Vec::new();
    };
    if value < 0 {
        return Vec::new();
    }
    let current = base_of(text);
    [
        (Base::Hex, "hexadecimal", format!("${value:X}")),
        (Base::Dec, "decimal", value.to_string()),
        (Base::Bin, "binary", format!("%{value:b}")),
    ]
    .into_iter()
    .filter(|(base, _, _)| Some(*base) != current)
    .map(|(_, label, formatted)| {
        let mut changes = HashMap::new();
        changes.insert(
            uri.clone(),
            vec![TextEdit {
                range,
                new_text: formatted.clone(),
            }],
        );
        CodeActionOrCommand::CodeAction(CodeAction {
            title: format!("Convert to {label} ({formatted})"),
            kind: Some(CodeActionKind::REFACTOR_REWRITE),
            edit: Some(WorkspaceEdit {
                changes: Some(changes),
                ..Default::default()
            }),
            ..Default::default()
        })
    })
    .collect()
}

/// Numeric literal base.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Base {
    Hex,
    Dec,
    Bin,
}

/// The base a numeric literal is written in, or `None` for octal (which isn't
/// one of the offered targets, so all three conversions are shown).
fn base_of(text: &str) -> Option<Base> {
    if text.starts_with('$') {
        Some(Base::Hex)
    } else if text.starts_with('%') {
        Some(Base::Bin)
    } else if text.len() > 1 && text.starts_with('0') {
        None // octal
    } else {
        Some(Base::Dec)
    }
}

/// Parse a nessemble numeric literal (`$hex`, `%bin`, `0octal`, or decimal).
fn parse_number(text: &str) -> Option<i64> {
    if let Some(hex) = text.strip_prefix('$') {
        i64::from_str_radix(hex, 16).ok()
    } else if let Some(bin) = text.strip_prefix('%') {
        i64::from_str_radix(bin, 2).ok()
    } else if text.len() > 1
        && text.starts_with('0')
        && text.bytes().all(|b| (b'0'..=b'7').contains(&b))
    {
        i64::from_str_radix(text, 8).ok()
    } else {
        text.parse::<i64>().ok()
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
        folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
        rename_provider: Some(OneOf::Left(true)),
        code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
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
    let init_params = connection.initialize(capabilities)?;
    let workspace_roots = workspace_roots_from_init(&init_params);
    main_loop(connection, workspace_roots)
}

/// Extract workspace folder roots from the `initialize` params, preferring
/// `workspaceFolders`, then the legacy `rootUri` / `rootPath`. An empty result
/// means single-file analysis (no project scanning).
fn workspace_roots_from_init(params: &serde_json::Value) -> Vec<PathBuf> {
    if let Some(folders) = params.get("workspaceFolders").and_then(|f| f.as_array()) {
        let roots: Vec<PathBuf> = folders
            .iter()
            .filter_map(|f| f.get("uri").and_then(serde_json::Value::as_str))
            .filter_map(|u| Url::parse(u).ok())
            .filter_map(|u| u.to_file_path().ok())
            .collect();
        if !roots.is_empty() {
            return roots;
        }
    }
    if let Some(uri) = params.get("rootUri").and_then(serde_json::Value::as_str) {
        if let Some(path) = Url::parse(uri).ok().and_then(|u| u.to_file_path().ok()) {
            return vec![path];
        }
    }
    if let Some(path) = params.get("rootPath").and_then(serde_json::Value::as_str) {
        return vec![PathBuf::from(path)];
    }
    Vec::new()
}

/// The message loop: answer requests (shutdown, completion) and, for each
/// document notification, update the store and push refreshed diagnostics.
fn main_loop(connection: &Connection, workspace_roots: Vec<PathBuf>) -> LspResult<Server> {
    let mut server = Server {
        workspace_roots,
        ..Server::default()
    };
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
                    FoldingRangeRequest::METHOD => {
                        let result = serde_json::from_value::<FoldingRangeParams>(req.params)
                            .ok()
                            .and_then(|p| server.folding_ranges(&p.text_document.uri));
                        Response::new_ok(req.id, result)
                    }
                    Rename::METHOD => {
                        let result = serde_json::from_value::<RenameParams>(req.params)
                            .ok()
                            .and_then(|p| {
                                let tdp = p.text_document_position;
                                server.rename(&tdp.text_document.uri, tdp.position, &p.new_name)
                            });
                        Response::new_ok(req.id, result)
                    }
                    CodeActionRequest::METHOD => {
                        let result = serde_json::from_value::<CodeActionParams>(req.params)
                            .ok()
                            .map(|p| server.code_actions(&p.text_document.uri, p.range))
                            .unwrap_or_default();
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
                for params in server.apply_notification(&note.method, note.params) {
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
        let diags = analyze(&uri, "  notareal\n", &HashSet::new()).diagnostics;
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(diags[0].range.start.line, 0);
        assert_eq!(diags[0].source.as_deref(), Some("nessemble"));
    }

    #[test]
    fn analyze_reports_the_correct_line() {
        let uri = Url::parse("file:///multi.asm").unwrap();
        // Two valid lines, then an unknown opcode on line 3 (0-based line 2).
        let diags = analyze(&uri, "  lda #$00\n  nop\n  notareal\n", &HashSet::new()).diagnostics;
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].range.start.line, 2);
    }

    #[test]
    fn analyze_collects_multiple_errors() {
        let uri = Url::parse("file:///m.asm").unwrap();
        let diags = analyze(&uri, "  notareal\n  alsobad\n", &HashSet::new()).diagnostics;
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
        let diags = analyze(&uri, "  lda foo\n", &HashSet::new()).diagnostics;
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].range.start.character, 6);
        assert_eq!(diags[0].range.end.character, 9);
    }

    #[test]
    fn analyze_is_clean_for_valid_source() {
        let uri = Url::parse("file:///ok.asm").unwrap();
        assert!(analyze(&uri, "  lda #$00\n  nop\n", &HashSet::new())
            .diagnostics
            .is_empty());
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
        assert_eq!(
            toks[0].token_type,
            tooling::TokenClass::Instruction.wire_id()
        );
        assert!(toks
            .iter()
            .any(|t| t.token_type == tooling::TokenClass::Number.wire_id()));
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
    fn hover_includes_preceding_comment_doc() {
        let mut server = Server::default();
        let text = concat!(
            "; Errant comment\n",     // 0
            "\n",                     // 1 — blank line breaks the run
            "; Always $42\n",         // 2
            "SPECIAL_VALUE = $42\n",  // 3
            "  lda #SPECIAL_VALUE\n", // 4
        );
        open(&mut server, "file:///doc.asm", text);
        let uri = Url::parse("file:///doc.asm").unwrap();
        // Cursor on `SPECIAL_VALUE` in the operand (line 4).
        let hover = server
            .hover(&uri, Position::new(4, 10))
            .expect("hover on SPECIAL_VALUE");
        let HoverContents::Markup(md) = hover.contents else {
            panic!("expected markup hover");
        };
        assert!(md.value.contains("(constant)"), "{}", md.value);
        // The contiguous comment directly above is included…
        assert!(md.value.contains("Always $42"), "{}", md.value);
        // …but the errant comment across the blank line is not.
        assert!(!md.value.contains("Errant comment"), "{}", md.value);
    }

    #[test]
    fn hover_joins_multiline_comment_doc_for_a_label() {
        let mut server = Server::default();
        let text = concat!(
            "; This is a fun subroutine that doubles the value in the accumulator (A) and sets\n",
            "; the value of X to SPECIAL_VALUE\n",
            "double_accumulator:\n",
            "  asl a\n",
            "  ldx #$42\n",
            "  jsr double_accumulator\n",
        );
        open(&mut server, "file:///sub.asm", text);
        let uri = Url::parse("file:///sub.asm").unwrap();
        // Cursor on `double_accumulator` in the `jsr` operand (line 5).
        let hover = server
            .hover(&uri, Position::new(5, 10))
            .expect("hover on double_accumulator");
        let HoverContents::Markup(md) = hover.contents else {
            panic!("expected markup hover");
        };
        assert!(md.value.contains("(label)"), "{}", md.value);
        assert!(
            md.value.contains("This is a fun subroutine"),
            "{}",
            md.value
        );
        assert!(
            md.value.contains("the value of X to SPECIAL_VALUE"),
            "{}",
            md.value
        );
    }

    #[test]
    fn hover_without_a_preceding_comment_omits_doc() {
        let mut server = Server::default();
        let text = "count = 5\n  lda #count\n";
        open(&mut server, "file:///nc.asm", text);
        let uri = Url::parse("file:///nc.asm").unwrap();
        let hover = server
            .hover(&uri, Position::new(1, 8))
            .expect("hover on count");
        let HoverContents::Markup(md) = hover.contents else {
            panic!("expected markup hover");
        };
        // Just the value line — no trailing doc paragraph.
        assert_eq!(md.value, "**count** (constant) = 5 (`$05`)");
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
        let cleared = server.apply_notification(
            DidCloseTextDocument::METHOD,
            serde_json::json!({ "textDocument": { "uri": "file:///b.asm" } }),
        );
        assert_eq!(cleared.len(), 1);
        assert!(cleared[0].diagnostics.is_empty());
        assert_eq!(server.document_count(), 0);
    }

    // ---- Phase 7: workspace-aware analysis --------------------------------

    /// A fresh, unique temp workspace directory.
    fn workspace(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static N: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "nessemble-lsp-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, name: &str, text: &str) {
        std::fs::write(dir.join(name), text).unwrap();
    }

    fn server_for(root: &Path) -> Server {
        Server {
            workspace_roots: vec![root.to_path_buf()],
            ..Default::default()
        }
    }

    fn did_open(server: &mut Server, path: &Path, text: &str) -> Vec<PublishDiagnosticsParams> {
        let uri = Url::from_file_path(path).unwrap();
        server.apply_notification(DidOpenTextDocument::METHOD, open_params(uri.as_str(), text))
    }

    fn diags_for<'a>(
        pubs: &'a [PublishDiagnosticsParams],
        path: &Path,
    ) -> Option<&'a Vec<Diagnostic>> {
        let uri = Url::from_file_path(path).unwrap();
        pubs.iter().find(|p| p.uri == uri).map(|p| &p.diagnostics)
    }

    #[test]
    fn fragment_symbols_resolve_via_the_entry_root() {
        // main.asm includes consts.asm (defines `palette`) then code.asm (uses
        // it). Opening code.asm alone would flag `palette` as undefined; with
        // project context it resolves.
        let w = workspace("frag");
        write(
            &w,
            "main.asm",
            ".include \"consts.asm\"\n.include \"code.asm\"\n",
        );
        write(&w, "consts.asm", "palette = $3F00\n");
        let code = w.join("code.asm");
        write(&w, "code.asm", "  lda palette\n");

        let mut server = server_for(&w);
        let pubs = did_open(&mut server, &code, "  lda palette\n");
        let d = diags_for(&pubs, &code).expect("code.asm published");
        assert!(d.is_empty(), "unexpected diagnostics: {d:?}");

        // Control: with no workspace, single-file analysis flags `palette`.
        let mut lonely = Server::default();
        let pubs = did_open(&mut lonely, &code, "  lda palette\n");
        let d = diags_for(&pubs, &code).expect("code.asm published");
        assert_eq!(d.len(), 1, "expected the cross-file false positive: {d:?}");
        assert!(d[0].message.contains("palette"));

        let _ = std::fs::remove_dir_all(&w);
    }

    #[test]
    fn fragment_still_reports_its_own_real_errors() {
        // A genuine error in the fragment survives project analysis; the
        // cross-file symbol does not get flagged.
        let w = workspace("real");
        write(
            &w,
            "main.asm",
            ".include \"consts.asm\"\n.include \"code.asm\"\n",
        );
        write(&w, "consts.asm", "palette = $3F00\n");
        let code = w.join("code.asm");
        let text = "  lda palette\n  notareal\n";
        write(&w, "code.asm", text);

        let mut server = server_for(&w);
        let pubs = did_open(&mut server, &code, text);
        let d = diags_for(&pubs, &code).expect("code.asm published");
        assert_eq!(d.len(), 1, "diagnostics: {d:?}");
        assert!(d[0].message.contains("notareal"), "{:?}", d[0].message);
        assert_eq!(d[0].range.start.line, 1);

        let _ = std::fs::remove_dir_all(&w);
    }

    #[test]
    fn symbol_defined_under_any_root_is_not_flagged() {
        // shared.asm is included by two roots; only r1 defines `thing`. The
        // intersection rule means `thing` is not flagged.
        let w = workspace("multiroot");
        write(
            &w,
            "r1.asm",
            ".include \"defs.asm\"\n.include \"shared.asm\"\n",
        );
        write(&w, "r2.asm", ".include \"shared.asm\"\n");
        write(&w, "defs.asm", "thing = 1\n");
        let shared = w.join("shared.asm");
        write(&w, "shared.asm", "  lda #thing\n");

        let mut server = server_for(&w);
        let pubs = did_open(&mut server, &shared, "  lda #thing\n");
        let d = diags_for(&pubs, &shared).expect("shared.asm published");
        assert!(d.is_empty(), "thing should resolve under r1: {d:?}");

        let _ = std::fs::remove_dir_all(&w);
    }

    #[test]
    fn goto_definition_crosses_files_in_a_project() {
        // `palette` is defined in consts.asm and used in code.asm. cmd-click on
        // the use should jump to consts.asm.
        let w = workspace("gotodef");
        write(
            &w,
            "main.asm",
            ".include \"consts.asm\"\n.include \"code.asm\"\n",
        );
        let consts = w.join("consts.asm");
        write(&w, "consts.asm", "palette = $3F00\n");
        let code = w.join("code.asm");
        let code_text = "  lda palette\n";
        write(&w, "code.asm", code_text);

        let mut server = server_for(&w);
        did_open(&mut server, &code, code_text);
        let code_uri = Url::from_file_path(&code).unwrap();
        // `palette` sits at cols 6..13 on line 0.
        let loc = server
            .goto_definition(&code_uri, Position::new(0, 8))
            .expect("cross-file definition found");
        assert_eq!(loc.uri, Url::from_file_path(&consts).unwrap());
        assert_eq!(loc.range.start, Position::new(0, 0)); // `palette` at consts:0

        let _ = std::fs::remove_dir_all(&w);
    }

    #[test]
    fn fixing_a_fragment_error_clears_it() {
        let w = workspace("clear");
        write(&w, "main.asm", ".include \"code.asm\"\n");
        let code = w.join("code.asm");
        write(&w, "code.asm", "  notareal\n");

        let mut server = server_for(&w);
        let pubs = did_open(&mut server, &code, "  notareal\n");
        assert_eq!(diags_for(&pubs, &code).unwrap().len(), 1);

        // Fix it via didChange → the error clears (empty publish for code.asm).
        let uri = Url::from_file_path(&code).unwrap();
        let changed = server.apply_notification(
            DidChangeTextDocument::METHOD,
            serde_json::json!({
                "textDocument": { "uri": uri.as_str(), "version": 2 },
                "contentChanges": [{ "text": "  nop\n" }]
            }),
        );
        assert!(
            diags_for(&changed, &code).unwrap().is_empty(),
            "error should clear: {changed:?}"
        );

        let _ = std::fs::remove_dir_all(&w);
    }

    // ---- Phase 8: folding, rename, code actions ---------------------------

    #[test]
    fn folding_ranges_cover_blocks_and_comment_runs() {
        let text = concat!(
            "; a comment\n",        // 0
            "; still commenting\n", // 1
            ".macrodef greet\n",    // 2
            "  nop\n",              // 3
            ".endm\n",              // 4
            ".ifdef FOO\n",         // 5
            "  nop\n",              // 6
            ".endif\n",             // 7
        );
        let ranges = folding_ranges(text);
        // Comment run 0..1.
        assert!(ranges.iter().any(|r| r.start_line == 0
            && r.end_line == 1
            && r.kind == Some(FoldingRangeKind::Comment)));
        // Macro block 2..4.
        assert!(ranges.iter().any(|r| r.start_line == 2
            && r.end_line == 4
            && r.kind == Some(FoldingRangeKind::Region)));
        // Conditional block 5..7.
        assert!(ranges.iter().any(|r| r.start_line == 5 && r.end_line == 7));
    }

    #[test]
    fn rename_edits_every_occurrence_in_open_buffers() {
        let mut server = Server::default();
        let text = "start:\n  jmp start\n  jmp start\n";
        open(&mut server, "file:///r.asm", text);
        let uri = Url::parse("file:///r.asm").unwrap();
        // Cursor on the label definition; rename to `begin`.
        let edit = server
            .rename(&uri, Position::new(0, 0), "begin")
            .expect("rename produces an edit");
        let edits = &edit.changes.unwrap()[&uri];
        assert_eq!(edits.len(), 3); // the definition + two uses
        assert!(edits.iter().all(|e| e.new_text == "begin"));
    }

    #[test]
    fn rename_rejects_an_invalid_identifier() {
        let mut server = Server::default();
        open(&mut server, "file:///ri.asm", "start:\n  jmp start\n");
        let uri = Url::parse("file:///ri.asm").unwrap();
        assert!(server.rename(&uri, Position::new(0, 0), "1bad").is_none());
        assert!(server
            .rename(&uri, Position::new(0, 0), "has space")
            .is_none());
    }

    #[test]
    fn code_action_converts_a_number_base() {
        let mut server = Server::default();
        // `$10` on the operand of an lda.
        open(&mut server, "file:///n.asm", "  lda #$10\n");
        let uri = Url::parse("file:///n.asm").unwrap();
        // Cursor inside `$10` (the literal spans cols 7..10).
        let actions =
            server.code_actions(&uri, Range::new(Position::new(0, 8), Position::new(0, 8)));
        let titles: Vec<String> = actions
            .iter()
            .filter_map(|a| match a {
                CodeActionOrCommand::CodeAction(c) => Some(c.title.clone()),
                CodeActionOrCommand::Command(_) => None,
            })
            .collect();
        // $10 == 16 decimal == %10000 binary; hex is the current base, so it's
        // not offered.
        assert!(
            titles.iter().any(|t| t.contains("decimal (16)")),
            "{titles:?}"
        );
        assert!(
            titles.iter().any(|t| t.contains("binary (%10000)")),
            "{titles:?}"
        );
        assert!(
            !titles.iter().any(|t| t.contains("hexadecimal")),
            "{titles:?}"
        );
    }

    #[test]
    fn code_action_is_empty_off_a_number() {
        let mut server = Server::default();
        open(&mut server, "file:///nn.asm", "  lda #$10\n");
        let uri = Url::parse("file:///nn.asm").unwrap();
        // Cursor on the mnemonic, not a number.
        let actions =
            server.code_actions(&uri, Range::new(Position::new(0, 2), Position::new(0, 2)));
        assert!(actions.is_empty());
    }

    // ---- custom pseudo-op awareness ---------------------------------------

    #[test]
    fn custom_pseudo_ops_are_not_flagged_and_resolve_to_scripts() {
        // A `--pseudo` mapping declares `.double`; the directive must not be
        // flagged, and cmd-click on it jumps to the script.
        let w = workspace("custom");
        write(&w, "pseudo.txt", ".double = double.rhai\n");
        let script = w.join("double.rhai");
        write(
            &w,
            "double.rhai",
            "fn custom(ints, texts) { [ints[0] * 2] }\n",
        );
        let main = w.join("main.asm");
        let text = "  .double 5\n";
        write(&w, "main.asm", text);

        let mut server = server_for(&w);
        let pubs = did_open(&mut server, &main, text);
        let d = diags_for(&pubs, &main).expect("main.asm published");
        assert!(
            d.is_empty(),
            "custom pseudo-op should not be flagged: {d:?}"
        );

        // cmd-click on `.double` (cols 2..9) opens the script.
        let main_uri = Url::from_file_path(&main).unwrap();
        let loc = server
            .goto_definition(&main_uri, Position::new(0, 4))
            .expect("custom pseudo-op resolves to its script");
        assert_eq!(loc.uri, Url::from_file_path(&script).unwrap());

        let _ = std::fs::remove_dir_all(&w);
    }

    #[test]
    fn unknown_custom_pseudo_op_is_still_flagged() {
        // With no mapping declaring `.double`, it remains an unknown directive.
        let w = workspace("nocustom");
        let main = w.join("main.asm");
        let text = "  .double 5\n";
        write(&w, "main.asm", text);

        let mut server = server_for(&w);
        let pubs = did_open(&mut server, &main, text);
        let d = diags_for(&pubs, &main).expect("main.asm published");
        assert!(!d.is_empty(), "unknown custom pseudo-op should be flagged");

        let _ = std::fs::remove_dir_all(&w);
    }
}
