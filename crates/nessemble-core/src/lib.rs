//! Core 6502/NES assembler for `nessemble-rs`.
//!
//! Phase 2 implements the non-iNES-output assembler: a hand-written lexer,
//! recursive-descent parser, and a two-pass assembler that reproduces the
//! reference tool's instruction encoding, symbol handling, expression
//! semantics, and error messages. Full iNES ROM output (header/banking/CHR)
//! arrives in Phase 3.

use std::path::{Path, PathBuf};

pub use nessemble_isa as isa;

mod assemble;
pub mod ast;
mod lexer;
mod parse;
mod preprocess;

pub use assemble::{Diag, ListSymbol};

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
}

/// Render the list-file (`-l`) contents from the defined symbols, mirroring the
/// reference `output_list` format: a `[constants]` section (`VALUE = NAME`) and
/// a `[labels]` section (`BANK/VALUE = NAME`), each sorted lexicographically by
/// its formatted line, separated by a blank line when both are present.
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
/// Includes are resolved relative to the current working directory and the
/// source is reported as `stdin` in diagnostics. Use [`assemble_file`] to
/// assemble a named file (which resolves includes relative to that file's
/// directory, like the reference tool).
pub fn assemble(source: &str, options: &Options) -> Result<Assembly, AssembleError> {
    let base = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    assemble_impl(source, options, base, "stdin")
}

/// Assemble the file at `path`, resolving includes relative to its directory.
pub fn assemble_file(path: &Path, options: &Options) -> Result<Assembly, AssembleError> {
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
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    assemble_impl(&source, options, base, &display_name(path))
}

/// The basename used to refer to `path` in diagnostics.
fn display_name(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("stdin")
        .to_string()
}

fn assemble_impl(
    source: &str,
    options: &Options,
    base_dir: PathBuf,
    top_name: &str,
) -> Result<Assembly, AssembleError> {
    let (tokens, files) = preprocess::preprocess(source, base_dir.clone(), top_name)
        .map_err(AssembleError::Diagnostic)?;
    let lines = parse::parse(tokens).map_err(|e| {
        AssembleError::Diagnostic(Diag {
            file: files.get(e.file as usize).cloned().unwrap_or_default(),
            line: e.line,
            message: e.message,
        })
    })?;
    let mut asm = assemble::Assembler::new(
        options.nes,
        options.undocumented,
        options.empty_byte,
        files,
        base_dir,
    );
    let rom = asm.run(&lines).map_err(AssembleError::Diagnostic)?;
    let symbols = asm.list_symbols();
    Ok(Assembly {
        rom,
        warnings: asm.take_warnings(),
        symbols,
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
