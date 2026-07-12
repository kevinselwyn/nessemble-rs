//! Language Server Protocol implementation for nessemble's assembly flavor.
//!
//! A synchronous [`lsp-server`](lsp_server) stdio server. It completes the LSP
//! lifecycle (`initialize` → advertise capabilities → `initialized` →
//! `shutdown`/`exit`), tracks open documents via
//! `textDocument/didOpen|didChange|didClose`, and — as of Phase 1 — assembles
//! each open buffer and publishes **diagnostics** (errors and warnings) via
//! `textDocument/publishDiagnostics`.
//!
//! Diagnostics are **line-level**: today's core reports a line but no column, so
//! each diagnostic covers its whole line. Column-accurate ranges, completion,
//! and formatting arrive in later phases.

use std::collections::HashMap;
use std::path::PathBuf;

use lsp_server::{Connection, ErrorCode, Message, Notification, Response};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Notification as _,
    PublishDiagnostics,
};
use lsp_types::{
    Diagnostic, DiagnosticSeverity, DidChangeTextDocumentParams, DidCloseTextDocumentParams,
    DidOpenTextDocumentParams, Position, PublishDiagnosticsParams, Range, ServerCapabilities,
    TextDocumentSyncCapability, TextDocumentSyncKind, Url,
};

use nessemble_core::{assemble_source_as, AssembleError, Diag, Options};

/// A boxed, thread-safe error, matching what the stdio transport surfaces.
type LspError = Box<dyn std::error::Error + Sync + Send>;
type LspResult<T> = Result<T, LspError>;

/// In-memory server state: the current text of every open document, keyed by
/// URI, kept in sync with the editor's buffers (not the on-disk copy).
#[derive(Default)]
pub struct Server {
    documents: HashMap<Url, String>,
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
                self.documents.insert(uri.clone(), p.text_document.text);
                Some(self.diagnostics_for(&uri))
            }
            DidChangeTextDocument::METHOD => {
                let p: DidChangeTextDocumentParams = serde_json::from_value(params).ok()?;
                // Full-sync: the final content change carries the whole document.
                let change = p.content_changes.into_iter().next_back()?;
                let uri = p.text_document.uri;
                self.documents.insert(uri.clone(), change.text);
                Some(self.diagnostics_for(&uri))
            }
            DidCloseTextDocument::METHOD => {
                let p: DidCloseTextDocumentParams = serde_json::from_value(params).ok()?;
                let uri = p.text_document.uri;
                self.documents.remove(&uri);
                // Publish an empty set to clear the editor's squiggles.
                Some(PublishDiagnosticsParams {
                    uri,
                    diagnostics: Vec::new(),
                    version: None,
                })
            }
            _ => None,
        }
    }

    /// Assemble the current text of `uri` and package its diagnostics.
    fn diagnostics_for(&self, uri: &Url) -> PublishDiagnosticsParams {
        let diagnostics = self
            .documents
            .get(uri)
            .map(|text| analyze(uri, text))
            .unwrap_or_default();
        PublishDiagnosticsParams {
            uri: uri.clone(),
            diagnostics,
            version: None,
        }
    }

    /// Number of documents currently open.
    #[must_use]
    pub fn document_count(&self) -> usize {
        self.documents.len()
    }

    /// The current text of an open document, if tracked.
    #[must_use]
    pub fn document_text(&self, uri: &Url) -> Option<&str> {
        self.documents.get(uri).map(String::as_str)
    }
}

/// Assemble `text` as the buffer at `uri` and translate the result into LSP
/// diagnostics. On success, any assembler warnings become `Warning`
/// diagnostics; on failure, the single error becomes an `Error` diagnostic.
/// (Collecting *all* errors at once is a later phase.)
fn analyze(uri: &Url, text: &str) -> Vec<Diagnostic> {
    let path = uri
        .to_file_path()
        .unwrap_or_else(|()| PathBuf::from(uri.path()));
    let top_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("stdin")
        .to_string();

    match assemble_source_as(&path, text, &Options::default()) {
        Ok(assembly) => assembly
            .warnings
            .iter()
            .map(|d| to_diagnostic(d, DiagnosticSeverity::WARNING, &top_name, text))
            .collect(),
        Err(AssembleError::Diagnostic(d)) => {
            vec![to_diagnostic(
                &d,
                DiagnosticSeverity::ERROR,
                &top_name,
                text,
            )]
        }
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

/// The capabilities advertised at `initialize`: full-text document sync.
/// Diagnostics are pushed (`publishDiagnostics`) and need no capability flag.
fn server_capabilities() -> ServerCapabilities {
    ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
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

/// The message loop: answer the shutdown handshake, and for each document
/// notification update the store and push refreshed diagnostics.
fn main_loop(connection: &Connection) -> LspResult<Server> {
    let mut server = Server::default();
    for msg in &connection.receiver {
        match msg {
            Message::Request(req) => {
                if connection.handle_shutdown(&req)? {
                    return Ok(server);
                }
                let resp = Response::new_err(
                    req.id,
                    ErrorCode::MethodNotFound as i32,
                    format!("unhandled request: {}", req.method),
                );
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

    #[test]
    fn analyze_flags_an_unknown_opcode() {
        let uri = Url::parse("file:///bad.asm").unwrap();
        let diags = analyze(&uri, "  notareal\n");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(diags[0].range.start.line, 0);
        assert_eq!(diags[0].source.as_deref(), Some("nessemble"));
    }

    #[test]
    fn analyze_reports_the_correct_line() {
        let uri = Url::parse("file:///multi.asm").unwrap();
        // Two valid lines, then an unknown opcode on line 3 (0-based line 2).
        let diags = analyze(&uri, "  lda #$00\n  nop\n  notareal\n");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].range.start.line, 2);
    }

    #[test]
    fn analyze_is_clean_for_valid_source() {
        let uri = Url::parse("file:///ok.asm").unwrap();
        assert!(analyze(&uri, "  lda #$00\n  nop\n").is_empty());
    }

    /// Drive the server through a full lifecycle over an in-memory connection,
    /// confirming it tracks documents and publishes diagnostics on open/change.
    #[test]
    fn publishes_diagnostics_over_the_lifecycle() {
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

        // didOpen an erroring buffer → expect a publishDiagnostics with 1 error.
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
        assert_eq!(published.uri.as_str(), "file:///a.asm");
        assert_eq!(published.diagnostics.len(), 1);
        assert_eq!(
            published.diagnostics[0].severity,
            Some(DiagnosticSeverity::ERROR)
        );

        // Fix it via didChange → diagnostics clear (empty set published).
        client
            .sender
            .send(Message::Notification(Notification {
                method: "textDocument/didChange".into(),
                params: serde_json::json!({
                    "textDocument": { "uri": "file:///a.asm", "version": 2 },
                    "contentChanges": [ { "text": "  nop\n" } ]
                }),
            }))
            .unwrap();
        let msg = client.receiver.recv().unwrap();
        let Message::Notification(note) = msg else {
            panic!("expected a publishDiagnostics notification, got {msg:?}");
        };
        let published: PublishDiagnosticsParams = serde_json::from_value(note.params).unwrap();
        assert!(published.diagnostics.is_empty());

        // shutdown → response → exit
        client
            .sender
            .send(Message::Request(Request {
                id: RequestId::from(2),
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
        let uri = Url::parse("file:///a.asm").unwrap();
        assert_eq!(server.document_text(&uri), Some("  nop\n"));
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
