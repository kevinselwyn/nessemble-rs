//! `format` subcommand: a prettier-style formatter for nessemble assembly.
//!
//! Phase 1 of `plans/005-formatter.md`: `nessemble format <path>...` formats
//! `.asm` sources with the default [`FormatOptions`]. A single file is printed
//! to stdout; `--write` edits files in place; `--check` is the CI gate (exit
//! non-zero, write nothing). A directory is walked recursively and requires
//! `--write` or `--check`. Configurable options and `.nessemblerc` discovery
//! (`--config` / `--no-config`) arrive in Phase 3; this phase always uses
//! [`FormatOptions::default`].

use std::path::{Path, PathBuf};

use nessemble_core::tooling::{format_with, FormatOptions};

use crate::{RETURN_EPERM, RETURN_OK, RETURN_USAGE};

/// File extension formatted during a directory walk (Phase 3 makes this
/// configurable via `.nessemblerc`).
const FORMATTED_EXT: &str = "asm";

/// Parsed `format` options.
#[derive(Default)]
struct Opts {
    write: bool,
    check: bool,
    paths: Vec<String>,
}

/// The outcome of parsing `format`'s own argument vector.
enum Parsed {
    Run(Opts),
    /// Print usage and exit with the given code (help → success-as-usage; a
    /// bad flag → usage error). Both use `RETURN_USAGE`, matching the rest of
    /// the CLI.
    Usage,
}

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
    if opts.paths.is_empty() {
        print!("{}", usage(exec));
        return RETURN_USAGE;
    }

    // Resolve inputs to a concrete, deterministic file list. A directory is
    // walked for `.asm` files and requires `--write`/`--check`; an explicitly
    // named file is always included regardless of extension.
    let mut files: Vec<PathBuf> = Vec::new();
    for p in &opts.paths {
        let path = Path::new(p);
        if path.is_dir() {
            if !opts.write && !opts.check {
                eprintln!("nessemble: formatting a directory requires --write or --check");
                return RETURN_USAGE;
            }
            collect_files(path, &mut files);
        } else if path.is_file() {
            files.push(path.to_path_buf());
        } else {
            eprintln!("nessemble: no such file or directory: {p}");
            return RETURN_EPERM;
        }
    }
    files.sort();
    files.dedup();

    if opts.write {
        write_mode(&files)
    } else if opts.check {
        check_mode(&files)
    } else {
        stdout_mode(exec, &files)
    }
}

/// Default-options formatting for `source`.
fn formatted(source: &str) -> String {
    format_with(source, &FormatOptions::default())
}

/// No `--write`/`--check`: print a single file's formatted text to stdout,
/// leaving it untouched. More than one input (or a directory, handled earlier)
/// is a usage error — stdout only makes sense for one file.
fn stdout_mode(exec: &str, files: &[PathBuf]) -> u8 {
    if files.len() != 1 {
        eprintln!("nessemble: formatting multiple files requires --write or --check");
        print!("{}", usage(exec));
        return RETURN_USAGE;
    }
    match std::fs::read_to_string(&files[0]) {
        Ok(source) => {
            print!("{}", formatted(&source));
            RETURN_OK
        }
        Err(e) => {
            eprintln!("nessemble: could not read `{}`: {e}", files[0].display());
            RETURN_EPERM
        }
    }
}

/// `--write`: rewrite each file that changes in place, reporting the path.
fn write_mode(files: &[PathBuf]) -> u8 {
    let mut code = RETURN_OK;
    for file in files {
        let source = match std::fs::read_to_string(file) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("nessemble: could not read `{}`: {e}", file.display());
                code = RETURN_EPERM;
                continue;
            }
        };
        let out = formatted(&source);
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
fn check_mode(files: &[PathBuf]) -> u8 {
    let mut unformatted = 0usize;
    let mut code = RETURN_OK;
    for file in files {
        let source = match std::fs::read_to_string(file) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("nessemble: could not read `{}`: {e}", file.display());
                code = RETURN_EPERM;
                continue;
            }
        };
        if formatted(&source) != source {
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

/// Recursively collect files with the formatted extension under `dir`.
fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some(FORMATTED_EXT) {
            out.push(path);
        }
    }
}

/// Parse `format`'s own arguments: `-w`/`--write`, `-c`/`--check`,
/// `-h`/`--help`, `--` (end of options), and path positionals.
fn parse(args: &[String]) -> Parsed {
    let mut opts = Opts::default();
    let mut rest_are_paths = false;
    for arg in args {
        if rest_are_paths {
            opts.paths.push(arg.clone());
            continue;
        }
        match arg.as_str() {
            "--" => rest_are_paths = true,
            "-w" | "--write" => opts.write = true,
            "-c" | "--check" => opts.check = true,
            "-h" | "--help" => return Parsed::Usage,
            other if other.starts_with('-') && other.len() > 1 => return Parsed::Usage,
            _ => opts.paths.push(arg.clone()),
        }
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
         \x20 -w, --write     rewrite files in place (required for a directory)\n\
         \x20 -c, --check     exit non-zero if any file is not formatted; write nothing\n\
         \x20 -h, --help      print this message\n"
    )
}
