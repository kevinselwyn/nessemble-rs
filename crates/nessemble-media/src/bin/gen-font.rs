//! Regenerate `src/font.chr` from `src/font.png`.
//!
//! `font.chr` is the bundled 8×8 font glyph data used by `.font`, produced by
//! running the `.incpng` conversion over the upstream `font.png` (the reference
//! project generates the same data with `utils/img2chr.py`). Run with:
//!
//! ```sh
//! cargo run -p nessemble-media --bin gen-font
//! ```

use std::path::Path;

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let png_bytes = std::fs::read(dir.join("font.png")).expect("read font.png");
    let png = nessemble_media::decode_png(&png_bytes).expect("decode font.png");
    let chr = nessemble_media::png_to_tiles(&png, 0, None);
    std::fs::write(dir.join("font.chr"), &chr).expect("write font.chr");
    println!("wrote {} bytes to src/font.chr", chr.len());
}
