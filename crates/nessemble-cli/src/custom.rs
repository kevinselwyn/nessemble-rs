//! Custom pseudo-op resolution: map a `.foo` directive to a script and run it.
//!
//! Directives resolve from the `-p`/`--pseudo` mapping file first (script paths
//! relative to the mapping file's own directory), then the installed
//! `~/.nessemble/scripts/scripts.txt` (paths relative to that scripts
//! directory) — matching the reference `pseudo_parse` precedence.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use nessemble_core::{parse_pseudo_mapping, CustomResolver};
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
    /// Locate the script a `.name` directive maps to, and whether it came from
    /// the `-p` project mapping (`true`) rather than the bundled scripts
    /// (`false`). Project scripts are the ones eligible for coverage.
    fn locate(&self, name: &str, base_dir: &Path) -> Result<(PathBuf, bool), String> {
        if let Some(rel) = self.pseudo_map.get(name) {
            // Relative to the mapping file's directory (falling back to the
            // source directory only if the mapping path had no parent, which
            // cannot happen once the mapping produced an entry).
            Ok((
                self.pseudo_dir.as_deref().unwrap_or(base_dir).join(rel),
                true,
            ))
        } else if let (Some(file), Some(dir)) =
            (self.scripts_map.get(name), self.scripts_dir.as_deref())
        {
            Ok((dir.join(file), false))
        } else {
            Err(t!("unknown-custom", pseudo = format!(".{name}")))
        }
    }

    fn resolve(
        &self,
        name: &str,
        ints: &[i64],
        texts: &[String],
        base_dir: &Path,
    ) -> Result<Vec<u8>, String> {
        let (path, _from_pseudo) = self.locate(name, base_dir)?;
        let source = std::fs::read_to_string(&path)
            .map_err(|_| t!("custom-not-exist", pseudo = format!(".{name}")))?;
        run_script(&source, ints, texts, base_dir)
    }
}

/// Construct the resolver state from the optional `-p` mapping file, also
/// consulting the installed bundled scripts (`~/.nessemble/scripts`).
fn make_resolver(pseudo_file: Option<&str>) -> Resolver {
    let scripts_dir = home::config_dir().map(|d| d.join("scripts"));
    Resolver {
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
    }
}

/// Build a resolver from the optional `-p` mapping file.
pub fn build_resolver(pseudo_file: Option<&str>) -> CustomResolver {
    let resolver = make_resolver(pseudo_file);
    Box::new(move |name, ints, texts, base_dir| resolver.resolve(name, ints, texts, base_dir))
}

/// Build a resolver that also records Rhai line coverage for **project** scripts
/// (those from the `-p` mapping; bundled scripts are excluded). Each `custom()`
/// invocation runs on an instrumented engine and accumulates into `coverage`.
#[cfg(feature = "coverage")]
pub fn build_resolver_with_coverage(
    pseudo_file: Option<&str>,
    coverage: nessemble_script::coverage::SharedCoverage,
) -> CustomResolver {
    let resolver = make_resolver(pseudo_file);
    Box::new(move |name, ints, texts, base_dir| {
        let (path, from_pseudo) = resolver.locate(name, base_dir)?;
        let source = std::fs::read_to_string(&path)
            .map_err(|_| t!("custom-not-exist", pseudo = format!(".{name}")))?;
        if from_pseudo {
            // Key by absolute path so the report is unambiguous across dirs.
            let key = path.canonicalize().unwrap_or(path);
            nessemble_script::coverage::run_with_coverage(
                &source, ints, texts, base_dir, &key, &coverage,
            )
        } else {
            run_script(&source, ints, texts, base_dir)
        }
    })
}

/// Read a `.name = path` mapping file into `name -> path` (name without dot),
/// via the shared [`parse_pseudo_mapping`] parser. A missing/unreadable file
/// yields an empty map.
fn read_mapping(path: impl AsRef<Path>) -> HashMap<String, String> {
    std::fs::read_to_string(path)
        .map(|text| parse_pseudo_mapping(&text).into_iter().collect())
        .unwrap_or_default()
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
