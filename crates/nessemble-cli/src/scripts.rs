//! `scripts` subcommand: install the bundled custom-pseudo-op scripts into
//! `~/.nessemble/scripts`.
//!
//! The scripting engine (Rhai) and the bundled `ease` script land in Phase 8;
//! for now this creates the scripts directory so the install location exists and
//! reports it, matching the reference's "Installed scripts to <path>" output.

use crate::home;

/// Run `scripts`, returning the process exit code.
pub fn run() -> u8 {
    let dir = match home::ensure_config_dir() {
        Ok(d) => d.join("scripts"),
        Err(e) => {
            eprintln!("nessemble: could not install scripts: {e}");
            return 1;
        }
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("nessemble: could not install scripts: {e}");
        return 1;
    }
    println!("Installed scripts to {}", dir.display());
    0
}
