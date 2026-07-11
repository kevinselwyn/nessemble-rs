//! `nessemble` command-line interface.
//!
//! Phase 6 completes the in-scope CLI: the assemble/check/coverage flags plus
//! the `init`, `config`, `reference`, and `scripts` subcommands, matching the
//! reference tool's usage/version/license text and exit codes. The out-of-scope
//! options (`-d`/`-R`/`-s`/`-r`) and commands (registry/package/user) are
//! omitted entirely — they are not parsed and appear nowhere in help.

mod config;
mod custom;
mod home;
mod init;
mod reference;
mod scripts;
mod usage;

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use nessemble_core::{
    assemble_file_with, assemble_with, render_coverage, render_list_file, AssembleError, Options,
};
use nessemble_i18n::t;

/// Return codes mirroring the reference tool.
const RETURN_OK: u8 = 0;
const RETURN_EPERM: u8 = 1;
const RETURN_USAGE: u8 = 129;

/// Parsed assemble-mode options plus any positional arguments.
#[derive(Default)]
struct Args {
    output: Option<String>,
    format: Option<String>,
    empty: Option<String>,
    undocumented: bool,
    check: bool,
    coverage: bool,
    list: Option<String>,
    pseudo: Option<String>,
    positionals: Vec<String>,
}

/// The outcome of parsing: either assemble/dispatch, or an early exit whose
/// output has already been printed.
enum Parsed {
    Run(Args),
    Exit(u8),
}

fn main() -> ExitCode {
    // Load any translator-provided locales (`~/.nessemble/locales/<lang>.ftl`)
    // so `NESSEMBLE_LANG` can select them, falling back to the embedded en-US.
    if let Some(dir) = home::config_dir() {
        nessemble_i18n::load_locale_dir(&dir.join("locales"));
    }

    let argv: Vec<String> = std::env::args().collect();
    let exec = argv
        .first()
        .cloned()
        .unwrap_or_else(|| "nessemble".to_string());

    let args = match parse(&argv, &exec) {
        Parsed::Run(a) => a,
        Parsed::Exit(code) => return ExitCode::from(code),
    };

    ExitCode::from(dispatch(&args))
}

/// Parse `argv` in the getopt permuting style: options may appear before or
/// after positionals. `-h`/`-v`/`-L` print immediately and stop.
fn parse(argv: &[String], exec: &str) -> Parsed {
    let mut args = Args::default();
    let mut i = 1;
    while i < argv.len() {
        let a = &argv[i];
        if a == "--" {
            args.positionals.extend(argv[i + 1..].iter().cloned());
            break;
        } else if let Some(long) = a.strip_prefix("--") {
            if let Some(p) = parse_long(long, argv, &mut i, &mut args, exec) {
                return p;
            }
        } else if a.starts_with('-') && a.len() > 1 {
            if let Some(p) = parse_short(a, argv, &mut i, &mut args, exec) {
                return p;
            }
        } else {
            args.positionals.push(a.clone());
            i += 1;
        }
    }
    Parsed::Run(args)
}

/// Handle a `--long[=value]` option. Returns `Some(Parsed::Exit)` for an
/// immediate action (help/version/license) or a usage error.
fn parse_long(
    long: &str,
    argv: &[String],
    i: &mut usize,
    args: &mut Args,
    exec: &str,
) -> Option<Parsed> {
    let (name, inline) = match long.split_once('=') {
        Some((n, v)) => (n, Some(v.to_string())),
        None => (long, None),
    };
    *i += 1;
    match name {
        "help" => return Some(action(&usage::usage(exec))),
        "version" => return Some(action(&usage::version())),
        "license" => return Some(action(&usage::license())),
        "undocumented" => args.undocumented = true,
        "check" => args.check = true,
        "coverage" => args.coverage = true,
        "output" => return take_value(inline, argv, i, exec, |v| args.output = Some(v)),
        "format" => return take_value(inline, argv, i, exec, |v| args.format = Some(v)),
        "empty" => return take_value(inline, argv, i, exec, |v| args.empty = Some(v)),
        "list" => return take_value(inline, argv, i, exec, |v| args.list = Some(v)),
        "pseudo" => return take_value(inline, argv, i, exec, |v| args.pseudo = Some(v)),
        _ => return Some(usage_error(exec)),
    }
    None
}

/// Handle a short-option cluster like `-cu` or `-o value` / `-ovalue`.
fn parse_short(
    cluster: &str,
    argv: &[String],
    i: &mut usize,
    args: &mut Args,
    exec: &str,
) -> Option<Parsed> {
    let bytes = cluster.as_bytes();
    let mut j = 1; // skip leading '-'
    *i += 1;
    while j < bytes.len() {
        let c = bytes[j] as char;
        match c {
            'h' => return Some(action(&usage::usage(exec))),
            'v' => return Some(action(&usage::version())),
            'L' => return Some(action(&usage::license())),
            'u' => args.undocumented = true,
            'c' => args.check = true,
            'C' => args.coverage = true,
            'o' | 'f' | 'e' | 'l' | 'p' => {
                // The remainder of the cluster is the value, else the next arg.
                let rest = &cluster[j + 1..];
                let value = if !rest.is_empty() {
                    Some(rest.to_string())
                } else if *i < argv.len() {
                    let v = argv[*i].clone();
                    *i += 1;
                    Some(v)
                } else {
                    None
                };
                let Some(value) = value else {
                    return Some(usage_error(exec));
                };
                match c {
                    'o' => args.output = Some(value),
                    'f' => args.format = Some(value),
                    'e' => args.empty = Some(value),
                    'l' => args.list = Some(value),
                    'p' => args.pseudo = Some(value),
                    _ => unreachable!(),
                }
                return None; // value consumed the rest of the cluster
            }
            _ => return Some(usage_error(exec)),
        }
        j += 1;
    }
    None
}

/// Resolve an option value from an inline `=value` or the next argument.
fn take_value(
    inline: Option<String>,
    argv: &[String],
    i: &mut usize,
    exec: &str,
    mut set: impl FnMut(String),
) -> Option<Parsed> {
    let value = match inline {
        Some(v) => Some(v),
        None if *i < argv.len() => {
            let v = argv[*i].clone();
            *i += 1;
            Some(v)
        }
        None => None,
    };
    match value {
        Some(v) => {
            set(v);
            None
        }
        None => Some(usage_error(exec)),
    }
}

/// Print `text` to stdout and exit with the usage return code (129), matching
/// the reference's `-h`/`-v`/`-L`.
fn action(text: &str) -> Parsed {
    print!("{text}");
    Parsed::Exit(RETURN_USAGE)
}

fn usage_error(exec: &str) -> Parsed {
    print!("{}", usage::usage(exec));
    Parsed::Exit(RETURN_USAGE)
}

/// Dispatch a parsed command line: a leading subcommand, or assemble mode.
fn dispatch(args: &Args) -> u8 {
    if let Some(first) = args.positionals.first() {
        match first.as_str() {
            "init" => return init::run(&args.positionals[1..]),
            "scripts" => return scripts::run(),
            "reference" => {
                let (out, code) = reference::run(
                    args.positionals.get(1).map(String::as_str),
                    args.positionals.get(2).map(String::as_str),
                );
                print!("{out}");
                return code;
            }
            "config" => return run_config(&args.positionals[1..]),
            _ => {}
        }
    }
    assemble_mode(args)
}

/// `config` / `config <key>` / `config <key> <val>`.
fn run_config(rest: &[String]) -> u8 {
    let result = match rest {
        [] => config::list(),
        [key] => config::get(key),
        [key, val, ..] => config::set(key, val).map(|()| None),
    };
    match result {
        Ok(Some(text)) => {
            println!("{text}");
            RETURN_OK
        }
        Ok(None) => RETURN_OK,
        Err(e) => {
            eprintln!("nessemble: {e}");
            RETURN_EPERM
        }
    }
}

/// Assemble the input (a positional file, or stdin) into the output.
fn assemble_mode(args: &Args) -> u8 {
    let mut options = Options {
        nes: matches!(args.format.as_deref(), Some(f) if f.eq_ignore_ascii_case("nes")),
        undocumented: args.undocumented,
        ..Options::default()
    };

    if let Some(empty) = args.empty.as_deref() {
        options.empty_byte = parse_hex_byte(empty);
    }

    let input: Option<PathBuf> = args.positionals.first().map(PathBuf::from);
    let result = match &input {
        Some(path) => assemble_file_with(
            path,
            &options,
            custom::build_resolver(args.pseudo.as_deref()),
        ),
        None => match read_stdin() {
            Ok(source) => assemble_with(
                &source,
                &options,
                custom::build_resolver(args.pseudo.as_deref()),
            ),
            Err(e) => {
                eprintln!("nessemble: could not read input: {e}");
                return RETURN_EPERM;
            }
        },
    };

    match result {
        Ok(assembly) => {
            for w in &assembly.warnings {
                eprintln!(
                    "{}",
                    t!(
                        "warning-line",
                        file = w.file,
                        line = w.line,
                        message = w.message
                    )
                );
            }
            if args.check {
                println!("{}", t!("no-errors"));
                return RETURN_OK;
            }
            let output = args.output.as_deref().unwrap_or("-");
            if let Err(e) = write_output(output, &assembly.rom) {
                eprintln!("nessemble: could not write output: {e}");
                return RETURN_EPERM;
            }
            if let Some(list) = args.list.as_deref() {
                if let Err(e) = std::fs::write(list, render_list_file(&assembly.symbols)) {
                    eprintln!("nessemble: could not write list file: {e}");
                    return RETURN_EPERM;
                }
            }
            // Coverage is reported only for an iNES ROM written to a file,
            // matching the reference guard.
            if args.coverage && output != "-" {
                if let Some(report) = &assembly.coverage {
                    print!("{}", render_coverage(report));
                }
            }
            RETURN_OK
        }
        Err(AssembleError::Diagnostic(d)) => {
            eprintln!(
                "{}",
                t!(
                    "error-line",
                    file = d.file,
                    line = d.line,
                    message = d.message
                )
            );
            RETURN_EPERM
        }
    }
}

/// Parse a hex byte for `-e`, matching the reference `hex2int` (invalid → 0).
fn parse_hex_byte(s: &str) -> u8 {
    let trimmed = s.trim_start_matches("0x").trim_start_matches("0X");
    u8::from_str_radix(trimmed, 16).unwrap_or(0)
}

fn read_stdin() -> std::io::Result<String> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    Ok(buf)
}

fn write_output(output: &str, bytes: &[u8]) -> std::io::Result<()> {
    if output == "-" {
        std::io::stdout().write_all(bytes)
    } else {
        std::fs::write(output, bytes)
    }
}
