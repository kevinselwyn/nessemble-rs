//! Core 6502/NES assembler for `nessemble-rs`.
//!
//! Phase 2 implements the non-iNES-output assembler: a hand-written lexer,
//! recursive-descent parser, and a two-pass assembler that reproduces the
//! reference tool's instruction encoding, symbol handling, expression
//! semantics, and error messages. Full iNES ROM output (header/banking/CHR)
//! arrives in Phase 3.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

pub use nessemble_isa as isa;

mod assemble;
pub mod ast;
pub mod coverage;
mod lexer;
mod parse;
mod preprocess;
pub mod tooling;

pub use assemble::{CoverageReport, CustomResolver, Diag, ListSymbol, SourceMap, SourceSpan};
pub use preprocess::FileOverlay;

/// The reference implementation version this crate targets for output parity.
pub const REFERENCE_VERSION: &str = "1.1.1";

/// Options controlling an assembly run.
#[derive(Debug, Clone)]
pub struct Options {
    /// Emit an iNES (`.nes`) header/layout (`-f nes`) — full output is Phase 3.
    pub nes: bool,
    /// Allow undocumented ("illegal") opcodes (`-u`).
    pub undocumented: bool,
    /// Byte used to fill unwritten ROM regions (`-e`).
    pub empty_byte: u8,
    /// Record a byte-exact [`SourceMap`] during assembly (for the `coverage`
    /// tooling path). Off by default; enabling it does not change the assembled
    /// bytes, only whether the map is collected and exposed on [`Assembly`].
    pub source_map: bool,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            nes: false,
            undocumented: false,
            empty_byte: 0xFF,
            source_map: false,
        }
    }
}

/// The error produced by a failed assembly: a single diagnostic with a source
/// line and message (reference-compatible text).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssembleError(pub Diag);

impl std::fmt::Display for AssembleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "line {}: {}", self.0.line, self.0.message)
    }
}

impl std::error::Error for AssembleError {}

/// The result of a successful assembly: output bytes plus any warnings
/// (emitted, in source order, exactly as the reference tool would).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assembly {
    /// Final output bytes (raw ROM, or a full iNES file in NES mode).
    pub rom: Vec<u8>,
    /// Warnings collected during assembly.
    pub warnings: Vec<Diag>,
    /// Defined symbols, for rendering the list file (`-l`).
    pub symbols: Vec<ListSymbol>,
    /// Per-bank write coverage (`-C`), or `None` when not in iNES mode.
    pub coverage: Option<CoverageReport>,
    /// Byte-exact source map, or `None` unless [`Options::source_map`] was set.
    pub source_map: Option<SourceMap>,
}

/// Render the coverage summary (`-C`) exactly as the reference `get_coverage`:
/// one `PRG XX:` / `CHR XX:` line per bank with `covered/total` counts.
#[must_use]
pub fn render_coverage(report: &CoverageReport) -> String {
    let mut out = String::new();
    for (i, covered) in report.prg.iter().enumerate() {
        let _ = writeln!(
            out,
            "PRG {:02X}: {:>5}/{:<5}",
            i, covered, report.prg_bank_size
        );
    }
    for (i, covered) in report.chr.iter().enumerate() {
        let _ = writeln!(
            out,
            "CHR {:02X}: {:>5}/{:<5}",
            i, covered, report.chr_bank_size
        );
    }
    out
}

/// Render the list-file (`-l`) contents from the defined symbols, mirroring the
/// reference `output_list` format: a `[constants]` section (`VALUE = NAME`) and
/// a `[labels]` section (`BANK/VALUE = NAME`), each sorted lexicographically by
/// its formatted line, separated by a blank line when both are present.
#[must_use]
pub fn render_list_file(symbols: &[ListSymbol]) -> String {
    let mut constants: Vec<String> = symbols
        .iter()
        .filter(|s| !s.label)
        .map(|s| format!("{:04X} = {}", s.value as u32, s.name))
        .collect();
    let mut labels: Vec<String> = symbols
        .iter()
        .filter(|s| s.label)
        .map(|s| format!("{:02X}/{:04X} = {}", s.bank as u32, s.value as u32, s.name))
        .collect();
    constants.sort();
    labels.sort();

    let mut out = String::new();
    if !constants.is_empty() {
        out.push_str("[constants]\n");
        for line in &constants {
            out.push_str(line);
            out.push('\n');
        }
    }
    if !labels.is_empty() {
        if !constants.is_empty() {
            out.push('\n');
        }
        out.push_str("[labels]\n");
        for line in &labels {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Assemble source text into output bytes.
///
/// Top-level includes and filename-based directives resolve relative to the
/// current working directory (nested includes resolve relative to the file that
/// contains them), and the source is reported as `stdin` in diagnostics. Custom
/// pseudo-ops (`.foo`) are unresolved; use [`assemble_with`] to supply a
/// resolver.
pub fn assemble(source: &str, options: &Options) -> Result<Assembly, AssembleError> {
    assemble_with(source, options, default_custom_resolver())
}

/// Assemble source text with a custom pseudo-op resolver.
pub fn assemble_with(
    source: &str,
    options: &Options,
    custom: CustomResolver,
) -> Result<Assembly, AssembleError> {
    let base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    assemble_impl(source, options, base, "stdin", custom)
}

/// Assemble the file at `path`, resolving includes and filename-based
/// directives relative to each file's own directory (the top-level file's
/// directory for its own directives, and each included file's directory for
/// the directives it contains).
pub fn assemble_file(path: &Path, options: &Options) -> Result<Assembly, AssembleError> {
    assemble_file_with(path, options, default_custom_resolver())
}

/// Assemble the file at `path` with a custom pseudo-op resolver.
pub fn assemble_file_with(
    path: &Path,
    options: &Options,
    custom: CustomResolver,
) -> Result<Assembly, AssembleError> {
    let source = std::fs::read_to_string(path).map_err(|e| {
        AssembleError(Diag {
            file: display_name(path),
            line: 0,
            message: format!("Could not open `{}`: {e}", path.display()),
        })
    })?;
    let base = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    assemble_impl(&source, options, base, &display_name(path), custom)
}

/// Assemble in-memory `source` as though it were the file at `path`: includes
/// and media resolve relative to `path`'s directory and diagnostics use its
/// display name, but the top-level text is taken from `source` rather than read
/// from disk. Intended for tooling that holds unsaved buffers (e.g. the language
/// server), where the editor's current text differs from the on-disk copy.
///
/// Custom pseudo-ops are unresolved (as in [`assemble`]).
pub fn assemble_source_as(
    path: &Path,
    source: &str,
    options: &Options,
) -> Result<Assembly, AssembleError> {
    let base = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    assemble_impl(
        source,
        options,
        base,
        &display_name(path),
        default_custom_resolver(),
    )
}

/// A best-effort diagnostic scan of an in-memory buffer, for tooling (the
/// language server). Unlike [`assemble`], it does **not** stop at the first
/// error: parse and assembly errors are collected with recovery so several
/// problems surface at once, alongside warnings and the defined symbols.
pub struct Diagnostics {
    /// All errors found (deduplicated), in source order.
    pub errors: Vec<Diag>,
    /// All warnings found (deduplicated).
    pub warnings: Vec<Diag>,
    /// Symbols defined by the (best-effort) assembly, for completion.
    pub symbols: Vec<ListSymbol>,
}

/// Collect all diagnostics for in-memory `source` as though it were the file at
/// `path` (see [`assemble_source_as`] for the path/base-directory semantics).
/// A preprocessing failure (e.g. a missing include) is reported as a single
/// error; otherwise parse errors are collected with recovery, and if the parse
/// is clean the assembler runs in collect mode to gather every error/warning.
#[must_use]
pub fn diagnose_source_as(path: &Path, source: &str, options: &Options) -> Diagnostics {
    diagnose_impl(path, source, options, None, default_custom_resolver())
}

/// Like [`diagnose_source_as`], but resolving `.include` / `.inestrn` targets
/// through `overlay` (an editor's unsaved buffers) before disk. This lets the
/// language server diagnose a file in the context of its whole project as the
/// editor currently sees it. See [`FileOverlay`].
#[must_use]
pub fn diagnose_source_with_overlay(
    path: &Path,
    source: &str,
    options: &Options,
    overlay: &FileOverlay,
) -> Diagnostics {
    diagnose_impl(
        path,
        source,
        options,
        Some(overlay),
        default_custom_resolver(),
    )
}

/// Like [`diagnose_source_with_overlay`], but with a caller-supplied resolver
/// for custom pseudo-ops (`.foo`). Tooling passes [`lenient_custom_resolver`] so
/// project-defined pseudo-instructions aren't reported as unknown. `overlay` may
/// be `None` for a plain (disk-only) scan.
#[must_use]
pub fn diagnose_source_with(
    path: &Path,
    source: &str,
    options: &Options,
    overlay: Option<&FileOverlay>,
    custom: CustomResolver,
) -> Diagnostics {
    diagnose_impl(path, source, options, overlay, custom)
}

fn diagnose_impl(
    path: &Path,
    source: &str,
    options: &Options,
    overlay: Option<&FileOverlay>,
    custom: CustomResolver,
) -> Diagnostics {
    let base = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let top_name = display_name(path);

    let pre = match preprocess::preprocess_with(source, base, &top_name, overlay) {
        Ok(pre) => pre,
        Err(diag) => {
            return Diagnostics {
                errors: vec![diag],
                warnings: Vec::new(),
                symbols: Vec::new(),
            }
        }
    };

    let (lines, parse_errors) = parse::parse_recovering(pre.tokens);
    if !parse_errors.is_empty() {
        // Syntax errors block semantic analysis (missing symbols would cascade),
        // so report them alone.
        let errors = parse_errors
            .into_iter()
            .map(|e| Diag {
                file: pre.files.get(e.file as usize).cloned().unwrap_or_default(),
                line: e.line,
                message: e.message,
            })
            .collect();
        return Diagnostics {
            errors,
            warnings: Vec::new(),
            symbols: Vec::new(),
        };
    }

    let mut asm = assemble::Assembler::new(
        options.nes,
        options.undocumented,
        options.empty_byte,
        pre.files,
        pre.dirs,
        custom,
    );
    let (errors, warnings) = asm.diagnostics(&lines);
    Diagnostics {
        errors,
        warnings,
        symbols: asm.list_symbols(),
    }
}

/// A project-wide diagnostic scan for tooling: like [`diagnose_source_with_overlay`],
/// but also returning the flattened file table so callers can map each
/// diagnostic (which names its file) back to a resolved path — and thus to the
/// right editor buffer. The language server uses this to assemble a project from
/// its entry file and distribute diagnostics across the open documents.
pub struct ProjectDiagnostics {
    /// All errors found (deduplicated), in source order.
    pub errors: Vec<Diag>,
    /// All warnings found (deduplicated).
    pub warnings: Vec<Diag>,
    /// Symbols defined by the (best-effort) assembly of the whole project.
    pub symbols: Vec<ListSymbol>,
    /// Display name of each flattened file, as referenced by [`Diag::file`],
    /// parallel to `paths`.
    pub files: Vec<String>,
    /// Resolved path of each flattened file, parallel to `files`.
    pub paths: Vec<PathBuf>,
}

/// Diagnose the project rooted at `path` (with in-memory `source` as its
/// top-level text), resolving includes through `overlay`. See
/// [`ProjectDiagnostics`] and [`diagnose_source_with_overlay`].
#[must_use]
pub fn diagnose_project(
    path: &Path,
    source: &str,
    options: &Options,
    overlay: &FileOverlay,
) -> ProjectDiagnostics {
    diagnose_project_with(path, source, options, overlay, default_custom_resolver())
}

/// Like [`diagnose_project`], but with a caller-supplied resolver for custom
/// pseudo-ops (`.foo`) — tooling passes [`lenient_custom_resolver`] so
/// project-defined pseudo-instructions aren't flagged as unknown.
#[must_use]
pub fn diagnose_project_with(
    path: &Path,
    source: &str,
    options: &Options,
    overlay: &FileOverlay,
    custom: CustomResolver,
) -> ProjectDiagnostics {
    let base = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let top_name = display_name(path);

    let pre = match preprocess::preprocess_with(source, base, &top_name, Some(overlay)) {
        Ok(pre) => pre,
        Err(diag) => {
            // Preprocessing failed (e.g. a missing include); report the single
            // error against the top-level file.
            return ProjectDiagnostics {
                errors: vec![diag],
                warnings: Vec::new(),
                symbols: Vec::new(),
                files: vec![top_name],
                paths: vec![path.to_path_buf()],
            };
        }
    };
    let files = pre.files.clone();
    let paths = pre.paths.clone();

    let (lines, parse_errors) = parse::parse_recovering(pre.tokens);
    if !parse_errors.is_empty() {
        let errors = parse_errors
            .into_iter()
            .map(|e| Diag {
                file: files.get(e.file as usize).cloned().unwrap_or_default(),
                line: e.line,
                message: e.message,
            })
            .collect();
        return ProjectDiagnostics {
            errors,
            warnings: Vec::new(),
            symbols: Vec::new(),
            files,
            paths,
        };
    }

    let mut asm = assemble::Assembler::new(
        options.nes,
        options.undocumented,
        options.empty_byte,
        pre.files,
        pre.dirs,
        custom,
    );
    let (errors, warnings) = asm.diagnostics(&lines);
    ProjectDiagnostics {
        errors,
        warnings,
        symbols: asm.list_symbols(),
        files,
        paths,
    }
}

/// The basename used to refer to `path` in diagnostics.
fn display_name(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("stdin")
        .to_string()
}

/// The default resolver: every custom pseudo-op is unknown.
fn default_custom_resolver() -> CustomResolver {
    Box::new(|name, _ints, _texts, _base| {
        Err(nessemble_i18n::t!(
            "unknown-custom",
            pseudo = format!(".{name}")
        ))
    })
}

/// Parse a `--pseudo`-style custom pseudo-op mapping into `(name, path)` pairs,
/// the name without its leading dot. A line contributes an entry only when it is
/// `<name> = <path>` with a valid directive identifier for the key (an ASCII
/// letter or `_`, then letters/digits/`_`) and a non-empty value; comments and
/// malformed lines are skipped. Shared by the CLI's `--pseudo` reader and the
/// language server's project scan so the two can't drift.
#[must_use]
pub fn parse_pseudo_mapping(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            let name = key.trim().trim_start_matches('.');
            let value = value.trim();
            (is_directive_name(name) && !value.is_empty())
                .then(|| (name.to_string(), value.to_string()))
        })
        .collect()
}

/// Whether `name` is a valid custom pseudo-op identifier — the lexer's rule: an
/// ASCII letter or `_`, followed by ASCII letters, digits, or `_`.
fn is_directive_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// A resolver for tooling that recognizes a fixed set of custom pseudo-op names
/// (e.g. those declared in a project's `--pseudo` mapping) without running their
/// scripts: a known `.foo` resolves to **no bytes** (so it isn't reported as an
/// unknown directive), while an unknown one still errors as usual.
///
/// The scripts are deliberately *not* executed — a language server must not run
/// arbitrary code from a workspace just to analyze a buffer — so the bytes a
/// custom pseudo-op would emit are not modeled, and addresses after it may be
/// approximate.
#[must_use]
#[allow(clippy::implicit_hasher)] // callers use the standard HashSet.
pub fn lenient_custom_resolver(known: std::collections::HashSet<String>) -> CustomResolver {
    Box::new(move |name, _ints, _texts, _base| {
        if known.contains(name) {
            Ok(Vec::new())
        } else {
            Err(nessemble_i18n::t!(
                "unknown-custom",
                pseudo = format!(".{name}")
            ))
        }
    })
}

fn assemble_impl(
    source: &str,
    options: &Options,
    base_dir: PathBuf,
    top_name: &str,
    custom: CustomResolver,
) -> Result<Assembly, AssembleError> {
    let pre = preprocess::preprocess(source, base_dir, top_name).map_err(AssembleError)?;
    let lines = parse::parse(pre.tokens).map_err(|e| {
        AssembleError(Diag {
            file: pre.files.get(e.file as usize).cloned().unwrap_or_default(),
            line: e.line,
            message: e.message,
        })
    })?;
    let mut asm = assemble::Assembler::new(
        options.nes,
        options.undocumented,
        options.empty_byte,
        pre.files,
        pre.dirs,
        custom,
    );
    asm.set_record_source_map(options.source_map);
    let rom = asm.run(&lines).map_err(AssembleError)?;
    let symbols = asm.list_symbols();
    let coverage = asm.coverage_report();
    let source_map = asm.source_map();
    Ok(Assembly {
        rom,
        warnings: asm.take_warnings(),
        symbols,
        coverage,
        source_map,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asm(src: &str) -> Vec<u8> {
        assemble(src, &Options::default())
            .expect("assembly succeeds")
            .rom
    }

    #[test]
    fn parse_pseudo_mapping_keeps_valid_entries_only() {
        let text = "\
.double = double.rhai
# a comment line (no `=`)
.ease = scripts/ease.rhai
bad line without equals
.empty =
= novalue
.no-ident = x.rhai
";
        assert_eq!(
            parse_pseudo_mapping(text),
            vec![
                ("double".to_string(), "double.rhai".to_string()),
                ("ease".to_string(), "scripts/ease.rhai".to_string()),
            ]
        );
    }

    #[test]
    fn defaults_match_reference() {
        let opts = Options::default();
        assert_eq!(opts.empty_byte, 0xFF);
    }

    #[test]
    fn diagnose_collects_multiple_assembler_errors() {
        // Two unknown-opcode lines: both are reported, not just the first.
        let d = diagnose_source_as(
            Path::new("t.asm"),
            "  notareal\n  alsobad\n",
            &Options::default(),
        );
        assert_eq!(d.errors.len(), 2, "errors: {:?}", d.errors);
        assert_eq!(d.errors[0].line, 1);
        assert_eq!(d.errors[1].line, 2);
    }

    #[test]
    fn diagnose_recovers_from_multiple_syntax_errors() {
        // A line starting with a register char is a statement error; recovery
        // reports both instead of stopping at the first.
        let d = diagnose_source_as(Path::new("t.asm"), "x = 1\ny = 2\n", &Options::default());
        assert_eq!(d.errors.len(), 2, "errors: {:?}", d.errors);
        assert_eq!((d.errors[0].line, d.errors[1].line), (1, 2));
    }

    #[test]
    fn diagnose_does_not_panic_on_unbalanced_deep_nesting() {
        // 20 nested `.ifdef` past the nesting limit must not index out of range.
        let mut src = String::new();
        for _ in 0..20 {
            src.push_str(".ifdef FOO\n");
        }
        src.push_str("  nop\n");
        let _ = diagnose_source_as(Path::new("t.asm"), &src, &Options::default());
    }

    #[test]
    fn diagnose_keeps_symbols_for_a_valid_buffer() {
        let d = diagnose_source_as(
            Path::new("t.asm"),
            "start:\n  nop\ncount = 5\n",
            &Options::default(),
        );
        assert!(d.errors.is_empty(), "errors: {:?}", d.errors);
        let names: Vec<&str> = d.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"start") && names.contains(&"count"));
    }

    #[test]
    fn overlay_supplies_an_include_absent_from_disk() {
        // main.asm `.include`s a file that is *not* on disk; the overlay
        // provides it, so the symbol it defines resolves and there are no errors.
        let dir = std::env::temp_dir().join(format!(
            "nessemble-overlay-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let main = dir.join("main.asm");
        let source = ".include \"child.asm\"\n  lda #thing\n";
        let overlay = |p: &Path| p.ends_with("child.asm").then(|| "thing = 5\n".to_string());

        let with = diagnose_source_with_overlay(&main, source, &Options::default(), &overlay);
        assert!(with.errors.is_empty(), "errors: {:?}", with.errors);
        assert!(with.symbols.iter().any(|s| s.name == "thing"));

        // Without the overlay the include can't be resolved (nothing on disk).
        let without = diagnose_source_as(&main, source, &Options::default());
        assert!(
            !without.errors.is_empty(),
            "expected a could-not-include error"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn overlay_takes_precedence_over_the_on_disk_file() {
        // The on-disk child errors (unknown opcode); the overlay's version is
        // clean. The overlay must win.
        let dir = std::env::temp_dir().join(format!(
            "nessemble-overlay-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let main = dir.join("main.asm");
        std::fs::write(dir.join("child.asm"), b"  notareal\n").unwrap();
        let source = ".include \"child.asm\"\n  lda #thing\n";
        let overlay = |p: &Path| p.ends_with("child.asm").then(|| "thing = 5\n".to_string());

        let with = diagnose_source_with_overlay(&main, source, &Options::default(), &overlay);
        assert!(
            with.errors.is_empty(),
            "overlay should win: {:?}",
            with.errors
        );

        // The on-disk version (no overlay) surfaces the error.
        let without = diagnose_source_as(&main, source, &Options::default());
        assert!(!without.errors.is_empty(), "disk child should error");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn comments_example() {
        // From tests/corpus/examples/comments.
        let src = "    LDA #$c0\n    TAX\n    INX\n    ADC #$c4\n    BRK\n";
        assert_eq!(asm(src), vec![0xA9, 0xC0, 0xAA, 0xE8, 0x69, 0xC4, 0x00]);
    }

    #[test]
    fn number_bases_zeropage() {
        let src = "    LDA <165\n    LDA <$A5\n    LDA <%10100101\n    LDA <0245\n    LDA <'A'\n";
        assert_eq!(
            asm(src),
            vec![0xA5, 165, 0xA5, 165, 0xA5, 165, 0xA5, 165, 0xA5, 65]
        );
    }

    #[test]
    fn label_resolves_to_offset() {
        // Two zeropage loads (2 bytes each), then a label at offset 4 used as
        // an absolute operand.
        let src = "    LDA <$00\n    LDA <$00\nlabel:\n    LDA label\n";
        assert_eq!(asm(src), vec![0xA5, 0x00, 0xA5, 0x00, 0xAD, 0x04, 0x00]);
    }

    #[test]
    fn expression_is_right_associative() {
        let src = "    LDA #10-2-3\n"; // 10-(2-3) = 11
        assert_eq!(asm(src), vec![0xA9, 11]);
    }

    #[test]
    fn undefined_symbol_errors() {
        let err = assemble("    LDA test\n", &Options::default()).unwrap_err();
        let AssembleError(d) = err;
        assert_eq!(d.line, 1);
        assert_eq!(d.message, "Symbol `test` was not defined");
    }

    #[test]
    fn unknown_opcode_errors() {
        let err = assemble("    BLA #$01\n", &Options::default()).unwrap_err();
        let AssembleError(d) = err;
        assert_eq!(d.message, "Unknown opcode `BLA`");
    }

    #[test]
    fn invalid_mode_errors() {
        let err = assemble("    LDA [$0000]\n", &Options::default()).unwrap_err();
        let AssembleError(d) = err;
        assert_eq!(d.message, "Invalid addressing mode");
    }

    #[test]
    fn text_macro_expands_with_args() {
        let src = ".macrodef SET\n    LDA #\\1\n    STA <\\2\n.endm\n    .macro SET, $12, $34\n";
        assert_eq!(asm(src), vec![0xA9, 0x12, 0x85, 0x34]);
    }

    #[test]
    fn undefined_macro_errors() {
        let err = assemble("    .macro missing\n", &Options::default()).unwrap_err();
        let AssembleError(d) = err;
        assert_eq!(d.message, "Macro `missing` was not defined");
    }

    #[test]
    fn conditional_selects_branches() {
        // A false `.if` suppresses its bytes (without advancing), the `.else`
        // block is emitted, and a true `.if` emits directly.
        let src = "\
.if 0\n.db $11\n.else\n.db $22\n.endif\n\
.if 1\n.db $33\n.endif\n";
        assert_eq!(asm(src), vec![0x22, 0x33]);
    }

    #[test]
    fn ifdef_checks_symbol_table() {
        let src = "FOO = $01\n.ifdef FOO\n.db $aa\n.else\n.db $bb\n.endif\n\
.ifdef BAR\n.db $cc\n.else\n.db $dd\n.endif\n";
        assert_eq!(asm(src), vec![0xAA, 0xDD]);
    }

    /// Assemble `src` in NES mode with source-map recording toggled by `map`.
    fn asm_nes(src: &str, map: bool) -> Assembly {
        let opts = Options {
            nes: true,
            source_map: map,
            ..Options::default()
        };
        assemble(src, &opts).expect("assembly succeeds")
    }

    #[test]
    fn source_map_is_none_unless_requested() {
        // Off by default: the map is absent and the assembled bytes are exactly
        // the same as when it is on (recording is side-effect free).
        let src = ".inesprg 1\n.ineschr 1\n    LDA #$01\n    BRK\n";
        let off = asm_nes(src, false);
        let on = asm_nes(src, true);
        assert!(off.source_map.is_none());
        assert!(on.source_map.is_some());
        assert_eq!(off.rom, on.rom, "recording must not change output bytes");
    }

    #[test]
    fn source_map_records_byte_exact_spans() {
        // `LDA #$01` (2 bytes) on line 3 at offset 0; `BRK` (1 byte) on line 4 at
        // offset 2. Both coalesce into one span per line.
        let src = ".inesprg 1\n.ineschr 1\n    LDA #$01\n    BRK\n";
        let map = asm_nes(src, true).source_map.expect("map present");
        let spans: Vec<_> = map
            .spans
            .iter()
            .map(|s| (s.file.as_ref().to_owned(), s.line, s.rom_offset, s.len))
            .collect();
        assert_eq!(
            spans,
            vec![
                ("stdin".to_string(), 3, 0, 2),
                ("stdin".to_string(), 4, 2, 1),
            ]
        );
    }

    #[test]
    fn source_map_coalesces_a_multi_byte_data_line() {
        // A four-byte `.db` on one line is a single span of length 4.
        let src = ".inesprg 1\n.ineschr 1\n    .db $de, $ad, $be, $ef\n";
        let map = asm_nes(src, true).source_map.expect("map present");
        assert_eq!(map.spans.len(), 1);
        assert_eq!(
            (map.spans[0].line, map.spans[0].rom_offset, map.spans[0].len),
            (3, 0, 4)
        );
    }

    #[test]
    fn source_map_union_matches_write_coverage() {
        // Every span byte is a written byte and vice versa: the total span length
        // equals the write-coverage byte count, and spans are disjoint and
        // in-bounds. This is the load-bearing invariant — the map accounts for
        // exactly the bytes the `-C` bitmap marks.
        // A single PRG bank maps at $C000; `.org $C800` jumps forward, leaving a
        // gap of unwritten bytes the map must not claim.
        let src = "\
.inesprg 1\n.ineschr 1\n\
    LDA #$01\n    STA $2000\n\
    .db $de, $ad, $be, $ef\n\
    .org $C800\n    RTS\n";
        let assembly = asm_nes(src, true);
        let map = assembly.source_map.expect("map present");
        let cov = assembly.coverage.expect("coverage present");
        let covered: usize = cov.prg.iter().map(|&c| c as usize).sum::<usize>()
            + cov.chr.iter().map(|&c| c as usize).sum::<usize>();

        let span_bytes: usize = map.spans.iter().map(|s| s.len).sum();
        assert_eq!(
            span_bytes, covered,
            "spans must account for every written byte"
        );

        // Disjoint and within the ROM image.
        let mut ranges: Vec<(usize, usize)> = map
            .spans
            .iter()
            .map(|s| (s.rom_offset, s.rom_offset + s.len))
            .collect();
        ranges.sort_unstable();
        let rom_len = assembly.rom.len() - 16; // drop the iNES header
        let mut prev_end = 0;
        for (start, end) in ranges {
            assert!(start >= prev_end, "spans overlap at offset {start}");
            assert!(end <= rom_len, "span past end of ROM image");
            prev_end = end;
        }
    }
}
