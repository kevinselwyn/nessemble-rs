//! Custom pseudo-op resolution: map a `.foo` directive to a script and run it.
//!
//! Directives resolve from the `-p`/`--pseudo` mapping file first (script paths
//! relative to the mapping file's own directory), then the installed
//! `~/.nessemble/scripts/scripts.txt` (paths relative to that scripts
//! directory) — matching the reference `pseudo_parse` precedence.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use nessemble_core::CustomResolver;
use nessemble_i18n::t;

use crate::home;

/// Where a `.foo` directive's script is looked up: the `--pseudo` mapping (with
/// paths relative to the mapping file's directory) and the installed bundled
/// scripts (relative to `~/.nessemble/scripts`).
struct Resolver {
    /// Directory of the `--pseudo` mapping file; `None` when no mapping was given.
    pseudo_dir: Option<PathBuf>,
    pseudo_map: HashMap<String, String>,
    scripts_dir: Option<PathBuf>,
    scripts_map: HashMap<String, String>,
}

impl Resolver {
    fn resolve(
        &self,
        name: &str,
        ints: &[i64],
        texts: &[String],
        base_dir: &Path,
    ) -> Result<Vec<u8>, String> {
        let pseudo = format!(".{name}");
        let path = if let Some(rel) = self.pseudo_map.get(name) {
            // Relative to the mapping file's directory (falling back to the
            // source directory only if the mapping path had no parent, which
            // cannot happen once the mapping produced an entry).
            self.pseudo_dir.as_deref().unwrap_or(base_dir).join(rel)
        } else if let (Some(file), Some(dir)) =
            (self.scripts_map.get(name), self.scripts_dir.as_deref())
        {
            dir.join(file)
        } else {
            return Err(t!("unknown-custom", pseudo = pseudo));
        };

        let source =
            std::fs::read_to_string(&path).map_err(|_| t!("custom-not-exist", pseudo = pseudo))?;
        run_script(&source, ints, texts, base_dir)
    }
}

/// Build a resolver from the optional `-p` mapping file. It also consults the
/// installed bundled scripts (`~/.nessemble/scripts`).
pub fn build_resolver(pseudo_file: Option<&str>) -> CustomResolver {
    let scripts_dir = home::config_dir().map(|d| d.join("scripts"));
    let resolver = Resolver {
        pseudo_map: pseudo_file.map(read_mapping).unwrap_or_default(),
        // Script paths in the mapping resolve relative to the mapping file's own
        // directory, so a `pseudo.txt` and its scripts travel together
        // regardless of where the assembled source lives.
        pseudo_dir: pseudo_file.map(|f| {
            Path::new(f)
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
        }),
        scripts_map: scripts_dir
            .as_ref()
            .map(|d| read_mapping(d.join("scripts.txt")))
            .unwrap_or_default(),
        scripts_dir,
    };

    Box::new(move |name, ints, texts, base_dir| resolver.resolve(name, ints, texts, base_dir))
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
fn run_script(
    source: &str,
    ints: &[i64],
    texts: &[String],
    base_dir: &Path,
) -> Result<Vec<u8>, String> {
    nessemble_script::run(source, ints, texts, base_dir)
}

#[cfg(not(feature = "scripting"))]
fn run_script(
    _source: &str,
    _ints: &[i64],
    _texts: &[String],
    _base_dir: &Path,
) -> Result<Vec<u8>, String> {
    Err("scripting is disabled".to_string())
}
