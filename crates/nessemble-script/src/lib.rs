//! Custom pseudo-op scripting host for `nessemble-rs`, built on **Rhai**.
//!
//! A script defines a `custom(ints, texts)` function that receives the numeric
//! and string arguments of a custom directive (e.g. `.sum 1, 2, 3` or
//! `.ease "easeInQuad"`) and returns the bytes to emit. This single pure-Rust
//! engine replaces the reference tool's JS/Lua/Scheme trio.
//!
//! # Host API (for script authors)
//!
//! - Define `fn custom(ints, texts) { … }`.
//! - `ints` is an array of integers (the numeric arguments); `texts` is an array
//!   of strings (the quoted-string arguments, with quotes already removed).
//! - Return the emitted bytes as an array of integers (each taken `& 0xFF`) or a
//!   blob. Returning `()` emits nothing.
//! - Signal an error with `throw "message"`; the message becomes the assembler
//!   diagnostic.
//! - Scripts may read and write files via the [`rhai-fs`](https://docs.rs/rhai-fs)
//!   package (`open_file`, `File#read_string`, `File#read_blob`, `File#write`,
//!   …), so a directive can pull bytes from disk. Because of this, pseudo-op
//!   scripts are **not** sandboxed from the filesystem — run only ones you trust.
//!
//! # Built-in helpers
//!
//! Beyond the Rhai standard library (arrays with `+=`/`append`/`extract`, string
//! indexing, `abs`, …), the host registers:
//!
//! - `read_blob(path)` / `decode_png_file(path)` — read (and decode) a file in
//!   one call, resolving relative paths like `open_file` (feature `fs`).
//! - `decode_png(blob)` → `#{ width, height, pixels }` and pixel accessors over
//!   that map: `img.r(x, y)`, `img.pixel(x, y)`, `img.tile(col, row, tw, th)`.
//! - `quantize(value, thresholds)` (also over an array of values) and
//!   `nes_shade(value)` (the NES 4-shade case; also over an array) to snap a
//!   grayscale value to a palette index.

use std::path::Path;
#[cfg(feature = "fs")]
use std::path::PathBuf;

#[cfg(feature = "fs")]
use rhai::packages::Package;
use rhai::{Array, Blob, Dynamic, Engine, EvalAltResult, Map};
#[cfg(feature = "fs")]
use rhai_fs::FilesystemPackage;

/// Run `source`'s `custom(ints, texts)` function and return the emitted bytes,
/// or a human-readable error message (a thrown string, or an engine error).
///
/// A relative path opened by the script (via rhai-fs's `open_file`) resolves
/// against `base_dir` — the directory of the source file that contains the
/// directive — matching how `.include` and the `.inc*` importers resolve paths.
/// Absolute paths are used as-is.
pub fn run(
    source: &str,
    ints: &[i64],
    texts: &[String],
    base_dir: &Path,
) -> Result<Vec<u8>, String> {
    let engine = engine(base_dir);
    let ast = engine.compile(source).map_err(|e| e.to_string())?;

    let int_arr: Array = ints.iter().map(|&i| Dynamic::from(i)).collect();
    let text_arr: Array = texts.iter().map(|t| Dynamic::from(t.clone())).collect();

    let mut scope = rhai::Scope::new();
    let result: Dynamic = engine
        .call_fn(&mut scope, &ast, "custom", (int_arr, text_arr))
        .map_err(|e| error_message(&e))?;

    dynamic_to_bytes(result)
}

/// A resource-guarded engine with filesystem access.
///
/// The [`FilesystemPackage`] from `rhai-fs` is registered so scripts can read
/// and write files (e.g. `open_file`, `File#read_string`, `File#write`), which
/// lets a custom directive pull bytes from disk rather than only computing them.
///
/// The runaway-script guards (operation/recursion/size limits) still apply, but
/// **filesystem access means scripts are no longer sandboxed** — a directive can
/// touch any path the assembler process can. Only run pseudo-op scripts you
/// trust, the same as any build tooling.
fn engine(base_dir: &Path) -> Engine {
    let mut engine = Engine::new();
    engine.set_max_operations(10_000_000);
    engine.set_max_call_levels(64);
    // Leave the string/array size limits unbounded (0): rhai-fs's `read_string`
    // / `read_blob` (with no explicit length) fill a buffer sized to these
    // limits, so a finite cap would pad a whole-file read out to the cap. A
    // whole file must come back as exactly its bytes; runaway *compute* is still
    // bounded by the operation and call-depth limits above.
    engine.set_max_string_size(0);
    engine.set_max_array_size(0);
    // Allow deeply-nested arithmetic expressions (e.g. easing polynomials).
    engine.set_max_expr_depths(0, 0);
    // Filesystem access (`open_file`, `File` I/O) for scripts that read/write
    // assets on disk. Compiled out on filesystem-less targets (feature `fs`
    // off), where the file API is simply absent.
    #[cfg(feature = "fs")]
    {
        FilesystemPackage::new().register_into_engine(&mut engine);
        // Root the script's relative paths at the directive's source directory,
        // overriding rhai-fs's default (which resolves against the process CWD).
        // rhai-fs turns a path string into a `PathBuf` via this `path` function,
        // so redefining it reroutes every relative `open_file`/`open_dir`.
        let base = base_dir.to_path_buf();
        engine.register_fn("path", {
            let base = base.clone();
            move |p: &str| -> PathBuf { resolve(&base, p) }
        });
        // `read_blob(path)` — read a whole file as a blob, resolving relative
        // paths against the source directory (same rooting as `open_file`). Saves
        // the `open_file(path, "r").read_blob()` handle/mode ceremony.
        engine.register_fn("read_blob", {
            let base = base.clone();
            move |p: &str| -> Result<Blob, Box<EvalAltResult>> {
                let full = resolve(&base, p);
                std::fs::read(&full).map_err(|e| -> Box<EvalAltResult> {
                    format!("read_blob: cannot read {}: {e}", full.display()).into()
                })
            }
        });
        // `decode_png_file(path)` — read and decode a PNG in one call
        // (`decode_png(read_blob(path))`).
        engine.register_fn("decode_png_file", {
            let base = base.clone();
            move |p: &str| -> Result<Map, Box<EvalAltResult>> {
                let full = resolve(&base, p);
                let bytes = std::fs::read(&full).map_err(|e| -> Box<EvalAltResult> {
                    format!("decode_png_file: cannot read {}: {e}", full.display()).into()
                })?;
                decode_png(bytes)
            }
        });
    }
    #[cfg(not(feature = "fs"))]
    let _ = base_dir;

    // PNG decoding for scripts: `decode_png(blob)` → a map of width/height and
    // interleaved RGBA pixels (typically fed an `open_file(...).read_blob()`).
    engine.register_fn("decode_png", decode_png);

    // Pixel/tile accessors over a `decode_png` map, so scripts don't recompute
    // `(y * width + x) * 4` offsets by hand:
    //   `img.r(x, y)`            → the pixel's red channel (grayscale value)
    //   `img.pixel(x, y)`        → `[r, g, b, a]`
    //   `img.tile(col, row, w, h)` → the w×h block's red channels, row-major
    engine.register_fn("r", img_channel_r);
    engine.register_fn("pixel", img_pixel);
    engine.register_fn("tile", img_tile);

    // Palette quantization. `quantize(value, thresholds)` counts how many
    // ascending `thresholds` `value` reaches (also accepts an array of values);
    // `nes_shade(value)` is the NES 4-shade case with thresholds [43, 128, 213]
    // (also accepts an array).
    engine.register_fn("quantize", quantize_int);
    engine.register_fn("quantize", quantize_arr);
    engine.register_fn("nes_shade", nes_shade_scalar);
    engine.register_fn("nes_shade", nes_shade_arr);
    engine
}

/// Resolve a script-supplied path against the directive's source directory:
/// relative paths join `base`, absolute paths are used as-is.
#[cfg(feature = "fs")]
fn resolve(base: &Path, p: &str) -> PathBuf {
    let path = PathBuf::from(p);
    if path.is_relative() {
        base.join(path)
    } else {
        path
    }
}

/// `decode_png(blob)` — decode PNG bytes (e.g. from `open_file(path).read_blob()`)
/// into `#{ width: int, height: int, pixels: [r, g, b, a, …] }`, where `pixels`
/// holds `width * height * 4` integers in row-major RGBA order. Throws if the
/// blob is not a valid PNG.
// Rhai's `register_fn` takes the argument by value (or `&mut`); owned lets it be
// called uniformly on variables, temporaries, and constants.
#[allow(clippy::needless_pass_by_value)]
fn decode_png(blob: Blob) -> Result<Map, Box<EvalAltResult>> {
    let img = nessemble_media::decode_png_rgba(&blob)
        .map_err(|_| -> Box<EvalAltResult> { "decode_png: input is not a valid PNG".into() })?;
    let pixels: Array = img
        .rgba
        .iter()
        .map(|&b| Dynamic::from(i64::from(b)))
        .collect();
    let mut map = Map::new();
    map.insert("width".into(), Dynamic::from(i64::from(img.width)));
    map.insert("height".into(), Dynamic::from(i64::from(img.height)));
    map.insert("pixels".into(), Dynamic::from(pixels));
    Ok(map)
}

/// Read the `width`/`height` integer fields of a `decode_png`-style map.
fn img_dims(img: &Map) -> Result<(i64, i64), Box<EvalAltResult>> {
    let field = |k: &str| -> Result<i64, Box<EvalAltResult>> {
        img.get(k)
            .and_then(|d| d.as_int().ok())
            .ok_or_else(|| -> Box<EvalAltResult> {
                format!("image map is missing integer field `{k}`").into()
            })
    };
    Ok((field("width")?, field("height")?))
}

/// Borrow a map's `pixels` array (without cloning it) and run `f` over it. The
/// closure form lets callers hold the read-lock guard without naming its type.
fn with_pixels<T>(
    img: &Map,
    f: impl FnOnce(&Array) -> Result<T, Box<EvalAltResult>>,
) -> Result<T, Box<EvalAltResult>> {
    let pixels = img
        .get("pixels")
        .and_then(rhai::Dynamic::read_lock::<Array>)
        .ok_or_else(|| -> Box<EvalAltResult> {
            "image map is missing an array `pixels` field".into()
        })?;
    f(&pixels)
}

/// Read one RGBA byte from a pixel array, mapping a short array to a clear error.
fn pixel_byte(pixels: &Array, idx: usize) -> Result<i64, Box<EvalAltResult>> {
    pixels
        .get(idx)
        .and_then(|d| d.as_int().ok())
        .ok_or_else(|| -> Box<EvalAltResult> { "image `pixels` array is truncated".into() })
}

/// `img.r(x, y)` — the red channel of pixel `(x, y)`. Images used by these
/// scripts are grayscale (R == G == B), so this is the shade value.
fn img_channel_r(img: &mut Map, x: i64, y: i64) -> Result<i64, Box<EvalAltResult>> {
    let (w, h) = img_dims(img)?;
    if x < 0 || y < 0 || x >= w || y >= h {
        return Err(format!("pixel ({x}, {y}) is out of bounds for {w}x{h} image").into());
    }
    with_pixels(img, |pixels| pixel_byte(pixels, ((y * w + x) * 4) as usize))
}

/// `img.pixel(x, y)` — the pixel as a `[r, g, b, a]` array.
fn img_pixel(img: &mut Map, x: i64, y: i64) -> Result<Array, Box<EvalAltResult>> {
    let (w, h) = img_dims(img)?;
    if x < 0 || y < 0 || x >= w || y >= h {
        return Err(format!("pixel ({x}, {y}) is out of bounds for {w}x{h} image").into());
    }
    let base = ((y * w + x) * 4) as usize;
    with_pixels(img, |pixels| {
        let mut out = Array::with_capacity(4);
        for k in 0..4 {
            out.push(Dynamic::from(pixel_byte(pixels, base + k)?));
        }
        Ok(out)
    })
}

/// `img.tile(col, row, tw, th)` — the `tw`×`th` block at tile coordinate
/// `(col, row)` as a flat, row-major array of red-channel (shade) values. Pairs
/// with `nes_shade`/`quantize` to turn a block into palette indices in one line.
fn img_tile(
    img: &mut Map,
    col: i64,
    row: i64,
    tw: i64,
    th: i64,
) -> Result<Array, Box<EvalAltResult>> {
    if tw <= 0 || th <= 0 {
        return Err(format!("tile size must be positive, got {tw}x{th}").into());
    }
    let (w, h) = img_dims(img)?;
    let (x0, y0) = (col * tw, row * th);
    if col < 0 || row < 0 || x0 + tw > w || y0 + th > h {
        return Err(format!(
            "tile ({col}, {row}) of size {tw}x{th} is out of bounds for {w}x{h} image"
        )
        .into());
    }
    with_pixels(img, |pixels| {
        let mut out = Array::with_capacity((tw * th) as usize);
        for py in 0..th {
            for px in 0..tw {
                let idx = (((y0 + py) * w + (x0 + px)) * 4) as usize;
                out.push(Dynamic::from(pixel_byte(pixels, idx)?));
            }
        }
        Ok(out)
    })
}

/// Count how many ascending `thresholds` `value` reaches — the palette index for
/// a value snapped to bands delimited by `thresholds`.
fn quantize_scalar(value: i64, thresholds: &Array) -> Result<i64, Box<EvalAltResult>> {
    let mut idx = 0;
    for t in thresholds {
        let tv = t.as_int().map_err(|ty| -> Box<EvalAltResult> {
            format!("quantize: threshold must be an integer, got {ty}").into()
        })?;
        if value >= tv {
            idx += 1;
        }
    }
    Ok(idx)
}

/// `quantize(value, thresholds)` — palette index for a single value.
#[allow(clippy::needless_pass_by_value)]
fn quantize_int(value: i64, thresholds: Array) -> Result<i64, Box<EvalAltResult>> {
    quantize_scalar(value, &thresholds)
}

/// `quantize(values, thresholds)` — palette index for each value in an array.
#[allow(clippy::needless_pass_by_value)]
fn quantize_arr(values: Array, thresholds: Array) -> Result<Array, Box<EvalAltResult>> {
    let mut out = Array::with_capacity(values.len());
    for v in &values {
        let vi = v.as_int().map_err(|ty| -> Box<EvalAltResult> {
            format!("quantize: value must be an integer, got {ty}").into()
        })?;
        out.push(Dynamic::from(quantize_scalar(vi, &thresholds)?));
    }
    Ok(out)
}

/// Midpoint thresholds between the four NES shades (0, 85, 170, 255).
const NES_SHADE_THRESHOLDS: [i64; 3] = [43, 128, 213];

fn nes_shade_of(value: i64) -> i64 {
    NES_SHADE_THRESHOLDS.iter().filter(|&&t| value >= t).count() as i64
}

/// `nes_shade(value)` — snap a grayscale value to NES palette index 0–3.
fn nes_shade_scalar(value: i64) -> i64 {
    nes_shade_of(value)
}

/// `nes_shade(values)` — snap each value in an array to NES palette index 0–3.
#[allow(clippy::needless_pass_by_value)]
fn nes_shade_arr(values: Array) -> Result<Array, Box<EvalAltResult>> {
    let mut out = Array::with_capacity(values.len());
    for v in &values {
        let vi = v.as_int().map_err(|ty| -> Box<EvalAltResult> {
            format!("nes_shade: value must be an integer, got {ty}").into()
        })?;
        out.push(Dynamic::from(nes_shade_of(vi)));
    }
    Ok(out)
}

/// Convert a script's return value into emitted bytes.
fn dynamic_to_bytes(value: Dynamic) -> Result<Vec<u8>, String> {
    if value.is_unit() {
        return Ok(Vec::new());
    }
    if value.is_blob() {
        let blob: Blob = value.cast();
        return Ok(blob);
    }
    if value.is_string() {
        // A returned string emits its bytes (like the reference Lua host).
        return Ok(value.into_string().unwrap_or_default().into_bytes());
    }
    if value.is_array() {
        let arr: Array = value.cast();
        let mut out = Vec::with_capacity(arr.len());
        for elem in arr {
            let n = elem
                .as_int()
                .map_err(|t| format!("custom() returned a `{t}` element, expected an integer"))?;
            out.push((n & 0xFF) as u8);
        }
        return Ok(out);
    }
    if let Ok(n) = value.as_int() {
        return Ok(vec![(n & 0xFF) as u8]);
    }
    Err("custom() must return an array of bytes, a blob, or a string".to_string())
}

/// Extract a diagnostic message from an engine error, preferring the raw string
/// of a `throw`n value (matching the reference, which surfaces the script's own
/// error text). Function-call wrappers are unwrapped so a `throw` inside a
/// helper still surfaces its bare message.
fn error_message(err: &EvalAltResult) -> String {
    match err {
        EvalAltResult::ErrorInFunctionCall(_, _, inner, _) => error_message(inner),
        EvalAltResult::ErrorRuntime(value, _) if value.is_string() => {
            value.clone().into_string().unwrap_or_default()
        }
        EvalAltResult::ErrorRuntime(value, _) => value.to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Base directory for scripts that don't touch the filesystem.
    fn cwd() -> &'static Path {
        Path::new(".")
    }

    #[test]
    fn sums_integer_arguments() {
        let src = "fn custom(ints, texts) { let s = 0; for i in ints { s += i; } [s % 256] }";
        assert_eq!(run(src, &[1, 2, 3], &[], cwd()).unwrap(), vec![6]);
    }

    #[test]
    fn float_math_matches_expectations() {
        // Integer args used in float math must be converted explicitly.
        let src = "fn custom(ints, texts) { \
                   let t = ints[0].to_float() / ints[1].to_float(); \
                   [(t * 16.0).floor().to_int() % 256] }";
        // (3 / 4) * 16 = 12
        assert_eq!(run(src, &[3, 4], &[], cwd()).unwrap(), vec![12]);
    }

    #[test]
    fn thrown_string_becomes_the_error() {
        let src = "fn custom(ints, texts) { throw \"bad thing\" }";
        assert_eq!(run(src, &[], &[], cwd()).unwrap_err(), "bad thing");
    }

    #[test]
    fn receives_string_arguments() {
        let src = "fn custom(ints, texts) { texts[0].to_blob() }";
        assert_eq!(run(src, &[], &["Hi".to_string()], cwd()).unwrap(), b"Hi");
    }

    /// A unique, freshly-created directory in the OS temp area, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicU32, Ordering};
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir().join(format!(
                "nessemble-script-{tag}-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn reads_a_file_relative_to_the_base_dir() {
        // A bare relative path resolves against `base_dir`, and rhai-fs returns
        // the file's bytes verbatim.
        let dir = TempDir::new("read");
        std::fs::write(dir.0.join("asset.bin"), b"\x01\x02\x03NES").unwrap();
        let src = r#"fn custom(ints, texts) { open_file("asset.bin", "r").read_blob() }"#;
        assert_eq!(run(src, &[], &[], &dir.0).unwrap(), b"\x01\x02\x03NES");
    }

    #[test]
    fn reads_a_named_file_as_a_string() {
        let dir = TempDir::new("read-str");
        std::fs::write(dir.0.join("note.txt"), b"hello").unwrap();
        let src = r#"fn custom(ints, texts) { open_file(texts[0], "r").read_string().to_blob() }"#;
        assert_eq!(
            run(src, &[], &["note.txt".to_string()], &dir.0).unwrap(),
            b"hello"
        );
    }

    #[test]
    fn writes_a_file_relative_to_the_base_dir() {
        // A script can also write: `open_file(path)` opens read/write (creating
        // or truncating), and `File#write` persists the bytes.
        let dir = TempDir::new("write");
        let src = r#"fn custom(ints, texts) { open_file("out.bin").write("ok"); () }"#;
        let out = run(src, &[], &[], &dir.0).unwrap();
        assert_eq!(out, Vec::<u8>::new());
        assert_eq!(std::fs::read(dir.0.join("out.bin")).unwrap(), b"ok");
    }

    #[test]
    fn absolute_paths_bypass_the_base_dir() {
        // An absolute path is used as-is, regardless of `base_dir`.
        let dir = TempDir::new("abs");
        let file = dir.0.join("data.bin");
        std::fs::write(&file, b"ABS").unwrap();
        let src = r#"fn custom(ints, texts) { open_file(texts[0], "r").read_blob() }"#;
        // `base_dir` is an unrelated directory; the absolute path still resolves.
        let out = run(
            src,
            &[],
            &[file.to_string_lossy().into_owned()],
            Path::new("/nonexistent-base"),
        );
        assert_eq!(out.unwrap(), b"ABS");
    }

    /// Encode an RGBA image to PNG bytes for the `decode_png` tests.
    fn png_bytes(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
        use image::ImageEncoder;
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(rgba, width, height, image::ExtendedColorType::Rgba8)
            .unwrap();
        out
    }

    #[test]
    fn decode_png_exposes_dimensions_and_rgba_pixels() {
        // The documented flow: open a PNG, read its bytes, and decode them into
        // `#{ width, height, pixels: [r, g, b, a, …] }`.
        let dir = TempDir::new("png");
        let png = png_bytes(2, 1, &[10, 20, 30, 40, 50, 60, 70, 80]);
        std::fs::write(dir.0.join("img.png"), &png).unwrap();

        let src = r#"
            fn custom(ints, texts) {
                let img = decode_png(open_file("img.png", "r").read_blob());
                let out = [img.width, img.height];
                out += img.pixels;
                out
            }
        "#;
        // width, height, then the two pixels' RGBA bytes.
        assert_eq!(
            run(src, &[], &[], &dir.0).unwrap(),
            vec![2, 1, 10, 20, 30, 40, 50, 60, 70, 80]
        );
    }

    #[test]
    fn decode_png_rejects_a_non_png_blob() {
        let dir = TempDir::new("png-bad");
        std::fs::write(dir.0.join("bad.png"), b"not a png").unwrap();
        let src = r#"fn custom(ints, texts) { decode_png(open_file("bad.png", "r").read_blob()) }"#;
        let err = run(src, &[], &[], &dir.0).unwrap_err();
        assert!(err.contains("not a valid PNG"), "unexpected error: {err}");
    }

    #[test]
    fn read_blob_and_decode_png_file_are_one_call_conveniences() {
        let dir = TempDir::new("read-blob");
        std::fs::write(dir.0.join("asset.bin"), b"\x01\x02\x03").unwrap();
        let png = png_bytes(1, 1, &[9, 8, 7, 255]);
        std::fs::write(dir.0.join("img.png"), &png).unwrap();

        // read_blob(path) == open_file(path, "r").read_blob()
        let src = r#"fn custom(ints, texts) { read_blob("asset.bin") }"#;
        assert_eq!(run(src, &[], &[], &dir.0).unwrap(), b"\x01\x02\x03");

        // decode_png_file(path) == decode_png(read_blob(path))
        let src = r#"
            fn custom(ints, texts) {
                let img = decode_png_file("img.png");
                [img.width, img.height, img.r(0, 0)]
            }
        "#;
        assert_eq!(run(src, &[], &[], &dir.0).unwrap(), vec![1, 1, 9]);
    }

    #[test]
    fn image_pixel_accessors_read_channels_and_tiles() {
        let dir = TempDir::new("img-accessors");
        // 2x2 image, one distinct grayscale value per pixel so offsets are visible.
        #[rustfmt::skip]
        let rgba = [
            10, 10, 10, 255,   20, 20, 20, 255,
            30, 30, 30, 255,   40, 40, 40, 255,
        ];
        std::fs::write(dir.0.join("g.png"), png_bytes(2, 2, &rgba)).unwrap();
        let src = r#"
            fn custom(ints, texts) {
                let img = decode_png_file("g.png");
                let out = [];
                out += img.pixel(1, 0);      // [20,20,20,255]
                out.push(img.r(0, 1));       // 30
                out += img.tile(0, 0, 2, 2); // whole image: [10,20,30,40]
                out
            }
        "#;
        assert_eq!(
            run(src, &[], &[], &dir.0).unwrap(),
            vec![20, 20, 20, 255, 30, 10, 20, 30, 40]
        );
    }

    #[test]
    fn out_of_bounds_pixel_access_is_an_error() {
        let dir = TempDir::new("img-oob");
        std::fs::write(dir.0.join("g.png"), png_bytes(1, 1, &[0, 0, 0, 255])).unwrap();
        let src = r#"fn custom(ints, texts) { [decode_png_file("g.png").r(5, 0)] }"#;
        let err = run(src, &[], &[], &dir.0).unwrap_err();
        assert!(err.contains("out of bounds"), "unexpected error: {err}");
    }

    #[test]
    fn quantize_and_nes_shade_snap_to_palette_indices() {
        // nes_shade uses thresholds [43, 128, 213]; scalar and array forms.
        let src = r"
            fn custom(ints, texts) {
                [
                    nes_shade(0), nes_shade(100), nes_shade(200), nes_shade(255),
                    quantize(100, [43, 128, 213]),
                ] + nes_shade([0, 50, 130, 220])
            }
        ";
        assert_eq!(
            run(src, &[], &[], cwd()).unwrap(),
            vec![0, 1, 2, 3, 1, 0, 1, 2, 3]
        );
    }

    #[test]
    fn rhai_stdlib_supports_the_refactor_helpers() {
        // Some custom pseudo-op scripts lean on stock Rhai (no host builtin
        // needed) for abs(), array `+=` / `extract`, and string indexing. Guard
        // those here so a feature-flag change that drops them is caught.
        let src = r#"
            fn custom(ints, texts) {
                let out = [];
                out += [1, 2, 3];                 // array extend via +=
                out += [9, 8, 7].extract(1, 2);   // sub-array [8, 7]
                out.push(abs(-4));                 // abs
                let s = "AB";
                out.push(s[1].to_int());           // string indexing -> 'B' = 66
                out
            }
        "#;
        assert_eq!(
            run(src, &[], &[], cwd()).unwrap(),
            vec![1, 2, 3, 8, 7, 4, 66]
        );
    }
}
