//! Header-byte coverage for the NES 2.0 (`.ines2`) directives. NES 2.0 output
//! is not produced by the reference v1.1.1 binary, so — like the iNES extension
//! tests — these assert the emitted 16-byte header directly rather than via a
//! golden ROM.

use nessemble_core::{assemble, AssembleError, Options};

/// Assemble `src` and return its 16-byte NES 2.0 header.
fn header(src: &str) -> [u8; 16] {
    let a = assemble(src, &Options::default()).expect("assembles");
    a.rom[..16].try_into().expect("at least a full header")
}

/// Assemble `src`, expecting failure, and return the diagnostic message.
fn err(src: &str) -> String {
    match assemble(src, &Options::default()) {
        Ok(_) => panic!("expected an error but assembly succeeded"),
        Err(AssembleError::Diagnostic(d)) => d.message,
    }
}

const BASE: &str = ".ines2 1\n.inesprg 1\n.ineschr 1\n";

#[test]
fn base_nes2_header_sets_the_signature() {
    let h = header(BASE);
    assert_eq!(&h[0..4], b"NES\x1A");
    assert_eq!(h[4], 1, "PRG LSB");
    assert_eq!(h[5], 1, "CHR LSB");
    assert_eq!(h[6], 0);
    assert_eq!(h[7], 0x08, "NES 2.0 identifier in bits 2-3");
    assert_eq!(&h[8..16], &[0; 8]);
}

#[test]
fn mapper_spans_three_nibbles() {
    // Mapper $123: low nibble -> byte 6 D4-7, mid -> byte 7 D4-7, high -> byte 8.
    let h = header(".ines2 1\n.inesprg 1\n.ineschr 1\n.inesmap $123\n");
    assert_eq!(h[6] & 0xF0, 0x30);
    assert_eq!(h[7], 0x08 | 0x20);
    assert_eq!(h[8] & 0x0F, 0x01);
}

#[test]
fn submapper_sets_byte8_high_nibble() {
    let h = header(".ines2 1\n.inesprg 1\n.ineschr 1\n.inesmap $123\n.inessubmap 5\n");
    assert_eq!(
        h[8], 0x51,
        "submapper 5 in D4-7, mapper high nibble 1 in D0-3"
    );
}

#[test]
fn prg_and_chr_sizes_widen_into_byte9() {
    let h = header(".ines2 1\n.inesprg $110\n.ineschr $201\n");
    assert_eq!(h[4], 0x10, "PRG LSB");
    assert_eq!(h[5], 0x01, "CHR LSB");
    assert_eq!(
        h[9], 0x21,
        "PRG MSB nibble 1 (D0-3), CHR MSB nibble 2 (D4-7)"
    );
}

#[test]
fn ram_sizes_are_logarithmic_shift_counts() {
    let h = header(
        ".ines2 1\n.inesprg 1\n.ineschr 1\n\
         .inesprgram 8192\n.inesprgnvram 8192\n.ineschrram 128\n.ineschrnvram 2097152\n",
    );
    assert_eq!(h[10], 0x77, "PRG-RAM and PRG-NVRAM both shift 7 (8 KiB)");
    assert_eq!(
        h[11], 0xF1,
        "CHR-RAM shift 1 (128 B), CHR-NVRAM shift 15 (2 MiB)"
    );
}

#[test]
fn timing_uses_byte12_with_tv_fallback() {
    assert_eq!(header(&format!("{BASE}.inestiming 2\n"))[12], 2);
    // `.inestv` (PAL) falls through to the timing byte when no explicit timing.
    assert_eq!(header(&format!("{BASE}.inestv 1\n"))[12], 1);
    // Explicit `.inestiming` wins over `.inestv`.
    assert_eq!(header(&format!("{BASE}.inestv 1\n.inestiming 3\n"))[12], 3);
}

#[test]
fn console_type_from_sugar_and_explicit() {
    assert_eq!(
        header(&format!("{BASE}.inesvs 1\n"))[7],
        0x09,
        "VS -> console type 1"
    );
    assert_eq!(
        header(&format!("{BASE}.inespc10 1\n"))[7],
        0x0A,
        "PC10 -> console type 2"
    );
    assert_eq!(header(&format!("{BASE}.inesconsole 1\n"))[7], 0x09);
}

#[test]
fn vs_ppu_and_hardware_fill_byte13() {
    let h = header(&format!("{BASE}.inesvs 1\n.inesvsppu 3\n.inesvshw 2\n"));
    assert_eq!(h[13], 0x23);
}

#[test]
fn misc_rom_and_expansion_fill_bytes_14_15() {
    let h = header(&format!("{BASE}.inesmiscrom 1\n.inesexpansion $2A\n"));
    assert_eq!(h[14], 1);
    assert_eq!(h[15], 0x2A);
}

#[test]
fn nes2_only_directive_without_mode_errors() {
    assert!(err(".inesprg 1\n.inessubmap 1\n").contains("NES 2.0"));
    assert!(err(".inesprg 1\n.ineschrram 128\n").contains("NES 2.0"));
}

#[test]
fn wide_mapper_or_size_without_mode_errors() {
    assert!(err(".inesprg 1\n.inesmap 300\n").contains("NES 2.0"));
    assert!(err(".inesprg 300\n").contains("NES 2.0"));
}

#[test]
fn invalid_ram_size_errors() {
    // 100 bytes is not a representable 64 << n size.
    let msg = err(".ines2 1\n.inesprg 1\n.inesprgram 100\n");
    assert!(msg.contains("size 100"), "got: {msg}");
}

#[test]
fn out_of_range_fields_error() {
    assert!(err(&format!("{BASE}.inessubmap 16\n")).contains("out of range"));
    assert!(err(&format!("{BASE}.inesexpansion 64\n")).contains("out of range"));
}

#[test]
fn conflicting_console_type_errors() {
    assert!(err(&format!("{BASE}.inesvs 1\n.inespc10 1\n")).contains("console type"));
}

#[test]
fn extended_console_type_is_rejected() {
    assert!(err(&format!("{BASE}.inesconsole 3\n")).contains("Extended console"));
}
