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

    /// A short human-readable label for the mode (e.g. `"zeropage,x"`), used in
    /// `reference` output and language-server completion/hover detail.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            AddressingMode::Implied => "implied",
            AddressingMode::Accumulator => "accumulator",
            AddressingMode::Relative => "relative",
            AddressingMode::Immediate => "immediate",
            AddressingMode::ZeroPage => "zeropage",
            AddressingMode::ZeroPageX => "zeropage,x",
            AddressingMode::ZeroPageY => "zeropage,y",
            AddressingMode::Absolute => "absolute",
            AddressingMode::AbsoluteX => "absolute,x",
            AddressingMode::AbsoluteY => "absolute,y",
            AddressingMode::Indirect => "indirect",
            AddressingMode::IndirectX => "indirect,x",
            AddressingMode::IndirectY => "indirect,y",
        }
    }
}

/// The assembler's directive catalog: each entry pairs one or more directive
/// spellings (slash-separated, e.g. `".db / .byte"`) with a one-line
/// description. This is language-level metadata (not strictly ISA), colocated
/// here as the single shared source of truth for the `reference` command and the
/// language server. Split an entry's name on `/` and whitespace for the
/// individual directive spellings.
pub const DIRECTIVES: &[(&str, &str)] = &[
    (".org", "set the program counter"),
    (
        ".phase / .dephase",
        "assemble for a different run-time address",
    ),
    (".db / .byte", "define bytes"),
    (".dw / .word", "define words (little-endian)"),
    (".ascii", "define bytes from a string"),
    (".fill", "fill a region with a byte"),
    (".hibytes / .lobytes", "define the high/low bytes of values"),
    (".checksum", "emit a CRC-32 of preceding data"),
    (".random", "emit pseudo-random bytes"),
    (".color", "emit nearest NES palette indices"),
    (".enum / .endenum", "assign incrementing constants"),
    (".rs / .rsset", "reserve sequential storage"),
    (
        ".if / .ifdef / .ifndef / .else / .endif",
        "conditional assembly",
    ),
    (".macro / .macrodef / .endm", "invoke / define macros"),
    (".include", "include another source file"),
    (".incbin", "include a raw binary"),
    (".incpng", "include a PNG as CHR tiles"),
    (".incpal", "include a PNG as a palette"),
    (".incrle", "include a run-length-encoded binary"),
    (".incwav", "include a WAV as DPCM"),
    (".font", "emit bundled font glyphs"),
    (".defchr", "define an 8x8 tile inline"),
    (
        ".inesprg / .ineschr / .inesmap / .inesmir / .inesbat / .ines4scr / .inesprgram / .inestv / .inesvs / .inespc10 / .inestrn",
        "iNES header fields",
    ),
    (
        ".ines2 / .inessubmap / .inesprgnvram / .ineschrram / .ineschrnvram / .inestiming / .inesconsole / .inesvsppu / .inesvshw / .inesmiscrom / .inesexpansion",
        "NES 2.0 header fields",
    ),
    (".prg / .chr / .segment", "select a PRG/CHR bank"),
];

/// Opcode has no special metadata.
pub const META_NONE: u8 = 0x00;
/// Instruction may incur a page-boundary timing penalty.
pub const META_BOUNDARY: u8 = 0x01;
/// Instruction is an undocumented ("illegal") opcode.
pub const META_UNDOCUMENTED: u8 = 0x02;

/// A set of 6502 registers, as a bitmask over `A`, `X`, `Y`, and `S`.
///
/// Flags are deliberately absent: nearly every instruction disturbs `N`/`Z`, so
/// a routine that does not declare them is not lying in any way a caller can act
/// on. See `plans/010-routine-signatures.md` §8.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RegSet(u8);

/// The accumulator bit of a [`RegSet`].
pub const REG_A: u8 = 1 << 0;
/// The `X` bit of a [`RegSet`].
pub const REG_X: u8 = 1 << 1;
/// The `Y` bit of a [`RegSet`].
pub const REG_Y: u8 = 1 << 2;
/// The stack-pointer bit of a [`RegSet`].
pub const REG_S: u8 = 1 << 3;

impl RegSet {
    /// The empty set.
    pub const EMPTY: RegSet = RegSet(0);
    /// Just the accumulator.
    pub const A: RegSet = RegSet(REG_A);
    /// Just `X`.
    pub const X: RegSet = RegSet(REG_X);
    /// Just `Y`.
    pub const Y: RegSet = RegSet(REG_Y);
    /// Just the stack pointer.
    pub const S: RegSet = RegSet(REG_S);

    /// Build a set from its raw bits (`REG_*`). Used by the generated table.
    #[must_use]
    pub const fn from_bits(bits: u8) -> RegSet {
        RegSet(bits)
    }

    /// The raw bits.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// Whether the set is empty.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Whether every register in `other` is in this set.
    #[must_use]
    pub const fn contains(self, other: RegSet) -> bool {
        self.0 & other.0 == other.0
    }

    /// The union of two sets.
    #[must_use]
    pub const fn union(self, other: RegSet) -> RegSet {
        RegSet(self.0 | other.0)
    }
}

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
    /// Registers this instruction writes, including those its addressing mode
    /// implies. Empty when [`effects_known`](Opcode::effects_known) is false.
    pub writes: RegSet,
    /// Registers this instruction reads, including those its addressing mode
    /// implies.
    pub reads: RegSet,
    /// Whether the register effects are recorded for this opcode. False for the
    /// undocumented opcodes, whose effects the verifier treats as unknown rather
    /// than guessing.
    pub effects_known: bool,
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
    fn every_documented_opcode_has_recorded_effects() {
        // The completeness gate: `effects.csv` cannot go stale behind
        // `opcodes.csv` without this failing.
        for op in OPCODES.iter().filter(|o| !o.is_undocumented()) {
            assert!(
                op.effects_known,
                "{} ({:?}, {:#04x}) has no entry in effects.csv",
                op.mnemonic, op.mode, op.opcode
            );
        }
    }

    #[test]
    fn illegal_mnemonics_have_unknown_effects() {
        // Effects are keyed by mnemonic, so the undocumented encodings of a
        // documented instruction (the illegal `NOP`s, `SBC` $EB) correctly
        // inherit its effects. The genuinely illegal mnemonics have none, and an
        // opcode with unknown effects never invents a write.
        for op in &OPCODES {
            assert!(
                op.effects_known || op.writes.is_empty(),
                "{} has unknown effects but claims a write",
                op.mnemonic
            );
        }
        for mnemonic in ["SLO", "LAX", "KIL", "DCP", "ARR", "XAA"] {
            let op = OPCODES
                .iter()
                .find(|o| o.mnemonic == mnemonic)
                .unwrap_or_else(|| panic!("{mnemonic} is in the table"));
            assert!(!op.effects_known, "{mnemonic} claims known effects");
        }
    }

    #[test]
    fn effects_follow_the_addressing_mode() {
        // The distinction that makes this table per-opcode rather than per
        // mnemonic: accumulator mode writes A, absolute mode does not.
        let asl_a = find("ASL", AddressingMode::Accumulator).expect("ASL A exists");
        assert!(asl_a.writes.contains(RegSet::A));
        assert!(asl_a.reads.contains(RegSet::A));
        let asl_abs = find("ASL", AddressingMode::Absolute).expect("ASL abs exists");
        assert!(asl_abs.writes.is_empty());

        // Indexed modes read their index register; non-indexed ones do not.
        let lda_zpx = find("LDA", AddressingMode::ZeroPageX).expect("LDA zp,X exists");
        assert!(lda_zpx.reads.contains(RegSet::X));
        assert!(lda_zpx.writes.contains(RegSet::A));
        let lda_zp = find("LDA", AddressingMode::ZeroPage).expect("LDA zp exists");
        assert!(!lda_zp.reads.contains(RegSet::X));

        let lda_absy = find("LDA", AddressingMode::AbsoluteY).expect("LDA abs,Y exists");
        assert!(lda_absy.reads.contains(RegSet::Y));

        // A store writes no register, but still reads the one it stores.
        let sta_abs = find("STA", AddressingMode::Absolute).expect("STA abs exists");
        assert!(sta_abs.writes.is_empty());
        assert!(sta_abs.reads.contains(RegSet::A));
    }

    #[test]
    fn only_txs_writes_the_stack_pointer() {
        // The documented approximation: pushes and pulls move S, but routines
        // balance them, so counting those as writes would flag every routine
        // that touches the stack.
        for op in OPCODES.iter().filter(|o| o.effects_known) {
            assert_eq!(
                op.writes.contains(RegSet::S),
                op.mnemonic == "TXS",
                "{} disagrees about writing S",
                op.mnemonic
            );
        }
    }

    #[test]
    fn reg_set_is_a_set() {
        let ax = RegSet::A.union(RegSet::X);
        assert!(ax.contains(RegSet::A));
        assert!(ax.contains(RegSet::X));
        assert!(!ax.contains(RegSet::Y));
        assert!(ax.contains(ax));
        assert!(RegSet::EMPTY.is_empty());
        assert!(!ax.is_empty());
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
