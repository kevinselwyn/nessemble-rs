//! `reference` subcommand: opcode and directive lookup backed by **locally
//! bundled data** (the `nessemble-isa` opcode table plus a static directive
//! list), rather than the reference tool's network call to the registry.

use std::collections::BTreeSet;

use nessemble_isa::{AddressingMode, OPCODES};

/// In-scope assembler directives with a one-line description.
const DIRECTIVES: &[(&str, &str)] = &[
    (".org", "set the program counter"),
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
        ".inesprg / .ineschr / .inesmap / .inesmir / .inestrn",
        "iNES header fields",
    ),
    (".prg / .chr / .segment", "select a PRG/CHR bank"),
];

/// The addressing-mode label used in reference output.
fn mode_name(mode: AddressingMode) -> &'static str {
    match mode {
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

/// Run `reference` with 0, 1, or 2 terms. Returns `(output, exit_code)`.
pub fn run(term1: Option<&str>, term2: Option<&str>) -> (String, u8) {
    match (term1, term2) {
        (None, _) => (list_categories(), 0),
        (Some(cat), None) => match cat.to_ascii_lowercase().as_str() {
            "instructions" | "instruction" | "opcodes" => (list_instructions(), 0),
            "directives" | "pseudos" | "pseudo" => (list_directives(), 0),
            other => (format!("Could not find info for `{other}`\n"), 1),
        },
        (Some(cat), Some(term)) => match cat.to_ascii_lowercase().as_str() {
            "instructions" | "instruction" | "opcodes" => instruction_detail(term),
            "directives" | "pseudos" | "pseudo" => directive_detail(term),
            other => (format!("Could not find info for `{other}`\n"), 1),
        },
    }
}

fn list_categories() -> String {
    "Categories:\n  instructions\n  directives\n".to_string()
}

fn list_instructions() -> String {
    let mnemonics: BTreeSet<&str> = OPCODES.iter().map(|o| o.mnemonic).collect();
    let mut out = String::from("Instructions:\n");
    for (i, m) in mnemonics.iter().enumerate() {
        out.push_str(m);
        out.push(if (i + 1) % 8 == 0 { '\n' } else { ' ' });
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn instruction_detail(mnemonic: &str) -> (String, u8) {
    let mut rows: Vec<&nessemble_isa::Opcode> = OPCODES
        .iter()
        .filter(|o| o.mnemonic.eq_ignore_ascii_case(mnemonic))
        .collect();
    if rows.is_empty() {
        return (format!("Could not find info for `{mnemonic}`\n"), 1);
    }
    rows.sort_by_key(|o| o.opcode);
    let mut out = format!("{}:\n", rows[0].mnemonic);
    for o in rows {
        let flag = if o.is_undocumented() {
            " (undocumented)"
        } else {
            ""
        };
        out.push_str(&format!(
            "  {:<12} ${:02X}  {} byte(s), {} cycles{}\n",
            mode_name(o.mode),
            o.opcode,
            o.length,
            o.timing,
            flag
        ));
    }
    (out, 0)
}

fn list_directives() -> String {
    let max = DIRECTIVES.iter().map(|(n, _)| n.len()).max().unwrap_or(0);
    let mut out = String::from("Directives:\n");
    for (name, desc) in DIRECTIVES {
        out.push_str(&format!("  {name:<max$}  {desc}\n"));
    }
    out
}

fn directive_detail(term: &str) -> (String, u8) {
    let needle = term.trim_start_matches('.');
    for (name, desc) in DIRECTIVES {
        if name.split(['/', ' ']).any(|n| {
            n.trim()
                .trim_start_matches('.')
                .eq_ignore_ascii_case(needle)
        }) {
            return (format!("{name}\n  {desc}\n"), 0);
        }
    }
    (format!("Could not find info for `{term}`\n"), 1)
}
