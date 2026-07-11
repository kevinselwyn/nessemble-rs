//! 6502 instruction-set tables for `nessemble-rs`.
//!
//! This crate is the shared, dependency-free source of truth for opcodes and
//! addressing modes. The [`OPCODES`] table is generated at build time from
//! `data/opcodes.csv` (see `build.rs`) so it stays byte-identical to the
//! reference project's table used for ROM-output parity.

/// 6502 addressing modes, matching the reference assembler's `MODE_*` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AddressingMode {
    Implied,
    Accumulator,
    Relative,
    Immediate,
    ZeroPage,
    ZeroPageX,
    ZeroPageY,
    Absolute,
    AbsoluteY,
    AbsoluteX,
    Indirect,
    IndirectX,
    IndirectY,
}

impl AddressingMode {
    /// Number of operand bytes this addressing mode encodes (0, 1, or 2).
    ///
    /// This is `instruction length - 1` for every opcode; provided here for
    /// convenience when decoding/encoding.
    #[must_use]
    pub const fn operand_bytes(self) -> u8 {
        match self {
            AddressingMode::Implied | AddressingMode::Accumulator => 0,
            AddressingMode::Relative
            | AddressingMode::Immediate
            | AddressingMode::ZeroPage
            | AddressingMode::ZeroPageX
            | AddressingMode::ZeroPageY
            | AddressingMode::IndirectX
            | AddressingMode::IndirectY => 1,
            AddressingMode::Absolute
            | AddressingMode::AbsoluteX
            | AddressingMode::AbsoluteY
            | AddressingMode::Indirect => 2,
        }
    }
}

/// Opcode has no special metadata.
pub const META_NONE: u8 = 0x00;
/// Instruction may incur a page-boundary timing penalty.
pub const META_BOUNDARY: u8 = 0x01;
/// Instruction is an undocumented ("illegal") opcode.
pub const META_UNDOCUMENTED: u8 = 0x02;

/// A single 6502 opcode definition.
#[derive(Debug, Clone, Copy)]
pub struct Opcode {
    /// Mnemonic, e.g. `"LDA"`. Upper-case as in the reference tables.
    pub mnemonic: &'static str,
    /// Addressing mode.
    pub mode: AddressingMode,
    /// Opcode byte value.
    pub opcode: u8,
    /// Total instruction length in bytes (opcode + operands).
    pub length: u8,
    /// Base cycle count.
    pub timing: u8,
    /// Metadata bitmask (see `META_*`).
    pub meta: u8,
}

impl Opcode {
    /// Whether this is an undocumented ("illegal") opcode.
    #[must_use]
    pub const fn is_undocumented(&self) -> bool {
        self.meta & META_UNDOCUMENTED != 0
    }

    /// Whether this opcode carries the page-boundary timing flag.
    #[must_use]
    pub const fn is_boundary(&self) -> bool {
        self.meta & META_BOUNDARY != 0
    }
}

include!(concat!(env!("OUT_DIR"), "/opcodes_gen.rs"));

/// Look up the opcode definition for a given opcode byte.
#[must_use]
pub fn by_byte(byte: u8) -> &'static Opcode {
    &OPCODES[byte as usize]
}

/// Find the opcode matching a mnemonic (case-insensitive) and addressing mode.
///
/// Returns the first match, mirroring the reference assembler's lookup order.
#[must_use]
pub fn find(mnemonic: &str, mode: AddressingMode) -> Option<&'static Opcode> {
    OPCODES
        .iter()
        .find(|o| o.mode == mode && o.mnemonic.eq_ignore_ascii_case(mnemonic))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_has_256_entries() {
        assert_eq!(OPCODES.len(), 256);
    }

    #[test]
    fn index_matches_opcode_value() {
        for (i, op) in OPCODES.iter().enumerate() {
            assert_eq!(i as u8, op.opcode, "row {i} has opcode {:#04x}", op.opcode);
        }
    }

    #[test]
    fn length_matches_mode_operand_bytes() {
        for op in &OPCODES {
            assert_eq!(
                op.length,
                op.mode.operand_bytes() + 1,
                "{} {:?} has length {} but mode implies {}",
                op.mnemonic,
                op.mode,
                op.length,
                op.mode.operand_bytes() + 1
            );
        }
    }

    #[test]
    fn known_opcodes_are_correct() {
        let brk = by_byte(0x00);
        assert_eq!(brk.mnemonic, "BRK");
        assert_eq!(brk.mode, AddressingMode::Implied);
        assert_eq!(brk.length, 1);
        assert_eq!(brk.timing, 7);

        let lda_imm = by_byte(0xA9);
        assert_eq!(lda_imm.mnemonic, "LDA");
        assert_eq!(lda_imm.mode, AddressingMode::Immediate);
        assert_eq!(lda_imm.length, 2);

        let nop = by_byte(0xEA);
        assert_eq!(nop.mnemonic, "NOP");
        assert_eq!(nop.mode, AddressingMode::Implied);
        assert!(!nop.is_undocumented());
    }

    #[test]
    fn find_by_mnemonic_and_mode() {
        let op = find("lda", AddressingMode::Immediate).expect("LDA immediate exists");
        assert_eq!(op.opcode, 0xA9);
        assert!(find("LDA", AddressingMode::Indirect).is_none());
    }

    #[test]
    fn has_expected_undocumented_count() {
        // The reference table marks the standard set of illegal opcodes.
        let undocumented = OPCODES.iter().filter(|o| o.is_undocumented()).count();
        assert!(
            undocumented > 100,
            "expected many undocumented opcodes, found {undocumented}"
        );
    }
}
