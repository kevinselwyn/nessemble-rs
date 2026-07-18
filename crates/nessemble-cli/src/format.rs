//! `format` subcommand: a prettier-style formatter for nessemble assembly.
//!
//! `nessemble format <path>...` formats `.asm` sources. A single file is printed
//! to stdout; `--write` edits files in place; `--check` is the CI gate (exit
//! non-zero, write nothing). A directory is walked recursively (for the
//! configured extensions) and requires `--write` or `--check`.
//!
//! Formatting is governed by a discovered `.nessemblerc` (see [`crate::rc`]);
//! `--config <file>` forces one and `--no-config` uses built-in defaults.
//! `.nessembleignore` globs exclude paths from directory walks, and per-glob
//! `overrides` refine options per file.

use std::path::{Path, PathBuf};

use clap::Args;
use nessemble_core::tooling::{format_with, FormatOptions};

use crate::rc::{Choice, Config};
use crate::{RETURN_EPERM, RETURN_OK, RETURN_USAGE};

/// Parsed `format` options. clap enforces the `--write`/`--check` and
/// `--config`/`--no-config` mutual exclusions and the presence of at least one
/// path; the remaining directory/multi-file rules are checked at runtime below
/// (they depend on filesystem inspection).
#[derive(Args)]
pub struct FormatArgs {
    /// rewrite files in place (required for a directory)
    #[arg(short = 'w', long, conflicts_with = "check")]
    write: bool,

    /// exit non-zero if any file is not formatted; write nothing
    #[arg(short = 'c', long)]
    check: bool,

    /// use <file> as the .nessemblerc
    #[arg(long, value_name = "file", conflicts_with = "no_config")]
    config: Option<String>,

    /// ignore any .nessemblerc; use built-in defaults
    #[arg(long = "no-config")]
    no_config: bool,

    /// assembly source file or directory to format
    #[arg(value_name = "path", required = true)]
    paths: Vec<String>,
}

/// A file to format together with its resolved options.
type Job = (PathBuf, FormatOptions);

/// Run `format` with its parsed options.
pub fn run(opts: &FormatArgs) -> u8 {
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
            if !opts.write && !opts.check {
                eprintln!("nessemble: formatting a directory requires --write or --check");
                return RETURN_USAGE;
            }
            let mut found = Vec::new();
            collect_files(path, &config, &mut found);
            for file in found {
                let options = config.options_for(&file);
                jobs.push((file, options));
            }
        } else if path.is_file() {
            // An explicitly named file is always formatted (extension/ignore
            // filters apply only to directory walks).
            let options = config.options_for(path);
            jobs.push((path.to_path_buf(), options));
        } else {
            eprintln!("nessemble: no such file or directory: {p}");
            return RETURN_EPERM;
        }
    }
    jobs.sort_by(|a, b| a.0.cmp(&b.0));
    jobs.dedup_by(|a, b| a.0 == b.0);

    if opts.write {
        write_mode(&jobs)
    } else if opts.check {
        check_mode(&jobs)
    } else {
        stdout_mode(&jobs)
    }
}

/// No `--write`/`--check`: print a single file's formatted text to stdout,
/// leaving it untouched. More than one input is a usage error — stdout only
/// makes sense for one file.
fn stdout_mode(jobs: &[Job]) -> u8 {
    if jobs.len() != 1 {
        eprintln!("nessemble: formatting multiple files requires --write or --check");
        return RETURN_USAGE;
    }
    let (file, options) = &jobs[0];
    match std::fs::read_to_string(file) {
        Ok(source) => {
            print!("{}", format_with(&source, options));
            RETURN_OK
        }
        Err(e) => {
            eprintln!("nessemble: could not read `{}`: {e}", file.display());
            RETURN_EPERM
        }
    }
}

/// `--write`: rewrite each file that changes in place, reporting the path.
fn write_mode(jobs: &[Job]) -> u8 {
    let mut code = RETURN_OK;
    for (file, options) in jobs {
        let source = match std::fs::read_to_string(file) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("nessemble: could not read `{}`: {e}", file.display());
                code = RETURN_EPERM;
                continue;
            }
        };
        let out = format_with(&source, options);
        if out != source {
            if let Err(e) = std::fs::write(file, &out) {
                eprintln!("nessemble: could not write `{}`: {e}", file.display());
                code = RETURN_EPERM;
                continue;
            }
            println!("formatted {}", file.display());
        }
    }
    code
}

/// `--check`: write nothing; list files that are not already formatted and
/// exit non-zero if any are found.
fn check_mode(jobs: &[Job]) -> u8 {
    let mut unformatted = 0usize;
    let mut code = RETURN_OK;
    for (file, options) in jobs {
        let source = match std::fs::read_to_string(file) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("nessemble: could not read `{}`: {e}", file.display());
                code = RETURN_EPERM;
                continue;
            }
        };
        if format_with(&source, options) != source {
            println!("{}", file.display());
            unformatted += 1;
        }
    }
    if unformatted > 0 {
        RETURN_EPERM
    } else {
        code
    }
}

/// Recursively collect formattable files under `dir`: those whose extension is
/// configured and that are not excluded by `.nessembleignore`.
fn collect_files(dir: &Path, config: &Config, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if config.is_ignored(&path) {
            continue;
        }
        if path.is_dir() {
            collect_files(&path, config, out);
        } else if config.has_formatted_ext(&path) {
            out.push(path);
        }
    }
}
