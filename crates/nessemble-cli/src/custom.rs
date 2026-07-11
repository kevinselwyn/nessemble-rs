//! Custom pseudo-op resolution: map a `.foo` directive to a script and run it.
//!
//! Directives resolve from the `-p`/`--pseudo` mapping file first (paths
//! relative to the source file's directory), then the installed
//! `~/.nessemble/scripts/scripts.txt` (paths relative to that scripts
//! directory) — matching the reference `pseudo_parse` precedence.

use std::collections::HashMap;
use std::path::Path;

use nessemble_core::CustomResolver;
use nessemble_i18n::t;

use crate::home;

/// Build a resolver from the optional `-p` mapping file. It also consults the
/// installed bundled scripts (`~/.nessemble/scripts`).
pub fn build_resolver(pseudo_file: Option<&str>) -> CustomResolver {
    let pseudo_map = pseudo_file.map(read_mapping).unwrap_or_default();
    let scripts_dir = home::config_dir().map(|d| d.join("scripts"));
    let scripts_map = scripts_dir
        .as_ref()
        .map(|d| read_mapping(d.join("scripts.txt")))
        .unwrap_or_default();

    Box::new(move |name, ints, texts, base_dir| {
        resolve(
            name,
            ints,
            texts,
            base_dir,
            &pseudo_map,
            &scripts_map,
            scripts_dir.as_deref(),
        )
    })
}

fn resolve(
    name: &str,
    ints: &[i64],
    texts: &[String],
    base_dir: &Path,
    pseudo_map: &HashMap<String, String>,
    scripts_map: &HashMap<String, String>,
    scripts_dir: Option<&Path>,
) -> Result<Vec<u8>, String> {
    let pseudo = format!(".{name}");
    let path = if let Some(rel) = pseudo_map.get(name) {
        base_dir.join(rel)
    } else if let (Some(file), Some(dir)) = (scripts_map.get(name), scripts_dir) {
        dir.join(file)
    } else {
        return Err(t!("unknown-custom", pseudo = pseudo));
    };

    let source =
        std::fs::read_to_string(&path).map_err(|_| t!("custom-not-exist", pseudo = pseudo))?;
    run_script(&source, ints, texts)
}

/// Read a `.name = path` mapping file into `name -> path` (name without dot).
fn read_mapping(path: impl AsRef<Path>) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Ok(text) = std::fs::read_to_string(path) else {
        return map;
    };
    for line in text.lines() {
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim().trim_start_matches('.');
            let value = value.trim();
            if !key.is_empty() && !value.is_empty() {
                map.insert(key.to_string(), value.to_string());
            }
        }
    }
    map
}

#[cfg(feature = "scripting")]
fn run_script(source: &str, ints: &[i64], texts: &[String]) -> Result<Vec<u8>, String> {
    nessemble_script::run(source, ints, texts)
}

#[cfg(not(feature = "scripting"))]
fn run_script(_source: &str, _ints: &[i64], _texts: &[String]) -> Result<Vec<u8>, String> {
    Err("scripting is disabled".to_string())
}
