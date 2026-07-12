//! Language Server Protocol implementation for nessemble's assembly flavor.
//!
//! Phase 0 (scaffold & transport): a synchronous [`lsp-server`](lsp_server)
//! stdio server that completes the LSP lifecycle (`initialize` → advertise
//! capabilities → `initialized` → `shutdown`/`exit`) and tracks the text of
//! open documents via `textDocument/didOpen|didChange|didClose`. It advertises
//! full-text document sync and performs **no analysis yet** — diagnostics,
//! completion, and formatting arrive in later phases.

use std::collections::HashMap;

use lsp_server::{Connection, ErrorCode, Message, Response};
use lsp_types::notification::{
    DidChangeTextDocument, DidCloseTextDocument, DidOpenTextDocument, Notification as _,
};
use lsp_types::{
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    ServerCapabilities, TextDocumentSyncCapability, TextDocumentSyncKind, Url,
};

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
    /// Apply a `textDocument/*` notification to the document store. Unknown
    /// notifications (and malformed params) are ignored, as LSP servers should.
    fn on_notification(&mut self, method: &str, params: serde_json::Value) {
        match method {
            DidOpenTextDocument::METHOD => {
                if let Ok(p) = serde_json::from_value::<DidOpenTextDocumentParams>(params) {
                    self.documents
                        .insert(p.text_document.uri, p.text_document.text);
                }
            }
            DidChangeTextDocument::METHOD => {
                if let Ok(p) = serde_json::from_value::<DidChangeTextDocumentParams>(params) {
                    // Full-sync: the final content change carries the whole
                    // document. (Incremental sync is not advertised.)
                    if let Some(change) = p.content_changes.into_iter().next_back() {
                        self.documents.insert(p.text_document.uri, change.text);
                    }
                }
            }
            DidCloseTextDocument::METHOD => {
                if let Ok(p) = serde_json::from_value::<DidCloseTextDocumentParams>(params) {
                    self.documents.remove(&p.text_document.uri);
                }
            }
            _ => {}
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

/// The capabilities advertised at `initialize`. Phase 0 advertises only
/// full-text document sync; feature capabilities are added as phases land.
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

/// The message loop: dispatch notifications to the document store and answer the
/// shutdown handshake. No request-based features are handled in Phase 0.
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
            Message::Notification(note) => server.on_notification(&note.method, note.params),
            Message::Response(_) => {}
        }
    }
    Ok(server)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_server::{Notification, Request, RequestId};

    /// Drive the server through a full lifecycle over an in-memory connection
    /// and confirm it tracks open/changed documents.
    #[test]
    fn tracks_documents_through_the_lifecycle() {
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
        assert!(matches!(
            client.receiver.recv().unwrap(),
            Message::Response(_)
        ));
        client
            .sender
            .send(Message::Notification(Notification {
                method: "initialized".into(),
                params: serde_json::json!({}),
            }))
            .unwrap();

        // didOpen
        client
            .sender
            .send(Message::Notification(Notification {
                method: "textDocument/didOpen".into(),
                params: serde_json::json!({
                    "textDocument": {
                        "uri": "file:///a.asm",
                        "languageId": "nessemble",
                        "version": 1,
                        "text": "  lda #$00\n"
                    }
                }),
            }))
            .unwrap();

        // didChange (full-sync: whole new text)
        client
            .sender
            .send(Message::Notification(Notification {
                method: "textDocument/didChange".into(),
                params: serde_json::json!({
                    "textDocument": { "uri": "file:///a.asm", "version": 2 },
                    "contentChanges": [ { "text": "  lda #$01\n" } ]
                }),
            }))
            .unwrap();

        // shutdown → response → exit
        client
            .sender
            .send(Message::Request(Request {
                id: RequestId::from(2),
                method: "shutdown".into(),
                params: serde_json::Value::Null,
            }))
            .unwrap();
        assert!(matches!(
            client.receiver.recv().unwrap(),
            Message::Response(_)
        ));
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
        assert_eq!(server.document_text(&uri), Some("  lda #$01\n"));
    }

    /// Closing a document removes it from the store.
    #[test]
    fn close_removes_the_document() {
        let mut server = Server::default();
        server.on_notification(
            DidOpenTextDocument::METHOD,
            serde_json::json!({
                "textDocument": {
                    "uri": "file:///b.asm", "languageId": "nessemble",
                    "version": 1, "text": "nop\n"
                }
            }),
        );
        assert_eq!(server.document_count(), 1);
        server.on_notification(
            DidCloseTextDocument::METHOD,
            serde_json::json!({ "textDocument": { "uri": "file:///b.asm" } }),
        );
        assert_eq!(server.document_count(), 0);
    }
}
