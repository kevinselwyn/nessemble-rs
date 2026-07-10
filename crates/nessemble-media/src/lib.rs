//! Asset importers for `nessemble-rs`: PNG→CHR, palettes, RLE, and WAV→DPCM.
//!
//! Implemented in Phase 5. This is a placeholder crate that reserves the seam
//! in the workspace.

/// Marker for planned importer kinds, kept so the public surface is stable while
/// the implementations land in Phase 5.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Importer {
    /// `.incbin` — raw binary include.
    Binary,
    /// `.incpng` — PNG → CHR tiles.
    Png,
    /// `.incpal` — palette include.
    Palette,
    /// `.incrle` — run-length-encoded include.
    Rle,
    /// `.incwav` — WAV → DPCM.
    Wav,
}
