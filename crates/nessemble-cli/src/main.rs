//! `nessemble` command-line interface.
//!
//! Phase 0 wires up argument parsing and the assemble entry point. The full CLI
//! surface (config/init/reference/scripts, exact help/usage parity) lands in
//! Phase 6, and the assembler itself in Phases 1–5. Out-of-scope options from
//! the reference tool (`-d`/`--disassemble`, `-R`/`--reassemble`,
//! `-s`/`--simulate`, and the registry/user commands) are intentionally absent.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use nessemble_core::{assemble, AssembleError, Options};

/// The source name reported in diagnostics (basename, like the reference tool).
fn source_name(input: &Option<PathBuf>) -> String {
    match input {
        Some(path) => path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("stdin")
            .to_string(),
        None => "stdin".to_string(),
    }
}

/// Return codes mirroring the reference tool (`RETURN_OK` / `RETURN_EPERM`).
const RETURN_OK: u8 = 0;
const RETURN_EPERM: u8 = 1;

#[derive(Parser, Debug)]
#[command(
    name = "nessemble",
    bin_name = "nessemble",
    version,
    about = "A 6502 assembler for the Nintendo Entertainment System",
    disable_help_subcommand = true
)]
struct Cli {
    /// Input assembly file (reads from stdin if omitted).
    input: Option<PathBuf>,

    /// Output file (`-` for stdout).
    #[arg(short = 'o', long = "output", default_value = "-", value_name = "FILE")]
    output: String,

    /// Output format (`nes` for an iNES ROM).
    #[arg(short = 'f', long = "format", value_name = "FORMAT")]
    format: Option<String>,

    /// Empty-fill byte, e.g. `0xFF` (hexadecimal).
    #[arg(short = 'e', long = "empty", value_name = "HEX")]
    empty: Option<String>,

    /// Allow undocumented ("illegal") opcodes.
    #[arg(short = 'u', long = "undocumented")]
    undocumented: bool,

    /// Only check the input for errors; produce no output.
    #[arg(short = 'c', long = "check")]
    check: bool,

    /// Emit code-coverage data.
    #[arg(short = 'C', long = "coverage")]
    coverage: bool,

    /// Write a list file to FILE.
    #[arg(short = 'l', long = "list", value_name = "FILE")]
    list: Option<String>,

    /// Custom pseudo-op script file.
    #[arg(short = 'p', long = "pseudo", value_name = "FILE")]
    pseudo: Option<String>,

    /// Print license information and exit.
    #[arg(long = "license")]
    license: bool,
}

fn parse_hex_byte(s: &str) -> Result<u8, String> {
    let trimmed = s.trim_start_matches("0x").trim_start_matches("0X");
    u8::from_str_radix(trimmed, 16).map_err(|_| format!("invalid hexadecimal byte: `{s}`"))
}

fn read_input(input: &Option<PathBuf>) -> std::io::Result<String> {
    match input {
        Some(path) => std::fs::read_to_string(path),
        None => {
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            Ok(buf)
        }
    }
}

fn write_output(output: &str, bytes: &[u8]) -> std::io::Result<()> {
    if output == "-" {
        std::io::stdout().write_all(bytes)
    } else {
        std::fs::write(output, bytes)
    }
}

fn run(cli: Cli) -> ExitCode {
    if cli.license {
        print_license();
        return ExitCode::from(RETURN_OK);
    }

    let mut options = Options {
        nes: matches!(cli.format.as_deref(), Some(f) if f.eq_ignore_ascii_case("nes")),
        undocumented: cli.undocumented,
        ..Options::default()
    };

    if let Some(empty) = cli.empty.as_deref() {
        match parse_hex_byte(empty) {
            Ok(b) => options.empty_byte = b,
            Err(e) => {
                eprintln!("nessemble: {e}");
                return ExitCode::from(RETURN_EPERM);
            }
        }
    }

    let source = match read_input(&cli.input) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("nessemble: could not read input: {e}");
            return ExitCode::from(RETURN_EPERM);
        }
    };

    match assemble(&source, &options) {
        Ok(rom) => {
            if cli.check {
                println!("No errors");
                return ExitCode::from(RETURN_OK);
            }
            if let Err(e) = write_output(&cli.output, &rom) {
                eprintln!("nessemble: could not write output: {e}");
                return ExitCode::from(RETURN_EPERM);
            }
            ExitCode::from(RETURN_OK)
        }
        Err(AssembleError::Diagnostic(d)) => {
            // Matches the reference format:
            // `Error in `<file>` on line <line>: <message>`
            eprintln!(
                "Error in `{}` on line {}: {}",
                source_name(&cli.input),
                d.line,
                d.message
            );
            ExitCode::from(RETURN_EPERM)
        }
    }
}

fn print_license() {
    println!("nessemble-rs — GPL-3.0-or-later");
    println!(
        "A fresh Rust reimplementation of nessemble (targeting v{}).",
        nessemble_core::REFERENCE_VERSION
    );
    println!("See the COPYING file for full license text.");
}

fn main() -> ExitCode {
    run(Cli::parse())
}
