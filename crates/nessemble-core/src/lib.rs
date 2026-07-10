//! Core 6502/NES assembler for `nessemble-rs`.
//!
//! Phase 2 implements the non-iNES-output assembler: a hand-written lexer,
//! recursive-descent parser, and a two-pass assembler that reproduces the
//! reference tool's instruction encoding, symbol handling, expression
//! semantics, and error messages. Full iNES ROM output (header/banking/CHR)
//! arrives in Phase 3.

pub use nessemble_isa as isa;

mod assemble;
pub mod ast;
mod lexer;
mod parse;

pub use assemble::Diag;

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
}

/// Assemble source text into output bytes.
pub fn assemble(source: &str, options: &Options) -> Result<Assembly, AssembleError> {
    let tokens = lexer::Lexer::new(source).tokenize();
    let lines = parse::parse(tokens).map_err(|e| {
        AssembleError::Diagnostic(Diag {
            line: e.line,
            message: e.message,
        })
    })?;
    let mut asm = assemble::Assembler::new(options.nes, options.undocumented, options.empty_byte);
    let rom = asm.run(&lines).map_err(AssembleError::Diagnostic)?;
    Ok(Assembly {
        rom,
        warnings: asm.take_warnings(),
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
}
