//! `scripts` subcommand: install the bundled custom-pseudo-op scripts into
//! `~/.nessemble/scripts`.
//!
//! The bundled scripts (currently the `ease` easing-curve script and its
//! `scripts.txt` mapping) are embedded at build time and written out so that
//! `.ease` resolves at assemble time without a `-p` file.

use crate::home;

/// The bundled scripts installed by `scripts` (relative name, contents).
const BUNDLED: &[(&str, &str)] = &[
    ("scripts.txt", include_str!("data/scripts/scripts.txt")),
    ("ease.rhai", include_str!("data/scripts/ease.rhai")),
];

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
    for (name, contents) in BUNDLED {
        if let Err(e) = std::fs::write(dir.join(name), contents) {
            eprintln!("nessemble: could not install scripts: {e}");
            return 1;
        }
    }
    println!(
        "{}",
        nessemble_i18n::t!("scripts-installed", path = dir.display())
    );
    0
}
