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
//!
//! Source may exclude lines from the report with the
//! `; @nessemble-coverage-ignore-next-line` and
//! `; @nessemble-coverage-ignore start` / `end` comment directives (`//` in Rhai
//! scripts); `--no-ignore` reports every line regardless.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use clap::{Args, ValueEnum};
use nessemble_core::coverage::{
    build_report_with_ignores, resolve_ignores, CdlSource, CoverageIgnores, FlatMaskCdl,
};
use nessemble_core::tooling;
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

    /// project root for `@/`-relative paths (default: nearest `.nessemblerc`, or
    /// the input file's directory)
    #[arg(long, value_name = "dir")]
    root: Option<String>,

    /// also report line coverage for the `-p` Rhai scripts
    #[arg(long)]
    scripts: bool,

    /// report every line, ignoring `@nessemble-coverage-ignore…` directives
    #[arg(long = "no-ignore")]
    no_ignore: bool,
}

/// Run `coverage` with its parsed options, returning the process exit code.
pub fn run(args: &CoverageArgs) -> u8 {
    // Assemble in NES mode with source-map recording. Coverage is defined over
    // PRG/CHR banks, so a non-NES assembly has nothing to report.
    let project_root = match crate::resolve_root_flag(args.root.as_deref()) {
        Ok(root) => root,
        Err(code) => return code,
    };
    let options = Options {
        nes: true,
        source_map: true,
        project_root,
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
        // Coverage needs every script to really execute (see `custom`), so the
        // persistent cache is bypassed on this path.
        None => custom::build_resolver(args.pseudo.as_deref(), false),
    };
    #[cfg(not(feature = "coverage"))]
    let resolver = {
        if args.scripts {
            eprintln!(
                "nessemble: this build lacks Rhai script-coverage support; ignoring --scripts"
            );
        }
        custom::build_resolver(args.pseudo.as_deref(), false)
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

    // Collect the `@nessemble-coverage-ignore…` exclusions from every source
    // file the assembly actually emitted from. Core does no file I/O, so the
    // reading and scanning happen here.
    let ignores = if args.no_ignore {
        CoverageIgnores::default()
    } else {
        collect_ignores(&source_map)
    };

    let mut report = build_report_with_ignores(&source_map, cdl.as_ref(), &ignores);

    // Fold in Rhai script coverage (each project script as its own file). The
    // same directives apply, written as `//` comments.
    #[cfg(feature = "coverage")]
    if let Some(cov) = &scripts_cov {
        let cov = cov.borrow();
        for (path, rows) in cov.files() {
            let display = path.display().to_string();
            let mut script_ignores = CoverageIgnores::default();
            if !args.no_ignore {
                collect_script_ignores(path, &display, &mut script_ignores);
            }
            let file = nessemble_core::coverage::FileCoverage::from_line_hits_with_ignores(
                display,
                rows,
                &script_ignores,
            );
            if file.lines.is_empty() && file.ignored > 0 {
                report.ignored_files += 1;
                continue;
            }
            report.files.push(file);
        }
    }

    // Rewrite every file path so `SF:` records are uniformly rooted: relative to
    // the current directory when the file is under it (clean, no `../..`, and
    // `genhtml report.lcov` resolves them from the project root), else absolute.
    // Then sort so the report order matches the displayed paths.
    let cwd = std::env::current_dir().ok();
    for file in &mut report.files {
        file.path = relative_to(cwd.as_deref(), &file.path);
    }
    report.files.sort_by(|a, b| a.path.cmp(&b.path));

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
    let mut summary = format!("coverage: {}/{} lines ({pct:.1}%)", t.covered(), t.total());
    // Exclusions are reported rather than silently vanishing, so an over-broad
    // ignore region shows up as a jump in this number.
    if t.ignored > 0 || t.ignored_files > 0 {
        let mut parts = Vec::new();
        if t.ignored > 0 {
            parts.push(format!("{} line{}", t.ignored, plural(t.ignored)));
        }
        if t.ignored_files > 0 {
            parts.push(format!(
                "{} file{}",
                t.ignored_files,
                plural(t.ignored_files)
            ));
        }
        let _ = write!(summary, " — {} ignored", parts.join(", "));
    }
    println!("{summary}");
    RETURN_OK
}

fn plural(n: u32) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// Read every source file the assembly emitted from and collect its
/// `@nessemble-coverage-ignore…` exclusions.
///
/// Keys are the source map's canonical paths, so exclusion happens before the
/// display-path rewrite. A file that cannot be re-read (deleted or renamed
/// mid-run) contributes no exclusions and a warning — a coverage report is never
/// blocked by a missing comment.
fn collect_ignores(source_map: &nessemble_core::SourceMap) -> CoverageIgnores {
    let mut ignores = CoverageIgnores::default();
    let mut seen: Vec<&str> = Vec::new();
    for span in &source_map.spans {
        let path: &str = &span.file;
        if seen.contains(&path) {
            continue;
        }
        seen.push(path);
        let Ok(text) = std::fs::read_to_string(path) else {
            eprintln!("nessemble: could not re-read `{path}` for coverage directives; ignoring it");
            continue;
        };
        let directives = tooling::scan_directives(&text);
        if directives.is_empty() {
            continue;
        }
        let significant = tooling::significant_lines(&text);
        resolve_ignores(
            path,
            &directives,
            &|line| next_significant(&significant, line),
            &mut ignores,
        );
    }
    ignores
}

/// Collect a Rhai script's exclusions. Scripts comment with `//`, and a
/// significant line is any non-blank line that is not a `//` comment.
#[cfg(feature = "coverage")]
fn collect_script_ignores(path: &Path, key: &str, ignores: &mut CoverageIgnores) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let directives = tooling::scan_line_comment_directives(&text, "//");
    if directives.is_empty() {
        return;
    }
    let significant: Vec<bool> = text
        .lines()
        .map(|l| {
            let t = l.trim_start();
            !t.is_empty() && !t.starts_with("//")
        })
        .collect();
    resolve_ignores(
        key,
        &directives,
        &|line| next_significant(&significant, line),
        ignores,
    );
}

/// The first significant line strictly after 1-based `line`, given a
/// significance flag per line (index `i` is line `i + 1`).
fn next_significant(significant: &[bool], line: u32) -> Option<u32> {
    significant
        .iter()
        .enumerate()
        .skip(line as usize)
        .find(|(_, &sig)| sig)
        .map(|(idx, _)| (idx + 1) as u32)
}

/// Present a source-file path for the report: relative to `base` (the current
/// directory) when the file sits under it, otherwise unchanged. The source map
/// gives canonical absolute paths, so this yields clean, `../..`-free relative
/// paths for in-tree files while leaving out-of-tree files absolute.
fn relative_to(base: Option<&Path>, path: &str) -> String {
    if let Some(base) = base {
        if let Ok(rel) = Path::new(path).strip_prefix(base) {
            return rel.display().to_string();
        }
    }
    path.to_string()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_to_strips_the_base_and_leaves_outsiders_alone() {
        let base = Path::new("/proj/root");
        // Under the base → clean relative path, no `../..`.
        assert_eq!(
            relative_to(Some(base), "/proj/root/src/main.asm"),
            "src/main.asm"
        );
        assert_eq!(
            relative_to(Some(base), "/proj/root/inc/tbl.asm"),
            "inc/tbl.asm"
        );
        // Outside the base → left absolute.
        assert_eq!(
            relative_to(Some(base), "/elsewhere/x.asm"),
            "/elsewhere/x.asm"
        );
        // No base (couldn't read cwd) → unchanged.
        assert_eq!(
            relative_to(None, "/proj/root/src/main.asm"),
            "/proj/root/src/main.asm"
        );
    }
}
