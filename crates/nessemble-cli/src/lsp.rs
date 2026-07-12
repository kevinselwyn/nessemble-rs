//! The `nessemble lsp` subcommand: run the language server over stdio.
//!
//! Gated behind the `lsp` cargo feature (on by default). Without it, the
//! subcommand still exists but reports that the build lacks LSP support.

/// Run the language server, returning the process exit code.
#[cfg(feature = "lsp")]
pub fn run() -> u8 {
    match nessemble_lsp::run() {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("nessemble: language server error: {e}");
            1
        }
    }
}

#[cfg(not(feature = "lsp"))]
pub fn run() -> u8 {
    eprintln!("nessemble: this build was compiled without language-server support");
    1
}
