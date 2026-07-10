//! Core 6502/NES assembler for `nessemble-rs`.
//!
//! Phase 0 establishes the crate seams and public entry point. The lexer,
//! parser, symbol table, and two-pass assembler land in later phases (1–5);
//! for now [`assemble`] is a well-typed stub that reports
//! [`AssembleError::NotImplemented`].

pub use nessemble_isa as isa;

/// The reference implementation version this crate targets for output parity.
pub const REFERENCE_VERSION: &str = "1.1.1";

/// Options controlling an assembly run, mirroring the reference CLI flags that
/// affect output. Expanded in later phases.
#[derive(Debug, Clone)]
pub struct Options {
    /// Emit an iNES (`.nes`) header/layout (`-f nes`).
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
#[non_exhaustive]
pub enum AssembleError {
    /// The assembler pipeline is not yet implemented (Phase 0 placeholder).
    NotImplemented,
}

impl std::fmt::Display for AssembleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AssembleError::NotImplemented => {
                write!(f, "assembler not yet implemented")
            }
        }
    }
}

impl std::error::Error for AssembleError {}

/// Assemble source text into ROM bytes.
///
/// Placeholder for Phase 0 — always returns [`AssembleError::NotImplemented`].
pub fn assemble(_source: &str, _options: &Options) -> Result<Vec<u8>, AssembleError> {
    Err(AssembleError::NotImplemented)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_reference() {
        let opts = Options::default();
        assert_eq!(opts.empty_byte, 0xFF);
        assert!(!opts.nes);
        assert!(!opts.undocumented);
    }

    #[test]
    fn assemble_is_stubbed() {
        assert_eq!(
            assemble("", &Options::default()),
            Err(AssembleError::NotImplemented)
        );
    }
}
