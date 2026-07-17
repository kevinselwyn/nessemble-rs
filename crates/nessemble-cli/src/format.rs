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

use nessemble_core::tooling::{format_with, FormatOptions};

use crate::rc::{Choice, Config};
use crate::{RETURN_EPERM, RETURN_OK, RETURN_USAGE};

/// Parsed `format` options.
#[derive(Default)]
struct Opts {
    write: bool,
    check: bool,
    config: Option<String>,
    no_config: bool,
    paths: Vec<String>,
}

/// The outcome of parsing `format`'s own argument vector.
enum Parsed {
    Run(Opts),
    /// Print usage and exit with `RETURN_USAGE` (help, or a bad flag).
    Usage,
}

/// A file to format together with its resolved options.
type Job = (PathBuf, FormatOptions);

/// Run `format` with its raw argument vector (everything after `format`).
pub fn run(exec: &str, args: &[String]) -> u8 {
    let opts = match parse(args) {
        Parsed::Run(o) => o,
        Parsed::Usage => {
            print!("{}", usage(exec));
            return RETURN_USAGE;
        }
    };

    if opts.write && opts.check {
        eprintln!("nessemble: --write and --check are mutually exclusive");
        print!("{}", usage(exec));
        return RETURN_USAGE;
    }
    if opts.config.is_some() && opts.no_config {
        eprintln!("nessemble: --config and --no-config are mutually exclusive");
        print!("{}", usage(exec));
        return RETURN_USAGE;
    }
    if opts.paths.is_empty() {
        print!("{}", usage(exec));
        return RETURN_USAGE;
    }

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
        stdout_mode(exec, &jobs)
    }
}

/// No `--write`/`--check`: print a single file's formatted text to stdout,
/// leaving it untouched. More than one input is a usage error — stdout only
/// makes sense for one file.
fn stdout_mode(exec: &str, jobs: &[Job]) -> u8 {
    if jobs.len() != 1 {
        eprintln!("nessemble: formatting multiple files requires --write or --check");
        print!("{}", usage(exec));
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

/// Parse `format`'s own arguments: `-w`/`--write`, `-c`/`--check`,
/// `--config <file>`, `--no-config`, `-h`/`--help`, `--` (end of options), and
/// path positionals.
fn parse(args: &[String]) -> Parsed {
    let mut opts = Opts::default();
    let mut rest_are_paths = false;
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if rest_are_paths {
            opts.paths.push(arg.clone());
            i += 1;
            continue;
        }
        match arg.as_str() {
            "--" => rest_are_paths = true,
            "-w" | "--write" => opts.write = true,
            "-c" | "--check" => opts.check = true,
            "--no-config" => opts.no_config = true,
            "--config" => {
                i += 1;
                match args.get(i) {
                    Some(value) => opts.config = Some(value.clone()),
                    None => return Parsed::Usage,
                }
            }
            other if other.starts_with("--config=") => {
                opts.config = Some(other["--config=".len()..].to_string());
            }
            other if other.starts_with('-') && other.len() > 1 => return Parsed::Usage,
            _ => opts.paths.push(arg.clone()),
        }
        i += 1;
    }
    Parsed::Run(opts)
}

/// The `format` subcommand's help text.
fn usage(exec: &str) -> String {
    format!(
        "Usage: {exec} format [<options>] <path> ...\n\
         \n\
         Format nessemble assembly source. A single file is printed to stdout;\n\
         --write edits files in place; --check reports files needing formatting.\n\
         \n\
         Options:\n\
         \x20 -w, --write         rewrite files in place (required for a directory)\n\
         \x20 -c, --check         exit non-zero if any file is not formatted; write nothing\n\
         \x20     --config <file> use <file> as the .nessemblerc\n\
         \x20     --no-config     ignore any .nessemblerc; use built-in defaults\n\
         \x20 -h, --help          print this message\n"
    )
}
