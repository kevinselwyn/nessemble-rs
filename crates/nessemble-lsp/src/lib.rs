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
//!   the buffer. Hovering `.color` additionally previews the NES palette entries
//!   its arguments map to — the whole list on the directive, one color on a
//!   single argument.
//! - **Workspace-aware diagnostics** (Phase 7): when a workspace folder is open,
//!   a file is analyzed in the context of the `.include` project it belongs to,
//!   so cross-file symbols aren't flagged as undefined.
//! - **Editing aids** (Phase 8): `textDocument/foldingRange` (macro/conditional
//!   blocks, subroutine bodies, and comment runs), `textDocument/rename` (a symbol across open
//!   buffers), and `textDocument/codeAction` (numeric base conversions).

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use lsp_server::{Connection, ErrorCode, Message, Notification, Response};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Notification as _,
    PublishDiagnostics,
};
use lsp_types::request::{
    CodeActionRequest, Completion, DocumentLinkRequest, DocumentSymbolRequest, FoldingRangeRequest,
    Formatting, GotoDefinition, HoverRequest, InlayHintRequest, References, Rename, Request as _,
    SemanticTokensFullRequest, SignatureHelpRequest,
};
use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOrCommand, CodeActionParams,
    CodeActionProviderCapability, CompletionItem, CompletionItemKind, CompletionOptions,
    CompletionParams, CompletionResponse, Diagnostic, DiagnosticSeverity,
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    DocumentFormattingParams, DocumentLink, DocumentLinkOptions, DocumentLinkParams,
    DocumentSymbol, DocumentSymbolParams, DocumentSymbolResponse, FoldingRange, FoldingRangeKind,
    FoldingRangeParams, FoldingRangeProviderCapability, GotoDefinitionParams,
    GotoDefinitionResponse, Hover, HoverContents, HoverParams, InlayHint, InlayHintKind,
    InlayHintLabel, InlayHintParams, Location, MarkupContent, MarkupKind, NumberOrString, OneOf,
    Position, PublishDiagnosticsParams, Range, ReferenceParams, RenameParams, SemanticToken,
    SemanticTokenModifier, SemanticTokenType, SemanticTokens, SemanticTokensFullOptions,
    SemanticTokensLegend, SemanticTokensOptions, SemanticTokensParams, SemanticTokensResult,
    SemanticTokensServerCapabilities, ServerCapabilities, SignatureHelp, SignatureHelpOptions,
    SignatureHelpParams, SymbolKind, TextDocumentSyncCapability, TextDocumentSyncKind, TextEdit,
    Url, WorkDoneProgressOptions, WorkspaceEdit,
};

use nessemble_core::tooling::{self, LexKind, RuleSeverity};
use nessemble_core::{
    diagnose_project_with, diagnose_source_with, lenient_custom_resolver, match_nes_color,
    parse_pseudo_mapping, Diag, ListSymbol, Options, ProjectDiagnostics, NES_PALETTE,
    PROJECT_ROOT_PREFIX,
};
use nessemble_isa::{DIRECTIVES, OPCODES};

mod api;
#[cfg(feature = "scripting")]
mod scripting;

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

/// Semantic-token **modifier** legend. Modifiers are a separate axis from token
/// types, so this is purely additive: a comment carrying a nessemble directive
/// stays `COMMENT` (wire id 5) and merely gains this bit, leaving
/// `TokenClass::wire_id` — a contract shared with the wasm highlighter — frozen.
const TOKEN_MODIFIERS: [SemanticTokenModifier; 1] = [SemanticTokenModifier::DOCUMENTATION];

/// The bit for [`SemanticTokenModifier::DOCUMENTATION`] in a token's modifier
/// bitset (index 0 of [`TOKEN_MODIFIERS`]).
const MODIFIER_DOCUMENTATION: u32 = 1 << 0;

/// A boxed, thread-safe error, matching what the stdio transport surfaces.
type LspError = Box<dyn std::error::Error + Sync + Send>;
type LspResult<T> = Result<T, LspError>;

/// What kind of document a buffer is — decides which whole family of request
/// handlers applies. Assembly features must never run over a `.rhai` buffer
/// (confident nonsense in the editor is worse than a missing feature — see
/// `plans/014-scripting-docs-and-tooling.md` §5.1), so every handler branches
/// on this first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum DocKind {
    #[default]
    Asm,
    Rhai,
}

/// The kind of a document opened at `uri`, from its extension — `.rhai` is
/// [`DocKind::Rhai`], everything else [`DocKind::Asm`] — cross-checked against
/// the `didOpen` notification's `languageId` only for an extensionless
/// buffer. `didChange` carries no `languageId`, so the extension is the
/// authority and the id is a tiebreak used at open time only.
fn doc_kind(uri: &Url, language_id: &str) -> DocKind {
    match uri_to_path(uri).extension().and_then(|e| e.to_str()) {
        Some(ext) if ext.eq_ignore_ascii_case("rhai") => DocKind::Rhai,
        None if language_id == "rhai" => DocKind::Rhai,
        Some(_) | None => DocKind::Asm,
    }
}

/// Per-document state: the current buffer text plus the user-defined symbols
/// (labels/constants, with their resolved values) from the last successful
/// assembly, used for completion and hover.
#[derive(Default)]
struct Document {
    text: String,
    symbols: Vec<ListSymbol>,
    kind: DocKind,
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
    /// Per-disk-file `.include` extraction, tagged with the file's `(mtime, len)`
    /// so rebuilding the include graph re-reads only files that actually changed
    /// (see [`Server::raw_include_targets`]). Open buffers are never cached here —
    /// they come straight from the always-current document store.
    include_cache: RefCell<HashMap<PathBuf, CachedIncludes>>,
}

/// The `.include`/`.inestrn` targets extracted from a disk file, tagged with the
/// file's `(mtime, len)` signature. `None` signature means the file's metadata
/// was unreadable (e.g. it is gone).
struct CachedIncludes {
    sig: Option<(std::time::SystemTime, u64)>,
    targets: Vec<String>,
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
                let kind = doc_kind(&uri, &p.text_document.language_id);
                self.documents.insert(
                    uri.clone(),
                    Document {
                        text: p.text_document.text,
                        symbols: Vec::new(),
                        kind,
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
        let results = self.with_lint(self.compute_diagnostics(changed));

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

    /// Append lint findings to every document being (re)published this round.
    /// Linting is intra-file, so each open buffer is scanned on its own — no
    /// include graph — honoring the `.nessemblerc` `lint` config discovered from
    /// the buffer's path. The findings are added on top of the assemble
    /// diagnostics already gathered for that document.
    fn with_lint(&self, mut results: DiagResults) -> DiagResults {
        for (uri, diags) in &mut results.per_file {
            if let Some(doc) = self.documents.get(uri) {
                // The assembly lint rules (register discipline, comment
                // directives, …) have no meaning against Rhai source; a
                // `.rhai` buffer's own findings come from `compute_diagnostics`.
                if doc.kind == DocKind::Asm {
                    diags.extend(lint_diagnostics(uri, &doc.text));
                }
            }
        }
        results
    }

    /// The raw `.include`/`.inestrn` targets of `file`. An open buffer is scanned
    /// directly (always current); a disk file's targets are cached and reused
    /// while its `(mtime, len)` is unchanged, so rebuilding the include graph on a
    /// keystroke re-reads only the files that actually changed on disk.
    fn raw_include_targets(&self, overlay: &HashMap<PathBuf, &str>, file: &Path) -> Vec<String> {
        let np = normalize(file);
        if let Some(text) = overlay.get(&np) {
            return include_targets(text);
        }
        let sig = std::fs::metadata(&np)
            .ok()
            .and_then(|m| Some((m.modified().ok()?, m.len())));
        let mut cache = self.include_cache.borrow_mut();
        if let Some(entry) = cache.get(&np) {
            if entry.sig == sig {
                return entry.targets.clone();
            }
        }
        let targets = std::fs::read_to_string(&np)
            .map(|t| include_targets(&t))
            .unwrap_or_default();
        cache.insert(
            np.clone(),
            CachedIncludes {
                sig,
                targets: targets.clone(),
            },
        );
        targets
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
        let Some(doc) = self.documents.get(changed) else {
            return DiagResults::default();
        };
        if doc.kind == DocKind::Rhai {
            return self.compute_diagnostics_rhai(changed, &doc.text);
        }
        let text = doc.text.as_str();
        let changed_path = normalize(&uri_to_path(changed));

        // Custom pseudo-ops declared in the workspace's `--pseudo` mapping files,
        // so they aren't reported as unknown directives.
        let known_custom: HashSet<String> = self.custom_scripts().into_keys().collect();

        if self.workspace_roots.is_empty() {
            return single_file(changed, text, &known_custom);
        }

        // An overlay + reader backed by the open buffers (borrowed, not cloned),
        // falling back to disk; the full text is materialized only for the files
        // actually read.
        let overlay_map: HashMap<PathBuf, &str> = self
            .documents
            .iter()
            .map(|(u, d)| (normalize(&uri_to_path(u)), d.text.as_str()))
            .collect();
        let overlay = |p: &Path| overlay_map.get(&normalize(p)).map(|s| (*s).to_string());
        let read = |p: &Path| overlay(p).or_else(|| std::fs::read_to_string(p).ok());

        // Discover the entry roots whose include-closure contains the file. The
        // graph reads each file's include lines through the mtime-cached
        // extractor, so an unchanged disk file is stat'd, not re-read.
        let candidates = scan_source_files(&self.workspace_roots)
            .into_iter()
            .chain(self.asm_document_paths());
        let graph = build_include_graph(candidates, &|file| {
            self.raw_include_targets(&overlay_map, file)
        });
        let roots = graph.entry_roots_for(&changed_path);
        if roots.is_empty() {
            return single_file(changed, text, &known_custom);
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

    /// The paths of every open [`DocKind::Asm`] document — the assembly-only
    /// counterpart of `self.documents.keys().map(uri_to_path)`, used to seed
    /// the `.include` graph so an open `.rhai` buffer never becomes a
    /// candidate node in it (`plans/014-scripting-docs-and-tooling.md` §5.1).
    fn asm_document_paths(&self) -> impl Iterator<Item = PathBuf> + '_ {
        self.documents
            .iter()
            .filter(|(_, d)| d.kind == DocKind::Asm)
            .map(|(u, _)| uri_to_path(u))
    }

    /// Diagnostics for a `.rhai` buffer: single-document, never project-wide
    /// (a script has no `.include` graph of its own). `is_mapped` — whether
    /// some workspace `pseudo.txt` maps a directive at this exact script path
    /// — feeds the `missing-custom` lint. Without the `scripting` feature
    /// this compiles nothing and reports nothing, matching §5.8.
    #[cfg_attr(not(feature = "scripting"), allow(clippy::unused_self))]
    fn compute_diagnostics_rhai(&self, changed: &Url, text: &str) -> DiagResults {
        let mut per_file = HashMap::new();
        #[cfg(feature = "scripting")]
        {
            let changed_path = normalize(&uri_to_path(changed));
            let is_mapped = self
                .custom_scripts()
                .values()
                .any(|script| normalize(script) == changed_path);
            per_file.insert(changed.clone(), scripting::diagnostics(text, is_mapped));
        }
        #[cfg(not(feature = "scripting"))]
        {
            let _ = text;
            per_file.insert(changed.clone(), Vec::new());
        }
        DiagResults {
            per_file,
            changed_symbols: Vec::new(),
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
    fn complete(&self, uri: &Url, pos: Position) -> Vec<CompletionItem> {
        let Some(doc) = self.documents.get(uri) else {
            let mut items = mnemonic_items();
            items.extend(directive_items());
            return items;
        };
        if doc.kind == DocKind::Rhai {
            return api::completions(&doc.text);
        }
        // Inside a comment, code completions are noise — offer the comment
        // directives instead, which are otherwise undiscoverable.
        if in_comment(&doc.text, pos) {
            let mut items = comment_directive_items();
            // Above a label, offer the whole signature block in one item. This
            // is how the annotations get discovered at all.
            if let Some(indent) = documentable_routine_indent(&doc.text, pos) {
                items.insert(0, signature_scaffold_item(&indent));
            }
            return items;
        }
        // Inside a filename argument, offer filenames — nothing else can go there.
        if let Some(items) = self.path_completions(uri, pos) {
            return items;
        }
        let mut items = mnemonic_items();
        items.extend(directive_items());
        items.extend(doc.symbols.iter().map(|s| symbol_item(&s.name)));
        items.extend(macro_names(&doc.text).iter().map(|name| macro_item(name)));
        items
    }

    /// Signature help for the call enclosing `pos` in the `.rhai` document at
    /// `uri`: parameters parsed from the catalog entry's signature, with the
    /// active parameter tracked across commas (§5.4). `None` for anything
    /// else — assembly has no call-with-arguments syntax to offer this for.
    fn signature_help(&self, uri: &Url, pos: Position) -> Option<SignatureHelp> {
        let doc = self.documents.get(uri)?;
        if doc.kind != DocKind::Rhai {
            return None;
        }
        api::signature_help(&doc.text, pos)
    }

    /// Produce a whole-document formatting edit for `uri`, or `None` if the
    /// document is unknown. An already-formatted document yields no edits.
    fn format_document(&self, uri: &Url) -> Option<Vec<TextEdit>> {
        let doc = self.documents.get(uri)?;
        if doc.kind == DocKind::Rhai {
            // Out of scope (§7): nessemble has no opinion about Rhai layout.
            return None;
        }
        let text = &doc.text;
        let formatted = tooling::format(text);
        if formatted == *text {
            return Some(Vec::new());
        }
        Some(vec![TextEdit {
            range: full_range(text),
            new_text: formatted,
        }])
    }

    /// Full-document semantic tokens for `uri`, or `None` if it is unknown —
    /// or if it is a `.rhai` document, out of scope like formatting (§7): the
    /// Rhai community extension already highlights it.
    fn semantic_tokens(&self, uri: &Url) -> Option<SemanticTokensResult> {
        let doc = self.documents.get(uri)?;
        if doc.kind == DocKind::Rhai {
            return None;
        }
        Some(SemanticTokensResult::Tokens(SemanticTokens {
            result_id: None,
            data: semantic_tokens(&doc.text),
        }))
    }

    /// An outline of the document at `uri`: every label, constant, and macro
    /// defined in the buffer, with its name range — or, for a `.rhai`
    /// document, every script-local `fn`, `custom` first (§5.5). `None` if
    /// `uri` is unknown.
    fn document_symbols(&self, uri: &Url) -> Option<Vec<DocumentSymbol>> {
        let doc = self.documents.get(uri)?;
        if doc.kind == DocKind::Rhai {
            return Some(rhai_document_symbols(&doc.text));
        }
        let text = &doc.text;
        let signatures = tooling::resolve_signatures(text);
        Some(
            definitions(text)
                .into_iter()
                .map(|d| {
                    let signature = signatures.iter().find(|s| s.name == d.name);
                    document_symbol(&d, signature)
                })
                .collect(),
        )
    }

    /// Resolve go-to-definition for the token under `pos` in the document at
    /// `uri`. A custom pseudo-op (`.foo`) jumps to its script file; a symbol
    /// jumps to its defining label/constant/macro — found in the current buffer
    /// first, then (with a workspace open) across the `.include` project, so
    /// cmd/ctrl-click reaches a definition in a sibling or parent file. In a
    /// `.rhai` document, jumps to the script-local `fn` under the cursor
    /// (§5.5; needs the `scripting` feature).
    fn goto_definition(&self, uri: &Url, pos: Position) -> Option<Location> {
        let doc = self.documents.get(uri)?;
        if doc.kind == DocKind::Rhai {
            let (name, _) = api::identifier_at(&doc.text, pos)?;
            let range = rhai_local_definition(&doc.text, &name)?;
            return Some(Location::new(uri.clone(), range));
        }
        let token = token_at(&doc.text, pos)?;
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

    /// Hover markdown for a custom pseudo-op directive (`.foo`): the script it
    /// resolves to, plus the doc comment (a run of `//` lines) immediately
    /// above its `custom` function, the same "comment run above the
    /// definition" convention [`preceding_doc`] uses for assembly symbols
    /// (`plans/014-scripting-docs-and-tooling.md` §5.6). `None` when `name`
    /// isn't a directive any workspace `pseudo.txt` maps.
    fn custom_directive_hover(&self, name: &str) -> Option<String> {
        let script = self.custom_scripts().remove(name.trim_start_matches('.'))?;
        let mut md = format!("**{name}** (custom pseudo-op) → `{}`", script.display());
        if let Ok(text) = std::fs::read_to_string(&script) {
            if let Some(doc) = rhai_doc_comment_above_custom(&text) {
                md.push_str("\n\n");
                md.push_str(&doc);
            }
        }
        Some(md)
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
        let overlay_map: HashMap<PathBuf, &str> = self
            .documents
            .iter()
            .map(|(u, d)| (normalize(&uri_to_path(u)), d.text.as_str()))
            .collect();
        let read = |p: &Path| {
            overlay_map
                .get(&normalize(p))
                .map(|s| (*s).to_string())
                .or_else(|| std::fs::read_to_string(p).ok())
        };
        let candidates = scan_source_files(&self.workspace_roots)
            .into_iter()
            .chain(self.asm_document_paths());
        let graph = build_include_graph(candidates, &|file| {
            self.raw_include_targets(&overlay_map, file)
        });
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
        let doc = self.documents.get(uri)?;
        if doc.kind == DocKind::Rhai {
            let (name, _) = api::identifier_at(&doc.text, pos)?;
            let def_range = rhai_local_definition(&doc.text, &name);
            let locations = rhai_local_references(&doc.text, &name)
                .into_iter()
                .filter(|r| include_declaration || Some(*r) != def_range)
                .map(|r| Location::new(uri.clone(), r))
                .collect();
            return Some(locations);
        }
        let text = &doc.text;
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
        if doc.kind == DocKind::Rhai {
            let (name, range) = api::identifier_at(&doc.text, pos)?;
            let markdown = api::hover(&name).or_else(|| rhai_local_fn_hover(&doc.text, &name))?;
            return Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: markdown,
                }),
                range: Some(range),
            });
        }
        // `.color` previews the palette it maps to, on the directive and on each
        // of its arguments, ahead of the generic directive/symbol hovers.
        if let Some(hover) = color_hover(&doc.text, pos, &doc.symbols) {
            return Some(hover);
        }
        let token = token_at(&doc.text, pos)?;
        let markdown = match token.kind {
            // A custom pseudo-op directive shows the script it maps to (and
            // its `custom` function's doc comment, if any); a built-in
            // directive shows its own description.
            LexKind::Directive => {
                directive_hover(token.text).or_else(|| self.custom_directive_hover(token.text))?
            }
            // A filename argument reports where it resolved to and what is there.
            LexKind::String => {
                let base = Self::base_dir(uri)?;
                let root = self.root_dir(uri);
                let arg = path_args(&doc.text)
                    .into_iter()
                    .find(|arg| arg.token_range == token.range && !arg.path.is_empty())?;
                path_arg_hover(root.as_deref(), &base, &arg)
            }
            LexKind::Ident => ident_hover(
                token.text,
                &doc.text,
                &doc.symbols,
                self.signature_for(uri, token.text).as_ref(),
            )?,
            LexKind::Comment => return comment_directive_hover(&doc.text, pos),
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

    /// The directory a document's filename arguments resolve against: the
    /// directory of the document itself, as the assembler uses. `None` for a
    /// non-`file:` URI (an untitled buffer), which has no directory at all.
    fn base_dir(uri: &Url) -> Option<PathBuf> {
        if uri.scheme() != "file" {
            return None;
        }
        uri_to_path(uri).parent().map(Path::to_path_buf)
    }

    /// The project root a `@/` path in this document resolves against, mirroring
    /// the assembler's ladder (`plans/012-project-root-paths.md` §4): the
    /// containing workspace folder wins when the document sits under one — a
    /// multi-root workspace's folder is the editor's equivalent of `--root`, an
    /// explicit override — else the nearest `.nessemblerc`/`.nessembleignore`
    /// marker walking up from the document, else the document's own directory.
    ///
    /// `None` only for a document with no [`base_dir`](Self::base_dir) at all
    /// (an untitled buffer).
    fn root_dir(&self, uri: &Url) -> Option<PathBuf> {
        let base = Self::base_dir(uri)?;
        let explicit = self
            .workspace_roots
            .iter()
            .find(|root| base.starts_with(root));
        Some(nessemble_core::project_root(
            explicit.map(PathBuf::as_path),
            &base,
        ))
    }

    /// Clickable links for every filename argument in the document at `uri`
    /// ([`path_args`]), so cmd/ctrl-clicking a path opens the file.
    ///
    /// A path that does not resolve to an existing file gets **no link** — the
    /// missing-file diagnostic is the feedback there, and a link that opens
    /// nothing is worse than no link.
    fn document_links(&self, uri: &Url) -> Option<Vec<DocumentLink>> {
        let doc = self.documents.get(uri)?;
        if doc.kind == DocKind::Rhai {
            return Some(Vec::new());
        }
        let base = Self::base_dir(uri)?;
        let root = self.root_dir(uri);
        let links = path_args(&doc.text)
            .into_iter()
            .filter_map(|arg| {
                let target =
                    nessemble_core::resolve_path_arg(root.as_deref(), &base, arg.path).ok()?;
                if !target.is_file() {
                    return None;
                }
                Some(DocumentLink {
                    range: arg.range,
                    target: Some(Url::from_file_path(&target).ok()?),
                    tooltip: None,
                    data: None,
                })
            })
            .collect();
        Some(links)
    }

    /// Filename completions inside a filename argument, or `None` when the cursor
    /// is not in one (in which case the ordinary code completions apply).
    ///
    /// Entries come from the directory the partially-typed path points at, so
    /// `"sprites/he` completes against `sprites/`. Files are filtered by what the
    /// directive can actually use ([`FILE_DIRECTIVES`]); directories are always
    /// offered, since they are on the way to a file.
    fn path_completions(&self, uri: &Url, pos: Position) -> Option<Vec<CompletionItem>> {
        let doc = self.documents.get(uri)?;
        let base = Self::base_dir(uri)?;
        let root = self.root_dir(uri);
        let arg = path_args(&doc.text).into_iter().find(|arg| {
            arg.token_range.start.line == pos.line
                && pos.character > arg.token_range.start.character
                && pos.character <= arg.token_range.end.character
        })?;

        // Complete against the part of the path before the cursor: everything up
        // to the last separator is the directory to list, the rest is the prefix
        // being typed. A leading `@/` is peeled off first and reattached to
        // `dir_part` afterwards — it is the project-root marker, not a directory
        // segment of its own, so `"@/ass` must split to `dir_part = "@/"` (the
        // root) rather than losing the slash to the generic split.
        let typed_len = pos.character.saturating_sub(arg.range.start.character) as usize;
        let typed: String = arg.path.chars().take(typed_len).collect();
        let (marker, rest) = match typed.strip_prefix(PROJECT_ROOT_PREFIX) {
            Some(rest) => (PROJECT_ROOT_PREFIX, rest),
            None => ("", typed.as_str()),
        };
        let (dir_part, partial) = match rest.rsplit_once('/') {
            Some((dir, rest)) => (format!("{marker}{dir}/"), rest.to_string()),
            None => (marker.to_string(), rest.to_string()),
        };

        let mut items = Vec::new();

        // Offer `@/` itself at the start of an empty argument (or after typing
        // just `@`), so the project-root escape is discoverable without knowing
        // it exists.
        if dir_part.is_empty()
            && PROJECT_ROOT_PREFIX
                .to_lowercase()
                .starts_with(&partial.to_lowercase())
        {
            items.push(CompletionItem {
                label: PROJECT_ROOT_PREFIX.to_string(),
                kind: Some(CompletionItemKind::FOLDER),
                ..CompletionItem::default()
            });
        }

        let Ok(dir) = nessemble_core::resolve_path_arg(root.as_deref(), &base, &dir_part) else {
            return Some(items);
        };
        let exts = file_directive_exts(&arg.directive);

        let Ok(read_dir) = std::fs::read_dir(dir) else {
            return Some(items);
        };
        for entry in read_dir.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || !name.to_lowercase().starts_with(&partial.to_lowercase()) {
                continue;
            }
            let is_dir = entry.file_type().is_ok_and(|kind| kind.is_dir());
            if !is_dir && !extension_allowed(&name, exts) {
                continue;
            }
            items.push(CompletionItem {
                label: if is_dir { format!("{name}/") } else { name },
                kind: Some(if is_dir {
                    CompletionItemKind::FOLDER
                } else {
                    CompletionItemKind::FILE
                }),
                ..CompletionItem::default()
            });
        }
        items.sort_by(|a, b| a.label.cmp(&b.label));
        Some(items)
    }

    /// Inlay hints for `range` in the document at `uri`: the clobber set of each
    /// `JSR`'s target, rendered at the end of the call line.
    ///
    /// This is the one surface that puts text on lines the author did not write,
    /// so it is deliberately narrow: only `JSR` lines, only when the target
    /// declares a clobber list, and clients toggle it off wholesale.
    fn inlay_hints(&self, uri: &Url, range: Range) -> Vec<InlayHint> {
        let Some(doc) = self.documents.get(uri) else {
            return Vec::new();
        };
        if doc.kind == DocKind::Rhai {
            return Vec::new();
        }
        let mut hints = Vec::new();
        for (idx, line) in doc.text.lines().enumerate() {
            let line_no = idx as u32;
            if line_no < range.start.line || line_no > range.end.line {
                continue;
            }
            let Some(target) = jsr_target(line) else {
                continue;
            };
            let Some(sig) = self.signature_for(uri, target) else {
                continue;
            };
            if !sig.declares_clobbers {
                continue;
            }
            hints.push(InlayHint {
                position: Position::new(line_no, utf16_len(line)),
                label: InlayHintLabel::String(format!(
                    "  ‹{}›",
                    tooling::format_slots(&sig.clobbers)
                )),
                kind: Some(InlayHintKind::PARAMETER),
                tooltip: None,
                padding_left: Some(true),
                padding_right: None,
                text_edits: None,
                data: None,
            });
        }
        hints
    }

    /// The routine signature declared for `name`, looked up in the document at
    /// `uri` first and then across every other open document.
    ///
    /// Project-wide on purpose: the point of a signature is to be readable at
    /// the **call site**, and a `JSR` routinely names a routine defined in a
    /// sibling or parent file.
    fn signature_for(&self, uri: &Url, name: &str) -> Option<tooling::Signature> {
        let here = self
            .documents
            .get(uri)
            .and_then(|doc| find_signature(&doc.text, name));
        here.or_else(|| {
            self.documents
                .iter()
                .filter(|(other, _)| *other != uri)
                .find_map(|(_, doc)| find_signature(&doc.text, name))
        })
    }

    /// Foldable regions in the document at `uri`: macro and conditional blocks,
    /// subroutine bodies (a label down to the first blank line), and runs of
    /// consecutive line comments — or, for a `.rhai` document, each `fn`'s body
    /// and its own comment runs (§5.5). `None` if `uri` is unknown.
    fn folding_ranges(&self, uri: &Url) -> Option<Vec<FoldingRange>> {
        let doc = self.documents.get(uri)?;
        if doc.kind == DocKind::Rhai {
            return Some(rhai_folding_ranges(&doc.text));
        }
        Some(folding_ranges(&doc.text))
    }

    /// Rename the symbol under `pos` in the document at `uri` to `new_name`,
    /// across every open **assembly** document (nessemble symbols share one
    /// global scope; renaming inside `.rhai` scripts is out of scope, like
    /// formatting — §7). `None` if the cursor isn't on an identifier or
    /// `new_name` is not a legal identifier.
    fn rename(&self, uri: &Url, pos: Position, new_name: &str) -> Option<WorkspaceEdit> {
        let doc = self.documents.get(uri)?;
        if doc.kind == DocKind::Rhai {
            return None;
        }
        let name = word_at(&doc.text, pos)?;
        if !is_identifier(new_name) {
            return None;
        }
        let mut changes: HashMap<Url, Vec<TextEdit>> = HashMap::new();
        for (doc_uri, doc) in self
            .documents
            .iter()
            .filter(|(_, d)| d.kind == DocKind::Asm)
        {
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
    /// conversions when the cursor is on a numeric literal, the deprecated
    /// directive rename, and the two routine-signature actions.
    fn code_actions(&self, uri: &Url, range: Range) -> Vec<CodeActionOrCommand> {
        let Some(doc) = self.documents.get(uri) else {
            return Vec::new();
        };
        if doc.kind == DocKind::Rhai {
            return Vec::new();
        }
        // A deprecated directive on the requested line gets a rename fix; it is
        // what makes the deprecation actionable rather than nagging.
        let fixes = deprecated_directive_fixes(uri, &doc.text, range);
        if !fixes.is_empty() {
            return fixes;
        }
        // The verifier found a missing register: offer to add it rather than
        // making the author retype the list.
        let fixes = undeclared_clobber_fixes(uri, &doc.text, range);
        if !fixes.is_empty() {
            return fixes;
        }
        // On an undocumented routine's label, offer the scaffold.
        let fixes = document_routine_actions(uri, &doc.text, range);
        if !fixes.is_empty() {
            return fixes;
        }
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

/// The compiled-AST half of `.rhai` editor support ([`scripting`]) reduced to
/// six free functions with an empty fallback when the `scripting` feature is
/// off — kept as plain functions, not `Server` methods, since none of them
/// touch server state.
#[cfg(feature = "scripting")]
fn rhai_document_symbols(text: &str) -> Vec<DocumentSymbol> {
    scripting::document_symbols(text)
}
#[cfg(not(feature = "scripting"))]
fn rhai_document_symbols(_text: &str) -> Vec<DocumentSymbol> {
    Vec::new()
}

#[cfg(feature = "scripting")]
fn rhai_local_definition(text: &str, name: &str) -> Option<Range> {
    scripting::local_definition(text, name)
}
#[cfg(not(feature = "scripting"))]
fn rhai_local_definition(_text: &str, _name: &str) -> Option<Range> {
    None
}

#[cfg(feature = "scripting")]
fn rhai_local_references(text: &str, name: &str) -> Vec<Range> {
    scripting::local_references(text, name)
}
#[cfg(not(feature = "scripting"))]
fn rhai_local_references(_text: &str, _name: &str) -> Vec<Range> {
    Vec::new()
}

#[cfg(feature = "scripting")]
fn rhai_local_fn_hover(text: &str, name: &str) -> Option<String> {
    scripting::local_fn_hover(text, name)
}
#[cfg(not(feature = "scripting"))]
fn rhai_local_fn_hover(_text: &str, _name: &str) -> Option<String> {
    None
}

#[cfg(feature = "scripting")]
fn rhai_doc_comment_above_custom(text: &str) -> Option<String> {
    scripting::doc_comment_above_custom(text)
}
#[cfg(not(feature = "scripting"))]
fn rhai_doc_comment_above_custom(_text: &str) -> Option<String> {
    None
}

#[cfg(feature = "scripting")]
fn rhai_folding_ranges(text: &str) -> Vec<FoldingRange> {
    scripting::folding_ranges(text)
}
#[cfg(not(feature = "scripting"))]
fn rhai_folding_ranges(_text: &str) -> Vec<FoldingRange> {
    Vec::new()
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

/// Lint the buffer `text` (identified by `uri`) and return its findings as LSP
/// diagnostics. The `.nessemblerc` `lint` config is discovered from the buffer's
/// path (best-effort: a missing/invalid config yields the built-in defaults or,
/// on a hard config error, no lint diagnostics — never a crash). Findings are
/// published at a deliberately gentle severity (`INFORMATION`/`HINT`) with
/// `source = "nessemble-lint"` and the rule id in `code`, so editors render them
/// as suggestions distinct from the assembler's errors and warnings.
fn lint_diagnostics(uri: &Url, text: &str) -> Vec<Diagnostic> {
    let path = uri_to_path(uri);
    let Ok(config) = nessemble_rc::Config::resolve(&path, &nessemble_rc::Choice::Discover) else {
        return Vec::new();
    };
    let lint_cfg = config.lint_for(&path);
    let ignore = |name: &str| lint_cfg.is_ignored_name(name);
    let opts = tooling::LintOptions {
        severities: lint_cfg.severities.clone(),
        window: lint_cfg.window,
        ignore: &ignore,
    };
    tooling::lint(text, &opts)
        .into_iter()
        .map(|f| {
            let severity = lint_severity(lint_cfg.severities.get(f.rule));
            // The rule writes the message (so the CLI report and the editor say
            // the same thing); it backtick-quotes the subject, which lets
            // `diagnostic_range` narrow to the offending token on the line.
            let message = f.message;
            let line = f.line.saturating_sub(1);
            Diagnostic {
                range: diagnostic_range(text, line, &message),
                severity: Some(severity),
                source: Some("nessemble-lint".to_string()),
                code: Some(NumberOrString::String(f.rule.id().to_string())),
                message,
                ..Default::default()
            }
        })
        .collect()
}

/// Map a lint rule's severity onto a gentle LSP severity: an `error` rule reads
/// as `INFORMATION`, a `warn` rule as `HINT` — both quieter than the assembler's
/// `WARNING`/`ERROR`. (`off` rules never produce findings.)
fn lint_severity(severity: RuleSeverity) -> DiagnosticSeverity {
    match severity {
        RuleSeverity::Error => DiagnosticSeverity::INFORMATION,
        _ => DiagnosticSeverity::HINT,
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

/// Build the `.include` graph for `candidates`, taking each file's raw include
/// targets from `raw_targets_of` (which reads open buffers or disk, with
/// caching) and resolving them file-relative, matching the assembler.
fn build_include_graph(
    candidates: impl IntoIterator<Item = PathBuf>,
    raw_targets_of: &impl Fn(&Path) -> Vec<String>,
) -> IncludeGraph {
    let mut includes: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
    for file in candidates {
        let norm = normalize(&file);
        if includes.contains_key(&norm) {
            continue;
        }
        let dir = file.parent().map_or_else(PathBuf::new, Path::to_path_buf);
        let targets = raw_targets_of(&file)
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
    // Lines whose comment carries a nessemble directive (well-formed or not):
    // both are marked, so a typo'd directive still reads as one.
    let (directives, malformed) = tooling::scan_directives_with_errors(text);
    let directive_lines: HashSet<u32> = directives
        .iter()
        .map(|d| d.line)
        .chain(malformed.iter().map(|m| m.line))
        .collect();
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
                let is_directive_comment =
                    kind == LexKind::Comment && directive_lines.contains(&(line + 1));
                data.push(SemanticToken {
                    delta_line,
                    delta_start,
                    length: len,
                    // Classification and its wire id are shared with the
                    // wasm/editor highlighter (`tooling::classify` +
                    // `TokenClass::wire_id`); the LSP keeps its own delta encoding
                    // and maps that id through `TOKEN_TYPES`.
                    token_type: tooling::classify(kind, piece).wire_id(),
                    // A directive comment keeps the `COMMENT` type and is
                    // distinguished by a modifier, so no wire id moves.
                    token_modifiers_bitset: if is_directive_comment {
                        MODIFIER_DOCUMENTATION
                    } else {
                        0
                    },
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
#[derive(Clone, Copy)]
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

/// Directives whose first string argument names a file, paired with the file
/// extensions completion offers inside that argument. `None` means every file:
/// a binary blob has no conventional suffix, and guessing one would hide the
/// author's own naming.
const FILE_DIRECTIVES: &[(&str, Option<&[&str]>)] = &[
    ("include", Some(&["asm", "inc", "s"])),
    ("inestrn", None),
    ("incbin", None),
    ("incpng", Some(&["png"])),
    ("incpal", Some(&["png"])),
    ("incrle", None),
    ("incwav", Some(&["wav"])),
];

/// Whether `directive`'s first string argument is a filename, so it is a path
/// without needing a `file://` declaration.
fn is_file_directive(directive: &str) -> bool {
    FILE_DIRECTIVES.iter().any(|(name, _)| *name == directive)
}

/// The extensions completion should offer inside `directive`'s filename argument.
/// `None` offers every file — either because the directive takes any blob, or
/// because it is a custom pseudo-op, whose script may read any format at all.
fn file_directive_exts(directive: &str) -> Option<&'static [&'static str]> {
    FILE_DIRECTIVES
        .iter()
        .find(|(name, _)| *name == directive)
        .and_then(|(_, exts)| *exts)
}

/// A filename argument found in a buffer.
///
/// Two kinds qualify: the first string argument of a [`FILE_DIRECTIVES`]
/// directive, whose argument is unambiguously a path, and *any* string argument
/// carrying the `file://` declaration — which is what makes a custom pseudo-op's
/// path visible to tooling without running its script.
struct PathArg<'a> {
    /// Lower-cased directive name, without its leading dot.
    directive: String,
    /// The path as written, with any `file://` prefix removed.
    path: &'a str,
    /// Range of the path text: inside the quotes, after the prefix. This is what
    /// a document link underlines, so the marker and quotes stay unadorned.
    range: Range,
    /// Range of the whole string token, used to match a hover position.
    token_range: Range,
}

/// Every filename argument in `source`, in source order.
///
/// An argument with an *empty* path is reported too — that is what a half-typed
/// `"` is, and completion needs it. Consumers that need a real file (links,
/// hover) reject it when the resolved path turns out not to be one.
///
/// A directive's arguments end at the line break, except that a line ending in a
/// comma continues onto the next — the same rule the parser applies to a custom
/// pseudo-op's argument list.
fn path_args(source: &str) -> Vec<PathArg<'_>> {
    let mut out = Vec::new();
    let mut directive: Option<String> = None;
    let mut first_string_seen = false;
    let mut line: Option<u32> = None;
    let mut continues = false;

    for tok in located_lexemes(source) {
        if line != Some(tok.range.start.line) {
            line = Some(tok.range.start.line);
            if !continues {
                directive = None;
                first_string_seen = false;
            }
        }
        continues = tok.kind == LexKind::Punct && tok.text == ",";

        match tok.kind {
            LexKind::Directive => {
                directive = Some(tok.text.trim_start_matches('.').to_ascii_lowercase());
                first_string_seen = false;
            }
            LexKind::String => {
                let Some(name) = directive.clone() else {
                    continue;
                };
                let is_importer_filename = is_file_directive(&name) && !first_string_seen;
                first_string_seen = true;

                let raw = tok.text;
                let opened = raw.starts_with('"');
                let inner_start = usize::from(opened);
                // An unterminated string runs to the end of the line, so only
                // trim a closing quote that is actually there.
                let inner_end = if opened && raw.len() > 1 && raw.ends_with('"') {
                    raw.len() - 1
                } else {
                    raw.len()
                };
                let inner = &raw[inner_start..inner_end];
                let (path, declared) = nessemble_core::strip_file_url(inner);
                if !declared && !is_importer_filename {
                    continue;
                }

                let lead = utf16_len(&raw[..inner_start])
                    + if declared {
                        utf16_len(nessemble_core::FILE_URL_PREFIX)
                    } else {
                        0
                    };
                let trail = utf16_len(&raw[inner_end..]);
                out.push(PathArg {
                    directive: name,
                    path,
                    range: Range::new(
                        Position::new(tok.range.start.line, tok.range.start.character + lead),
                        Position::new(tok.range.end.line, tok.range.end.character - trail),
                    ),
                    token_range: tok.range,
                });
            }
            _ => {}
        }
    }
    out
}

/// Whether `name`'s extension is one a directive accepts. `None` accepts every
/// file (see [`FILE_DIRECTIVES`]).
fn extension_allowed(name: &str, exts: Option<&[&str]>) -> bool {
    let Some(exts) = exts else {
        return true;
    };
    Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| exts.iter().any(|want| ext.eq_ignore_ascii_case(want)))
}

/// Hover markdown for a filename argument: where the path resolved to, and what
/// is there. Answers "is it finding the file I think it is?" without a build —
/// including, for a `@/` path, *what root it picked* (plan 012 §9's mitigation
/// for a `.nessemblerc` elsewhere silently moving the root).
fn path_arg_hover(root: Option<&Path>, base: &Path, arg: &PathArg<'_>) -> String {
    let target = match nessemble_core::resolve_path_arg(root, base, arg.path) {
        Ok(target) => target,
        Err(e) => return format!("**{}**", e.message(arg.path)),
    };
    let detail = match std::fs::metadata(&target) {
        Ok(meta) if meta.is_dir() => "directory".to_string(),
        Ok(meta) => match png_dimensions(&target) {
            Some((w, h)) => format!("{} bytes · {w}×{h} PNG", meta.len()),
            None => format!("{} bytes", meta.len()),
        },
        Err(_) => "**not found**".to_string(),
    };
    format!("`{}`\n\n{detail}", target.display())
}

/// The pixel dimensions of a PNG, read from its IHDR header.
///
/// Only the first 24 bytes are read: hover fires on every mouse pause, and
/// decoding a full-resolution image to learn two numbers would be wasteful.
fn png_dimensions(path: &Path) -> Option<(u32, u32)> {
    use std::io::Read as _;

    const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
    let mut header = [0u8; 24];
    std::fs::File::open(path)
        .ok()?
        .read_exact(&mut header)
        .ok()?;
    if &header[..8] != SIGNATURE || &header[12..16] != b"IHDR" {
        return None;
    }
    let width = u32::from_be_bytes(header[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(header[20..24].try_into().ok()?);
    Some((width, height))
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

/// Build a document-outline entry for a definition. A documented routine carries
/// its clobber list in the outline's detail, so a whole file's register
/// discipline is visible in one panel.
fn document_symbol(def: &Definition, signature: Option<&tooling::Signature>) -> DocumentSymbol {
    let (kind, base) = match def.kind {
        DefKind::Label => (SymbolKind::FUNCTION, "label"),
        DefKind::Constant => (SymbolKind::CONSTANT, "constant"),
        DefKind::Macro => (SymbolKind::FUNCTION, "macro"),
    };
    let detail = match signature.filter(|s| s.declares_clobbers) {
        Some(sig) => format!("{base} · clobbers {}", tooling::format_slots(&sig.clobbers)),
        None => base.to_string(),
    };
    #[allow(deprecated)] // `deprecated` field is required but unused.
    DocumentSymbol {
        name: def.name.clone(),
        detail: Some(detail),
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
///
/// When the identifier names a routine with a signature, the signature table is
/// rendered too — so hovering the operand of a `JSR` answers "does this call eat
/// my `Y`?" without leaving the call site.
fn ident_hover(
    name: &str,
    text: &str,
    symbols: &[ListSymbol],
    signature: Option<&tooling::Signature>,
) -> Option<String> {
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
        if let Some(sig) = signature {
            md.push_str("\n\n");
            md.push_str(&signature_md(sig));
        }
        return Some(md);
    }
    // A routine the assembler could not resolve to an address still has a
    // contract worth showing.
    if let Some(sig) = signature {
        return Some(format!("**{name}** (routine)\n\n{}", signature_md(sig)));
    }
    if macro_names(text).iter().any(|m| m == name) {
        return Some(format!("**{name}** (macro)"));
    }
    None
}

/// The label a line calls with a direct `JSR`, or `None` for anything else —
/// including an indirect `JSR ($…)`, which names no label.
fn jsr_target(line: &str) -> Option<&str> {
    let code = line.split(';').next().unwrap_or("").trim();
    let (mnemonic, operand) = code.split_once(char::is_whitespace)?;
    if !mnemonic.eq_ignore_ascii_case("jsr") {
        return None;
    }
    let target = operand.trim();
    (!target.is_empty()
        && target
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.'))
    .then_some(target)
}

/// The signature declared for `name` in `text`, if any.
fn find_signature(text: &str, name: &str) -> Option<tooling::Signature> {
    tooling::resolve_signatures(text)
        .into_iter()
        .find(|s| s.name == name)
}

/// Render a routine signature as hover markdown: a table of inputs, a table of
/// outputs, and the clobber list.
fn signature_md(sig: &tooling::Signature) -> String {
    use std::fmt::Write as _;

    let mut md = String::new();
    for (heading, rows) in [("in", &sig.params), ("out", &sig.returns)] {
        if rows.is_empty() {
            continue;
        }
        let _ = writeln!(md, "| {heading} | |\n| --- | --- |");
        for (slot, description) in rows {
            let _ = writeln!(md, "| `{slot}` | {description} |");
        }
        md.push('\n');
    }
    if sig.declares_clobbers {
        let _ = write!(
            md,
            "**clobbers** `{}`",
            tooling::format_slots(&sig.clobbers)
        );
    }
    md.trim_end().to_string()
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

// ---- `.color` palette preview ---------------------------------------------

/// The pseudo-instruction whose arguments hover previews as palette entries.
const COLOR_DIRECTIVE: &str = ".color";

/// The pitch of one swatch square in the hover image, in pixels. The square
/// itself is inset by a pixel on each side, which both draws its border and
/// separates it from its neighbor.
const SWATCH: u32 = 18;

/// One argument of a `.color` argument list.
struct ColorArg {
    /// The argument as written, with whitespace squeezed out.
    text: String,
    range: Range,
    /// What the argument evaluates to, or `None` when this buffer can't resolve
    /// it (an undefined symbol, an expression form the scan doesn't cover).
    value: Option<i64>,
}

/// A `.color` pseudo-instruction: the directive token and the arguments
/// following it.
struct ColorCall {
    directive: Range,
    args: Vec<ColorArg>,
}

/// Hover for `.color`, previewing the NES palette entries its arguments are
/// mapped to: the whole list when the cursor is on the directive, and the one
/// color when it is on a single argument.
///
/// `None` when `pos` is on neither, or on an argument whose value this buffer
/// can't resolve — the caller then falls back to the ordinary hover, so an
/// unresolved symbol still reports itself.
fn color_hover(text: &str, pos: Position, symbols: &[ListSymbol]) -> Option<Hover> {
    let call = color_calls(text, symbols).into_iter().find(|c| {
        range_contains(c.directive, pos) || c.args.iter().any(|a| range_contains(a.range, pos))
    })?;
    let (value, range) = if range_contains(call.directive, pos) {
        (color_list_markdown(&call.args)?, call.directive)
    } else {
        let (i, arg) = call
            .args
            .iter()
            .enumerate()
            .find(|(_, a)| range_contains(a.range, pos))?;
        (color_arg_markdown(arg, i, call.args.len())?, arg.range)
    };
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }),
        range: Some(range),
    })
}

/// Every `.color` pseudo-instruction in `text`, with its arguments split out and
/// evaluated against `symbols`. The spelling is matched exactly, as the
/// assembler matches directive names — `.COLOR` assembles to nothing to preview.
fn color_calls(text: &str, symbols: &[ListSymbol]) -> Vec<ColorCall> {
    let tokens = located_lexemes(text);
    tokens
        .iter()
        .enumerate()
        .filter(|(_, t)| t.kind == LexKind::Directive && t.text == COLOR_DIRECTIVE)
        .map(|(i, t)| ColorCall {
            directive: t.range,
            args: color_args(&tokens[i + 1..], t.range.end.line, symbols),
        })
        .collect()
}

/// Split the tokens following a `.color` into its comma-separated arguments and
/// evaluate each.
///
/// The list runs to the end of `line` and continues onto the following line
/// after a trailing comma — the rule the parser's `parse_expr_list` follows.
/// Comments are trivia, and a comma nested in parentheses doesn't separate
/// arguments.
fn color_args(tokens: &[Located<'_>], line: u32, symbols: &[ListSymbol]) -> Vec<ColorArg> {
    let mut groups: Vec<Vec<Located<'_>>> = Vec::new();
    let mut current: Vec<Located<'_>> = Vec::new();
    let mut line = line;
    let mut depth = 0u32;
    let mut after_comma = false;

    for token in tokens {
        if token.kind == LexKind::Comment {
            continue;
        }
        if token.range.start.line != line {
            if !after_comma {
                break; // the statement ended with the line
            }
            line = token.range.start.line;
        }
        after_comma = false;
        match (token.kind, token.text) {
            (LexKind::Punct, "(") => depth += 1,
            (LexKind::Punct, ")") => depth = depth.saturating_sub(1),
            (LexKind::Punct, ",") if depth == 0 => {
                groups.push(std::mem::take(&mut current));
                after_comma = true;
                continue;
            }
            _ => {}
        }
        current.push(*token);
    }
    groups.push(current);

    groups
        .into_iter()
        .filter(|g| !g.is_empty())
        .map(|g| ColorArg {
            text: g.iter().map(|t| t.text).collect(),
            range: Range::new(g[0].range.start, g[g.len() - 1].range.end),
            value: eval_argument(&g, symbols),
        })
        .collect()
}

/// Evaluate one `.color` argument, mirroring the assembler's expression
/// semantics: numeric and character literals, symbol references, `HIGH()`,
/// `LOW()`, `BANK()`, parentheses, and binary operators at a single precedence
/// level, right-associative.
///
/// `None` when the expression names something the buffer can't resolve, or uses
/// a form outside that grammar (an anonymous `:+` label, a macro argument) —
/// there is then no color to show rather than a wrong one.
fn eval_argument(tokens: &[Located<'_>], symbols: &[ListSymbol]) -> Option<i64> {
    let mut rest = tokens;
    let value = eval_expr(&mut rest, symbols)?;
    rest.is_empty().then_some(value)
}

fn eval_expr(tokens: &mut &[Located<'_>], symbols: &[ListSymbol]) -> Option<i64> {
    let left = eval_primary(tokens, symbols)?;
    let Some((op, width)) = binary_op(tokens) else {
        return Some(left);
    };
    *tokens = &tokens[width..];
    let right = eval_expr(tokens, symbols)?;
    Some(apply_op(left, op, right))
}

fn eval_primary(tokens: &mut &[Located<'_>], symbols: &[ListSymbol]) -> Option<i64> {
    let (token, rest) = tokens.split_first()?;
    *tokens = rest;
    match token.kind {
        LexKind::Number => parse_number(token.text),
        // `'x'` is the character's byte value, as the assembler's lexer reads it.
        LexKind::Char => token.text.as_bytes().get(1).map(|b| i64::from(*b)),
        LexKind::Ident => eval_ident(token.text, tokens, symbols),
        LexKind::Punct if token.text == "(" => {
            let inner = eval_expr(tokens, symbols)?;
            eat_punct(tokens, ")").then_some(inner)
        }
        _ => None,
    }
}

/// An identifier in an expression: one of the three built-in calls (spelled
/// upper-case, as the assembler's lexer recognizes them), or a symbol reference
/// resolved against the buffer's symbols.
fn eval_ident(name: &str, tokens: &mut &[Located<'_>], symbols: &[ListSymbol]) -> Option<i64> {
    let symbol = |n: &str| symbols.iter().find(|s| s.name == n);
    if !matches!(name, "HIGH" | "LOW" | "BANK") {
        return symbol(name).map(|s| s.value);
    }
    if !eat_punct(tokens, "(") {
        return None;
    }
    if name == "BANK" {
        // `BANK()` takes a symbol name, not an expression.
        let (token, rest) = tokens.split_first()?;
        *tokens = rest;
        let bank = symbol(token.text).map(|s| s.bank as i64)?;
        return eat_punct(tokens, ")").then_some(bank);
    }
    let inner = eval_expr(tokens, symbols)?;
    let value = if name == "HIGH" {
        (inner >> 8) & 0xFF
    } else {
        inner & 0xFF
    };
    eat_punct(tokens, ")").then_some(value)
}

/// Consume a leading punctuation token spelled `text`, reporting whether it was
/// there.
fn eat_punct(tokens: &mut &[Located<'_>], text: &str) -> bool {
    match tokens.split_first() {
        Some((token, rest)) if token.kind == LexKind::Punct && token.text == text => {
            *tokens = rest;
            true
        }
        _ => false,
    }
}

/// A binary operator of the expression grammar.
#[derive(Clone, Copy)]
enum Op {
    Add,
    Sub,
    Mul,
    Div,
    Pow,
    And,
    Or,
    Xor,
    Shl,
    Shr,
    Mod,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
}

/// The binary operator at the head of `tokens`, and how many tokens it spans.
/// The lossless lexer emits punctuation one character at a time, so a
/// two-character operator is a pair of *adjacent* tokens — `< <` with a space
/// between is not a shift.
fn binary_op(tokens: &[Located<'_>]) -> Option<(Op, usize)> {
    let first = tokens.first().filter(|t| t.kind == LexKind::Punct)?;
    let second = tokens
        .get(1)
        .filter(|t| t.kind == LexKind::Punct && t.range.start == first.range.end);
    if let Some(second) = second {
        let pair = match (first.text, second.text) {
            ("*", "*") => Some(Op::Pow),
            (">", ">") => Some(Op::Shr),
            ("<", "<") => Some(Op::Shl),
            ("=", "=") => Some(Op::Eq),
            ("!", "=") => Some(Op::Ne),
            ("<", "=") => Some(Op::Le),
            (">", "=") => Some(Op::Ge),
            _ => None,
        };
        if let Some(op) = pair {
            return Some((op, 2));
        }
    }
    let single = match first.text {
        "+" => Op::Add,
        "-" => Op::Sub,
        "*" => Op::Mul,
        "/" => Op::Div,
        "&" => Op::And,
        "|" => Op::Or,
        "^" => Op::Xor,
        "%" => Op::Mod,
        "<" => Op::Lt,
        ">" => Op::Gt,
        _ => return None,
    };
    Some((single, 1))
}

/// Apply a binary operator, matching the assembler's evaluator — including its
/// division and modulo by zero yielding `0`, and its shift counts masking to 63.
/// Arithmetic wraps rather than overflowing: an editor's hover must not be able
/// to panic the server, whatever a buffer mid-edit contains.
fn apply_op(a: i64, op: Op, b: i64) -> i64 {
    match op {
        Op::Add => a.wrapping_add(b),
        Op::Sub => a.wrapping_sub(b),
        Op::Mul => a.wrapping_mul(b),
        Op::Div => {
            if b == 0 {
                0
            } else {
                a.wrapping_div(b)
            }
        }
        Op::Pow => (a as f64).powf(b as f64) as i64,
        Op::And => a & b,
        Op::Or => a | b,
        Op::Xor => a ^ b,
        Op::Shr => a >> (b & 63),
        Op::Shl => a << (b & 63),
        Op::Mod => {
            if b == 0 {
                0
            } else {
                a.wrapping_rem(b)
            }
        }
        Op::Eq => i64::from(a == b),
        Op::Ne => i64::from(a != b),
        Op::Lt => i64::from(a < b),
        Op::Gt => i64::from(a > b),
        Op::Le => i64::from(a <= b),
        Op::Ge => i64::from(a >= b),
    }
}

/// The NES palette entry a `.color` argument maps to: the index byte the
/// assembler emits, and the RGB the PPU shows for it. Shares the assembler's own
/// matcher, so the preview can't drift from the emitted ROM.
fn nes_entry(value: i64) -> (u8, u32) {
    let rgb = (value & 0xFF_FFFF) as u32;
    let index = match_nes_color(
        ((rgb >> 16) & 0xFF) as u8,
        ((rgb >> 8) & 0xFF) as u8,
        (rgb & 0xFF) as u8,
    );
    (index, NES_PALETTE[index as usize])
}

/// Hover markdown for a whole `.color` argument list: the row of NES colors the
/// arguments map to, then a table detailing each. `None` for a `.color` with no
/// arguments, which has nothing to preview.
fn color_list_markdown(args: &[ColorArg]) -> Option<String> {
    use std::fmt::Write as _;

    if args.is_empty() {
        return None;
    }
    let swatches: Vec<Option<u32>> = args
        .iter()
        .map(|a| a.value.map(|v| nes_entry(v).1))
        .collect();
    let plural = if args.len() == 1 { "color" } else { "colors" };

    let mut md = format!(
        "**{COLOR_DIRECTIVE}** — {} {plural} mapped to the NES palette\n\n",
        args.len()
    );
    md.push_str(&swatch_image(&swatches, "NES colors"));
    md.push_str("\n\n| argument | RGB | index | NES color |\n| --- | --- | --- | --- |\n");
    for arg in args {
        // Writing to a String is infallible.
        let _ = match arg.value.map(|v| (v, nes_entry(v))) {
            Some((value, (index, rgb))) => writeln!(
                md,
                "| `{}` | `#{:06X}` | `${index:02X}` | `#{rgb:06X}` |",
                arg.text,
                value & 0xFF_FFFF,
            ),
            // An argument nothing in the buffer defines: shown, but unresolved.
            None => writeln!(md, "| `{}` | ? | ? | ? |", arg.text),
        };
    }
    Some(md)
}

/// Hover markdown for a single `.color` argument: just the one NES color it maps
/// to. `None` when the argument's value is unresolved.
fn color_arg_markdown(arg: &ColorArg, index: usize, total: usize) -> Option<String> {
    use std::fmt::Write as _;

    let value = arg.value?;
    let (palette, rgb) = nes_entry(value);
    let mut md = format!(
        "**{COLOR_DIRECTIVE}** — argument {} of {total}\n\n",
        index + 1
    );
    md.push_str(&swatch_image(&[Some(rgb)], &format!("#{rgb:06X}")));
    // Writing to a String is infallible.
    let _ = write!(
        md,
        "\n\n`{}` (`#{:06X}`) → NES color `${palette:02X}` (`#{rgb:06X}`)",
        arg.text,
        value & 0xFF_FFFF,
    );
    Some(md)
}

/// A markdown image of `colors` as a row of swatches, drawn as an inline SVG
/// data URI: a graphical client shows the colors themselves, and a terminal
/// client falls back to the alt text — which is why every color hover also
/// spells its values out in text.
fn swatch_image(colors: &[Option<u32>], alt: &str) -> String {
    use std::fmt::Write as _;

    let width = SWATCH * colors.len() as u32;
    let mut svg =
        format!("<svg xmlns='http://www.w3.org/2000/svg' width='{width}' height='{SWATCH}'>");
    for (i, color) in colors.iter().enumerate() {
        let x = SWATCH * i as u32 + 1;
        let side = SWATCH - 2;
        // An unresolved argument keeps its slot as an empty outline, so the row
        // still lines up with the table under it.
        let fill = color.map_or_else(|| "none".to_string(), |rgb| format!("#{rgb:06X}"));
        let _ = write!(
            svg,
            "<rect x='{x}' y='1' width='{side}' height='{side}' \
             fill='{fill}' stroke='#808080' stroke-width='1'/>"
        );
    }
    svg.push_str("</svg>");
    format!(
        "![{alt}](data:image/svg+xml;base64,{})",
        base64(svg.as_bytes())
    )
}

/// Standard base64 (RFC 4648) of `data`, for the swatch data URI. Spelled out
/// here rather than taken as a dependency: it is the server's only use for one.
fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let bytes = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let group = (u32::from(bytes[0]) << 16) | (u32::from(bytes[1]) << 8) | u32::from(bytes[2]);
        for i in 0..4 {
            // A short final chunk is padded: one byte encodes two digits, two
            // bytes encode three.
            if i <= chunk.len() {
                let digit = (group >> (18 - 6 * i)) & 0x3F;
                out.push(ALPHABET[digit as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Whether `pos` falls within `range`, inclusive of both ends so a cursor at a
/// boundary still resolves (the rule [`token_at`] uses). Unlike a token, a
/// `.color` argument list can span lines, so this compares whole positions.
fn range_contains(range: Range, pos: Position) -> bool {
    (range.start.line, range.start.character) <= (pos.line, pos.character)
        && (pos.line, pos.character) <= (range.end.line, range.end.character)
}

/// A block-directive folding tag: a macro body or a conditional block.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BlockTag {
    Macro,
    If,
}

/// Foldable regions: `.macrodef`…`.endm` and `.if*`…`.endif` blocks (nested via
/// a stack), subroutine bodies (a label definition down to the first blank
/// line), plus runs of two or more consecutive line comments.
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

    ranges.extend(subroutine_ranges(text, &lines));
    ranges
}

/// Subroutine folds: each label definition down to the first following blank
/// line (the first `\n\n`), or the end of the buffer when none follows. The
/// fold spans the label line through the last non-blank line of its body, so a
/// subroutine collapses to just its `label:` header. A label with no body line
/// before the blank (`end == start`) yields nothing, since there is nothing to
/// hide.
fn subroutine_ranges(text: &str, lines: &[&str]) -> Vec<FoldingRange> {
    let mut ranges = Vec::new();
    for def in definitions(text) {
        if def.kind != DefKind::Label {
            continue;
        }
        let start = def.range.start.line;
        // Extend to the last non-blank line before the first blank line.
        let mut end = start;
        for (offset, line) in lines.iter().enumerate().skip(start as usize + 1) {
            if line.trim().is_empty() {
                break;
            }
            end = offset as u32;
        }
        if end > start {
            ranges.push(fold(start, end, FoldingRangeKind::Region));
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

/// Whether `pos` sits inside a comment — the context in which comment
/// directives, rather than code, are worth completing.
fn in_comment(text: &str, pos: Position) -> bool {
    located_lexemes(text).into_iter().any(|t| {
        t.kind == LexKind::Comment
            && t.range.start.line == pos.line
            && pos.character > t.range.start.character
            && pos.character <= t.range.end.character
    })
}

/// The comment-directive completions: every registry name, with
/// `@nessemble-coverage-ignore` offered pre-filled as `start` and as `end`
/// rather than as a bare stem the user would be left to complete (and get
/// flagged for).
fn comment_directive_items() -> Vec<CompletionItem> {
    let mut items = Vec::new();
    for name in tooling::DirectiveName::ALL {
        let canonical = name.canonical();
        match name {
            // Both region directives are useless as a bare stem, so offer each
            // bound pre-filled rather than leaving the author to complete it
            // (and get flagged for it).
            tooling::DirectiveName::CoverageIgnore | tooling::DirectiveName::LintIgnore => {
                for bound in ["start", "end"] {
                    items.push(directive_completion(
                        format!("{canonical} {bound}"),
                        directive_docs(name),
                    ));
                }
            }
            tooling::DirectiveName::Format => items.push(directive_completion(
                format!("{canonical} stride="),
                directive_docs(name),
            )),
            // A signature tag is useless without a slot, so offer the space the
            // author is about to type anyway.
            tooling::DirectiveName::Param
            | tooling::DirectiveName::Returns
            | tooling::DirectiveName::Clobbers => items.push(directive_completion(
                format!("{canonical} "),
                directive_docs(name),
            )),
            tooling::DirectiveName::CoverageIgnoreNextLine
            | tooling::DirectiveName::LintIgnoreNextLine => {
                items.push(directive_completion(
                    canonical.to_string(),
                    directive_docs(name),
                ));
            }
        }
    }
    items
}

/// The comment prefix (`;` plus one space, at the line's own indent) to use for a
/// signature block, when the line at `pos` is a comment that a label follows and
/// that label has no signature yet. `None` otherwise.
fn documentable_routine_indent(text: &str, pos: Position) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let line = lines.get(pos.line as usize)?;
    let indent: String = line
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect();
    // Scan down past blank and comment lines the way the resolver binds.
    let target = lines
        .iter()
        .skip(pos.line as usize + 1)
        .find(|l| !l.trim().is_empty() && !l.trim_start().starts_with(';'))?;
    let name = target.trim().strip_suffix(':')?;
    if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    // Already documented? Then the scaffold would duplicate tags.
    if find_signature(text, name).is_some() {
        return None;
    }
    Some(indent)
}

/// The one composite completion that scaffolds a whole signature block.
fn signature_scaffold_item(indent: &str) -> CompletionItem {
    let body = format!(
        "@nessemble-param ${{1:A}} ${{2:description}}\n\
         {indent}; @nessemble-returns ${{3:C}} ${{4:description}}\n\
         {indent}; @nessemble-clobbers ${{5:A, X}}"
    );
    CompletionItem {
        label: "@nessemble routine signature block".to_string(),
        kind: Some(CompletionItemKind::SNIPPET),
        detail: Some("nessemble comment directive".to_string()),
        insert_text: Some(body),
        insert_text_format: Some(lsp_types::InsertTextFormat::SNIPPET),
        // Sort ahead of the individual tags: above a label, the block is what
        // the author almost always wants.
        sort_text: Some("0".to_string()),
        documentation: Some(lsp_types::Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value: "Document the routine below: the registers it takes, what it returns, and \
                    what it destroys. `A`, `X`, `Y`, and `S` in the clobber list are checked \
                    against what the routine actually writes."
                .to_string(),
        })),
        ..Default::default()
    }
}

fn directive_completion(label: String, documentation: &str) -> CompletionItem {
    CompletionItem {
        label,
        kind: Some(CompletionItemKind::SNIPPET),
        detail: Some("nessemble comment directive".to_string()),
        documentation: Some(lsp_types::Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value: documentation.to_string(),
        })),
        ..Default::default()
    }
}

/// One paragraph of documentation per directive — the single source of truth
/// behind both completion and hover.
fn directive_docs(name: tooling::DirectiveName) -> &'static str {
    match name {
        tooling::DirectiveName::Format => {
            "Format the next `.db`/`.dw`/`.color` run with these values per line, \
             overriding `dataPerLine`. Multiple strides cycle and the last one repeats \
             (`stride=2,1`). Blank lines, comment lines, and label or constant \
             definitions in between are skipped, so the hint can sit above the run's \
             label and carry an explanation."
        }
        tooling::DirectiveName::CoverageIgnore => {
            "Bound a region excluded from `nessemble coverage`. `start` opens it, `end` \
             closes it; a region left unclosed runs to the end of the file, which is how a \
             whole file opts out."
        }
        tooling::DirectiveName::CoverageIgnoreNextLine => {
            "Exclude the next significant line from `nessemble coverage` (blank and comment \
             lines in between are skipped, so an explanation may follow this directive)."
        }
        tooling::DirectiveName::Param => {
            "Document a register or memory slot the routine below **reads on entry**. One slot \
             per tag; everything after the slot is its description. Slots are `A`, `X`, `Y`, \
             `S`, `P`, a flag (`C`, `Z`, `N`, `V`, `D`, `I`), a memory symbol (`[tmp1]`), or an \
             address or range (`$10-$1F`)."
        }
        tooling::DirectiveName::Returns => {
            "Document a slot the routine below **defines on exit** — `@nessemble-returns C` is \
             how a 6502 routine returns a boolean. A returned slot is clobbered by definition \
             and need not be repeated in `@nessemble-clobbers`."
        }
        tooling::DirectiveName::Clobbers => {
            "Declare the slots the routine below **destroys**; anything not listed is preserved. \
             `none` claims it preserves everything. `A`, `X`, `Y`, and `S` are checked against \
             what the routine actually writes; flags and memory slots are documentation."
        }
        tooling::DirectiveName::LintIgnoreNextLine => {
            "Suppress lint findings reported on the next significant line (blank and comment \
             lines in between are skipped, so this may sit above a whole annotation block and \
             still land on the label). Bare, it suppresses every rule; with a comma-separated \
             list of rule ids, only those."
        }
        tooling::DirectiveName::LintIgnore => {
            "Bound a region in which lint findings are suppressed. `start` opens it, `end` \
             closes it; an unclosed region runs to the end of the file. Bare, it suppresses \
             every rule; a comma-separated list of rule ids on the `start` suppresses only \
             those."
        }
    }
}

/// Hover for a directive comment: the directive's documentation plus its
/// canonical spelling and argument syntax. `None` for ordinary prose comments.
fn comment_directive_hover(text: &str, pos: Position) -> Option<Hover> {
    use std::fmt::Write as _;

    let (directives, _) = tooling::scan_directives_with_errors(text);
    let d = directives.into_iter().find(|d| d.line == pos.line + 1)?;
    let syntax = match d.name.arg_syntax() {
        "" => d.name.canonical().to_string(),
        args => format!("{} {args}", d.name.canonical()),
    };
    let mut value = format!("```asm\n; {syntax}\n```\n\n{}", directive_docs(d.name));
    if d.deprecated {
        let _ = write!(
            value,
            "\n\n**Deprecated spelling** — use `{}`.",
            d.name.canonical()
        );
    }
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value,
        }),
        range: None,
    })
}

/// A quick fix rewriting a deprecated directive token (`@fmt`) to its canonical
/// spelling, for any deprecated directive on the requested range's line.
fn deprecated_directive_fixes(uri: &Url, text: &str, range: Range) -> Vec<CodeActionOrCommand> {
    let (directives, _) = tooling::scan_directives_with_errors(text);
    directives
        .into_iter()
        .filter(|d| d.deprecated && d.line == range.start.line + 1)
        .map(|d| {
            let token_range = byte_range_to_lsp(text, d.start, d.end);
            let canonical = d.name.canonical();
            let mut changes = HashMap::new();
            changes.insert(
                uri.clone(),
                vec![TextEdit {
                    range: token_range,
                    new_text: canonical.to_string(),
                }],
            );
            CodeActionOrCommand::CodeAction(CodeAction {
                title: format!("Rename to `{canonical}`"),
                kind: Some(CodeActionKind::QUICKFIX),
                edit: Some(WorkspaceEdit {
                    changes: Some(changes),
                    ..Default::default()
                }),
                ..Default::default()
            })
        })
        .collect()
}

/// Quick fixes for an `undeclared-clobber` on the requested line: rewrite the
/// routine's `@nessemble-clobbers` list to include the register the verifier
/// found it writing, in canonical order.
///
/// The verifier already knows which register is missing; making the author
/// retype the list is the difference between a rule people fix and a rule people
/// switch off.
fn undeclared_clobber_fixes(uri: &Url, text: &str, range: Range) -> Vec<CodeActionOrCommand> {
    let line = range.start.line + 1;
    let Some(sig) = tooling::resolve_signatures(text)
        .into_iter()
        .find(|s| s.declares_clobbers && s.line == line)
    else {
        return Vec::new();
    };
    let Some(missing) = tooling::missing_clobbers(text, &sig.name) else {
        return Vec::new();
    };
    if missing.is_empty() {
        return Vec::new();
    }

    // The tag to rewrite: the `@nessemble-clobbers` bound to this routine.
    let Some(directive) = tooling::scan_directives(text).into_iter().find(|d| {
        d.own_line
            && d.name == tooling::DirectiveName::Clobbers
            && d.line < sig.line
            && d.line >= sig.first_tag_line
    }) else {
        return Vec::new();
    };
    let mut slots = sig.clobbers.clone();
    slots.extend(missing.iter().cloned());
    let new_list = tooling::format_slots(&slots);
    let added = tooling::format_slots(&missing);

    // Replace from the end of the directive token to the start of any trailing
    // prose (or end of line), so `; @nessemble-clobbers A ; why` keeps its why.
    let line_start = line_start_byte(text, directive.line as usize - 1);
    let line_text = text[line_start..].lines().next().unwrap_or("");
    let args_start = directive.end;
    let args_end = line_start + trailing_prose_offset(line_text, args_start - line_start);
    let mut changes = HashMap::new();
    changes.insert(
        uri.clone(),
        vec![TextEdit {
            range: byte_range_to_lsp(text, args_start, args_end),
            new_text: format!(" {new_list}"),
        }],
    );
    vec![CodeActionOrCommand::CodeAction(CodeAction {
        title: format!("Add `{added}` to `@nessemble-clobbers`"),
        kind: Some(CodeActionKind::QUICKFIX),
        edit: Some(WorkspaceEdit {
            changes: Some(changes),
            ..Default::default()
        }),
        ..Default::default()
    })]
}

/// The byte offset, within `line_text`, where the argument text starting at
/// `from` ends: at the `;` that opens trailing prose, or end of line, with any
/// whitespace before it excluded so the prose keeps its separating space.
fn trailing_prose_offset(line_text: &str, from: usize) -> usize {
    let end = match line_text[from..].find(';') {
        Some(i) => from + i,
        None => line_text.len(),
    };
    line_text[from..end].trim_end().len() + from
}

/// The byte offset of 0-based `line` in `text`.
fn line_start_byte(text: &str, line: usize) -> usize {
    text.split_inclusive('\n')
        .take(line)
        .map(str::len)
        .sum::<usize>()
}

/// A "Document this routine" action on an undocumented label: insert a signature
/// block above it, at the label's own indentation.
fn document_routine_actions(uri: &Url, text: &str, range: Range) -> Vec<CodeActionOrCommand> {
    let lines: Vec<&str> = text.lines().collect();
    let idx = range.start.line as usize;
    let Some(line) = lines.get(idx) else {
        return Vec::new();
    };
    let Some(name) = line.trim().strip_suffix(':') else {
        return Vec::new();
    };
    if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return Vec::new();
    }
    if find_signature(text, name).is_some() {
        return Vec::new();
    }
    let indent: String = line
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect();
    let block = format!(
        "{indent}; @nessemble-param A description\n\
         {indent}; @nessemble-returns C description\n\
         {indent}; @nessemble-clobbers A, X\n"
    );
    let at = Position::new(range.start.line, 0);
    let mut changes = HashMap::new();
    changes.insert(
        uri.clone(),
        vec![TextEdit {
            range: Range::new(at, at),
            new_text: block,
        }],
    );
    vec![CodeActionOrCommand::CodeAction(CodeAction {
        title: format!("Document routine `{name}`"),
        kind: Some(CodeActionKind::REFACTOR),
        edit: Some(WorkspaceEdit {
            changes: Some(changes),
            ..Default::default()
        }),
        ..Default::default()
    })]
}

/// Convert a byte range that lies within one line into an LSP [`Range`]
/// (UTF-16 columns).
fn byte_range_to_lsp(text: &str, start: usize, end: usize) -> Range {
    let line = text[..start].matches('\n').count() as u32;
    let line_start = text[..start].rfind('\n').map_or(0, |i| i + 1);
    let col = utf16_len(&text[line_start..start]);
    let len = utf16_len(&text[start..end]);
    Range::new(Position::new(line, col), Position::new(line, col + len))
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
    match text.as_bytes().last() {
        _ if text.starts_with('$') => Some(Base::Hex),
        _ if text.starts_with('%') => Some(Base::Bin),
        // The suffixed spellings (`1Ah`, `1011b`, `17o`, `42d`).
        Some(b'h') => Some(Base::Hex),
        Some(b'b') => Some(Base::Bin),
        Some(b'd') => Some(Base::Dec),
        Some(b'o') => None,                                   // octal
        _ if text.len() > 1 && text.starts_with('0') => None, // octal
        _ => Some(Base::Dec),
    }
}

/// Parse a nessemble numeric literal into its value: the prefixed spellings
/// (`$hex`, `%bin`, `0octal`, decimal) and the suffixed ones (`1Ah`, `1011b`,
/// `17o`, `42d`) the assembler's lexer also accepts.
fn parse_number(text: &str) -> Option<i64> {
    if let Some(hex) = text.strip_prefix('$') {
        return i64::from_str_radix(hex, 16).ok();
    }
    if let Some(bin) = text.strip_prefix('%') {
        return i64::from_str_radix(bin, 2).ok();
    }
    // A suffix only makes a literal when every digit before it fits the radix;
    // otherwise the text falls through (`bad` is not binary `b`).
    for (suffix, radix) in [(b'h', 16), (b'b', 2), (b'o', 8), (b'd', 10)] {
        if let Some(digits) = text
            .strip_suffix(suffix as char)
            .filter(|d| !d.is_empty() && d.bytes().all(|b| (b as char).is_digit(radix)))
        {
            return i64::from_str_radix(digits, radix).ok();
        }
    }
    if text.len() > 1 && text.starts_with('0') && text.bytes().all(|b| (b'0'..=b'7').contains(&b)) {
        return i64::from_str_radix(text, 8).ok();
    }
    text.parse::<i64>().ok()
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
            // `/` re-triggers inside a filename argument, so completion keeps
            // offering entries as the author walks into a subdirectory.
            trigger_characters: Some(vec![".".to_string(), "/".to_string()]),
            ..Default::default()
        }),
        document_formatting_provider: Some(OneOf::Left(true)),
        document_link_provider: Some(DocumentLinkOptions {
            resolve_provider: Some(false),
            work_done_progress_options: WorkDoneProgressOptions::default(),
        }),
        semantic_tokens_provider: Some(SemanticTokensServerCapabilities::SemanticTokensOptions(
            SemanticTokensOptions {
                work_done_progress_options: WorkDoneProgressOptions::default(),
                legend: SemanticTokensLegend {
                    token_types: TOKEN_TYPES.to_vec(),
                    token_modifiers: TOKEN_MODIFIERS.to_vec(),
                },
                range: Some(false),
                full: Some(SemanticTokensFullOptions::Bool(true)),
            },
        )),
        document_symbol_provider: Some(OneOf::Left(true)),
        // `.rhai` call signature help (§5.4): re-triggers on `,` as the author
        // moves between arguments.
        signature_help_provider: Some(SignatureHelpOptions {
            trigger_characters: Some(vec!["(".to_string(), ",".to_string()]),
            retrigger_characters: None,
            work_done_progress_options: WorkDoneProgressOptions::default(),
        }),
        definition_provider: Some(OneOf::Left(true)),
        references_provider: Some(OneOf::Left(true)),
        hover_provider: Some(lsp_types::HoverProviderCapability::Simple(true)),
        folding_range_provider: Some(FoldingRangeProviderCapability::Simple(true)),
        rename_provider: Some(OneOf::Left(true)),
        code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
        inlay_hint_provider: Some(OneOf::Left(true)),
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
                            .map(|p| {
                                server.complete(
                                    &p.text_document_position.text_document.uri,
                                    p.text_document_position.position,
                                )
                            })
                            .unwrap_or_default();
                        Response::new_ok(req.id, CompletionResponse::Array(items))
                    }
                    DocumentLinkRequest::METHOD => {
                        let links = serde_json::from_value::<DocumentLinkParams>(req.params)
                            .ok()
                            .and_then(|p| server.document_links(&p.text_document.uri));
                        Response::new_ok(req.id, links)
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
                    InlayHintRequest::METHOD => {
                        let result = serde_json::from_value::<InlayHintParams>(req.params)
                            .ok()
                            .map(|p| server.inlay_hints(&p.text_document.uri, p.range))
                            .unwrap_or_default();
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
                    SignatureHelpRequest::METHOD => {
                        let result = serde_json::from_value::<SignatureHelpParams>(req.params)
                            .ok()
                            .and_then(|p| {
                                let tdp = p.text_document_position_params;
                                server.signature_help(&tdp.text_document.uri, tdp.position)
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
        let ls = labels(server.complete(&uri, Position::new(4, 2)));
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

    // ---- `.color` palette preview -----------------------------------------

    /// The markdown of the hover at `pos` in a freshly opened `text`.
    fn hover_markdown(uri: &str, text: &str, pos: Position) -> Option<String> {
        let mut server = Server::default();
        open(&mut server, uri, text);
        let hover = server.hover(&Url::parse(uri).unwrap(), pos)?;
        let HoverContents::Markup(md) = hover.contents else {
            panic!("expected markup hover");
        };
        Some(md.value)
    }

    #[test]
    fn hover_on_color_previews_every_argument() {
        // Hovering the directive lists the whole argument run: each argument's
        // source RGB, the palette index the assembler emits, and the color the
        // PPU shows for it — plus one image of the run as those colors.
        let md = hover_markdown(
            "file:///col.asm",
            "  .color $FF0000, $00FF00, $0000FF\n",
            Position::new(0, 4),
        )
        .expect("hover on .color");
        assert!(md.starts_with("**.color** — 3 colors"), "{md}");
        assert!(
            md.contains("![NES colors](data:image/svg+xml;base64,"),
            "{md}"
        );
        assert!(
            md.contains("| `$FF0000` | `#FF0000` | `$16` | `#F83800` |"),
            "{md}"
        );
        assert!(
            md.contains("| `$00FF00` | `#00FF00` | `$19` | `#00B800` |"),
            "{md}"
        );
        assert!(
            md.contains("| `$0000FF` | `#0000FF` | `$01` | `#0000FC` |"),
            "{md}"
        );
    }

    #[test]
    fn hover_on_one_color_argument_shows_just_that_color() {
        // The cursor is inside the second argument, so only its color is shown.
        let md = hover_markdown(
            "file:///col1.asm",
            "  .color $FF0000, $00FF00\n",
            Position::new(0, 20),
        )
        .expect("hover on an argument");
        assert!(md.starts_with("**.color** — argument 2 of 2"), "{md}");
        assert!(md.contains("![#00B800](data:image/svg+xml;base64,"), "{md}");
        assert!(
            md.contains("`$00FF00` (`#00FF00`) → NES color `$19` (`#00B800`)"),
            "{md}"
        );
        // The whole-list table belongs to the directive hover, not this one.
        assert!(!md.contains("| argument |"), "{md}");
    }

    #[test]
    fn color_hover_resolves_expressions_and_symbols() {
        // An argument is an expression, not just a literal: constants defined in
        // the buffer resolve, and arithmetic is evaluated as the assembler does.
        let text = "RED = $FF0000\nSHIFT = 8\n  \
                    .color RED, $00FF00 >> SHIFT, $FFFFFF & $00FF00, LOW($1234FF)\n";
        let md =
            hover_markdown("file:///cole.asm", text, Position::new(2, 4)).expect("hover on .color");
        assert!(
            md.contains("| `RED` | `#FF0000` | `$16` | `#F83800` |"),
            "{md}"
        );
        // $00FF00 >> 8 == $FF, a blue so dark it matches palette entry $01.
        assert!(md.contains("| `$00FF00>>SHIFT` | `#0000FF` |"), "{md}");
        assert!(
            md.contains("| `$FFFFFF&$00FF00` | `#00FF00` | `$19` |"),
            "{md}"
        );
        assert!(
            md.contains("| `LOW($1234FF)` | `#0000FF` | `$01` |"),
            "{md}"
        );
    }

    #[test]
    fn color_hover_follows_a_trailing_comma_onto_the_next_line() {
        // A trailing comma continues the argument list, exactly as the parser
        // reads it, so the run's later colors are previewed too.
        let text = "  .color $FF0000,\n          $00FF00\n";
        let md =
            hover_markdown("file:///colc.asm", text, Position::new(0, 4)).expect("hover on .color");
        assert!(md.starts_with("**.color** — 2 colors"), "{md}");
        // …and hovering the continued argument previews that one color.
        let md = hover_markdown("file:///colc2.asm", text, Position::new(1, 12))
            .expect("hover on the continued argument");
        assert!(md.starts_with("**.color** — argument 2 of 2"), "{md}");
    }

    #[test]
    fn color_hover_ignores_what_follows_the_statement() {
        // Without a trailing comma the list ends with the line, and a trailing
        // comment is not an argument.
        let text = "  .color $FF0000 ; red\n  .db $01, $02\n";
        let md =
            hover_markdown("file:///cold.asm", text, Position::new(0, 4)).expect("hover on .color");
        assert!(md.starts_with("**.color** — 1 color"), "{md}");
    }

    #[test]
    fn color_hover_falls_back_when_an_argument_is_unresolved() {
        // An undefined symbol has no color to show, so the ordinary hover runs:
        // the list marks the argument unresolved, and the argument itself gets
        // whatever the generic hover can say about it (here, nothing).
        let text = "  .color $FF0000, NOPE\n";
        let md =
            hover_markdown("file:///colu.asm", text, Position::new(0, 4)).expect("hover on .color");
        assert!(md.contains("| `NOPE` | ? | ? | ? |"), "{md}");
        assert!(hover_markdown("file:///colu2.asm", text, Position::new(0, 19)).is_none());
    }

    #[test]
    fn color_hover_matches_the_assembled_bytes() {
        // The preview must never disagree with the ROM: the indices in the
        // hover table are the bytes `.color` actually emits.
        let source = "  .color $FF0000, $00FF00, $123456, $FFFFFF\n";
        let rom = nessemble_core::assemble(source, &Options::default())
            .expect("assembles")
            .rom;
        let md = hover_markdown("file:///colb.asm", source, Position::new(0, 4))
            .expect("hover on .color");
        for byte in rom {
            assert!(
                md.contains(&format!("| `${byte:02X}` | ")),
                "${byte:02X} in {md}"
            );
        }
    }

    #[test]
    fn color_hover_is_only_for_color() {
        // Another data directive keeps its ordinary description hover.
        let md = hover_markdown("file:///colo.asm", "  .db $FF0000\n", Position::new(0, 4))
            .expect("hover on .db");
        assert!(md.starts_with("**.db** (directive)"), "{md}");
        // A bare `.color` has nothing to preview, so it falls back too.
        let md = hover_markdown("file:///colo2.asm", "  .color\n", Position::new(0, 4))
            .expect("hover on a bare .color");
        assert!(md.starts_with("**.color** (directive)"), "{md}");
    }

    #[test]
    fn swatch_images_are_valid_base64() {
        // The encoder is hand-rolled, so pin it to the RFC 4648 test vectors,
        // padding included.
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn numeric_literals_parse_in_every_spelling() {
        // Both the prefixed and the suffixed spellings the assembler accepts.
        assert_eq!(parse_number("$1A"), Some(26));
        assert_eq!(parse_number("%1011"), Some(11));
        assert_eq!(parse_number("017"), Some(15));
        assert_eq!(parse_number("42"), Some(42));
        assert_eq!(parse_number("1Ah"), Some(26));
        assert_eq!(parse_number("1011b"), Some(11));
        assert_eq!(parse_number("17o"), Some(15));
        assert_eq!(parse_number("42d"), Some(42));
        // A word that merely ends in a radix letter is not a literal.
        assert_eq!(parse_number("bad"), None);
        assert_eq!(parse_number("h"), None);
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
        diags_for_uri(pubs, &uri)
    }

    fn diags_for_uri<'a>(
        pubs: &'a [PublishDiagnosticsParams],
        uri: &Url,
    ) -> Option<&'a Vec<Diagnostic>> {
        pubs.iter().find(|p| &p.uri == uri).map(|p| &p.diagnostics)
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

    #[test]
    fn include_graph_reflects_an_external_change_to_an_include_line() {
        // The include *structure* is cached per disk file by (mtime, len), so
        // changing it on disk must invalidate that cache. main.asm pulls in
        // consts.asm (which defines `palette`); after main.asm is rewritten on
        // disk to drop that include, re-analysis must flag `palette` as undefined
        // — proving the cached include list for main.asm was not served stale.
        let w = workspace("inc-cache");
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
        assert!(
            diags_for(&pubs, &code).expect("published").is_empty(),
            "palette should resolve via the project initially"
        );

        // Rewrite main.asm on disk so it no longer includes consts.asm. The file
        // is shorter, so its (mtime, len) signature changes even if the clock's
        // mtime granularity is coarse.
        write(&w, "main.asm", ".include \"code.asm\"\n");
        let uri = Url::from_file_path(&code).unwrap();
        let changed = server.apply_notification(
            DidChangeTextDocument::METHOD,
            serde_json::json!({
                "textDocument": { "uri": uri.as_str(), "version": 2 },
                "contentChanges": [{ "text": "  lda palette\n" }]
            }),
        );
        let d = diags_for(&changed, &code).expect("published");
        assert_eq!(d.len(), 1, "palette should now be undefined: {d:?}");
        assert!(d[0].message.contains("palette"));

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
    fn folding_ranges_fold_subroutines_to_the_blank_line() {
        let text = concat!(
            "reset:\n",     // 0
            "  sei\n",      // 1
            "  cld\n",      // 2
            "  rts\n",      // 3
            "\n",           // 4  (the first \n\n after `reset`)
            "loop:\n",      // 5
            "  jmp loop\n", // 6
        );
        let ranges = folding_ranges(text);
        // `reset` folds from its label (0) through its last body line (3), the
        // line before the blank at 4.
        assert!(ranges.iter().any(|r| r.start_line == 0
            && r.end_line == 3
            && r.kind == Some(FoldingRangeKind::Region)));
        // `loop` runs to the end of the buffer (no trailing blank line).
        assert!(ranges.iter().any(|r| r.start_line == 5 && r.end_line == 6));
    }

    #[test]
    fn folding_ranges_skip_a_bodyless_label() {
        // A label immediately followed by a blank line has nothing to fold.
        let text = "empty:\n\n  nop\n";
        let ranges = folding_ranges(text);
        assert!(!ranges.iter().any(|r| r.start_line == 0));
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

    // ─── `.rhai` documents (plan 014, Phases 2–3) ──────────────────────────────

    /// A `.rhai` buffer is never analyzed as assembly: opening one whose text
    /// would be a pile of assembler errors (and is also invalid Rhai) must not
    /// yield `source: "nessemble"` diagnostics, and every assembly-only
    /// surface must answer with nothing rather than confident nonsense
    /// (`plans/014-scripting-docs-and-tooling.md` §5.1, Risks table).
    #[test]
    fn rhai_document_is_never_analyzed_as_assembly() {
        let mut server = Server::default();
        let uri = Url::parse("file:///script.rhai").unwrap();
        let text = "!!! not valid rhai or asm {{{ .foo bar";
        let pubs =
            server.apply_notification(DidOpenTextDocument::METHOD, open_params(uri.as_str(), text));
        let diags = diags_for_uri(&pubs, &uri).expect("published");
        assert!(
            diags
                .iter()
                .all(|d| d.source.as_deref() != Some("nessemble")),
            "an assembly diagnostic leaked into a `.rhai` buffer: {diags:?}"
        );
        assert!(diags
            .iter()
            .all(|d| d.source.as_deref() != Some("nessemble-lint")));

        assert!(server.format_document(&uri).is_none_or(|e| e.is_empty()));
        assert!(server.semantic_tokens(&uri).is_none());
        assert!(server.code_actions(&uri, Range::default()).is_empty());
        assert!(server
            .inlay_hints(&uri, Range::new(Position::new(0, 0), Position::new(10, 0)))
            .is_empty());
        assert!(server
            .rename(&uri, Position::new(0, 0), "renamed")
            .is_none());
        assert!(server.document_links(&uri).is_none_or(|l| l.is_empty()));
    }

    /// `.rhai` detection also works for an extensionless buffer, from the
    /// `didOpen` notification's `languageId` (§5.1).
    #[test]
    fn rhai_kind_falls_back_to_the_language_id_for_an_extensionless_buffer() {
        let uri = Url::parse("file:///untitled:Untitled-1").unwrap();
        assert_eq!(doc_kind(&uri, "rhai"), DocKind::Rhai);
        assert_eq!(doc_kind(&uri, "nessemble"), DocKind::Asm);
    }

    #[test]
    fn rhai_completion_offers_the_host_api_catalog() {
        let mut server = Server::default();
        let uri = Url::parse("file:///c.rhai").unwrap();
        server.apply_notification(
            DidOpenTextDocument::METHOD,
            open_params(uri.as_str(), "fn custom(ints, texts) { [] }\n"),
        );
        let items = server.complete(&uri, Position::new(0, 0));
        assert!(items.iter().any(|i| i.label == "nes_shade"));
        assert!(items.iter().any(|i| i.label == "custom"));
    }

    #[test]
    fn rhai_hover_shows_the_catalog_entry() {
        let mut server = Server::default();
        let uri = Url::parse("file:///h.rhai").unwrap();
        let text = "fn custom(ints, texts) { nes_shade(ints[0]) }\n";
        server.apply_notification(DidOpenTextDocument::METHOD, open_params(uri.as_str(), text));
        let hover = server
            .hover(&uri, Position::new(0, 27))
            .expect("hovering `nes_shade`");
        let HoverContents::Markup(md) = hover.contents else {
            panic!("expected markdown hover");
        };
        assert!(md.value.contains("nes_shade(value)"));
    }

    #[test]
    fn rhai_signature_help_reaches_the_editor() {
        let mut server = Server::default();
        let uri = Url::parse("file:///s.rhai").unwrap();
        let text = "fn custom(ints, texts) { format_hex(255,";
        server.apply_notification(DidOpenTextDocument::METHOD, open_params(uri.as_str(), text));
        let help = server
            .signature_help(&uri, Position::new(0, text.chars().count() as u32))
            .expect("inside a call");
        assert_eq!(help.active_parameter, Some(1));
    }

    #[test]
    fn signature_help_is_none_for_an_assembly_document() {
        let mut server = Server::default();
        let uri = Url::parse("file:///s.asm").unwrap();
        server.apply_notification(
            DidOpenTextDocument::METHOD,
            open_params(uri.as_str(), "  lda #$00\n"),
        );
        assert!(server.signature_help(&uri, Position::new(0, 5)).is_none());
    }

    #[test]
    fn custom_directive_hover_shows_the_scripts_path_and_doc_comment() {
        let w = workspace("hover-custom");
        write(&w, "pseudo.txt", ".double = double.rhai\n");
        write(
            &w,
            "double.rhai",
            "// Doubles the first integer argument.\nfn custom(ints, texts) { [ints[0] * 2] }\n",
        );
        let main = w.join("main.asm");
        let text = "  .double 5\n";
        write(&w, "main.asm", text);

        let mut server = server_for(&w);
        did_open(&mut server, &main, text);
        let main_uri = Url::from_file_path(&main).unwrap();
        let hover = server
            .hover(&main_uri, Position::new(0, 4))
            .expect("hovering `.double`");
        let HoverContents::Markup(md) = hover.contents else {
            panic!("expected markdown hover");
        };
        assert!(md.value.contains("double.rhai"));
        // The doc comment is part of the compiled-AST half (§5.8): gated
        // behind `scripting` by design, even though reading it is a text
        // scan. The path half above holds either way.
        #[cfg(feature = "scripting")]
        assert!(md.value.contains("Doubles the first integer argument."));

        let _ = std::fs::remove_dir_all(&w);
    }

    #[cfg(feature = "scripting")]
    #[test]
    fn rhai_lints_reach_the_editor() {
        let mut server = Server::default();
        let uri = Url::parse("file:///l.rhai").unwrap();
        let text = "const SCALE = 3;\nfn custom(ints, texts) { [SCALE] }\n";
        let pubs =
            server.apply_notification(DidOpenTextDocument::METHOD, open_params(uri.as_str(), text));
        let diags = diags_for_uri(&pubs, &uri).expect("published");
        assert!(diags
            .iter()
            .any(|d| d.code == Some(NumberOrString::String("top-level-statement".to_string()))));
    }

    #[cfg(feature = "scripting")]
    #[test]
    fn rhai_document_symbols_and_folding_and_local_navigation() {
        let mut server = Server::default();
        let uri = Url::parse("file:///n.rhai").unwrap();
        let text = "fn helper(x) { x }\nfn custom(ints, texts) { helper(ints[0]) }\n";
        server.apply_notification(DidOpenTextDocument::METHOD, open_params(uri.as_str(), text));

        let syms = server.document_symbols(&uri).expect("known document");
        assert_eq!(syms.len(), 2);
        assert_eq!(syms[0].name, "custom");

        let folds = server.folding_ranges(&uri).expect("known document");
        assert!(folds.is_empty(), "no fn body spans more than one line here");

        // `helper` on the `custom` call site (line 1, inside `helper(...)`).
        let call_pos = Position::new(1, 27);
        let def = server
            .goto_definition(&uri, call_pos)
            .expect("jumps to the script-local fn");
        assert_eq!(def.range.start.line, 0);

        let refs = server
            .references(&uri, call_pos, true)
            .expect("known document");
        assert_eq!(refs.len(), 2);
    }

    // ─── Lint diagnostics ─────────────────────────────────────────────────────

    /// The lint diagnostics of `text` (those sourced from `nessemble-lint`).
    fn lint_only(diags: &[Diagnostic]) -> Vec<&Diagnostic> {
        diags
            .iter()
            .filter(|d| d.source.as_deref() == Some("nessemble-lint"))
            .collect()
    }

    #[test]
    fn lint_flags_an_undocumented_block_at_a_gentle_severity() {
        let uri = Url::parse("file:///lint_undoc.asm").unwrap();
        // No `.nessemblerc` up the tree → built-in defaults (warn, window 3).
        let diags = lint_diagnostics(&uri, "\nwidget:\n    rts\n");
        assert_eq!(diags.len(), 1);
        let d = &diags[0];
        assert_eq!(d.source.as_deref(), Some("nessemble-lint"));
        assert_eq!(d.severity, Some(DiagnosticSeverity::HINT));
        assert_eq!(
            d.code,
            Some(NumberOrString::String("require-block-comment".into()))
        );
        // Range narrows to the `widget` label on line 1 (0-based).
        assert_eq!(d.range.start.line, 1);
        assert_eq!(d.range.start.character, 0);
        assert_eq!(d.range.end.character, 6);
    }

    #[test]
    fn lint_is_clean_when_a_comment_is_near() {
        let uri = Url::parse("file:///lint_clean.asm").unwrap();
        let diags = lint_diagnostics(&uri, "\n; documents the routine\nwidget:\n    rts\n");
        assert!(diags.is_empty());
    }

    #[test]
    fn lint_diagnostics_publish_on_open_and_clear_when_documented() {
        let dir = workspace("lint-open");
        let file = dir.join("a.asm");
        let mut server = Server::default();

        // Open an undocumented block → a lint diagnostic is published.
        let pubs = did_open(&mut server, &file, "\nwidget:\n    rts\n");
        let d = diags_for(&pubs, &file).expect("a.asm published");
        assert_eq!(lint_only(d).len(), 1);

        // Change the buffer to add a nearby comment → the lint diagnostic clears.
        let uri = Url::from_file_path(&file).unwrap();
        let pubs = server.apply_notification(
            DidChangeTextDocument::METHOD,
            serde_json::json!({
                "textDocument": { "uri": uri, "version": 2 },
                "contentChanges": [{ "text": "\n; now documented\nwidget:\n    rts\n" }]
            }),
        );
        let d = diags_for(&pubs, &file).expect("a.asm republished");
        assert!(
            lint_only(d).is_empty(),
            "comment should clear the lint finding"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lint_honors_nessemblerc_ignore() {
        let dir = workspace("lint-ignore");
        write(&dir, ".nessemblerc", r#"{"lint":{"ignore":["^loc_"]}}"#);
        let file = dir.join("a.asm");
        let uri = Url::from_file_path(&file).unwrap();
        let diags = lint_diagnostics(&uri, "\nloc_c000:\n    nop\n    rts\n\nreal:\n    rts\n");
        let subjects: Vec<&str> = diags.iter().map(|d| d.message.as_str()).collect();
        assert!(
            !subjects.iter().any(|m| m.contains("loc_c000")),
            "{subjects:?}"
        );
        assert!(subjects.iter().any(|m| m.contains("real")), "{subjects:?}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lint_off_rule_is_silent() {
        let dir = workspace("lint-off");
        write(
            &dir,
            ".nessemblerc",
            r#"{"lint":{"rules":{"require-block-comment":"off"}}}"#,
        );
        let file = dir.join("a.asm");
        let uri = Url::from_file_path(&file).unwrap();
        let diags = lint_diagnostics(&uri, "\nwidget:\n    rts\n");
        assert!(diags.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── Comment directives ──────────────────────────────────────────────────

    #[test]
    fn lint_publishes_a_directive_diagnostic() {
        let dir = workspace("lint-directive");
        let file = dir.join("a.asm");
        let uri = Url::from_file_path(&file).unwrap();
        let diags = lint_diagnostics(&uri, "; @nessemble-formt stride=2\n.db $01\n");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].source.as_deref(), Some("nessemble-lint"));
        assert_eq!(
            diags[0].code,
            Some(NumberOrString::String("unknown-comment-directive".into()))
        );
        assert!(diags[0].message.contains("@nessemble-formt"), "{diags:?}");
        assert_eq!(diags[0].severity, Some(DiagnosticSeverity::HINT));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn completion_inside_a_comment_offers_directives_not_mnemonics() {
        let mut server = Server::default();
        open(&mut server, "file:///d.asm", "; @\n  lda #$00\n");
        let uri = Url::parse("file:///d.asm").unwrap();
        let ls = labels(server.complete(&uri, Position::new(0, 3)));
        assert!(
            ls.iter().any(|l| l == "@nessemble-coverage-ignore start"),
            "{ls:?}"
        );
        assert!(
            ls.iter().any(|l| l == "@nessemble-coverage-ignore end"),
            "{ls:?}"
        );
        assert!(
            ls.iter()
                .any(|l| l == "@nessemble-coverage-ignore-next-line"),
            "{ls:?}"
        );
        assert!(
            ls.iter().any(|l| l == "@nessemble-format stride="),
            "{ls:?}"
        );
        assert!(!ls.iter().any(|l| l == "lda"), "code leaked into a comment");

        // Outside a comment the ordinary completions still apply.
        let ls = labels(server.complete(&uri, Position::new(1, 5)));
        assert!(ls.iter().any(|l| l == "lda"), "{ls:?}");
    }

    #[test]
    fn hover_documents_a_comment_directive() {
        let mut server = Server::default();
        open(
            &mut server,
            "file:///h.asm",
            "; @nessemble-coverage-ignore start\n  nop\n",
        );
        let uri = Url::parse("file:///h.asm").unwrap();
        let hover = server.hover(&uri, Position::new(0, 10)).expect("hover");
        let HoverContents::Markup(m) = hover.contents else {
            panic!("expected markup");
        };
        assert!(m.value.contains("@nessemble-coverage-ignore start|end"));
        assert!(m.value.contains("end of the file"));

        // A deprecated spelling says so, and an ordinary comment has no hover.
        open(&mut server, "file:///h2.asm", "; @fmt stride=2\n.db $01\n");
        let uri2 = Url::parse("file:///h2.asm").unwrap();
        let hover = server.hover(&uri2, Position::new(0, 4)).expect("hover");
        let HoverContents::Markup(m) = hover.contents else {
            panic!("expected markup");
        };
        assert!(m.value.contains("Deprecated"), "{}", m.value);

        open(&mut server, "file:///h3.asm", "; just a note\n  nop\n");
        let uri3 = Url::parse("file:///h3.asm").unwrap();
        assert!(server.hover(&uri3, Position::new(0, 5)).is_none());
    }

    #[test]
    fn code_action_renames_a_deprecated_directive() {
        let mut server = Server::default();
        open(&mut server, "file:///q.asm", "  ; @fmt stride=2\n.db $01\n");
        let uri = Url::parse("file:///q.asm").unwrap();
        let actions =
            server.code_actions(&uri, Range::new(Position::new(0, 5), Position::new(0, 5)));
        let CodeActionOrCommand::CodeAction(action) = &actions[0] else {
            panic!("expected a code action");
        };
        assert_eq!(action.title, "Rename to `@nessemble-format`");
        let edits = action.edit.as_ref().unwrap().changes.as_ref().unwrap();
        let edit = &edits[&uri][0];
        assert_eq!(edit.new_text, "@nessemble-format");
        // The edit covers exactly the `@fmt` token.
        assert_eq!(
            edit.range,
            Range::new(Position::new(0, 4), Position::new(0, 8))
        );
    }

    #[test]
    fn semantic_tokens_mark_a_directive_comment_with_a_modifier() {
        let text = "; @nessemble-format stride=2\n; ordinary note\n.db $01\n";
        let tokens = semantic_tokens(text);
        // Both comments keep the COMMENT type (wire id 5); only the directive
        // one carries the documentation modifier.
        let comment_type = tooling::TokenClass::Comment.wire_id();
        let comments: Vec<&SemanticToken> = tokens
            .iter()
            .filter(|t| t.token_type == comment_type)
            .collect();
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0].token_modifiers_bitset, MODIFIER_DOCUMENTATION);
        assert_eq!(comments[1].token_modifiers_bitset, 0);
        // A mistyped directive is marked too, so it does not read as prose.
        let tokens = semantic_tokens("; @nessemble-formt\n");
        assert_eq!(tokens[0].token_modifiers_bitset, MODIFIER_DOCUMENTATION);
    }

    #[test]
    fn semantic_token_legend_declares_the_modifier() {
        let caps = server_capabilities();
        let Some(SemanticTokensServerCapabilities::SemanticTokensOptions(opts)) =
            caps.semantic_tokens_provider
        else {
            panic!("expected semantic tokens options");
        };
        assert_eq!(
            opts.legend.token_modifiers,
            vec![SemanticTokenModifier::DOCUMENTATION]
        );
        // The type legend is unchanged — the wire ids stay frozen.
        assert_eq!(opts.legend.token_types.len(), 7);
    }

    // ---- Routine signatures -----------------------------------------------

    /// A documented routine and a call site for it, in one buffer.
    const SIGNED: &str = "; Draw one metasprite.\n\
                          ;\n\
                          ; @nessemble-param A metasprite index\n\
                          ; @nessemble-returns C set when clipped\n\
                          ; @nessemble-clobbers A, X, [oam_cursor]\n\
                          draw_metasprite:\n\
                          \x20   RTS\n\
                          \n\
                          main:\n\
                          \x20   JSR draw_metasprite\n\
                          \x20   RTS\n";

    #[test]
    fn hover_on_a_call_site_shows_the_callees_signature() {
        // The headline surface: the contract is readable where the call is,
        // without scrolling to the callee.
        let md = hover_markdown("file:///sig.asm", SIGNED, Position::new(9, 12))
            .expect("hover on the JSR operand");
        assert!(md.contains("| in | |"), "{md}");
        assert!(md.contains("| `A` | metasprite index |"), "{md}");
        assert!(md.contains("| `C` | set when clipped |"), "{md}");
        assert!(md.contains("**clobbers** `A, X, [oam_cursor]`"), "{md}");
    }

    #[test]
    fn hover_on_the_definition_shows_the_same_signature() {
        let md = hover_markdown("file:///sig.asm", SIGNED, Position::new(5, 3))
            .expect("hover on the label");
        assert!(md.contains("**clobbers** `A, X, [oam_cursor]`"), "{md}");
        // The prose above the tags still comes through as documentation.
        assert!(md.contains("Draw one metasprite."), "{md}");
    }

    #[test]
    fn hover_finds_a_signature_in_another_open_document() {
        // A `JSR` routinely names a routine defined in a sibling file, which is
        // exactly where the contract is least visible.
        let mut server = Server::default();
        open(
            &mut server,
            "file:///lib.asm",
            "; @nessemble-clobbers Y\nhelper:\n    RTS\n",
        );
        open(&mut server, "file:///main.asm", "main:\n    JSR helper\n");
        let uri = Url::parse("file:///main.asm").unwrap();
        let hover = server.hover(&uri, Position::new(1, 10)).expect("hover");
        let HoverContents::Markup(m) = hover.contents else {
            panic!("expected markup");
        };
        assert!(m.value.contains("**clobbers** `Y`"), "{}", m.value);
    }

    #[test]
    fn outline_detail_carries_the_clobber_list() {
        let mut server = Server::default();
        open(&mut server, "file:///o2.asm", SIGNED);
        let uri = Url::parse("file:///o2.asm").unwrap();
        let syms = server.document_symbols(&uri).expect("known document");
        let drawn = syms
            .iter()
            .find(|s| s.name == "draw_metasprite")
            .expect("the routine is in the outline");
        assert_eq!(
            drawn.detail.as_deref(),
            Some("label · clobbers A, X, [oam_cursor]")
        );
        // An undocumented label keeps the plain detail.
        let main = syms.iter().find(|s| s.name == "main").expect("main");
        assert_eq!(main.detail.as_deref(), Some("label"));
    }

    #[test]
    fn completion_offers_the_signature_tags_and_a_scaffold_above_a_label() {
        let mut server = Server::default();
        open(&mut server, "file:///c2.asm", "; @\nwidget:\n    RTS\n");
        let uri = Url::parse("file:///c2.asm").unwrap();
        let ls = labels(server.complete(&uri, Position::new(0, 3)));
        assert!(ls.iter().any(|l| l == "@nessemble-param "), "{ls:?}");
        assert!(ls.iter().any(|l| l == "@nessemble-returns "), "{ls:?}");
        assert!(ls.iter().any(|l| l == "@nessemble-clobbers "), "{ls:?}");
        assert!(
            ls.iter().any(|l| l == "@nessemble routine signature block"),
            "the scaffold is offered above a label: {ls:?}"
        );

        // Not above a label, the scaffold would bind to nothing.
        open(&mut server, "file:///c3.asm", "; @\n.db $01\n");
        let uri3 = Url::parse("file:///c3.asm").unwrap();
        let ls = labels(server.complete(&uri3, Position::new(0, 3)));
        assert!(
            !ls.iter().any(|l| l == "@nessemble routine signature block"),
            "{ls:?}"
        );
    }

    #[test]
    fn code_action_documents_an_undocumented_routine() {
        let mut server = Server::default();
        open(&mut server, "file:///r.asm", "widget:\n    RTS\n");
        let uri = Url::parse("file:///r.asm").unwrap();
        let actions =
            server.code_actions(&uri, Range::new(Position::new(0, 0), Position::new(0, 0)));
        let CodeActionOrCommand::CodeAction(action) = &actions[0] else {
            panic!("expected a code action");
        };
        assert_eq!(action.title, "Document routine `widget`");
        let edits = action.edit.as_ref().unwrap().changes.as_ref().unwrap();
        assert!(edits[&uri][0].new_text.contains("@nessemble-clobbers"));

        // An already-documented routine is not offered it again.
        open(
            &mut server,
            "file:///r2.asm",
            "; @nessemble-clobbers A\nwidget:\n",
        );
        let uri2 = Url::parse("file:///r2.asm").unwrap();
        let actions =
            server.code_actions(&uri2, Range::new(Position::new(1, 0), Position::new(1, 0)));
        assert!(actions.is_empty(), "{actions:?}");
    }

    #[test]
    fn quick_fix_adds_the_missing_register_to_the_clobber_list() {
        let mut server = Server::default();
        open(
            &mut server,
            "file:///f.asm",
            "; @nessemble-clobbers A ; only A, supposedly\ndraw:\n    LDA #$00\n    LDX #$10\n    RTS\n",
        );
        let uri = Url::parse("file:///f.asm").unwrap();
        // The diagnostic sits on the routine's label line.
        let actions =
            server.code_actions(&uri, Range::new(Position::new(1, 0), Position::new(1, 0)));
        let CodeActionOrCommand::CodeAction(action) = &actions[0] else {
            panic!("expected a code action");
        };
        assert_eq!(action.title, "Add `X` to `@nessemble-clobbers`");
        let edits = action.edit.as_ref().unwrap().changes.as_ref().unwrap();
        let edit = &edits[&uri][0];
        assert_eq!(edit.new_text, " A, X", "canonical order");
        // The replaced span is exactly the argument text — the trailing prose
        // and the space before it survive.
        assert_eq!(
            edit.range,
            Range::new(Position::new(0, 21), Position::new(0, 23))
        );
        let mut fixed = String::from("; @nessemble-clobbers A ; only A, supposedly");
        fixed.replace_range(21..23, &edit.new_text);
        assert_eq!(fixed, "; @nessemble-clobbers A, X ; only A, supposedly");
    }

    #[test]
    fn inlay_hints_show_a_callees_clobbers_on_jsr_lines() {
        let mut server = Server::default();
        open(&mut server, "file:///i.asm", SIGNED);
        let uri = Url::parse("file:///i.asm").unwrap();
        let all = Range::new(Position::new(0, 0), Position::new(20, 0));
        let hints = server.inlay_hints(&uri, all);
        assert_eq!(hints.len(), 1, "only the JSR line: {hints:?}");
        assert_eq!(hints[0].position.line, 9);
        let InlayHintLabel::String(label) = &hints[0].label else {
            panic!("expected a string label");
        };
        assert!(label.contains("A, X, [oam_cursor]"), "{label}");

        // A call to an undocumented routine gets no hint.
        open(
            &mut server,
            "file:///i2.asm",
            "main:\n    JSR helper\nhelper:\n    RTS\n",
        );
        let uri2 = Url::parse("file:///i2.asm").unwrap();
        assert!(server.inlay_hints(&uri2, all).is_empty());
    }

    #[test]
    fn signature_diagnostics_reach_the_editor() {
        let dir = workspace("sig-diag");
        let file = dir.join("a.asm");
        let uri = Url::from_file_path(&file).unwrap();
        let diags = lint_diagnostics(
            &uri,
            "; @nessemble-clobbers none\ndraw:\n    LDX #$00\n    RTS\n",
        );
        let messages: Vec<&str> = diags.iter().map(|d| d.message.as_str()).collect();
        assert!(
            messages.iter().any(|m| m.contains("writes X")),
            "{messages:?}"
        );
        assert!(diags
            .iter()
            .all(|d| d.source.as_deref() == Some("nessemble-lint")));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lint_error_rule_maps_to_information() {
        let dir = workspace("lint-err");
        write(
            &dir,
            ".nessemblerc",
            r#"{"lint":{"rules":{"require-block-comment":"error"}}}"#,
        );
        let file = dir.join("a.asm");
        let uri = Url::from_file_path(&file).unwrap();
        let diags = lint_diagnostics(&uri, "\nwidget:\n    rts\n");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Some(DiagnosticSeverity::INFORMATION));

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- filename arguments: links, hover, completion ----------------------

    /// A throwaway directory tree for the filesystem-touching surfaces, plus the
    /// `file:` URI of a document written into it.
    struct PathTree {
        root: PathBuf,
    }

    impl PathTree {
        fn new(tag: &str) -> PathTree {
            static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "nessemble-lsp-paths-{tag}-{}-{n}",
                std::process::id()
            ));
            std::fs::create_dir_all(&root).expect("create root");
            PathTree { root }
        }

        fn write(&self, rel: &str, bytes: &[u8]) {
            let path = self.root.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("create parent");
            }
            std::fs::write(path, bytes).expect("write file");
        }

        /// A 24-byte PNG header declaring `w`×`h`. Hover reads only the IHDR, so
        /// this is all the surface under test ever looks at.
        fn write_png_header(&self, rel: &str, w: u32, h: u32) {
            let mut bytes = Vec::from(*b"\x89PNG\r\n\x1a\n");
            bytes.extend_from_slice(&13u32.to_be_bytes());
            bytes.extend_from_slice(b"IHDR");
            bytes.extend_from_slice(&w.to_be_bytes());
            bytes.extend_from_slice(&h.to_be_bytes());
            self.write(rel, &bytes);
        }

        /// Open `text` as `main.asm` in this tree and return the server and URI.
        fn open_main(&self, text: &str) -> (Server, Url) {
            self.write("main.asm", text.as_bytes());
            let uri = Url::from_file_path(self.root.join("main.asm")).expect("file uri");
            let mut server = Server::default();
            open(&mut server, uri.as_str(), text);
            (server, uri)
        }

        /// Open `text` as `<rel_dir>/main.asm` in this tree, with `workspace_roots`
        /// set to this tree's root — the multi-root-workspace case Phase 5's `@/`
        /// support has to mirror.
        fn open_nested(&self, rel_dir: &str, text: &str) -> (Server, Url) {
            let rel = format!("{rel_dir}/main.asm");
            self.write(&rel, text.as_bytes());
            let uri = Url::from_file_path(self.root.join(&rel)).expect("file uri");
            let mut server = Server {
                workspace_roots: vec![self.root.clone()],
                ..Server::default()
            };
            open(&mut server, uri.as_str(), text);
            (server, uri)
        }
    }

    impl Drop for PathTree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// The source text a link's range covers, for asserting what got underlined.
    fn slice_range(text: &str, range: Range) -> String {
        let line = text.lines().nth(range.start.line as usize).unwrap_or("");
        line.chars()
            .skip(range.start.character as usize)
            .take((range.end.character - range.start.character) as usize)
            .collect()
    }

    #[test]
    fn document_links_cover_declared_and_importer_paths() {
        let tree = PathTree::new("links");
        tree.write("map.png", b"x");
        tree.write("logo.chr", b"x");
        tree.write("defs.asm", b"\n");
        let text = ".tilemap \"file://map.png\"\n.incbin \"logo.chr\"\n.include \"defs.asm\"\n";
        let (server, uri) = tree.open_main(text);

        let links = server.document_links(&uri).expect("known document");
        let covered: Vec<String> = links.iter().map(|l| slice_range(text, l.range)).collect();
        // The path only: not the quotes, and not the `file://` marker.
        assert_eq!(covered, ["map.png", "logo.chr", "defs.asm"]);
        assert!(links.iter().all(|l| l.target.is_some()));
        assert!(links[0]
            .target
            .as_ref()
            .unwrap()
            .as_str()
            .ends_with("map.png"));
    }

    #[test]
    fn a_missing_path_gets_no_link() {
        let tree = PathTree::new("missing");
        let (server, uri) = tree.open_main(".incbin \"gone.chr\"\n.tilemap \"file://gone.png\"\n");

        assert!(server.document_links(&uri).expect("known").is_empty());
    }

    #[test]
    fn an_undeclared_custom_argument_is_not_a_path() {
        // Without the declaration the assembler cannot know the string is a path,
        // and neither can the editor — even when a file of that name is right there.
        let tree = PathTree::new("undeclared");
        tree.write("map.png", b"x");
        let (server, uri) = tree.open_main(".tilemap \"map.png\"\n");

        assert!(server.document_links(&uri).expect("known").is_empty());
    }

    #[test]
    fn a_data_string_is_not_a_path() {
        let tree = PathTree::new("data");
        tree.write("hi", b"x");
        let (server, uri) = tree.open_main(".db \"hi\"\n.ascii \"hi\"\n");

        assert!(server.document_links(&uri).expect("known").is_empty());
    }

    #[test]
    fn only_the_first_string_of_an_importer_is_its_filename() {
        // `.incbin "logo.chr", 0, 16` takes numbers after the path; a second
        // string would be a syntax error, but must never be treated as a path.
        let tree = PathTree::new("firstonly");
        tree.write("logo.chr", b"x");
        tree.write("other.chr", b"x");
        let (server, uri) = tree.open_main(".incbin \"logo.chr\", \"other.chr\"\n");

        let links = server.document_links(&uri).expect("known");
        assert_eq!(links.len(), 1);
    }

    #[test]
    fn a_declared_argument_on_a_continuation_line_still_links() {
        let tree = PathTree::new("continued");
        tree.write("b.png", b"x");
        let text = ".tilemap 1,\n  \"file://b.png\"\n";
        let (server, uri) = tree.open_main(text);

        let links = server.document_links(&uri).expect("known");
        assert_eq!(links.len(), 1, "links: {links:?}");
        assert_eq!(slice_range(text, links[0].range), "b.png");
    }

    #[test]
    fn an_untitled_buffer_has_no_links() {
        let mut server = Server::default();
        open(&mut server, "untitled:Untitled-1", ".incbin \"logo.chr\"\n");
        let uri = Url::parse("untitled:Untitled-1").unwrap();

        // No directory to resolve against, so there is nothing to point at.
        assert!(server.document_links(&uri).is_none());
    }

    #[test]
    fn hover_on_a_declared_path_shows_where_it_resolved() {
        let tree = PathTree::new("hover");
        tree.write("map.bin", b"12345");
        let (server, uri) = tree.open_main(".tilemap \"file://map.bin\"\n");

        let hover = server.hover(&uri, Position::new(0, 20)).expect("hover");
        let HoverContents::Markup(md) = hover.contents else {
            panic!("expected markup");
        };
        assert!(md.value.contains("map.bin"), "hover: {}", md.value);
        assert!(md.value.contains("5 bytes"), "hover: {}", md.value);
    }

    #[test]
    fn hover_on_a_png_argument_shows_its_dimensions() {
        let tree = PathTree::new("png");
        tree.write_png_header("sprite.png", 128, 64);
        let (server, uri) = tree.open_main(".incpng \"sprite.png\"\n");

        let hover = server.hover(&uri, Position::new(0, 12)).expect("hover");
        let HoverContents::Markup(md) = hover.contents else {
            panic!("expected markup");
        };
        assert!(md.value.contains("128×64 PNG"), "hover: {}", md.value);
    }

    #[test]
    fn hover_on_a_missing_path_says_not_found() {
        let tree = PathTree::new("hovermissing");
        let (server, uri) = tree.open_main(".incbin \"gone.chr\"\n");

        let hover = server.hover(&uri, Position::new(0, 12)).expect("hover");
        let HoverContents::Markup(md) = hover.contents else {
            panic!("expected markup");
        };
        assert!(md.value.contains("not found"), "hover: {}", md.value);
    }

    #[test]
    fn completion_inside_a_png_argument_offers_only_pngs() {
        let tree = PathTree::new("completepng");
        tree.write("sprite.png", b"x");
        tree.write("tiles.PNG", b"x");
        tree.write("notes.txt", b"x");
        tree.write("assets/deep.png", b"x");
        let (server, uri) = tree.open_main(".incpng \"\"\n");

        let labels: Vec<String> = server
            .complete(&uri, Position::new(0, 9))
            .into_iter()
            .map(|i| i.label)
            .collect();
        assert!(labels.contains(&"sprite.png".to_string()), "{labels:?}");
        // Extension matching is case-insensitive, and directories are always
        // offered because they are on the way to a file.
        assert!(labels.contains(&"tiles.PNG".to_string()), "{labels:?}");
        assert!(labels.contains(&"assets/".to_string()), "{labels:?}");
        assert!(!labels.contains(&"notes.txt".to_string()), "{labels:?}");
        assert!(
            !labels.iter().any(|l| l == "lda"),
            "no code items: {labels:?}"
        );
    }

    #[test]
    fn completion_inside_a_declared_custom_argument_offers_every_file() {
        // A script may read any format, so nothing is filtered out there.
        let tree = PathTree::new("completeany");
        tree.write("curve.json", b"x");
        tree.write("sprite.png", b"x");
        let (server, uri) = tree.open_main(".ease \"file://\"\n");

        let labels: Vec<String> = server
            .complete(&uri, Position::new(0, 14))
            .into_iter()
            .map(|i| i.label)
            .collect();
        assert!(labels.contains(&"curve.json".to_string()), "{labels:?}");
        assert!(labels.contains(&"sprite.png".to_string()), "{labels:?}");
    }

    #[test]
    fn completion_walks_into_a_subdirectory_and_filters_by_prefix() {
        let tree = PathTree::new("completedeep");
        tree.write("assets/hero.png", b"x");
        tree.write("assets/house.png", b"x");
        tree.write("assets/villain.png", b"x");
        let (server, uri) = tree.open_main(".incpng \"assets/ho\"\n");

        let labels: Vec<String> = server
            .complete(&uri, Position::new(0, 19))
            .into_iter()
            .map(|i| i.label)
            .collect();
        assert_eq!(labels, ["house.png"], "{labels:?}");
    }

    #[test]
    fn completion_outside_a_filename_argument_is_unaffected() {
        let tree = PathTree::new("completecode");
        let (server, uri) = tree.open_main("  ld\n");

        let labels: Vec<String> = server
            .complete(&uri, Position::new(0, 4))
            .into_iter()
            .map(|i| i.label)
            .collect();
        assert!(labels.iter().any(|l| l == "lda"), "{labels:?}");
    }

    // ---- `@/` project-root paths (plan 012, Phase 5) ------------------------

    #[test]
    fn document_links_resolve_a_project_root_path_via_the_workspace_folder() {
        let tree = PathTree::new("rootlinks");
        tree.write("assets/logo.chr", b"x");
        let text = ".incbin \"@/assets/logo.chr\"\n";
        // The entry file sits under a nested subdirectory; only the workspace
        // folder (the tree's root) makes `@/` resolve to the sibling `assets/`.
        let (server, uri) = tree.open_nested("src", text);

        let links = server.document_links(&uri).expect("known document");
        assert_eq!(links.len(), 1, "links: {links:?}");
        assert!(
            links[0]
                .target
                .as_ref()
                .unwrap()
                .as_str()
                .ends_with("assets/logo.chr"),
            "{:?}",
            links[0].target
        );
    }

    #[test]
    fn hover_on_a_project_root_path_names_where_it_landed() {
        let tree = PathTree::new("roothover");
        tree.write("assets/logo.chr", b"12345");
        let text = ".incbin \"@/assets/logo.chr\"\n";
        let (server, uri) = tree.open_nested("src/deep", text);

        let hover = server.hover(&uri, Position::new(0, 15)).expect("hover");
        let HoverContents::Markup(md) = hover.contents else {
            panic!("expected markup");
        };
        // Landed at the workspace root's `assets/`, not `src/deep/assets/`.
        assert!(md.value.contains("assets/logo.chr"), "hover: {}", md.value);
        assert!(!md.value.contains("src/deep"), "hover: {}", md.value);
        assert!(md.value.contains("5 bytes"), "hover: {}", md.value);
    }

    #[test]
    fn hover_on_a_project_root_escape_names_the_problem() {
        let tree = PathTree::new("rootescape");
        let (server, uri) = tree.open_main(".incbin \"@/../outside.bin\"\n");

        let hover = server.hover(&uri, Position::new(0, 15)).expect("hover");
        let HoverContents::Markup(md) = hover.contents else {
            panic!("expected markup");
        };
        assert!(
            md.value.contains("resolves outside the project root"),
            "hover: {}",
            md.value
        );
    }

    #[test]
    fn completion_offers_at_slash_at_the_start_of_an_empty_argument() {
        let tree = PathTree::new("completeatslash");
        let (server, uri) = tree.open_main(".incbin \"\"\n");

        let labels: Vec<String> = server
            .complete(&uri, Position::new(0, 9))
            .into_iter()
            .map(|i| i.label)
            .collect();
        assert!(labels.contains(&"@/".to_string()), "{labels:?}");
    }

    #[test]
    fn completion_inside_a_project_root_path_completes_against_the_root() {
        let tree = PathTree::new("completerootpath");
        tree.write("assets/logo.chr", b"x");
        tree.write("assets/sprite.chr", b"x");
        // Completion is happening from `src/`, well away from `assets/` — only
        // the `@/` resolution makes the sibling directory visible here.
        let (server, uri) = tree.open_nested("src", ".incbin \"@/assets/lo\"\n");

        let labels: Vec<String> = server
            .complete(&uri, Position::new(0, 21))
            .into_iter()
            .map(|i| i.label)
            .collect();
        assert_eq!(labels, ["logo.chr"], "{labels:?}");
    }
}
