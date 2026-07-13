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
        engine.register_fn("path", move |p: &str| -> PathBuf {
            let path = PathBuf::from(p);
            if path.is_relative() {
                base.join(path)
            } else {
                path
            }
        });
    }
    #[cfg(not(feature = "fs"))]
    let _ = base_dir;

    // PNG decoding for scripts: `decode_png(blob)` → a map of width/height and
    // interleaved RGBA pixels (typically fed an `open_file(...).read_blob()`).
    engine.register_fn("decode_png", decode_png);
    engine
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
}
