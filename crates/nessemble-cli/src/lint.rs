//! `lint` subcommand: report style problems in nessemble assembly.
//!
//! `nessemble lint <path>...` scans `.asm` sources for lint findings and prints
//! an ESLint-style, per-file report. It never rewrites source — the formatter
//! (`nessemble format`) owns rewriting; the linter only reports. A rule
//! configured as `error` (in `.nessemblerc`) fails the run; a `warn` does not,
//! unless `--max-warnings` is exceeded.
//!
//! Configuration is the discovered `.nessemblerc` `lint` block (see
//! [`nessemble_rc`]); `--config <file>` forces one and `--no-config` uses
//! built-in defaults (the one rule at `warn`, window 3, no ignores).
//!
//! **Program mode** (`plans/023-whole-program-clobber-analysis.md` §2.1): when
//! the project assembles from an entry file — `--entry`, the `.nessemblerc`
//! `entry`, or a file argument that assembles on its own — the routine rules
//! run over the assembled program. Every other file is linted in **file
//! mode**, exactly as before.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use clap::Args;
use nessemble_core::tooling::{
    lint, lint_programs, Finding, LintOptions, ProgramFile, RuleId, RuleSeverity,
};
use nessemble_core::{build_program, Options, ProgramBuild};
use nessemble_rc::{Choice, Config, LintConfig};

use crate::{RETURN_EPERM, RETURN_OK};

/// Parsed `lint` options.
#[derive(Args)]
pub struct LintArgs {
    /// use <file> as the .nessemblerc
    #[arg(long, value_name = "file", conflicts_with = "no_config")]
    config: Option<String>,

    /// ignore any .nessemblerc; use built-in defaults
    #[arg(long = "no-config")]
    no_config: bool,

    /// exit non-zero if more than <n> warnings are reported (errors always fail)
    #[arg(long = "max-warnings", value_name = "n")]
    max_warnings: Option<usize>,

    /// report errors only; suppress warning-level findings
    #[arg(long)]
    quiet: bool,

    /// assemble the program from <file> and run the routine rules over it
    /// (repeatable; default: the `.nessemblerc` `entry`)
    #[arg(long, value_name = "file")]
    entry: Vec<String>,

    /// custom pseudo-instruction functions for assembling the entry (default:
    /// the `.nessemblerc` `assemble.pseudo`)
    #[arg(short = 'p', long, value_name = "pseudo.txt")]
    pseudo: Option<String>,

    /// project root for `@/`-relative paths when assembling the entry
    #[arg(long, value_name = "dir")]
    root: Option<String>,

    /// do not read or write the custom pseudo-instruction cache
    #[arg(long)]
    no_cache: bool,

    /// assembly source file or directory to lint
    #[arg(value_name = "path", required = true)]
    paths: Vec<String>,
}

/// A file to lint: the path as reports print it, and whether it was named on
/// the command line (rather than found by walking a directory).
struct Job {
    path: PathBuf,
    named: bool,
}

/// One file to report, by canonical path.
struct Reported {
    /// The path as the report's header prints it.
    shown: PathBuf,
    lint: LintConfig,
}

/// An entry assembled for program mode.
struct Built {
    entry: PathBuf,
    build: ProgramBuild,
    /// The entry came from `--entry` or the rc (a failure is reported).
    explicit: bool,
}

/// Run `lint` with its parsed options.
pub fn run(opts: &LintArgs) -> u8 {
    let choice = if opts.no_config {
        Choice::NoConfig
    } else if let Some(path) = &opts.config {
        Choice::Explicit(PathBuf::from(path))
    } else {
        Choice::Discover
    };

    let mut jobs: Vec<Job> = Vec::new();
    let mut configs: Vec<Config> = Vec::new();
    for p in &opts.paths {
        let path = Path::new(p);
        let config = match Config::resolve(path, &choice) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("nessemble: {e}");
                return RETURN_EPERM;
            }
        };
        if path.is_dir() {
            let mut found = Vec::new();
            crate::format::collect_files(path, &config, &mut found);
            jobs.extend(found.into_iter().map(|path| Job { path, named: false }));
        } else if path.is_file() {
            jobs.push(Job {
                path: path.to_path_buf(),
                named: true,
            });
        } else {
            eprintln!("nessemble: no such file or directory: {p}");
            return RETURN_EPERM;
        }
        configs.push(config);
    }

    let root = match crate::resolve_root_flag(opts.root.as_deref()) {
        Ok(root) => root,
        Err(code) => return code,
    };

    // The entries, by §2.1's precedence.
    let mut explicit: Vec<PathBuf> = Vec::new();
    for config in &configs {
        match explicit_entries(opts, config) {
            Ok(list) => {
                for e in list {
                    if !explicit.iter().any(|x| same_file(x, &e)) {
                        explicit.push(e);
                    }
                }
            }
            Err(code) => return code,
        }
        if !opts.entry.is_empty() {
            break;
        }
    }
    let opportunistic: Vec<PathBuf> = if explicit.is_empty() {
        jobs.iter()
            .filter(|j| j.named)
            .map(|j| j.path.clone())
            .collect()
    } else {
        Vec::new()
    };

    let mut code = RETURN_OK;
    let mut built: Vec<Built> = Vec::new();
    for (entry, is_explicit) in explicit
        .iter()
        .map(|e| (e, true))
        .chain(opportunistic.iter().map(|e| (e, false)))
    {
        let config = &configs[0];
        let build = assemble(entry, opts, config, root.clone());
        if is_explicit && build.program.is_none() {
            for d in &build.errors {
                crate::print_diag("error-line", d);
            }
            eprintln!(
                "nessemble: lint: `{}` did not assemble, so routine rules were skipped for it",
                entry.display()
            );
            code = RETURN_EPERM;
        }
        built.push(Built {
            entry: entry.clone(),
            build,
            explicit: is_explicit,
        });
    }

    // What to report: the named and walked files, plus every file in the
    // closure of an entry that was itself named.
    let mut reported: BTreeMap<PathBuf, Reported> = BTreeMap::new();
    for job in &jobs {
        let canon = canonical(&job.path);
        let lint = resolve_lint(&job.path, &choice);
        reported.entry(canon).or_insert(Reported {
            shown: job.path.clone(),
            lint,
        });
    }
    for b in &built {
        if b.build.program.is_none() {
            continue;
        }
        let named = jobs.iter().any(|j| j.named && same_file(&j.path, &b.entry));
        if !named {
            continue;
        }
        for path in &b.build.paths {
            let canon = canonical(path);
            if reported.contains_key(&canon) {
                continue;
            }
            let config = Config::resolve(path, &choice).ok();
            if config.as_ref().is_some_and(|c| c.is_ignored(path)) {
                continue;
            }
            reported.insert(
                canon.clone(),
                Reported {
                    shown: display_rel(&canon),
                    lint: config.map_or_else(LintConfig::default, |c| c.lint_for(path)),
                },
            );
        }
    }

    // Program mode: every file of an assembled program (reported or not, so
    // the contracts of the whole closure are known).
    let programs: Vec<_> = built
        .iter()
        .filter_map(|b| b.build.program.clone())
        .collect();
    let mut closure: Vec<PathBuf> = Vec::new();
    for b in built.iter().filter(|b| b.build.program.is_some()) {
        for path in &b.build.paths {
            let canon = canonical(path);
            if !closure.contains(&canon) {
                closure.push(canon);
            }
        }
    }
    // A failed explicit entry's files get no routine rules at all (§2.1).
    let skipped: HashSet<PathBuf> = built
        .iter()
        .filter(|b| b.explicit && b.build.program.is_none())
        .flat_map(|b| b.build.paths.iter().map(|p| canonical(p)))
        .filter(|p| !closure.contains(p))
        .collect();

    let mut findings: BTreeMap<PathBuf, Vec<Finding>> = BTreeMap::new();
    if !programs.is_empty() {
        let mut sources: Vec<(PathBuf, String, LintConfig)> = Vec::new();
        for canon in &closure {
            let Ok(source) = std::fs::read_to_string(canon) else {
                continue;
            };
            let lint = match reported.get(canon) {
                Some(r) => r.lint.clone(),
                None => resolve_lint(canon, &choice),
            };
            sources.push((canon.clone(), source, lint));
        }
        let ignores: Vec<_> = sources
            .iter()
            .map(|(_, _, cfg)| move |n: &str| cfg.is_ignored_name(n))
            .collect();
        let options: Vec<LintOptions> = sources
            .iter()
            .zip(&ignores)
            .map(|((_, _, cfg), ignore)| LintOptions {
                severities: cfg.severities.clone(),
                window: cfg.window,
                ignore,
            })
            .collect();
        let files: Vec<ProgramFile> = sources
            .iter()
            .zip(&options)
            .map(|((path, source, _), options)| ProgramFile {
                path,
                source,
                options,
                report: reported.contains_key(path),
            })
            .collect();
        let result = lint_programs(&programs, &files);
        for ((path, _, _), f) in sources.iter().zip(result.findings) {
            if reported.contains_key(path) {
                findings.insert(path.clone(), f);
            }
        }
    }

    // File mode for everything else.
    let mut io_code = RETURN_OK;
    for (canon, r) in &reported {
        if findings.contains_key(canon) {
            continue;
        }
        let source = match std::fs::read_to_string(&r.shown) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("nessemble: could not read `{}`: {e}", r.shown.display());
                io_code = RETURN_EPERM;
                continue;
            }
        };
        let ignore = |name: &str| r.lint.is_ignored_name(name);
        let mut severities = r.lint.severities.clone();
        if skipped.contains(canon) {
            for rule in RuleId::ALL.into_iter().filter(|r| r.needs_program()) {
                severities.set(rule, RuleSeverity::Off);
            }
        }
        let options = LintOptions {
            severities,
            window: r.lint.window,
            ignore: &ignore,
        };
        findings.insert(canon.clone(), lint(&source, &options));
    }

    let mut rows: Vec<(&Reported, &[Finding])> = reported
        .iter()
        .filter_map(|(canon, r)| findings.get(canon).map(|f| (r, f.as_slice())))
        .collect();
    rows.sort_by(|a, b| a.0.shown.cmp(&b.0.shown));
    let report_code = report(&rows, opts);
    [io_code, code, report_code]
        .into_iter()
        .find(|&c| c != RETURN_OK)
        .unwrap_or(RETURN_OK)
}

/// Assemble `entry` for program mode, reading from disk.
fn assemble(entry: &Path, opts: &LintArgs, config: &Config, root: Option<PathBuf>) -> ProgramBuild {
    let options = Options {
        project_root: root,
        rules: config.assemble_rules(),
        ..Options::default()
    };
    let pseudo = opts
        .pseudo
        .clone()
        .or_else(|| config.pseudo().map(|p| p.display().to_string()));
    let resolver = crate::custom::build_resolver(pseudo.as_deref(), !opts.no_cache, None);
    let source = std::fs::read_to_string(entry).unwrap_or_default();
    let none = |_: &Path| None;
    build_program(entry, &source, &options, &none, resolver)
}

/// The lint configuration for `path`, or the defaults when its config cannot
/// be read (already reported when the path was a job).
fn resolve_lint(path: &Path, choice: &Choice) -> LintConfig {
    Config::resolve(path, choice).map_or_else(|_| LintConfig::default(), |c| c.lint_for(path))
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn same_file(a: &Path, b: &Path) -> bool {
    canonical(a) == canonical(b)
}

/// The explicit entries — `--entry` flags, else the `.nessemblerc` `entry` —
/// each checked to exist. A missing one is reported and fails the run.
fn explicit_entries(opts: &LintArgs, config: &Config) -> Result<Vec<PathBuf>, u8> {
    if !opts.entry.is_empty() {
        let mut out = Vec::new();
        for e in &opts.entry {
            let path = PathBuf::from(e);
            if !path.is_file() {
                eprintln!("nessemble: entry `{e}` does not exist");
                return Err(RETURN_EPERM);
            }
            out.push(path);
        }
        return Ok(out);
    }
    let entries = config.entries();
    for entry in &entries {
        if !entry.is_file() {
            let rc = config
                .rc_path()
                .map(|p| display_rel(&p).display().to_string())
                .unwrap_or_default();
            let shown = config
                .rc_dir()
                .and_then(|d| entry.strip_prefix(d).ok())
                .map_or_else(|| entry.display().to_string(), |p| p.display().to_string());
            eprintln!("nessemble: entry `{shown}` (set in `{rc}`) does not exist");
            return Err(RETURN_EPERM);
        }
    }
    Ok(entries)
}

/// `path` relative to the current directory when it is under it.
fn display_rel(path: &Path) -> PathBuf {
    std::env::current_dir()
        .ok()
        .and_then(|cwd| {
            let cwd = cwd.canonicalize().unwrap_or(cwd);
            path.strip_prefix(&cwd).ok().map(Path::to_path_buf)
        })
        .unwrap_or_else(|| path.to_path_buf())
}

/// Print the grouped report and return the exit code.
fn report(files: &[(&Reported, &[Finding])], opts: &LintArgs) -> u8 {
    let mut total_errors = 0usize;
    let mut total_warnings = 0usize;

    for (file, findings) in files {
        // Map each finding to its severity, filtering out warnings under
        // `--quiet`, and collect the rows to print for this file.
        let mut rows: Vec<(&Finding, &'static str)> = Vec::new();
        for f in *findings {
            match file.lint.severities.get(f.rule) {
                RuleSeverity::Error => {
                    total_errors += 1;
                    rows.push((f, "error"));
                }
                RuleSeverity::Warn => {
                    if opts.quiet {
                        continue;
                    }
                    total_warnings += 1;
                    rows.push((f, "warning"));
                }
                // Off rules never produce findings.
                RuleSeverity::Off => {}
            }
        }

        if rows.is_empty() {
            continue;
        }
        println!("\n{}", file.shown.display());
        for (f, severity) in rows {
            // ESLint's row shape: position, severity, what is wrong, rule id.
            // The message names the subject, so it is not printed twice.
            println!(
                "  {:>5}:{:<3} {:<7}  {}  {}",
                f.line,
                f.column,
                severity,
                f.message,
                f.rule.id()
            );
        }
    }

    print_summary(total_errors, total_warnings);

    if total_errors > 0 {
        return RETURN_EPERM;
    }
    if let Some(max) = opts.max_warnings {
        if total_warnings > max {
            return RETURN_EPERM;
        }
    }
    RETURN_OK
}

/// Print the ESLint-style summary footer.
fn print_summary(errors: usize, warnings: usize) {
    let total = errors + warnings;
    if total == 0 {
        println!("✓ No problems.");
        return;
    }
    println!(
        "\n✖ {total} problem{} ({errors} error{}, {warnings} warning{})",
        plural(total),
        plural(errors),
        plural(warnings),
    );
}

/// The plural suffix (`""` for 1, `"s"` otherwise).
fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}
