//! `.nessemblerc` configuration for the `format` subcommand (Phase 3 of
//! `plans/005-formatter.md`).
//!
//! A prettier-style JSON config, discovered by walking up from the file (or
//! directory) being formatted. Keys are strict (`deny_unknown_fields`): an
//! unknown key is a hard error, not a silent no-op. The `serde`-derived schema
//! lives here in the CLI and maps onto `nessemble_core::tooling::FormatOptions`,
//! keeping `serde` out of the core crate.
//!
//! Also handles `.nessembleignore` (gitignore-style globs excluding paths from
//! directory walks) and prettier-style per-glob `overrides`. Glob support is a
//! focused subset — `*`, `**`, `?` — matched against paths relative to the
//! config file's directory; negation (`!`) is not supported.

use std::path::{Path, PathBuf};

use nessemble_core::tooling::{FormatOptions, IndentStyle};
use serde::Deserialize;

/// The formatting options settable in a `.nessemblerc` or an `overrides` entry.
/// Every field is optional; an absent field leaves the inherited value. Unknown
/// keys are rejected. (Case-normalization keys arrive with Phase 4.)
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RcOptions {
    indent_style: Option<String>,
    indent_width: Option<usize>,
    comma_spacing: Option<bool>,
    final_newline: Option<bool>,
    data_per_line: Option<usize>,
    respect_stride_hints: Option<bool>,
    blank_line_after_return: Option<bool>,
    max_consecutive_blank_lines: Option<usize>,
}

impl RcOptions {
    /// Overlay the set options onto `base`, validating enumerated values.
    fn apply(&self, base: &mut FormatOptions) -> Result<(), String> {
        if let Some(s) = &self.indent_style {
            base.indent_style = match s.as_str() {
                "space" => IndentStyle::Space,
                "tab" => IndentStyle::Tab,
                other => {
                    return Err(format!(
                        "invalid indentStyle `{other}` (expected \"space\" or \"tab\")"
                    ))
                }
            };
        }
        if let Some(w) = self.indent_width {
            base.indent_width = w;
        }
        if let Some(b) = self.comma_spacing {
            base.comma_spacing = b;
        }
        if let Some(b) = self.final_newline {
            base.final_newline = b;
        }
        if let Some(n) = self.data_per_line {
            base.data_per_line = n;
        }
        if let Some(b) = self.respect_stride_hints {
            base.respect_stride_hints = b;
        }
        if let Some(b) = self.blank_line_after_return {
            base.blank_line_after_return = b;
        }
        if let Some(n) = self.max_consecutive_blank_lines {
            base.max_consecutive_blank_lines = n;
        }
        Ok(())
    }
}

/// A prettier-style per-glob override: `{ "files": <glob>, "options": { … } }`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RcOverride {
    files: String,
    #[serde(default)]
    options: RcOptions,
}

/// The full `.nessemblerc` document.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RcConfig {
    #[serde(default)]
    extensions: Option<Vec<String>>,
    #[serde(default)]
    overrides: Vec<RcOverride>,
    #[serde(flatten)]
    options: RcOptions,
}

/// A resolved configuration for a set of inputs: base options, the directory
/// extension filter, ignore globs, and per-glob overrides — all anchored at a
/// single `root` directory for relative glob matching.
pub struct Config {
    base: FormatOptions,
    extensions: Vec<String>,
    ignore: Vec<String>,
    overrides: Vec<(String, RcOptions)>,
    root: PathBuf,
}

/// How the caller wants configuration resolved.
pub enum Choice {
    /// Walk up from the input to find `.nessemblerc` (the default).
    Discover,
    /// Ignore any `.nessemblerc`; use built-in defaults.
    NoConfig,
    /// Use this file as the `.nessemblerc`.
    Explicit(PathBuf),
}

impl Config {
    /// Built-in defaults (no `.nessemblerc`): default options and `.asm`.
    fn defaults(root: PathBuf) -> Config {
        Config {
            base: FormatOptions::default(),
            extensions: vec!["asm".to_string()],
            ignore: Vec::new(),
            overrides: Vec::new(),
            root,
        }
    }

    /// Parse a `.nessemblerc` file into a [`Config`] rooted at its directory.
    fn from_file(path: &Path) -> Result<Config, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("could not read config `{}`: {e}", path.display()))?;
        let rc: RcConfig = serde_json::from_str(&text)
            .map_err(|e| format!("invalid config `{}`: {e}", path.display()))?;

        let mut base = FormatOptions::default();
        rc.options
            .apply(&mut base)
            .map_err(|e| format!("in `{}`: {e}", path.display()))?;

        // Validate every override's options eagerly so per-file resolution is
        // infallible.
        let mut overrides = Vec::new();
        for ov in rc.overrides {
            let mut probe = base.clone();
            ov.options.apply(&mut probe).map_err(|e| {
                format!("in `{}` overrides for `{}`: {e}", path.display(), ov.files)
            })?;
            overrides.push((ov.files, ov.options));
        }

        let extensions = rc.extensions.map_or_else(
            || vec!["asm".to_string()],
            |list| list.iter().map(|e| normalize_ext(e)).collect(),
        );

        Ok(Config {
            base,
            extensions,
            ignore: Vec::new(),
            overrides,
            root: dir_of(path),
        })
    }

    /// Resolve configuration for a top-level input path under `choice`.
    pub fn resolve(input: &Path, choice: &Choice) -> Result<Config, String> {
        let start_dir = if input.is_dir() {
            input.to_path_buf()
        } else {
            dir_of(input)
        };
        // Canonicalize so the upward walk reaches the filesystem root even for
        // relative or deep inputs (and so relative-path matching is consistent).
        let start_dir = start_dir.canonicalize().unwrap_or(start_dir);

        let mut config = match choice {
            Choice::NoConfig => Config::defaults(start_dir.clone()),
            Choice::Explicit(path) => Config::from_file(path)?,
            Choice::Discover => {
                match find_upwards(&start_dir, &[".nessemblerc", ".nessemblerc.json"]) {
                    Some(path) => Config::from_file(&path)?,
                    None => Config::defaults(start_dir.clone()),
                }
            }
        };

        // `.nessembleignore` is discovered independently of `.nessemblerc`,
        // except under `--no-config` (which means "no project config at all").
        if !matches!(choice, Choice::NoConfig) {
            if let Some(path) = find_upwards(&start_dir, &[".nessembleignore"]) {
                config.ignore = read_ignore(&path);
                config.root = dir_of(&path);
            }
        }

        Ok(config)
    }

    /// Whether `file`'s extension is in the configured set (directory walks).
    pub fn has_formatted_ext(&self, file: &Path) -> bool {
        file.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| self.extensions.contains(&ext.to_ascii_lowercase()))
    }

    /// Whether `file` is excluded by a `.nessembleignore` glob.
    pub fn is_ignored(&self, file: &Path) -> bool {
        if self.ignore.is_empty() {
            return false;
        }
        let rel = rel_to(&self.root, file);
        self.ignore.iter().any(|g| matches_path_glob(g, &rel))
    }

    /// The effective options for `file`: base options plus every matching
    /// override, applied in order. Overrides were validated at load, so this
    /// does not fail.
    pub fn options_for(&self, file: &Path) -> FormatOptions {
        let mut opts = self.base.clone();
        if !self.overrides.is_empty() {
            let rel = rel_to(&self.root, file);
            for (glob, ov) in &self.overrides {
                if matches_path_glob(glob, &rel) {
                    let _ = ov.apply(&mut opts);
                }
            }
        }
        opts
    }
}

/// The directory containing `path`, normalizing an empty parent (a bare
/// filename) to `.` so it is a usable directory.
fn dir_of(path: &Path) -> PathBuf {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// Normalize a configured extension (`".asm"` or `"asm"`) to a bare lowercase
/// extension (`"asm"`) for comparison against `Path::extension`.
fn normalize_ext(ext: &str) -> String {
    ext.trim_start_matches('.').to_ascii_lowercase()
}

/// Parse a `.nessembleignore` file into a list of non-empty, non-comment globs.
fn read_ignore(path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_string)
        .collect()
}

/// Walk up from `start` (inclusive) looking for any of `names`, returning the
/// first match.
fn find_upwards(start: &Path, names: &[&str]) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        for name in names {
            let candidate = d.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        dir = d.parent();
    }
    None
}

/// The path of `file` relative to `root`, using `/` separators. Best-effort:
/// canonicalizes both when possible, falling back to lexical paths.
fn rel_to(root: &Path, file: &Path) -> String {
    let root_c = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let file_c = file.canonicalize().unwrap_or_else(|_| file.to_path_buf());
    let rel = file_c.strip_prefix(&root_c).unwrap_or(&file_c);
    rel.to_string_lossy().replace('\\', "/")
}

/// Match a gitignore/prettier-style glob against a `/`-separated relative path.
/// A pattern containing `/` (or anchored with a leading `/`) matches the whole
/// path; a slash-free pattern also matches any single path segment, so
/// `*.asm` excludes at any depth.
fn matches_path_glob(pattern: &str, path: &str) -> bool {
    let anchored = pattern.starts_with('/');
    let pat = pattern.trim_start_matches('/').trim_end_matches('/');
    if anchored || pat.contains('/') {
        glob_match(pat, path)
    } else {
        glob_match(pat, path) || path.split('/').any(|seg| glob_match(pat, seg))
    }
}

/// Glob match supporting `?` (one non-`/` char), `*` (any run within a path
/// segment), and `**` (any run, crossing `/`; `**/` also matches zero
/// directories).
fn glob_match(pattern: &str, s: &str) -> bool {
    glob_rec(pattern.as_bytes(), s.as_bytes())
}

fn glob_rec(p: &[u8], s: &[u8]) -> bool {
    let mut pi = 0;
    let mut si = 0;
    while pi < p.len() {
        match p[pi] {
            b'*' => {
                let double = pi + 1 < p.len() && p[pi + 1] == b'*';
                if double {
                    let rest = &p[pi + 2..];
                    // `**/` matches zero or more directories: also try skipping
                    // the following `/`.
                    if rest.first() == Some(&b'/') && glob_rec(&rest[1..], &s[si..]) {
                        return true;
                    }
                    let mut k = si;
                    loop {
                        if glob_rec(rest, &s[k..]) {
                            return true;
                        }
                        if k >= s.len() {
                            return false;
                        }
                        k += 1;
                    }
                } else {
                    let rest = &p[pi + 1..];
                    let mut k = si;
                    loop {
                        if glob_rec(rest, &s[k..]) {
                            return true;
                        }
                        if k >= s.len() || s[k] == b'/' {
                            return false;
                        }
                        k += 1;
                    }
                }
            }
            b'?' => {
                if si >= s.len() || s[si] == b'/' {
                    return false;
                }
                pi += 1;
                si += 1;
            }
            c => {
                if si >= s.len() || s[si] != c {
                    return false;
                }
                pi += 1;
                si += 1;
            }
        }
    }
    si == s.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_single_star_stays_within_segment() {
        assert!(glob_match("*.asm", "a.asm"));
        assert!(!glob_match("*.asm", "sub/a.asm"));
        assert!(glob_match("src/*.asm", "src/a.asm"));
        assert!(!glob_match("src/*.asm", "src/sub/a.asm"));
    }

    #[test]
    fn glob_double_star_crosses_segments_and_matches_zero_dirs() {
        assert!(glob_match("src/**/*.asm", "src/a.asm"));
        assert!(glob_match("src/**/*.asm", "src/data/a.asm"));
        assert!(glob_match("src/**/*.asm", "src/data/deep/a.asm"));
        assert!(!glob_match("src/**/*.asm", "other/a.asm"));
    }

    #[test]
    fn slash_free_pattern_matches_any_segment() {
        assert!(matches_path_glob("*.asm", "src/data/a.asm"));
        assert!(matches_path_glob("build", "x/build/y.asm"));
        assert!(!matches_path_glob("build", "x/builder/y.asm"));
    }

    #[test]
    fn question_mark_matches_one_non_separator() {
        assert!(glob_match("a?.asm", "a1.asm"));
        assert!(!glob_match("a?.asm", "a12.asm"));
    }

    #[test]
    fn rc_options_reject_unknown_keys() {
        let err = serde_json::from_str::<RcConfig>(r#"{"dataPerline": 4}"#);
        assert!(err.is_err());
    }

    #[test]
    fn rc_options_map_onto_format_options() {
        let rc: RcConfig = serde_json::from_str(
            r#"{"indentStyle":"tab","dataPerLine":4,"blankLineAfterReturn":false}"#,
        )
        .unwrap();
        let mut base = FormatOptions::default();
        rc.options.apply(&mut base).unwrap();
        assert_eq!(base.indent_style, IndentStyle::Tab);
        assert_eq!(base.data_per_line, 4);
        assert!(!base.blank_line_after_return);
    }

    #[test]
    fn invalid_indent_style_is_an_error() {
        let rc: RcConfig = serde_json::from_str(r#"{"indentStyle":"wide"}"#).unwrap();
        let mut base = FormatOptions::default();
        assert!(rc.options.apply(&mut base).is_err());
    }
}
