//! `~/.nessemble` path helpers, mirroring the reference `home.c` layout.

use std::path::PathBuf;

/// The user's home directory (`$HOME`, matching the reference's `getpwuid`
/// lookup for the common case).
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// The `~/.nessemble` configuration directory.
pub fn config_dir() -> Option<PathBuf> {
    home_dir().map(|h| h.join(".nessemble"))
}

/// A path under `~/.nessemble`, creating the directory if needed.
pub fn ensure_config_dir() -> std::io::Result<PathBuf> {
    let dir = config_dir().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, nessemble_i18n::t!("no-home"))
    })?;
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}
