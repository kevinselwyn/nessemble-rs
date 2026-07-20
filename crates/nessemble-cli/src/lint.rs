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

use std::path::{Path, PathBuf};

use clap::Args;
use nessemble_core::tooling::{lint, LintOptions, RuleSeverity};
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

    /// assembly source file or directory to lint
    #[arg(value_name = "path", required = true)]
    paths: Vec<String>,
}

/// A file to lint together with its resolved lint configuration.
type Job = (PathBuf, LintConfig);

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
            for file in found {
                let lint_cfg = config.lint_for(&file);
                jobs.push((file, lint_cfg));
            }
        } else if path.is_file() {
            let lint_cfg = config.lint_for(path);
            jobs.push((path.to_path_buf(), lint_cfg));
        } else {
            eprintln!("nessemble: no such file or directory: {p}");
            return RETURN_EPERM;
        }
    }
    jobs.sort_by(|a, b| a.0.cmp(&b.0));
    jobs.dedup_by(|a, b| a.0 == b.0);

    report(&jobs, opts)
}

/// Lint every job, print the grouped report, and return the exit code.
fn report(jobs: &[Job], opts: &LintArgs) -> u8 {
    let mut total_errors = 0usize;
    let mut total_warnings = 0usize;
    let mut io_code = RETURN_OK;

    for (file, lint_cfg) in jobs {
        let source = match std::fs::read_to_string(file) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("nessemble: could not read `{}`: {e}", file.display());
                io_code = RETURN_EPERM;
                continue;
            }
        };

        let ignore = |name: &str| lint_cfg.is_ignored_name(name);
        let options = LintOptions {
            severities: lint_cfg.severities.clone(),
            window: lint_cfg.window,
            ignore: &ignore,
        };
        let findings = lint(&source, &options);

        // Map each finding to its severity, filtering out warnings under
        // `--quiet`, and collect the rows to print for this file.
        let mut rows: Vec<(&nessemble_core::tooling::Finding, &'static str)> = Vec::new();
        for f in &findings {
            match lint_cfg.severities.get(f.rule) {
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
        println!("\n{}", file.display());
        for (f, severity) in rows {
            println!(
                "  {:>5}:{:<3} {:<7}  {}  {}",
                f.line,
                f.column,
                severity,
                f.subject,
                f.rule.id()
            );
        }
    }

    print_summary(total_errors, total_warnings);

    if io_code != RETURN_OK {
        return io_code;
    }
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
