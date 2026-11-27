//! The `nessemble lsp` subcommand: run the language server over stdio.
//!
//! Gated behind the `lsp` cargo feature (on by default). Without it, the
//! subcommand still exists but reports that the build lacks LSP support.

/// Run the language server, returning the process exit code.
#[cfg(feature = "lsp")]
pub fn run() -> u8 {
    // The editor reads custom pseudo-ops from the cache a build left, and
    // never runs a script (`plans/023-whole-program-clobber-analysis.md` §3.6).
    let hooks = nessemble_lsp::ServerHooks {
        custom_resolver: Box::new(|pseudo| {
            let pseudo = pseudo.map(|p| p.display().to_string());
            crate::custom::build_cache_reader(pseudo.as_deref())
        }),
    };
    match nessemble_lsp::run_with(hooks) {
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
