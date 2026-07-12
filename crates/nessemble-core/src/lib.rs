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
mod lexer;
mod parse;
mod preprocess;
pub mod tooling;

pub use assemble::{CoverageReport, CustomResolver, Diag, ListSymbol};

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
}

impl Default for Options {
    fn default() -> Self {
        Options {
            nes: false,
            undocumented: false,
            empty_byte: 0xFF,
        }
    }
}

/// Errors produced while assembling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssembleError {
    /// A diagnostic with a source line and message (reference-compatible text).
    Diagnostic(Diag),
}

impl std::fmt::Display for AssembleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AssembleError::Diagnostic(d) => write!(f, "line {}: {}", d.line, d.message),
        }
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
        AssembleError::Diagnostic(Diag {
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
    let base = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let top_name = display_name(path);

    let pre = match preprocess::preprocess(source, base, &top_name) {
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
        default_custom_resolver(),
    );
    let (errors, warnings) = asm.diagnostics(&lines);
    Diagnostics {
        errors,
        warnings,
        symbols: asm.list_symbols(),
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

fn assemble_impl(
    source: &str,
    options: &Options,
    base_dir: PathBuf,
    top_name: &str,
    custom: CustomResolver,
) -> Result<Assembly, AssembleError> {
    let pre =
        preprocess::preprocess(source, base_dir, top_name).map_err(AssembleError::Diagnostic)?;
    let lines = parse::parse(pre.tokens).map_err(|e| {
        AssembleError::Diagnostic(Diag {
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
    let rom = asm.run(&lines).map_err(AssembleError::Diagnostic)?;
    let symbols = asm.list_symbols();
    let coverage = asm.coverage_report();
    Ok(Assembly {
        rom,
        warnings: asm.take_warnings(),
        symbols,
        coverage,
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
        match err {
            AssembleError::Diagnostic(d) => {
                assert_eq!(d.line, 1);
                assert_eq!(d.message, "Symbol `test` was not defined");
            }
        }
    }

    #[test]
    fn unknown_opcode_errors() {
        let err = assemble("    BLA #$01\n", &Options::default()).unwrap_err();
        let AssembleError::Diagnostic(d) = err;
        assert_eq!(d.message, "Unknown opcode `BLA`");
    }

    #[test]
    fn invalid_mode_errors() {
        let err = assemble("    LDA [$0000]\n", &Options::default()).unwrap_err();
        let AssembleError::Diagnostic(d) = err;
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
        let AssembleError::Diagnostic(d) = err;
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
}
