//! Generates the 256-entry 6502 opcode table from `data/opcodes.csv`, with
//! per-opcode register effects merged in from `data/effects.csv`.
//!
//! `opcodes.csv` is the single source of truth for the table itself (imported
//! from the upstream reference project's `src/static/opcodes.csv`) and is left
//! verbatim so it can be re-imported. Each row is:
//!
//! ```text
//! "MNEMONIC",MODE_MACRO,0xNN,length,timing,meta
//! ```
//!
//! `effects.csv` is ours: which registers each mnemonic writes and reads, for
//! the clobber verifier. Mode-derived effects (indexed modes read their index
//! register; accumulator mode reads and writes `A`) are applied here rather than
//! listed per row, so the data file stays one line per mnemonic.
//!
//! We emit a `static OPCODES: [Opcode; 256]` indexed by opcode value.

use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::Path;

fn mode_variant(macro_name: &str) -> &'static str {
    match macro_name.trim() {
        "MODE_IMPLIED" => "AddressingMode::Implied",
        "MODE_ACCUMULATOR" => "AddressingMode::Accumulator",
        "MODE_RELATIVE" => "AddressingMode::Relative",
        "MODE_IMMEDIATE" => "AddressingMode::Immediate",
        "MODE_ZEROPAGE" => "AddressingMode::ZeroPage",
        "MODE_ZEROPAGE_X" => "AddressingMode::ZeroPageX",
        "MODE_ZEROPAGE_Y" => "AddressingMode::ZeroPageY",
        "MODE_ABSOLUTE" => "AddressingMode::Absolute",
        "MODE_ABSOLUTE_Y" => "AddressingMode::AbsoluteY",
        "MODE_ABSOLUTE_X" => "AddressingMode::AbsoluteX",
        "MODE_INDIRECT" => "AddressingMode::Indirect",
        "MODE_INDIRECT_X" => "AddressingMode::IndirectX",
        "MODE_INDIRECT_Y" => "AddressingMode::IndirectY",
        other => panic!("unknown addressing mode macro in opcodes.csv: {other:?}"),
    }
}

/// The register bits a mode contributes on top of its mnemonic's own effects:
/// an indexed mode reads its index register, and accumulator mode reads and
/// writes `A`. Returns `(writes, reads)`.
fn mode_effects(mode_macro: &str) -> (u8, u8) {
    match mode_macro.trim() {
        "MODE_ACCUMULATOR" => (REG_A, REG_A),
        "MODE_ZEROPAGE_X" | "MODE_ABSOLUTE_X" | "MODE_INDIRECT_X" => (0, REG_X),
        "MODE_ZEROPAGE_Y" | "MODE_ABSOLUTE_Y" | "MODE_INDIRECT_Y" => (0, REG_Y),
        _ => (0, 0),
    }
}

const REG_A: u8 = 1 << 0;
const REG_X: u8 = 1 << 1;
const REG_Y: u8 = 1 << 2;
const REG_S: u8 = 1 << 3;

/// Parse a register-set field (`"AX"`, `""`) into its bitmask.
fn parse_regs(field: &str, line: &str) -> u8 {
    field.trim().chars().fold(0, |bits, c| {
        bits | match c.to_ascii_uppercase() {
            'A' => REG_A,
            'X' => REG_X,
            'Y' => REG_Y,
            'S' => REG_S,
            other => panic!("unknown register {other:?} in effects.csv row: {line:?}"),
        }
    })
}

/// Read `effects.csv` into mnemonic → (writes, reads). A mnemonic absent from
/// the map has unknown effects.
fn read_effects(path: &Path) -> HashMap<String, (u8, u8)> {
    let csv =
        fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    let mut out = HashMap::new();
    for line in csv.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split(',').collect();
        assert!(
            fields.len() == 3,
            "effects.csv row has {} fields, expected 3: {line:?}",
            fields.len()
        );
        let mnemonic = fields[0].trim().to_ascii_uppercase();
        let writes = parse_regs(fields[1], line);
        let reads = parse_regs(fields[2], line);
        assert!(
            out.insert(mnemonic, (writes, reads)).is_none(),
            "duplicate mnemonic in effects.csv: {line:?}"
        );
    }
    out
}

fn parse_int(field: &str) -> u32 {
    let field = field.trim();
    if let Some(hex) = field
        .strip_prefix("0x")
        .or_else(|| field.strip_prefix("0X"))
    {
        u32::from_str_radix(hex, 16).unwrap_or_else(|_| panic!("bad hex field: {field:?}"))
    } else {
        field
            .parse()
            .unwrap_or_else(|_| panic!("bad integer field: {field:?}"))
    }
}

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let csv_path = Path::new(&manifest_dir).join("data/opcodes.csv");
    println!("cargo:rerun-if-changed={}", csv_path.display());

    let effects_path = Path::new(&manifest_dir).join("data/effects.csv");
    println!("cargo:rerun-if-changed={}", effects_path.display());

    let csv = fs::read_to_string(&csv_path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", csv_path.display()));
    let effects = read_effects(&effects_path);

    let mut rows: Vec<String> = Vec::with_capacity(256);
    for (lineno, line) in csv.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split(',').collect();
        assert!(
            fields.len() == 6,
            "opcodes.csv line {} has {} fields, expected 6: {line:?}",
            lineno + 1,
            fields.len()
        );

        let mnemonic = fields[0].trim().trim_matches('"');
        let mode = mode_variant(fields[1]);
        let opcode = parse_int(fields[2]);
        let length = parse_int(fields[3]);
        let timing = parse_int(fields[4]);
        let meta = parse_int(fields[5]);

        assert!(opcode < 256, "opcode out of range: {opcode}");
        assert!(
            opcode as usize == rows.len(),
            "opcodes.csv must be ordered by opcode value; row {} declares opcode {:#04x} but expected {:#04x}",
            lineno + 1,
            opcode,
            rows.len()
        );

        // Effects are the mnemonic's own, plus whatever the addressing mode
        // implies. A mnemonic with no entry stays unknown, mode or not.
        let (writes, reads, known) = match effects.get(&mnemonic.to_ascii_uppercase()) {
            Some(&(writes, reads)) => {
                let (mode_writes, mode_reads) = mode_effects(fields[1]);
                (writes | mode_writes, reads | mode_reads, true)
            }
            None => (0, 0, false),
        };

        rows.push(format!(
            "    Opcode {{ mnemonic: {mnemonic:?}, mode: {mode}, opcode: {opcode:#04x}, length: {length}, timing: {timing}, meta: {meta:#04x}, \
             writes: RegSet::from_bits({writes:#04x}), reads: RegSet::from_bits({reads:#04x}), effects_known: {known} }},"
        ));
    }

    assert!(
        rows.len() == 256,
        "expected exactly 256 opcode rows, got {}",
        rows.len()
    );

    let generated = format!(
        "// @generated by build.rs from data/opcodes.csv — do not edit.\n\
         /// The full 256-entry 6502 opcode table, indexed by opcode byte.\n\
         pub static OPCODES: [Opcode; 256] = [\n{}\n];\n",
        rows.join("\n")
    );

    let out_dir = env::var("OUT_DIR").expect("OUT_DIR");
    let out_path = Path::new(&out_dir).join("opcodes_gen.rs");
    fs::write(&out_path, generated)
        .unwrap_or_else(|e| panic!("cannot write {}: {e}", out_path.display()));
}
