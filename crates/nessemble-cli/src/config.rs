//! `config` subcommand: a tab-separated key/value store at `~/.nessemble/config`
//! (mirroring the reference `config.c` file format), reduced to the in-scope
//! general key/value behavior — the reference's only built-in key, `registry`,
//! belongs to the out-of-scope package registry and is not seeded.

use std::io::Write;
use std::path::PathBuf;

use crate::home;

const CONFIG_FILENAME: &str = "config";

/// The `~/.nessemble/config` path, ensuring the directory exists.
fn config_path() -> std::io::Result<PathBuf> {
    Ok(home::ensure_config_dir()?.join(CONFIG_FILENAME))
}

/// Parse the config file into `(key, value)` pairs, preserving order.
fn read_pairs() -> std::io::Result<Vec<(String, String)>> {
    let path = config_path()?;
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e),
    };
    let mut pairs = Vec::new();
    for line in text.lines() {
        let mut parts = line.splitn(2, '\t');
        if let (Some(k), Some(v)) = (parts.next(), parts.next()) {
            if !k.is_empty() {
                pairs.push((k.to_string(), v.to_string()));
            }
        }
    }
    Ok(pairs)
}

fn write_pairs(pairs: &[(String, String)]) -> std::io::Result<()> {
    let path = config_path()?;
    let mut file = std::fs::File::create(&path)?;
    for (k, v) in pairs {
        writeln!(file, "{k}\t{v}")?;
    }
    Ok(())
}

/// `config` with no arguments: list every stored key/value, one per line.
pub fn list() -> Result<Option<String>, String> {
    let pairs = read_pairs().map_err(|e| e.to_string())?;
    if pairs.is_empty() {
        return Ok(None);
    }
    let max = pairs.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
    let mut out = String::new();
    for (i, (k, v)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        out.push_str(k);
        for _ in 0..(max - k.len() + 1) {
            out.push(' ');
        }
        out.push_str(v);
    }
    Ok(Some(out))
}

/// `config <key>`: print the stored value, or fail if unset.
pub fn get(key: &str) -> Result<Option<String>, String> {
    let pairs = read_pairs().map_err(|e| e.to_string())?;
    match pairs.iter().rev().find(|(k, _)| k == key) {
        Some((_, v)) => Ok(Some(v.clone())),
        None => Err(nessemble_i18n::t!("config-no-set", key = key)),
    }
}

/// `config <key> <val>`: set (or update) a key.
pub fn set(key: &str, value: &str) -> Result<(), String> {
    let mut pairs = read_pairs().map_err(|e| e.to_string())?;
    match pairs.iter_mut().find(|(k, _)| k == key) {
        Some(pair) => pair.1 = value.to_string(),
        None => pairs.push((key.to_string(), value.to_string())),
    }
    write_pairs(&pairs).map_err(|e| e.to_string())
}
