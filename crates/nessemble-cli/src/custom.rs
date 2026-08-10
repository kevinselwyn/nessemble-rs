//! Custom pseudo-op resolution: map a `.foo` directive to a script and run it.
//!
//! Directives resolve from the `-p`/`--pseudo` mapping file first (script paths
//! relative to the mapping file's own directory), then the installed
//! `~/.nessemble/scripts/scripts.txt` (paths relative to that scripts
//! directory) — matching the reference `pseudo_parse` precedence.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use nessemble_core::{parse_pseudo_mapping, CustomOutput, CustomResolver};
use nessemble_i18n::t;
use rayon::prelude::*;

use crate::{cache, home};

/// Where a `.foo` directive's script is looked up: the `--pseudo` mapping (with
/// paths relative to the mapping file's directory) and the installed bundled
/// scripts (relative to `~/.nessemble/scripts`).
struct Resolver {
    /// Directory of the `--pseudo` mapping file; `None` when no mapping was given.
    pseudo_dir: Option<PathBuf>,
    pseudo_map: HashMap<String, String>,
    scripts_dir: Option<PathBuf>,
    scripts_map: HashMap<String, String>,
    /// The persistent cache of emitted bytes, or `None` when it is switched off
    /// (`--no-cache`), unavailable (no home directory), or bypassed because the
    /// caller needs every script to actually execute (coverage).
    cache: Option<cache::Cache>,
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
        root: Option<&Path>,
    ) -> Result<CustomOutput, String> {
        let (path, _from_pseudo) = self.locate(name, base_dir)?;

        // A cache hit answers without reading, compiling, or running the script.
        // Only ever a `CustomOutput::Bytes` entry lands here — `emit_source`
        // output is never written to the cache in the first place (below).
        let key = self
            .cache
            .as_ref()
            .and_then(|_| cache::Key::new(name, ints, texts, base_dir, root, &path));
        if let (Some(cache), Some(key)) = (self.cache.as_ref(), key.as_ref()) {
            if let Some(bytes) = cache.get(key) {
                return Ok(CustomOutput::Bytes(bytes));
            }
        }

        let source = std::fs::read_to_string(&path)
            .map_err(|_| t!("custom-not-exist", pseudo = format!(".{name}")))?;

        // Without a cache to fill there is nothing to gain from recording the
        // script's inputs, so take the plain path.
        let (Some(cache), Some(key)) = (self.cache.as_ref(), key) else {
            return run_script(&source, ints, texts, base_dir, root);
        };
        let outcome = run_with_inputs(&source, ints, texts, base_dir, root)?;
        if let (true, CustomOutput::Bytes(bytes)) = (outcome.cacheable, &outcome.output) {
            cache.put(&key, &outcome.inputs, bytes);
        }
        Ok(outcome.output)
    }

    /// Prewarm the on-disk cache for `candidates`, running the ones that
    /// aren't already cached **concurrently**, ahead of the sequential
    /// emission passes that will call [`Self::resolve`] for the same
    /// invocations (`plans/013-structured-data-parsing.md` §7). A script is
    /// the expensive part of a directive on a script-heavy build (decoding a
    /// PNG, parsing XML, …); every invocation is independent by construction,
    /// so nothing about running several of them at once changes what any one
    /// of them computes.
    ///
    /// A no-op when caching is off (`self.cache` is `None`): with nowhere to
    /// stash a concurrently-computed result for the sequential pass to pick
    /// up, running ahead of time would only waste the work, never save it.
    /// Two call sites can share identical arguments, so candidates are
    /// deduplicated first — otherwise two threads could race to compute and
    /// write the very same cache entry.
    fn prewarm(&self, candidates: &[nessemble_core::PrewarmCandidate]) {
        if self.cache.is_none() {
            return;
        }
        let mut seen = std::collections::HashSet::new();
        let unique: Vec<&nessemble_core::PrewarmCandidate> =
            candidates.iter().filter(|c| seen.insert(*c)).collect();
        unique.par_iter().for_each(|c| self.prewarm_one(c));
    }

    /// Prewarm a single candidate, unless it is already cached or its script
    /// is not safe to run an extra, uncounted time.
    ///
    /// Prewarming happens entirely ahead of — and independently of —
    /// `custom_memo`, the assembler's own once-per-call-site memoization
    /// (`Assembler::exec_custom`): running a script here does not stop the
    /// real, sequential pass from also running it once, as it always has.
    /// That composes cleanly for a *cacheable* result (the sequential pass's
    /// own call just hits the now-warm cache instead), but not for one that
    /// isn't: a script that writes a file, or draws randomness, must run
    /// exactly as many times as the sequential passes call it — running it
    /// here too would be a real extra side effect, not merely wasted work.
    /// [`nessemble_script::is_pure`] answers that *without* running the
    /// script, so the check costs a compile, not an execution.
    fn prewarm_one(&self, c: &nessemble_core::PrewarmCandidate) {
        let Some(cache) = self.cache.as_ref() else {
            return;
        };
        let Ok((path, _)) = self.locate(&c.name, &c.base_dir) else {
            return;
        };
        let Some(key) = cache::Key::new(
            &c.name,
            &c.ints,
            &c.texts,
            &c.base_dir,
            c.root.as_deref(),
            &path,
        ) else {
            return;
        };
        if cache.get(&key).is_some() {
            return; // Already warm — nothing to do.
        }
        let Ok(source) = std::fs::read_to_string(&path) else {
            return;
        };
        if !is_pure(&source) {
            return;
        }
        let _ = self.resolve(&c.name, &c.ints, &c.texts, &c.base_dir, c.root.as_deref());
    }
}

/// Construct the resolver state from the optional `-p` mapping file, also
/// consulting the installed bundled scripts (`~/.nessemble/scripts`).
fn make_resolver(pseudo_file: Option<&str>, caching: bool) -> Resolver {
    let scripts_dir = home::config_dir().map(|d| d.join("scripts"));
    Resolver {
        cache: caching.then(cache::Cache::open).flatten(),
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
///
/// `caching` enables the persistent cache of emitted bytes
/// ([`cache`]); pass `false` for `--no-cache`, or when every script must really
/// run (see [`build_resolver_with_coverage`]).
pub fn build_resolver(pseudo_file: Option<&str>, caching: bool) -> CustomResolver {
    let resolver = make_resolver(pseudo_file, caching);
    Box::new(move |name, ints, texts, base_dir, root| {
        resolver.resolve(name, ints, texts, base_dir, root)
    })
}

/// Like [`build_resolver`], but first prewarming the cache concurrently for
/// every candidate a scan of the program already found safe to resolve ahead
/// of time (`nessemble_core::prewarm_candidates_file`/`prewarm_candidates`,
/// `plans/013-structured-data-parsing.md` §7). Identical to `build_resolver`
/// otherwise — same lookup precedence, same caching behavior, same resolver
/// shape — except the sequential emission passes that follow are likely to
/// find several scripts already warm.
pub fn build_resolver_prewarmed(
    pseudo_file: Option<&str>,
    caching: bool,
    candidates: &[nessemble_core::PrewarmCandidate],
) -> CustomResolver {
    let resolver = make_resolver(pseudo_file, caching);
    resolver.prewarm(candidates);
    Box::new(move |name, ints, texts, base_dir, root| {
        resolver.resolve(name, ints, texts, base_dir, root)
    })
}

/// Build a resolver that also records Rhai line coverage for **project** scripts
/// (those from the `-p` mapping; bundled scripts are excluded). Each `custom()`
/// invocation runs on an instrumented engine and accumulates into `coverage`.
#[cfg(feature = "coverage")]
pub fn build_resolver_with_coverage(
    pseudo_file: Option<&str>,
    coverage: nessemble_script::coverage::SharedCoverage,
) -> CustomResolver {
    // Never cached: a hit runs nothing, so it would record no lines and report a
    // covered script as uncovered.
    let resolver = make_resolver(pseudo_file, false);
    Box::new(move |name, ints, texts, base_dir, root| {
        let (path, from_pseudo) = resolver.locate(name, base_dir)?;
        let source = std::fs::read_to_string(&path)
            .map_err(|_| t!("custom-not-exist", pseudo = format!(".{name}")))?;
        if from_pseudo {
            // Key by absolute path so the report is unambiguous across dirs.
            let key = path.canonicalize().unwrap_or(path);
            nessemble_script::coverage::run_with_coverage(
                &source, ints, texts, base_dir, root, &key, &coverage,
            )
        } else {
            run_script(&source, ints, texts, base_dir, root)
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
    root: Option<&Path>,
) -> Result<CustomOutput, String> {
    nessemble_script::run_with_root(source, ints, texts, base_dir, root)
}

/// Run a script, reporting the files it read and whether its bytes may be cached
/// (see [`nessemble_script::run_with_inputs`]).
#[cfg(feature = "scripting")]
fn run_with_inputs(
    source: &str,
    ints: &[i64],
    texts: &[String],
    base_dir: &Path,
    root: Option<&Path>,
) -> Result<nessemble_script::RunOutcome, String> {
    nessemble_script::run_with_inputs_and_root(source, ints, texts, base_dir, root)
}

/// Without the scripting feature there is nothing to run, and so nothing to
/// record or cache.
#[cfg(not(feature = "scripting"))]
fn run_with_inputs(
    _source: &str,
    _ints: &[i64],
    _texts: &[String],
    _base_dir: &Path,
    _root: Option<&Path>,
) -> Result<Outcome, String> {
    Err("scripting is disabled".to_string())
}

/// Stand-in for [`nessemble_script::RunOutcome`] when scripting is compiled out.
#[cfg(not(feature = "scripting"))]
pub struct Outcome {
    pub output: CustomOutput,
    pub inputs: Vec<PathBuf>,
    pub cacheable: bool,
}

#[cfg(not(feature = "scripting"))]
fn run_script(
    _source: &str,
    _ints: &[i64],
    _texts: &[String],
    _base_dir: &Path,
    _root: Option<&Path>,
) -> Result<CustomOutput, String> {
    Err("scripting is disabled".to_string())
}

#[cfg(feature = "scripting")]
fn is_pure(source: &str) -> bool {
    nessemble_script::is_pure(source)
}

/// Without the scripting feature nothing can run at all, so there is nothing
/// safe to prewarm.
#[cfg(not(feature = "scripting"))]
fn is_pure(_source: &str) -> bool {
    false
}
