//! Header-byte coverage for the iNES `.ines*` directives, including the Tier 1
//! extensions (`.inesbat`, `.ines4scr`, `.inesprgram`, `.inestv`) and the Tier 2
//! extensions (`.inesvs`, `.inespc10`). These fields are not produced by the
//! reference v1.1.1 binary, so they are checked here by asserting the emitted
//! 16-byte header directly rather than via a golden ROM.

use nessemble_core::{assemble, Options};

/// Assemble `src` and return its 16-byte iNES header.
fn header(src: &str) -> [u8; 16] {
    let a = assemble(src, &Options::default()).expect("assembles");
    a.rom[..16].try_into().expect("at least a full header")
}

#[test]
fn base_header_is_unchanged() {
    let h = header(".inesprg 1\n.ineschr 1\n");
    assert_eq!(&h[0..4], b"NES\x1A");
    assert_eq!(h[4], 1, "PRG count");
    assert_eq!(h[5], 1, "CHR count");
    assert_eq!(&h[6..16], &[0; 10], "flags/extensions default to zero");
}

#[test]
fn battery_sets_flags6_bit1() {
    let h = header(".inesprg 1\n.ineschr 1\n.inesbat 1\n");
    assert_eq!(h[6], 0b0000_0010);
}

#[test]
fn four_screen_sets_flags6_bit3() {
    let h = header(".inesprg 1\n.ineschr 1\n.ines4scr 1\n");
    assert_eq!(h[6], 0b0000_1000);
}

#[test]
fn flags6_bits_combine_with_mirroring_and_mapper() {
    // Vertical mirroring (bit 0), battery (bit 1), four-screen (bit 3), and the
    // low mapper nibble (bits 4-7) all pack into byte 6 together.
    let h = header(".inesprg 1\n.ineschr 1\n.inesmap 3\n.inesmir 1\n.inesbat 1\n.ines4scr 1\n");
    assert_eq!(h[6], 0b0011_1011);
    assert_eq!(h[7], 0x00, "mapper 3 has no high nibble");
}

#[test]
fn prgram_size_sets_byte8() {
    let h = header(".inesprg 1\n.ineschr 1\n.inesprgram 4\n");
    assert_eq!(h[8], 4);
}

#[test]
fn tv_system_sets_byte9_and_mirrors_pal_into_byte10() {
    let ntsc = header(".inesprg 1\n.ineschr 1\n.inestv 0\n");
    assert_eq!(ntsc[9], 0);
    assert_eq!(ntsc[10], 0);
    let pal = header(".inesprg 1\n.ineschr 1\n.inestv 1\n");
    assert_eq!(pal[9], 1);
    assert_eq!(pal[10], 0b10, "PAL mirrors into Flags 10 bits 0-1");
}

#[test]
fn vs_unisystem_sets_flags7_bit0() {
    let h = header(".inesprg 1\n.ineschr 1\n.inesvs 1\n");
    assert_eq!(h[7], 0b0000_0001);
}

#[test]
fn playchoice10_sets_flags7_bit1() {
    let h = header(".inesprg 1\n.ineschr 1\n.inespc10 1\n");
    assert_eq!(h[7], 0b0000_0010);
}

#[test]
fn flags7_bits_combine_with_mapper_high_nibble() {
    // Mapper 0x20 (high nibble set) plus VS and PlayChoice-10 all pack together.
    let h = header(".inesprg 1\n.ineschr 1\n.inesmap $20\n.inesvs 1\n.inespc10 1\n");
    assert_eq!(h[7], 0b0010_0011);
}

#[test]
fn extensions_leave_bytes_11_to_15_zero() {
    // Setting every extension field must not disturb the trailing padding, so the
    // file is not misdetected as NES 2.0 or corrupt.
    let h = header(
        ".inesprg 1\n.ineschr 1\n.inesbat 1\n.ines4scr 1\n.inesprgram 8\n.inestv 1\n.inesvs 1\n.inespc10 1\n",
    );
    assert_eq!(&h[11..16], &[0; 5]);
}
