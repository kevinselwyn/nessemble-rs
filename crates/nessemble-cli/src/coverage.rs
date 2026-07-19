//! `coverage` subcommand: report **runtime execution coverage** of an assembled
//! ROM against a CDL (Code/Data Logger) capture from an emulator.
//!
//! `nessemble coverage <infile.asm> --cdl <file.cdl>` assembles the source with
//! a byte-exact source map, classifies each PRG-emitting line against the merged
//! CDL(s), and writes JSON and/or LCOV reports (plus a one-line stdout summary).
//! It never writes a ROM.
//!
//! FCEUX and Mesen flat-mask CDLs are supported (`--emulator`, default `fceux`);
//! the two are the same size but bit-incompatible, so the emulator is explicit.
//! `BizHawk`'s container format is a later phase.

use std::path::{Path, PathBuf};

use clap::{Args, ValueEnum};
use nessemble_core::coverage::{build_report, CdlSource, FlatMaskCdl};
use nessemble_core::{assemble_file_with, AssembleError, Options};

use crate::custom;
use crate::{RETURN_EPERM, RETURN_OK};

/// Bytes per PRG bank (16 KiB) and CHR bank (8 KiB).
const PRG_BANK: usize = 0x4000;
const CHR_BANK: usize = 0x2000;

/// Which emulator's flat-mask CDL to read. FCEUX and Mesen share bits 0–1 but
/// diverge above them, so the format must be stated (there is no reliable
/// auto-detect between two same-size masks).
#[derive(Clone, Copy, ValueEnum)]
enum Emulator {
    Fceux,
    Mesen,
}

/// Which report(s) to emit.
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Format {
    Json,
    Lcov,
    All,
}

/// Parsed `coverage` options.
#[derive(Args)]
pub struct CoverageArgs {
    /// assembly source to assemble
    #[arg(value_name = "infile.asm")]
    infile: String,

    /// CDL capture to read (repeatable; multiple files are merged by bitwise OR)
    #[arg(long = "cdl", value_name = "file.cdl", required = true)]
    cdl: Vec<String>,

    /// emulator CDL format
    #[arg(long, value_name = "name", default_value = "fceux")]
    emulator: Emulator,

    /// report format
    #[arg(long, value_name = "fmt", default_value = "all")]
    format: Format,

    /// output file (single format), or directory (for `all`); defaults to cwd
    #[arg(long, value_name = "path")]
    out: Option<String>,

    /// use custom pseudo-instruction functions
    #[arg(short = 'p', long, value_name = "pseudo.txt")]
    pseudo: Option<String>,

    /// also report line coverage for the `-p` Rhai scripts
    #[arg(long)]
    scripts: bool,
}

/// Run `coverage` with its parsed options, returning the process exit code.
pub fn run(args: &CoverageArgs) -> u8 {
    // Assemble in NES mode with source-map recording. Coverage is defined over
    // PRG/CHR banks, so a non-NES assembly has nothing to report.
    let options = Options {
        nes: true,
        source_map: true,
        ..Options::default()
    };
    // When `--scripts` is requested (and supported), the resolver also records
    // Rhai line coverage into `scripts_cov`, which outlives the assembly.
    #[cfg(feature = "coverage")]
    let scripts_cov = args.scripts.then(|| {
        std::rc::Rc::new(std::cell::RefCell::new(
            nessemble_script::coverage::ScriptCoverage::new(),
        ))
    });
    #[cfg(feature = "coverage")]
    let resolver = match &scripts_cov {
        Some(cov) => custom::build_resolver_with_coverage(args.pseudo.as_deref(), cov.clone()),
        None => custom::build_resolver(args.pseudo.as_deref()),
    };
    #[cfg(not(feature = "coverage"))]
    let resolver = {
        if args.scripts {
            eprintln!(
                "nessemble: this build lacks Rhai script-coverage support; ignoring --scripts"
            );
        }
        custom::build_resolver(args.pseudo.as_deref())
    };

    let assembly = match assemble_file_with(Path::new(&args.infile), &options, resolver) {
        Ok(a) => a,
        Err(AssembleError(d)) => {
            eprintln!("nessemble: {}: line {}: {}", args.infile, d.line, d.message);
            return RETURN_EPERM;
        }
    };

    let Some(source_map) = assembly.source_map else {
        eprintln!("nessemble: coverage requires an iNES ROM (assemble with `-f nes`)");
        return RETURN_EPERM;
    };

    // PRG/CHR sizes come from the assembled iNES header (bytes 4 and 5), which
    // also fixes the CDL's PRG/CHR boundary and its expected total size.
    let Some(&prg_banks) = assembly.rom.get(4) else {
        eprintln!("nessemble: assembled output is not an iNES ROM");
        return RETURN_EPERM;
    };
    let chr_banks = assembly.rom.get(5).copied().unwrap_or(0);
    let prg_len = prg_banks as usize * PRG_BANK;
    let chr_len = chr_banks as usize * CHR_BANK;

    let cdl_bytes = match load_and_merge_cdls(&args.cdl, prg_len, chr_len) {
        Ok(b) => b,
        Err(code) => return code,
    };

    let cdl: Box<dyn CdlSource> = match match args.emulator {
        Emulator::Fceux => FlatMaskCdl::fceux(cdl_bytes, prg_len),
        Emulator::Mesen => FlatMaskCdl::mesen(cdl_bytes, prg_len),
    } {
        Ok(c) => Box::new(c),
        Err(e) => {
            eprintln!("nessemble: {e}");
            return RETURN_EPERM;
        }
    };

    #[cfg(feature = "coverage")]
    let mut report = build_report(&source_map, cdl.as_ref());
    #[cfg(not(feature = "coverage"))]
    let report = build_report(&source_map, cdl.as_ref());

    // Fold in Rhai script coverage (each project script as its own file), then
    // re-sort so scripts and asm files interleave by path.
    #[cfg(feature = "coverage")]
    if let Some(cov) = &scripts_cov {
        let cov = cov.borrow();
        for (path, rows) in cov.files() {
            report
                .files
                .push(nessemble_core::coverage::FileCoverage::from_line_hits(
                    path.display().to_string(),
                    rows,
                ));
        }
        report.files.sort_by(|a, b| a.path.cmp(&b.path));
    }

    if let Err(code) = write_reports(&report, args.format, args.out.as_deref()) {
        return code;
    }

    // One-line human summary regardless of the machine format(s) written.
    let t = report.totals();
    let pct = if t.total() > 0 {
        f64::from(t.covered()) / f64::from(t.total()) * 100.0
    } else {
        0.0
    };
    println!("coverage: {}/{} lines ({pct:.1}%)", t.covered(), t.total());
    RETURN_OK
}

/// Read every `--cdl` file, verify each is the expected size, and OR them into
/// one mask. The expected size is the header-less ROM image (`prg_len + chr_len`)
/// — the strongest same-ROM check a flat mask allows, since it carries no ROM
/// identity of its own.
fn load_and_merge_cdls(paths: &[String], prg_len: usize, chr_len: usize) -> Result<Vec<u8>, u8> {
    let expected = prg_len + chr_len;
    let mut merged = vec![0u8; expected];
    for path in paths {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("nessemble: could not read CDL `{path}`: {e}");
                return Err(RETURN_EPERM);
            }
        };
        if bytes.len() != expected {
            eprintln!(
                "nessemble: CDL `{path}` is {} bytes but this ROM's PRG+CHR is {expected} bytes \
                 (PRG {prg_len} + CHR {chr_len}); it must come from the ROM this source assembles \
                 to (equal sizes still do not guarantee the same build)",
                bytes.len()
            );
            return Err(RETURN_EPERM);
        }
        for (m, b) in merged.iter_mut().zip(&bytes) {
            *m |= *b;
        }
    }
    Ok(merged)
}

/// Write the requested report format(s). For `all`, `out` is a directory (cwd by
/// default) receiving `coverage.json` + `coverage.lcov`; for a single format,
/// `out` is the output file (defaulting to `coverage.<ext>` in cwd).
fn write_reports(
    report: &nessemble_core::coverage::CoverageReport,
    format: Format,
    out: Option<&str>,
) -> Result<(), u8> {
    let write = |path: &Path, contents: String| -> Result<(), u8> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            if let Err(e) = std::fs::create_dir_all(parent) {
                eprintln!("nessemble: could not create `{}`: {e}", parent.display());
                return Err(RETURN_EPERM);
            }
        }
        if let Err(e) = std::fs::write(path, contents) {
            eprintln!("nessemble: could not write `{}`: {e}", path.display());
            return Err(RETURN_EPERM);
        }
        eprintln!("wrote {}", path.display());
        Ok(())
    };

    match format {
        Format::Json => {
            let path = out.map_or_else(|| PathBuf::from("coverage.json"), PathBuf::from);
            write(&path, report.to_json())?;
        }
        Format::Lcov => {
            let path = out.map_or_else(|| PathBuf::from("coverage.lcov"), PathBuf::from);
            write(&path, report.to_lcov())?;
        }
        Format::All => {
            let dir = out.map_or_else(|| PathBuf::from("."), PathBuf::from);
            write(&dir.join("coverage.json"), report.to_json())?;
            write(&dir.join("coverage.lcov"), report.to_lcov())?;
        }
    }
    Ok(())
}
