//! Color matching used by `.incpng` (2-bit grayscale) and `.incpal` (nearest
//! NES palette entry). Mirrors the reference `get_color` / `match_color`.

/// The four 1-bit-per-sample grayscale levels used by `.incpng`.
const COLORS_2BIT: [i32; 4] = [0x00, 0x55, 0xAA, 0xFF];

/// The 64-entry NES palette (RGB), used by `.incpal`.
const COLORS_FULL: [u32; 64] = [
    0x7C7C7C, 0x0000FC, 0x0000BC, 0x4428BC, 0x940084, 0xA80020, 0xA81000, 0x881400, 0x503000,
    0x007800, 0x006800, 0x005800, 0x004058, 0x000000, 0x000000, 0x000000, 0xBCBCBC, 0x0078F8,
    0x0058F8, 0x6844FC, 0xD800CC, 0xE40058, 0xF83800, 0xE45C10, 0xAC7C00, 0x00B800, 0x00A800,
    0x00A844, 0x008888, 0x000000, 0x000000, 0x000000, 0xF8F8F8, 0x3CBCFC, 0x6888FC, 0x9878F8,
    0xF878F8, 0xF85898, 0xF87858, 0xFCA044, 0xF8B800, 0xB8F818, 0x58D854, 0x58F898, 0x00E8D8,
    0x787878, 0x000000, 0x000000, 0xFCFCFC, 0xA4E4FC, 0xB8B8F8, 0xD8B8F8, 0xF8B8F8, 0xF8A4C0,
    0xF0D0B0, 0xFCE0A8, 0xF8D878, 0xD8F878, 0xB8F8B8, 0xB8F8D8, 0x00FCFC, 0xF8D8F8, 0x000000,
    0x000000,
];

/// Nearest 2-bit grayscale index (`0..=3`) for an RGB triple, using the average
/// of the three channels — matching the reference `get_color`.
pub fn two_bit_color(r: u8, g: u8, b: u8) -> u8 {
    let avg = (r as i32 + g as i32 + b as i32) / 3;
    let mut diff = 256;
    let mut color = 0usize;
    for (i, level) in COLORS_2BIT.iter().enumerate() {
        if (level - avg).abs() < diff {
            diff = (level - avg).abs();
            color = i;
        }
    }
    color as u8
}

/// Nearest NES palette index for an RGB triple (Euclidean distance, truncated to
/// an integer, first match), with index `0x0D` remapped to `0x0F` — matching the
/// reference `match_color`.
pub fn match_nes_color(r: u8, g: u8, b: u8) -> u8 {
    let mut diff: i32 = 0xFFFFFF;
    let mut color: usize = 0;
    for (i, rgb) in COLORS_FULL.iter().enumerate() {
        let r2 = ((rgb >> 16) & 0xFF) as i32;
        let g2 = ((rgb >> 8) & 0xFF) as i32;
        let b2 = (rgb & 0xFF) as i32;
        let dr = (r2 - r as i32) as f64;
        let dg = (g2 - g as i32) as f64;
        let db = (b2 - b as i32) as f64;
        let next_diff = (dr * dr + dg * dg + db * db).sqrt() as i32;
        if next_diff < diff {
            diff = next_diff;
            color = i;
        }
    }
    if color == 0x0D {
        0x0F
    } else {
        color as u8
    }
}
