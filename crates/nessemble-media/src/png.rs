//! PNG decoding and the `.incpng` (→ CHR tiles) / `.incpal` (→ palette)
//! conversions. Mirrors the reference `png.c` + `pseudo/incpng.c` / `incpal.c`.

use crate::color::{match_nes_color, two_bit_color};

/// A decoded PNG as 8-bit RGB, row-major (`(y * width + x) * 3`), matching what
/// the reference gets from `stbi_load(..., 3)`.
pub struct Png {
    pub width: u32,
    pub height: u32,
    /// `width * height * 3` bytes.
    pub rgb: Vec<u8>,
}

/// The single failure mode surfaced to the assembler (matching the reference's
/// generic "Could not load PNG").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PngError;

impl Png {
    #[inline]
    fn pixel(&self, x: u32, y: u32) -> (u8, u8, u8) {
        let i = ((y * self.width + x) * 3) as usize;
        (self.rgb[i], self.rgb[i + 1], self.rgb[i + 2])
    }
}

/// Decode PNG bytes to forced-RGB pixels (dropping any alpha), like the
/// reference's `stbi_load` with three requested components.
pub fn decode_png(bytes: &[u8]) -> Result<Png, PngError> {
    let img = image::load_from_memory(bytes).map_err(|_| PngError)?;
    let rgb = img.to_rgb8();
    Ok(Png {
        width: rgb.width(),
        height: rgb.height(),
        rgb: rgb.into_raw(),
    })
}

/// A decoded PNG as 8-bit RGBA, row-major (`(y * width + x) * 4`), preserving
/// the alpha channel. Used by the scripting host's `decode_png`, which exposes
/// full RGBA pixels (unlike [`decode_png`], which forces RGB for `.incpng`).
pub struct PngRgba {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, in `R, G, B, A` order.
    pub rgba: Vec<u8>,
}

/// Decode PNG bytes to 8-bit RGBA pixels, keeping the alpha channel.
pub fn decode_png_rgba(bytes: &[u8]) -> Result<PngRgba, PngError> {
    let img = image::load_from_memory(bytes).map_err(|_| PngError)?;
    let rgba = img.to_rgba8();
    Ok(PngRgba {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
    })
}

/// Convert a PNG to CHR tiles (`.incpng`): each 8×8 tile becomes two bitplanes
/// (low then high) of 8 bytes. `offset` skips leading tiles; `limit` (`None` =
/// all) caps how many tiles after `offset` are emitted.
#[must_use]
pub fn png_to_tiles(png: &Png, offset: i32, limit: Option<i32>) -> Vec<u8> {
    let mut out = Vec::new();
    let mut tile_index: i32 = -1;
    let mut h = 0;
    while h < png.height {
        let mut w = 0;
        while w < png.width {
            tile_index += 1;
            let skip = tile_index < offset || limit.is_some_and(|l| tile_index - offset >= l);
            if !skip {
                emit_plane(png, &mut out, h, w, 0);
                emit_plane(png, &mut out, h, w, 1);
            }
            w += 8;
        }
        h += 8;
    }
    out
}

/// Emit one 8-byte bitplane (`bit` = 0 low, 1 high) of the tile at `(w, h)`.
fn emit_plane(png: &Png, out: &mut Vec<u8>, h: u32, w: u32, bit: u32) {
    for y in h..h + 8 {
        let mut byte = 0u8;
        for x in w..w + 8 {
            let (r, g, b) = png.pixel(x, y);
            let color = two_bit_color(r, g, b);
            byte |= (((color as u32 >> bit) & 0x01) as u8) << (7 - (x % 8));
        }
        out.push(byte);
    }
}

/// Convert a PNG to a palette (`.incpal`): scan pixels row-major, emitting each
/// nearest-NES-color when it differs from the previous one, up to four entries.
#[must_use]
pub fn png_to_palette(png: &Png) -> Vec<u8> {
    let mut out = Vec::new();
    let mut last_color: i32 = -1;
    for y in 0..png.height {
        for x in 0..png.width {
            if out.len() >= 4 {
                return out;
            }
            let (r, g, b) = png.pixel(x, y);
            let color = match_nes_color(r, g, b) as i32;
            if color != last_color {
                out.push(color as u8);
                last_color = color;
            }
        }
    }
    out
}
