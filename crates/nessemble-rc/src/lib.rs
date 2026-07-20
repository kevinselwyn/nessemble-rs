//! `.nessemblerc` configuration for the `format` and `lint` subcommands.
//!
//! A prettier-style JSON config, discovered by walking up from the file (or
//! directory) being processed. Keys are strict (`deny_unknown_fields`): an
//! unknown key is a hard error, not a silent no-op. The `serde`-derived schema
//! lives here (a small crate shared by the CLI and the language server) and maps
//! onto `nessemble_core::tooling::{FormatOptions, LintOptions}`, keeping `serde`
//! and `regex` out of the core crate.
//!
//! Also handles `.nessembleignore` (gitignore-style globs excluding paths from
//! directory walks) and prettier-style per-glob `overrides`. Glob support is a
//! focused subset — `*`, `**`, `?` — matched against paths relative to the
//! config file's directory; negation (`!`) is not supported.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use nessemble_core::tooling::{
    Case, FormatOptions, IndentStyle, RuleId, RuleSeverity, SeverityMap,
};
use regex_lite::Regex;
use serde::Deserialize;
use serde_json::Value;

/// The default `require-block-comment` search window when unset.
const DEFAULT_LINT_WINDOW: usize = 3;

/// The formatting options settable in a `.nessemblerc` or an `overrides` entry.
/// Every field is optional; an absent field leaves the inherited value. Unknown
/// keys are rejected.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RcOptions {
    indent_style: Option<String>,
    indent_width: Option<usize>,
    comma_spacing: Option<bool>,
    final_newline: Option<bool>,
    indent_directives: Option<bool>,
    align_continuations: Option<bool>,
    data_per_line: Option<usize>,
    respect_stride_hints: Option<bool>,
    blank_line_after_return: Option<bool>,
    max_consecutive_blank_lines: Option<usize>,
    mnemonic_case: Option<String>,
    hex_digit_case: Option<String>,
    /// Linter configuration (severities, window, ignore regexes). Present both at
    /// the config root and inside an `overrides` block.
    lint: Option<RcLint>,
}

/// Parse a case keyword (`"preserve"`/`"lower"`/`"upper"`) for a named key.
fn parse_case(key: &str, value: &str) -> Result<Case, String> {
    match value {
        "preserve" => Ok(Case::Preserve),
        "lower" => Ok(Case::Lower),
        "upper" => Ok(Case::Upper),
        other => Err(format!(
            "invalid {key} `{other}` (expected \"preserve\", \"lower\", or \"upper\")"
        )),
    }
}

/// Overlay each set (`Some`) scalar field of `$self` onto the matching field of
/// `$base`, leaving unset (`None`) fields untouched. Only for `Copy` fields that
/// map through verbatim; the enum-validated fields are handled explicitly.
macro_rules! overlay {
    ($base:expr, $self:expr, $($field:ident),+ $(,)?) => {
        $(
            if let Some(v) = $self.$field {
                $base.$field = v;
            }
        )+
    };
}

impl RcOptions {
    /// Overlay the set formatting options onto `base`, validating enumerated
    /// values. The `lint` field is handled separately (see [`Config`]).
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
        overlay!(
            base,
            self,
            indent_width,
            comma_spacing,
            final_newline,
            indent_directives,
            align_continuations,
            data_per_line,
            respect_stride_hints,
            blank_line_after_return,
            max_consecutive_blank_lines,
        );
        if let Some(s) = &self.mnemonic_case {
            base.mnemonic_case = parse_case("mnemonicCase", s)?;
        }
        if let Some(s) = &self.hex_digit_case {
            base.hex_digit_case = parse_case("hexDigitCase", s)?;
        }
        Ok(())
    }
}

/// The `lint` block of a `.nessemblerc` (or an `overrides` entry). `rules` maps a
/// rule id to either a severity string (`"warn"`) or a `[severity, options]`
/// pair; `ignore` is a list of regexes matched against a label's name.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RcLint {
    /// Rule id → severity or `[severity, options]`. Parsed from a raw `Value` so
    /// severity/options and unknown-rule errors carry precise messages.
    #[serde(default)]
    rules: BTreeMap<String, Value>,
    /// Names matching any pattern are exempt from every rule. `None` means
    /// inherit (an override that omits `ignore` keeps the base list); `Some([])`
    /// clears it.
    #[serde(default)]
    ignore: Option<Vec<String>>,
}

/// The options block for a single rule (`{ "window": N }`).
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RcRuleOptions {
    window: Option<usize>,
}

/// Parse a severity keyword for a named rule.
fn parse_severity(rule: &str, value: &str) -> Result<RuleSeverity, String> {
    match value {
        "off" => Ok(RuleSeverity::Off),
        "warn" => Ok(RuleSeverity::Warn),
        "error" => Ok(RuleSeverity::Error),
        other => Err(format!(
            "rule `{rule}` has invalid severity `{other}` (expected \"off\", \"warn\", or \"error\")"
        )),
    }
}

/// Parse a rule's config value into a severity and its options: either a bare
/// severity string, or a `[severity]` / `[severity, options]` array.
fn parse_rule_value(rule: &str, value: &Value) -> Result<(RuleSeverity, RcRuleOptions), String> {
    match value {
        Value::String(s) => Ok((parse_severity(rule, s)?, RcRuleOptions::default())),
        Value::Array(arr) => {
            if arr.is_empty() || arr.len() > 2 {
                return Err(format!(
                    "rule `{rule}` must be [severity] or [severity, options]"
                ));
            }
            let sev = arr[0]
                .as_str()
                .ok_or_else(|| format!("rule `{rule}` severity must be a string"))?;
            let sev = parse_severity(rule, sev)?;
            let opts = match arr.get(1) {
                Some(v) => serde_json::from_value::<RcRuleOptions>(v.clone())
                    .map_err(|e| format!("rule `{rule}` options: {e}"))?,
                None => RcRuleOptions::default(),
            };
            Ok((sev, opts))
        }
        _ => Err(format!(
            "rule `{rule}` must be a severity string or [severity, options] array"
        )),
    }
}

/// Compile a list of ignore patterns, failing loudly on a bad regex.
fn compile_patterns(patterns: &[String]) -> Result<Vec<Regex>, String> {
    patterns
        .iter()
        .map(|p| Regex::new(p).map_err(|e| format!("invalid ignore pattern `{p}`: {e}")))
        .collect()
}

/// A validated set of lint changes parsed from an `RcLint`: severities to set,
/// an optional window override, and an optional replacement ignore list.
struct LintDelta {
    severities: Vec<(RuleId, RuleSeverity)>,
    window: Option<usize>,
    ignore: Option<Vec<Regex>>,
}

/// Validate and resolve an `RcLint` into a [`LintDelta`]: unknown rule names,
/// bad severities/options, and malformed regexes are hard errors.
fn resolve_lint(rc: &RcLint) -> Result<LintDelta, String> {
    let mut severities = Vec::new();
    let mut window = None;
    for (name, value) in &rc.rules {
        let rule = RuleId::from_id(name).ok_or_else(|| format!("unknown lint rule `{name}`"))?;
        let (severity, opts) = parse_rule_value(name, value)?;
        severities.push((rule, severity));
        if let Some(w) = opts.window {
            window = Some(w);
        }
    }
    let ignore = match &rc.ignore {
        Some(patterns) => Some(compile_patterns(patterns)?),
        None => None,
    };
    Ok(LintDelta {
        severities,
        window,
        ignore,
    })
}

/// A resolved linter configuration for one file: per-rule severities, the
/// comment window, and the compiled ignore patterns.
#[derive(Clone)]
pub struct LintConfig {
    /// Per-rule severities (a rule may be `off`).
    pub severities: SeverityMap,
    /// The `require-block-comment` search window.
    pub window: usize,
    /// Compiled ignore patterns matched against a label's name.
    pub ignore: Vec<Regex>,
}

impl Default for LintConfig {
    fn default() -> Self {
        LintConfig {
            severities: SeverityMap::default(),
            window: DEFAULT_LINT_WINDOW,
            ignore: Vec::new(),
        }
    }
}

impl LintConfig {
    /// Apply a validated delta on top of this config.
    fn apply(&mut self, delta: &LintDelta) {
        for (rule, severity) in &delta.severities {
            self.severities.set(*rule, *severity);
        }
        if let Some(window) = delta.window {
            self.window = window;
        }
        if let Some(ignore) = &delta.ignore {
            self.ignore.clone_from(ignore);
        }
    }

    /// Whether `name` matches any ignore pattern.
    #[must_use]
    pub fn is_ignored_name(&self, name: &str) -> bool {
        self.ignore.iter().any(|r| r.is_match(name))
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

/// A resolved configuration for a set of inputs: base format + lint options, the
/// directory extension filter, ignore globs, and per-glob overrides — all
/// anchored at a single `root` directory for relative glob matching.
pub struct Config {
    base: FormatOptions,
    base_lint: LintConfig,
    extensions: Vec<String>,
    ignore: Vec<String>,
    overrides: Vec<(String, RcOptions)>,
    lint_overrides: Vec<(String, LintDelta)>,
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
            base_lint: LintConfig::default(),
            extensions: vec!["asm".to_string()],
            ignore: Vec::new(),
            overrides: Vec::new(),
            lint_overrides: Vec::new(),
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

        // Base lint config: defaults with the root `lint` block layered on.
        let mut base_lint = LintConfig::default();
        if let Some(rc_lint) = &rc.options.lint {
            let delta =
                resolve_lint(rc_lint).map_err(|e| format!("in `{}` lint: {e}", path.display()))?;
            base_lint.apply(&delta);
        }

        // Validate every override eagerly (format probe + lint resolution) so
        // per-file resolution is infallible.
        let mut overrides = Vec::new();
        let mut lint_overrides = Vec::new();
        for ov in rc.overrides {
            let mut probe = base.clone();
            ov.options.apply(&mut probe).map_err(|e| {
                format!("in `{}` overrides for `{}`: {e}", path.display(), ov.files)
            })?;
            if let Some(rc_lint) = &ov.options.lint {
                let delta = resolve_lint(rc_lint).map_err(|e| {
                    format!(
                        "in `{}` overrides for `{}` lint: {e}",
                        path.display(),
                        ov.files
                    )
                })?;
                lint_overrides.push((ov.files.clone(), delta));
            }
            overrides.push((ov.files, ov.options));
        }

        let extensions = rc.extensions.map_or_else(
            || vec!["asm".to_string()],
            |list| list.iter().map(|e| normalize_ext(e)).collect(),
        );

        Ok(Config {
            base,
            base_lint,
            extensions,
            ignore: Vec::new(),
            overrides,
            lint_overrides,
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
    #[must_use]
    pub fn has_formatted_ext(&self, file: &Path) -> bool {
        file.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|ext| self.extensions.contains(&ext.to_ascii_lowercase()))
    }

    /// Whether `file` is excluded by a `.nessembleignore` glob.
    #[must_use]
    pub fn is_ignored(&self, file: &Path) -> bool {
        if self.ignore.is_empty() {
            return false;
        }
        let rel = rel_to(&self.root, file);
        self.ignore.iter().any(|g| matches_path_glob(g, &rel))
    }

    /// The effective formatting options for `file`: base options plus every
    /// matching override, applied in order. Overrides were validated at load, so
    /// this does not fail.
    #[must_use]
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

    /// The effective linter configuration for `file`: base lint config plus every
    /// matching override delta, applied in order.
    #[must_use]
    pub fn lint_for(&self, file: &Path) -> LintConfig {
        let mut cfg = self.base_lint.clone();
        if !self.lint_overrides.is_empty() {
            let rel = rel_to(&self.root, file);
            for (glob, delta) in &self.lint_overrides {
                if matches_path_glob(glob, &rel) {
                    cfg.apply(delta);
                }
            }
        }
        cfg
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
    fn align_continuations_defaults_on_and_maps_onto_format_options() {
        // Default is on; the key can turn it off.
        let mut base = FormatOptions::default();
        assert!(base.align_continuations);
        let rc: RcConfig = serde_json::from_str(r#"{"alignContinuations":false}"#).unwrap();
        rc.options.apply(&mut base).unwrap();
        assert!(!base.align_continuations);
    }

    #[test]
    fn align_continuations_participates_in_overrides() {
        // Base turns it off; a per-glob override turns it back on for matching
        // files — exercising the full `options_for` layering path.
        let rc: RcConfig = serde_json::from_str(
            r#"{
                "alignContinuations": false,
                "overrides": [
                    { "files": "src/**/*.asm", "options": { "alignContinuations": true } }
                ]
            }"#,
        )
        .unwrap();
        let mut base = FormatOptions::default();
        rc.options.apply(&mut base).unwrap();
        let config = Config {
            base,
            base_lint: LintConfig::default(),
            extensions: vec!["asm".to_string()],
            ignore: Vec::new(),
            overrides: rc
                .overrides
                .into_iter()
                .map(|o| (o.files, o.options))
                .collect(),
            lint_overrides: Vec::new(),
            root: PathBuf::from("."),
        };
        // A non-matching file keeps the base (off); a matching one gets the
        // override (on).
        assert!(
            !config
                .options_for(Path::new("other/x.asm"))
                .align_continuations
        );
        assert!(
            config
                .options_for(Path::new("src/data/x.asm"))
                .align_continuations
        );
    }

    #[test]
    fn indent_directives_maps_onto_format_options() {
        let mut base = FormatOptions::default();
        assert!(!base.indent_directives);
        let rc: RcConfig = serde_json::from_str(r#"{"indentDirectives":true}"#).unwrap();
        rc.options.apply(&mut base).unwrap();
        assert!(base.indent_directives);
    }

    #[test]
    fn invalid_indent_style_is_an_error() {
        let rc: RcConfig = serde_json::from_str(r#"{"indentStyle":"wide"}"#).unwrap();
        let mut base = FormatOptions::default();
        assert!(rc.options.apply(&mut base).is_err());
    }

    #[test]
    fn case_keys_map_onto_format_options() {
        let rc: RcConfig =
            serde_json::from_str(r#"{"mnemonicCase":"upper","hexDigitCase":"lower"}"#).unwrap();
        let mut base = FormatOptions::default();
        rc.options.apply(&mut base).unwrap();
        assert_eq!(base.mnemonic_case, Case::Upper);
        assert_eq!(base.hex_digit_case, Case::Lower);
    }

    #[test]
    fn invalid_case_value_is_an_error() {
        let rc: RcConfig = serde_json::from_str(r#"{"mnemonicCase":"title"}"#).unwrap();
        let mut base = FormatOptions::default();
        assert!(rc.options.apply(&mut base).is_err());
    }

    // ─── Lint config ──────────────────────────────────────────────────────────

    /// Resolve a `.nessemblerc` JSON string into a base [`LintConfig`].
    fn base_lint(json: &str) -> Result<LintConfig, String> {
        let rc: RcConfig = serde_json::from_str(json).map_err(|e| e.to_string())?;
        let mut cfg = LintConfig::default();
        if let Some(rc_lint) = &rc.options.lint {
            cfg.apply(&resolve_lint(rc_lint)?);
        }
        Ok(cfg)
    }

    #[test]
    fn lint_defaults_warn_with_no_config() {
        let cfg = LintConfig::default();
        assert_eq!(
            cfg.severities.get(RuleId::RequireBlockComment),
            RuleSeverity::Warn
        );
        assert_eq!(cfg.window, DEFAULT_LINT_WINDOW);
        assert!(cfg.ignore.is_empty());
    }

    #[test]
    fn lint_severity_and_window_map_from_config() {
        let cfg =
            base_lint(r#"{"lint":{"rules":{"require-block-comment":["error",{"window":5}]}}}"#)
                .unwrap();
        assert_eq!(
            cfg.severities.get(RuleId::RequireBlockComment),
            RuleSeverity::Error
        );
        assert_eq!(cfg.window, 5);
    }

    #[test]
    fn lint_bare_severity_string_maps() {
        let cfg = base_lint(r#"{"lint":{"rules":{"require-block-comment":"off"}}}"#).unwrap();
        assert_eq!(
            cfg.severities.get(RuleId::RequireBlockComment),
            RuleSeverity::Off
        );
    }

    #[test]
    fn lint_ignore_patterns_compile_and_match() {
        let cfg = base_lint(r#"{"lint":{"ignore":["^loc_","^data_"]}}"#).unwrap();
        assert!(cfg.is_ignored_name("loc_8000"));
        assert!(cfg.is_ignored_name("data_c000"));
        assert!(!cfg.is_ignored_name("sound_engine"));
    }

    #[test]
    fn lint_unknown_rule_is_an_error() {
        assert!(base_lint(r#"{"lint":{"rules":{"require-block-commnt":"warn"}}}"#).is_err());
    }

    #[test]
    fn lint_invalid_severity_is_an_error() {
        assert!(base_lint(r#"{"lint":{"rules":{"require-block-comment":"loud"}}}"#).is_err());
    }

    #[test]
    fn lint_unknown_rule_option_is_an_error() {
        assert!(
            base_lint(r#"{"lint":{"rules":{"require-block-comment":["warn",{"windwo":3}]}}}"#)
                .is_err()
        );
    }

    #[test]
    fn lint_bad_regex_is_an_error() {
        assert!(base_lint(r#"{"lint":{"ignore":["[unclosed"]}}"#).is_err());
    }

    #[test]
    fn lint_unknown_key_is_an_error() {
        assert!(base_lint(r#"{"lint":{"widnow":3}}"#).is_err());
    }

    #[test]
    fn lint_overrides_layer_per_glob() {
        // Base warns everywhere; an override turns the rule off for src/data.
        let rc: RcConfig = serde_json::from_str(
            r#"{
                "lint": { "rules": { "require-block-comment": "error" } },
                "overrides": [
                    { "files": "src/data/**/*.asm",
                      "options": { "lint": { "rules": { "require-block-comment": "off" } } } }
                ]
            }"#,
        )
        .unwrap();
        let mut base_lint = LintConfig::default();
        base_lint.apply(&resolve_lint(rc.options.lint.as_ref().unwrap()).unwrap());
        let lint_overrides: Vec<(String, LintDelta)> = rc
            .overrides
            .iter()
            .filter_map(|o| {
                o.options
                    .lint
                    .as_ref()
                    .map(|l| (o.files.clone(), resolve_lint(l).unwrap()))
            })
            .collect();
        let config = Config {
            base: FormatOptions::default(),
            base_lint,
            extensions: vec!["asm".to_string()],
            ignore: Vec::new(),
            overrides: Vec::new(),
            lint_overrides,
            root: PathBuf::from("."),
        };
        assert_eq!(
            config
                .lint_for(Path::new("src/code/x.asm"))
                .severities
                .get(RuleId::RequireBlockComment),
            RuleSeverity::Error
        );
        assert_eq!(
            config
                .lint_for(Path::new("src/data/tables/x.asm"))
                .severities
                .get(RuleId::RequireBlockComment),
            RuleSeverity::Off
        );
    }
}
