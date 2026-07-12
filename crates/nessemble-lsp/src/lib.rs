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
//!
//! Column-accurate ranges, formatting, and highlighting arrive in later phases.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use lsp_server::{Connection, ErrorCode, Message, Notification, Response};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Notification as _,
    PublishDiagnostics,
};
use lsp_types::request::{Completion, Request as _};
use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionOptions, CompletionParams, CompletionResponse,
    Diagnostic, DiagnosticSeverity, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, Position, PublishDiagnosticsParams, Range, ServerCapabilities,
    TextDocumentSyncCapability, TextDocumentSyncKind, Url,
};

use nessemble_core::{assemble_source_as, AssembleError, Diag, Options};
use nessemble_isa::{DIRECTIVES, OPCODES};

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
