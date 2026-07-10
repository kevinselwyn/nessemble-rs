//! Asset importers for `nessemble-rs`: raw binary, PNG→CHR, palette matching,
//! RLE, WAV→DPCM, and the bundled font.
//!
//! Each importer reproduces the reference tool's algorithm exactly (see the
//! upstream `pseudo/*.c`, `png.c`, and `wav.c`) and returns the emitted bytes
//! as a `Vec<u8>`; the caller (the assembler) writes them through its normal
//! `write_byte` path so banking, coverage, and conditional gating still apply.

mod color;
mod png;
mod rle;
mod wav;

pub use color::{match_nes_color, two_bit_color};
pub use png::{decode_png, png_to_palette, png_to_tiles, Png, PngError};
pub use rle::rle_encode;
pub use wav::{wav_to_dpcm, WavError};

/// The bundled 8×8 font as CHR data (128 glyphs × 16 bytes), used by `.font`.
///
/// This is the byte-for-byte output of the reference `img2chr.py` conversion
/// (identical to the `.incpng` algorithm) over the upstream `font.png`; it is
/// committed as data so the build needs no image at compile time. See
/// `src/bin/gen-font.rs` in this crate for regeneration.
static FONT_CHR: &[u8] = include_bytes!("font.chr");

/// The 16 CHR bytes for ASCII glyph `ch` (`0x00..=0x7F`).
///
/// Returns an empty slice for code points outside the table.
pub fn font_glyph(ch: usize) -> &'static [u8] {
    let start = ch * 0x10;
    FONT_CHR.get(start..start + 0x10).unwrap_or(&[])
}

/// Extract the bytes of `data[offset..limit]` for `.incbin`.
///
/// `limit` is an absolute end index (matching the reference); `None` means the
/// end of the data. Out-of-range bounds are clamped.
pub fn incbin_slice(data: &[u8], offset: usize, limit: Option<usize>) -> Vec<u8> {
    let end = limit.unwrap_or(data.len()).min(data.len());
    let start = offset.min(end);
    data[start..end].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incbin_offset_and_limit() {
        let data = b"0123456789";
        assert_eq!(incbin_slice(data, 0, None), b"0123456789");
        // `limit` is an absolute end index, not a count.
        assert_eq!(incbin_slice(data, 0, Some(3)), b"012");
        assert_eq!(incbin_slice(data, 7, None), b"789");
        // Clamped bounds never panic.
        assert_eq!(incbin_slice(data, 20, Some(30)), b"");
    }

    #[test]
    fn font_table_is_128_glyphs() {
        assert_eq!(FONT_CHR.len(), 128 * 0x10);
        assert_eq!(font_glyph(0x41).len(), 0x10);
        assert!(font_glyph(0x200).is_empty());
    }

    #[test]
    fn wav_rejects_non_wav_and_short() {
        assert_eq!(wav_to_dpcm(b"xx", 24), Err(WavError::ShortRead));
        assert_eq!(wav_to_dpcm(b"NOTAWAVEHDR!", 24), Err(WavError::NotWav));
    }

    #[test]
    fn rle_round_trip_shape() {
        // A run of >2 identical bytes encodes as count/value; the stream ends
        // with 0xFF.
        assert_eq!(rle_encode(&[0xAA; 5]), vec![0x05, 0xAA, 0xFF]);
        // One or two distinct bytes buffer as a literal run (0x80 | len).
        assert_eq!(rle_encode(&[0x01, 0x02]), vec![0x82, 0x01, 0x02, 0xFF]);
    }
}
