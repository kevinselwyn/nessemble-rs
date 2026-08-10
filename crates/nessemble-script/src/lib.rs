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
//! - `decode_png(blob)` → an opaque image handle with `img.width`, `img.height`,
//!   `img.pixels` and the accessors `img.r(x, y)`, `img.pixel(x, y)`,
//!   `img.tile(col, row, tw, th)`. Copying the handle (passing it to a function,
//!   assigning it) is a refcount bump, not a copy of the pixels.
//! - Cell matching against a sheet of equally-sized cells:
//!   `bank.find_cell(src, col, row, w, h)`,
//!   `bank.cell_equals(index, src, col, row, w, h)` and
//!   `bank.nearest_cell(src, col, row, w, h)`, all comparing NES shade indices.
//! - `quantize(value, thresholds)` (also over an array of values) and
//!   `nes_shade(value)` (the NES 4-shade case; also over an array) to snap a
//!   grayscale value to a palette index.
//! - `parse_xml(source)` / `parse_xml_file(path)` (feature `fs` for the file
//!   form) — a dependency-free XML parser ([`xml`]) returning a root `xml_node`
//!   handle with `.name`, `.attrs`, `.attr(name)`, `.children`, `.text`,
//!   `.find(name)` and `.find_all(name)`. No namespaces, no DTDs (a `<!DOCTYPE`
//!   is a parse error), entities limited to the five predefined ones plus
//!   numeric character references. See `plans/013-structured-data-parsing.md`.
//! - `parse_json(source)` / `parse_json_file(path)` (feature `fs` for the file
//!   form) — parses JSON into native Rhai values (map/array/string/int/float/
//!   bool/`()`) via [`json`].
//! - `parse_int_list(text, delim)` / `parse_int_list(text, delim, radix)` — bulk
//!   numeric decoding: split on a literal delimiter, trim, skip empty fields,
//!   parse each remaining one.
//! - `to_char(value)`, `trimmed()` (a non-mutating `trim()` — the stock one
//!   mutates in place and returns `()`), and `format_hex(value, width)`
//!   (assembly-style `$`-prefixed, zero-padded hex). `parse_int(str, radix)` and
//!   `blob.as_string()` cover the same-named gaps reported against an older Rhai
//!   release — both already exist in the version this crate depends on and need
//!   no host code.
//! - The [`rhai-rand`](https://docs.rs/rhai-rand) package (feature `rand`):
//!   `rand()`, `rand(min, max)`, `rand_float()`, `rand_bool()`, and the array
//!   `shuffle()` / `sample()` helpers, for procedural noise and randomized data
//!   tables. Compiled out on targets without a system entropy source (e.g.
//!   `wasm32-unknown-unknown`), where the functions are absent.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::Path;
#[cfg(feature = "fs")]
use std::path::PathBuf;
#[cfg(not(feature = "fs"))]
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

#[cfg(any(feature = "fs", feature = "rand"))]
use rhai::packages::Package;
use rhai::{Array, Blob, Dynamic, Engine, EvalAltResult, Map};
#[cfg(feature = "fs")]
use rhai_fs::FilesystemPackage;
#[cfg(feature = "rand")]
use rhai_rand::RandomPackage;

#[cfg(feature = "coverage")]
pub mod coverage;
mod json;
pub mod purity;
mod xml;

use xml::XmlNode;

/// Run `source`'s `custom(ints, texts)` function and return the emitted bytes,
/// or a human-readable error message (a thrown string, or an engine error).
///
/// A relative path opened by the script (via rhai-fs's `open_file`) resolves
/// against `base_dir` — the directory of the source file that contains the
/// directive — matching how `.include` and the `.inc*` importers resolve paths.
/// Absolute paths are used as-is. A `@/`-prefixed path is an error (no project
/// root); use [`run_with_root`] to give scripts `@/` support.
pub fn run(
    source: &str,
    ints: &[i64],
    texts: &[String],
    base_dir: &Path,
) -> Result<Vec<u8>, String> {
    run_impl(source, ints, texts, base_dir, None, None, false).map(|(bytes, _)| bytes)
}

/// Like [`run`], but a `@/`-prefixed path the script resolves itself (via
/// `read_blob`, `decode_png_file`, `parse_xml_file`, `parse_json_file`, or
/// rhai-fs's `open_file`) joins against `root` instead of erroring — the same
/// resolution every other path-taking argument gets
/// (`plans/013-structured-data-parsing.md` §11.1). `root` is `None` where no
/// project root could be determined, in which case a `@/` path is still an
/// error, exactly as [`run`] leaves it.
pub fn run_with_root(
    source: &str,
    ints: &[i64],
    texts: &[String],
    base_dir: &Path,
    root: Option<&Path>,
) -> Result<Vec<u8>, String> {
    run_impl(source, ints, texts, base_dir, root, None, false).map(|(bytes, _)| bytes)
}

/// The paths a script resolved through the host's file API, in sorted order.
type Recorder = Rc<RefCell<BTreeSet<PathBuf>>>;

/// What one `custom()` invocation produced, and what it touched.
///
/// `inputs` is what makes caching this run sound: it is every path the script
/// actually resolved through the host's file API on *this* invocation, so a cache
/// can check those files for changes rather than guessing at a dependency set.
/// `cacheable` is `false` when the script did something whose result must not be
/// reused — see [`purity`].
#[derive(Debug, Clone)]
pub struct RunOutcome {
    /// The bytes the directive emits.
    pub bytes: Vec<u8>,
    /// Absolute paths the script resolved through the host's file API.
    pub inputs: Vec<PathBuf>,
    /// Whether these bytes may be reused on a later build.
    pub cacheable: bool,
    /// Why the run is not cacheable, when it is not.
    pub impurity: Option<purity::Impurity>,
}

/// Like [`run`], but also reporting every file the script read and whether the
/// result may be cached ([`RunOutcome`]).
pub fn run_with_inputs(
    source: &str,
    ints: &[i64],
    texts: &[String],
    base_dir: &Path,
) -> Result<RunOutcome, String> {
    run_with_inputs_and_root(source, ints, texts, base_dir, None)
}

/// Like [`run_with_inputs`], with [`run_with_root`]'s `@/` support.
pub fn run_with_inputs_and_root(
    source: &str,
    ints: &[i64],
    texts: &[String],
    base_dir: &Path,
    root: Option<&Path>,
) -> Result<RunOutcome, String> {
    let recorder = Recorder::default();
    let (bytes, impurity) = run_impl(source, ints, texts, base_dir, root, Some(&recorder), true)?;
    let inputs: Vec<PathBuf> = recorder.borrow().iter().cloned().collect();
    Ok(RunOutcome {
        bytes,
        inputs,
        cacheable: impurity.is_none(),
        impurity,
    })
}

/// Compile and call `custom()`, optionally recording the paths it opens and
/// scanning it for the things that make a result unsafe to reuse.
fn run_impl(
    source: &str,
    ints: &[i64],
    texts: &[String],
    base_dir: &Path,
    root: Option<&Path>,
    recorder: Option<&Recorder>,
    scan: bool,
) -> Result<(Vec<u8>, Option<purity::Impurity>), String> {
    let engine = engine_recording(base_dir, root, recorder);
    let ast = engine.compile(source).map_err(|e| e.to_string())?;
    let impurity = if scan { purity::impurity(&ast) } else { None };

    let int_arr: Array = ints.iter().map(|&i| Dynamic::from(i)).collect();
    let text_arr: Array = texts.iter().map(|t| Dynamic::from(t.clone())).collect();

    let mut scope = rhai::Scope::new();
    let result: Dynamic = engine
        .call_fn(&mut scope, &ast, "custom", (int_arr, text_arr))
        .map_err(|e| error_message(&e))?;

    Ok((dynamic_to_bytes(result)?, impurity))
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
fn engine(base_dir: &Path, root: Option<&Path>) -> Engine {
    engine_recording(base_dir, root, None)
}

/// [`engine`], optionally recording every path the script resolves through the
/// host's file API into `recorder` (see [`RunOutcome::inputs`]).
///
/// The registrations below are the *only* routes from a path string to a file:
/// rhai-fs turns every path into a `PathBuf` through `path`, and the shorthands
/// (`read_blob`, `decode_png_file`, `parse_xml_file`, `parse_json_file`) resolve
/// their own. Recording there therefore sees whatever the script opened,
/// including a filename it computed itself — which is why a cache keyed on the
/// result does not need the source to declare its inputs. **Any new host
/// function that opens a file must call `record`/`resolve` here too** — that is
/// the entire correctness argument for the on-disk cache
/// (`plans/011-pseudo-op-caching.md`, `plans/013-structured-data-parsing.md`
/// §3), and there is no separate check that catches an omission.
///
/// `root` is the project root a `@/`-prefixed path resolves against
/// (`plans/013-structured-data-parsing.md` §11.1); `None` where none could be
/// determined, in which case `@/` in a script-resolved path is an error.
fn engine_recording(base_dir: &Path, root: Option<&Path>, recorder: Option<&Recorder>) -> Engine {
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
        let root_buf = root.map(Path::to_path_buf);
        let rec = recorder.cloned();
        engine.register_fn("path", {
            let base = base.clone();
            let root = root_buf.clone();
            let rec = rec.clone();
            move |p: &str| -> Result<PathBuf, Box<EvalAltResult>> {
                let full = resolve(&base, root.as_deref(), p)
                    .map_err(|e| -> Box<EvalAltResult> { e.into() })?;
                Ok(record(rec.as_ref(), full))
            }
        });
        // `read_blob(path)` — read a whole file as a blob, resolving relative
        // paths against the source directory (same rooting as `open_file`). Saves
        // the `open_file(path, "r").read_blob()` handle/mode ceremony.
        engine.register_fn("read_blob", {
            let base = base.clone();
            let root = root_buf.clone();
            let rec = rec.clone();
            move |p: &str| -> Result<Blob, Box<EvalAltResult>> {
                let full = resolve(&base, root.as_deref(), p)
                    .map_err(|e| -> Box<EvalAltResult> { format!("read_blob: {e}").into() })?;
                let full = record(rec.as_ref(), full);
                std::fs::read(&full).map_err(|e| -> Box<EvalAltResult> {
                    format!("read_blob: cannot read {}: {e}", full.display()).into()
                })
            }
        });
        // `decode_png_file(path)` — read and decode a PNG in one call
        // (`decode_png(read_blob(path))`).
        engine.register_fn("decode_png_file", {
            let base = base.clone();
            let root = root_buf.clone();
            let rec = rec.clone();
            move |p: &str| -> Result<Image, Box<EvalAltResult>> {
                let full =
                    resolve(&base, root.as_deref(), p).map_err(|e| -> Box<EvalAltResult> {
                        format!("decode_png_file: {e}").into()
                    })?;
                let full = record(rec.as_ref(), full);
                let bytes = std::fs::read(&full).map_err(|e| -> Box<EvalAltResult> {
                    format!("decode_png_file: cannot read {}: {e}", full.display()).into()
                })?;
                decode_png(bytes)
            }
        });
        // `parse_xml_file(path)` — read and parse an XML document in one call
        // (`parse_xml(read_blob(path).as_string())`, roughly).
        engine.register_fn("parse_xml_file", {
            let base = base.clone();
            let root = root_buf.clone();
            let rec = rec.clone();
            move |p: &str| -> Result<XmlNode, Box<EvalAltResult>> {
                let full = resolve(&base, root.as_deref(), p)
                    .map_err(|e| -> Box<EvalAltResult> { format!("parse_xml_file: {e}").into() })?;
                let full = record(rec.as_ref(), full);
                let bytes = std::fs::read(&full).map_err(|e| -> Box<EvalAltResult> {
                    format!("parse_xml_file: cannot read {}: {e}", full.display()).into()
                })?;
                let text = String::from_utf8(bytes).map_err(|e| -> Box<EvalAltResult> {
                    format!("parse_xml_file: {} is not valid UTF-8: {e}", full.display()).into()
                })?;
                xml::parse(&text).map_err(|e| -> Box<EvalAltResult> {
                    format!("parse_xml_file: {}:{e}", full.display()).into()
                })
            }
        });
        // `parse_json_file(path)` — read and parse a JSON document in one call.
        engine.register_fn("parse_json_file", {
            let base = base.clone();
            let root = root_buf.clone();
            let rec = rec.clone();
            move |p: &str| -> Result<Dynamic, Box<EvalAltResult>> {
                let full =
                    resolve(&base, root.as_deref(), p).map_err(|e| -> Box<EvalAltResult> {
                        format!("parse_json_file: {e}").into()
                    })?;
                let full = record(rec.as_ref(), full);
                let text = std::fs::read_to_string(&full).map_err(|e| -> Box<EvalAltResult> {
                    format!("parse_json_file: cannot read {}: {e}", full.display()).into()
                })?;
                json::parse(&text).map_err(|e| -> Box<EvalAltResult> {
                    format!("parse_json_file: {}: {e}", full.display()).into()
                })
            }
        });
    }
    #[cfg(not(feature = "fs"))]
    {
        let _ = base_dir;
        let _ = root;
        let _ = recorder;
    }

    // `parse_xml(source)` / `parse_json(source)` — parse a document already in
    // hand (e.g. read via `open_file`/`read_blob` on a target with `fs` but no
    // one-call shorthand wanted, or a string built some other way). No file
    // access, so these are registered regardless of the `fs` feature.
    engine.register_type_with_name::<XmlNode>("xml_node");
    engine.register_fn(
        "parse_xml",
        |source: &str| -> Result<XmlNode, Box<EvalAltResult>> {
            xml::parse(source)
                .map_err(|e| -> Box<EvalAltResult> { format!("parse_xml: {e}").into() })
        },
    );
    engine.register_fn(
        "parse_json",
        |source: &str| -> Result<Dynamic, Box<EvalAltResult>> {
            json::parse(source)
                .map_err(|e| -> Box<EvalAltResult> { format!("parse_json: {e}").into() })
        },
    );

    // `xml_node` accessors: `.name`, `.attrs` (a map, sorted by key — see
    // `plans/013-structured-data-parsing.md` §2.5 for why this is not
    // insertion-ordered), `.attr(name)`, `.children`, `.text`, `.find(name)`,
    // `.find_all(name)`.
    engine.register_get("name", |n: &mut XmlNode| n.name().to_string());
    engine.register_get("attrs", |n: &mut XmlNode| -> Map {
        n.attrs()
            .iter()
            .map(|(k, v)| (k.as_str().into(), Dynamic::from(v.clone())))
            .collect()
    });
    engine.register_fn("attr", |n: &mut XmlNode, name: &str| -> Dynamic {
        n.attr(name)
            .map_or(Dynamic::UNIT, |v| Dynamic::from(v.to_string()))
    });
    engine.register_get("children", |n: &mut XmlNode| -> Array {
        n.children().iter().cloned().map(Dynamic::from).collect()
    });
    engine.register_get("text", |n: &mut XmlNode| -> Dynamic {
        n.text()
            .map_or(Dynamic::UNIT, |t| Dynamic::from(t.to_string()))
    });
    engine.register_fn("find", |n: &mut XmlNode, name: &str| -> Dynamic {
        n.find(name).map_or(Dynamic::UNIT, Dynamic::from)
    });
    engine.register_fn("find_all", |n: &mut XmlNode, name: &str| -> Array {
        n.find_all(name).into_iter().map(Dynamic::from).collect()
    });

    // `parse_int_list(text, delim)` / `parse_int_list(text, delim, radix)` —
    // bulk numeric decoding for the delimited columns structured formats store
    // grids and arrays as. One native call rather than one interpreter
    // iteration per element.
    engine.register_fn("parse_int_list", parse_int_list_dec);
    engine.register_fn("parse_int_list", parse_int_list_radix);

    // Small string/blob gaps found prototyping against structured text
    // (`plans/013-structured-data-parsing.md` §4): `trim()` mutates and returns
    // `()`, so `trimmed()` is the non-mutating form; `to_char` builds a string
    // from an int the way scripts need when assembling bytes read out of a
    // blob; `format_hex` is assembly's own `$XX`/`$XXXX` spelling. `to_hex`
    // (no padding, no `$`), `parse_int(str, radix)`, and `blob.as_string()`
    // already exist in this crate's Rhai version and need no host code.
    engine.register_fn("to_char", to_char);
    engine.register_fn("trimmed", trimmed);
    engine.register_fn("format_hex", format_hex);

    // Random-number functions (`rand`, `rand(min, max)`, `rand_float`,
    // `rand_bool`, and array `shuffle`/`sample`) for scripts that need
    // procedural noise or randomized data tables. Compiled out on targets
    // without a system entropy source (feature `rand` off), where the functions
    // are simply absent.
    #[cfg(feature = "rand")]
    RandomPackage::new().register_into_engine(&mut engine);

    // PNG decoding for scripts: `decode_png(blob)` → an opaque image handle
    // (typically fed an `open_file(...).read_blob()`).
    engine.register_type_with_name::<Image>("image");
    engine.register_fn("decode_png", decode_png);

    // Dimensions, and the whole pixel plane on demand. `pixels` is built only
    // when a script asks for it — materialising `width * height * 4` Rhai values
    // costs seconds and hundreds of megabytes on a full-resolution image, and
    // the accessors below cover every use of it.
    engine.register_get("width", img_width);
    engine.register_get("height", img_height);
    engine.register_get("pixels", img_pixels);

    // Pixel/tile accessors over an image, so scripts don't recompute
    // `(y * width + x) * 4` offsets by hand:
    //   `img.r(x, y)`            → the pixel's red channel (grayscale value)
    //   `img.pixel(x, y)`        → `[r, g, b, a]`
    //   `img.tile(col, row, w, h)` → the w×h block's red channels, row-major
    engine.register_fn("r", img_channel_r);
    engine.register_fn("pixel", img_pixel);
    engine.register_fn("tile", img_tile);

    // Cell matching: locate a `w`×`h` cell of one image among the cells of a
    // bank image (a CHR sheet, tile sheet, …), comparing NES shade indices.
    //   `bank.find_cell(src, col, row, w, h)`         → index, or -1
    //   `bank.cell_equals(i, src, col, row, w, h)`    → does cell `i` match?
    //   `bank.nearest_cell(src, col, row, w, h)`      → closest index (never -1)
    engine.register_fn("find_cell", img_find_cell);
    engine.register_fn("cell_equals", img_cell_equals);
    engine.register_fn("nearest_cell", img_nearest_cell);

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

/// Resolve a script-supplied path against the directive's source directory,
/// or — for a `@/`-prefixed path — the project root: the same rule
/// [`nessemble_core::resolve_path_arg`] applies to every other path-taking
/// argument (`plans/013-structured-data-parsing.md` §11.1,
/// `plans/012-project-root-paths.md`). `root` is `None` where no project root
/// could be determined, in which case a `@/` path is an error naming the sigil
/// rather than a silent fallback, matching [`nessemble_core::PathArgError`].
#[cfg(feature = "fs")]
fn resolve(base: &Path, root: Option<&Path>, p: &str) -> Result<PathBuf, String> {
    nessemble_core::resolve_path_arg(root, base, p).map_err(|e| e.message(p))
}

/// A decoded image, as scripts see it.
///
/// The pixels live behind an [`Arc`], so cloning an `Image` — which Rhai does
/// for *every* function argument that is not the method receiver — is a refcount
/// bump rather than a copy of the whole image. Handing an image to a helper
/// function is therefore free, and scripts can factor image work out of
/// `custom()` without falling off a performance cliff.
#[derive(Clone)]
struct Image(Arc<ImageData>);

/// The pixel planes behind an [`Image`].
struct ImageData {
    width: usize,
    height: usize,
    /// `width * height * 4` bytes in row-major `R, G, B, A` order.
    rgba: Vec<u8>,
    /// `width * height` NES shade indices (0–3): each pixel's red channel put
    /// through the same snapping `nes_shade` uses. Precomputed so cell matching
    /// compares whole rows as byte slices.
    shades: Vec<u8>,
}

impl ImageData {
    /// Byte offset of pixel `(x, y)` in [`Self::rgba`].
    fn offset(&self, x: usize, y: usize) -> usize {
        (y * self.width + x) * 4
    }

    /// The shades of the pixel row `[x0, x0 + w)` at `y`.
    fn shade_row(&self, x0: usize, y: usize, w: usize) -> &[u8] {
        let start = y * self.width + x0;
        &self.shades[start..start + w]
    }
}

/// `decode_png(blob)` — decode PNG bytes (e.g. from `open_file(path).read_blob()`)
/// into an image handle exposing `width`, `height`, `pixels` and the pixel
/// accessors. Throws if the blob is not a valid PNG.
// Rhai's `register_fn` takes the argument by value (or `&mut`); owned lets it be
// called uniformly on variables, temporaries, and constants.
#[allow(clippy::needless_pass_by_value)]
fn decode_png(blob: Blob) -> Result<Image, Box<EvalAltResult>> {
    let img = nessemble_media::decode_png_rgba(&blob)
        .map_err(|_| -> Box<EvalAltResult> { "decode_png: input is not a valid PNG".into() })?;
    let shades = img
        .rgba
        .iter()
        .step_by(4)
        .map(|&r| nes_shade_of(i64::from(r)) as u8)
        .collect();
    Ok(Image(Arc::new(ImageData {
        width: img.width as usize,
        height: img.height as usize,
        rgba: img.rgba,
        shades,
    })))
}

/// `img.width` — the image width in pixels.
fn img_width(img: &mut Image) -> i64 {
    img.0.width as i64
}

/// `img.height` — the image height in pixels.
fn img_height(img: &mut Image) -> i64 {
    img.0.height as i64
}

/// `img.pixels` — every channel as a flat, row-major `[r, g, b, a, …]` array of
/// `width * height * 4` integers.
///
/// Built on demand: for a full-resolution image this is tens of millions of Rhai
/// values, so decoding must not pay for it up front. Prefer `img.pixel(x, y)` /
/// `img.tile(…)`, which read straight from the decoded bytes.
fn img_pixels(img: &mut Image) -> Array {
    img.0
        .rgba
        .iter()
        .map(|&b| Dynamic::from(i64::from(b)))
        .collect()
}

/// Check that pixel `(x, y)` is inside `img`, naming `call` in the error.
fn pixel_in_bounds(call: &str, img: &ImageData, x: i64, y: i64) -> Result<(), Box<EvalAltResult>> {
    let (w, h) = (img.width as i64, img.height as i64);
    if x < 0 || y < 0 || x >= w || y >= h {
        return Err(format!("{call}: pixel ({x}, {y}) is out of bounds for {w}x{h} image").into());
    }
    Ok(())
}

/// `img.r(x, y)` — the red channel of pixel `(x, y)`. Images used by these
/// scripts are grayscale (R == G == B), so this is the shade value.
fn img_channel_r(img: &mut Image, x: i64, y: i64) -> Result<i64, Box<EvalAltResult>> {
    pixel_in_bounds("r", &img.0, x, y)?;
    Ok(i64::from(img.0.rgba[img.0.offset(x as usize, y as usize)]))
}

/// `img.pixel(x, y)` — the pixel as a `[r, g, b, a]` array.
fn img_pixel(img: &mut Image, x: i64, y: i64) -> Result<Array, Box<EvalAltResult>> {
    pixel_in_bounds("pixel", &img.0, x, y)?;
    let base = img.0.offset(x as usize, y as usize);
    Ok(img.0.rgba[base..base + 4]
        .iter()
        .map(|&b| Dynamic::from(i64::from(b)))
        .collect())
}

/// `img.tile(col, row, tw, th)` — the `tw`×`th` block at tile coordinate
/// `(col, row)` as a flat, row-major array of red-channel (shade) values. Pairs
/// with `nes_shade`/`quantize` to turn a block into palette indices in one line.
fn img_tile(
    img: &mut Image,
    col: i64,
    row: i64,
    tw: i64,
    th: i64,
) -> Result<Array, Box<EvalAltResult>> {
    if tw <= 0 || th <= 0 {
        return Err(format!("tile size must be positive, got {tw}x{th}").into());
    }
    let (w, h) = (img.0.width as i64, img.0.height as i64);
    let (x0, y0) = (col.saturating_mul(tw), row.saturating_mul(th));
    if col < 0 || row < 0 || x0.saturating_add(tw) > w || y0.saturating_add(th) > h {
        return Err(format!(
            "tile ({col}, {row}) of size {tw}x{th} is out of bounds for {w}x{h} image"
        )
        .into());
    }
    let (x0, y0) = (x0 as usize, y0 as usize);
    let mut out = Array::with_capacity((tw * th) as usize);
    for py in 0..th as usize {
        for px in 0..tw as usize {
            out.push(Dynamic::from(i64::from(
                img.0.rgba[img.0.offset(x0 + px, y0 + py)],
            )));
        }
    }
    Ok(out)
}

/// Validate a cell size, naming `call` in the error.
fn cell_size(call: &str, w: i64, h: i64) -> Result<(usize, usize), Box<EvalAltResult>> {
    if w <= 0 || h <= 0 {
        return Err(format!("{call}: cell size must be positive, got {w}x{h}").into());
    }
    Ok((w as usize, h as usize))
}

/// Top-left pixel of the `w`×`h` cell at grid position `(col, row)` of `src`,
/// erroring (rather than panicking) if that cell falls outside the image.
fn cell_origin(
    call: &str,
    src: &ImageData,
    col: i64,
    row: i64,
    w: usize,
    h: usize,
) -> Result<(usize, usize), Box<EvalAltResult>> {
    let (iw, ih) = (src.width as i64, src.height as i64);
    let (x0, y0) = (col.saturating_mul(w as i64), row.saturating_mul(h as i64));
    if col < 0 || row < 0 || x0.saturating_add(w as i64) > iw || y0.saturating_add(h as i64) > ih {
        return Err(format!(
            "{call}: cell ({col}, {row}) of size {w}x{h} is out of bounds for {iw}x{ih} image"
        )
        .into());
    }
    Ok((x0 as usize, y0 as usize))
}

/// How the bank image is gridded into `w`×`h` cells: `(columns, rows)`, using
/// `floor(width / w)` by `floor(height / h)` — the same gridding `tile()` uses,
/// so a bank whose dimensions are not whole multiples simply has the ragged
/// right/bottom edge left out. Errors if no whole cell fits.
fn bank_grid(
    call: &str,
    bank: &ImageData,
    w: usize,
    h: usize,
) -> Result<(usize, usize), Box<EvalAltResult>> {
    let (cols, rows) = (bank.width / w, bank.height / h);
    if cols == 0 || rows == 0 {
        return Err(format!(
            "{call}: {}x{} bank image has no whole {w}x{h} cells",
            bank.width, bank.height
        )
        .into());
    }
    Ok((cols, rows))
}

/// Top-left pixel of cell `index` of a bank gridded into `cols` columns.
fn bank_cell_origin(index: usize, cols: usize, w: usize, h: usize) -> (usize, usize) {
    ((index % cols) * w, (index / cols) * h)
}

/// Do the two `w`×`h` cells draw the same thing? Compares NES shade indices, so
/// pixels that differ only below the snapping threshold — e.g. data stashed in
/// the low nibble of a byte — compare equal.
fn cells_equal(
    bank: &ImageData,
    (bx, by): (usize, usize),
    src: &ImageData,
    (sx, sy): (usize, usize),
    w: usize,
    h: usize,
) -> bool {
    (0..h).all(|dy| bank.shade_row(bx, by + dy, w) == src.shade_row(sx, sy + dy, w))
}

/// Summed absolute per-pixel shade difference between two `w`×`h` cells.
fn cells_distance(
    bank: &ImageData,
    (bx, by): (usize, usize),
    src: &ImageData,
    (sx, sy): (usize, usize),
    w: usize,
    h: usize,
) -> u64 {
    (0..h)
        .map(|dy| {
            let (b, s) = (
                bank.shade_row(bx, by + dy, w),
                src.shade_row(sx, sy + dy, w),
            );
            b.iter()
                .zip(s)
                .map(|(&a, &c)| u64::from(a.abs_diff(c)))
                .sum::<u64>()
        })
        .sum()
}

/// `bank.find_cell(src, col, row, w, h)` — index of the `w`×`h` cell at grid
/// position `(col, row)` of `src` among `bank`'s `w`×`h` cells, scanning left to
/// right then top to bottom. The **lowest** matching index wins when several
/// bank cells draw the same thing; `-1` when none match exactly.
// `src` is a cheap `Arc` clone, and taking it by value lets Rhai pass variables,
// temporaries, and constants alike.
#[allow(clippy::needless_pass_by_value)]
fn img_find_cell(
    bank: &mut Image,
    src: Image,
    col: i64,
    row: i64,
    w: i64,
    h: i64,
) -> Result<i64, Box<EvalAltResult>> {
    let (w, h) = cell_size("find_cell", w, h)?;
    let origin = cell_origin("find_cell", &src.0, col, row, w, h)?;
    let (cols, rows) = bank_grid("find_cell", &bank.0, w, h)?;
    for index in 0..cols * rows {
        let at = bank_cell_origin(index, cols, w, h);
        if cells_equal(&bank.0, at, &src.0, origin, w, h) {
            return Ok(index as i64);
        }
    }
    Ok(-1)
}

/// `bank.cell_equals(index, src, col, row, w, h)` — does cell `index` of `bank`
/// draw the same thing as the cell at `(col, row)` of `src`? Cheap validation of
/// an index a caller already has (e.g. one stored alongside the image). An
/// `index` outside the bank is simply `false`.
#[allow(clippy::needless_pass_by_value)]
fn img_cell_equals(
    bank: &mut Image,
    index: i64,
    src: Image,
    col: i64,
    row: i64,
    w: i64,
    h: i64,
) -> Result<bool, Box<EvalAltResult>> {
    let (w, h) = cell_size("cell_equals", w, h)?;
    let origin = cell_origin("cell_equals", &src.0, col, row, w, h)?;
    let (cols, rows) = bank_grid("cell_equals", &bank.0, w, h)?;
    if index < 0 || index >= (cols * rows) as i64 {
        return Ok(false);
    }
    let at = bank_cell_origin(index as usize, cols, w, h);
    Ok(cells_equal(&bank.0, at, &src.0, origin, w, h))
}

/// `bank.nearest_cell(src, col, row, w, h)` — index of the bank cell closest to
/// the cell at `(col, row)` of `src` by summed per-pixel shade distance, the
/// fallback for a cell with no exact match. Ties go to the lowest index, and a
/// bank with at least one whole cell always yields an index (never `-1`).
#[allow(clippy::needless_pass_by_value)]
fn img_nearest_cell(
    bank: &mut Image,
    src: Image,
    col: i64,
    row: i64,
    w: i64,
    h: i64,
) -> Result<i64, Box<EvalAltResult>> {
    let (w, h) = cell_size("nearest_cell", w, h)?;
    let origin = cell_origin("nearest_cell", &src.0, col, row, w, h)?;
    let (cols, rows) = bank_grid("nearest_cell", &bank.0, w, h)?;
    let (mut best, mut best_distance) = (0, u64::MAX);
    for index in 0..cols * rows {
        let at = bank_cell_origin(index, cols, w, h);
        let distance = cells_distance(&bank.0, at, &src.0, origin, w, h);
        if distance < best_distance {
            (best, best_distance) = (index, distance);
            if distance == 0 {
                break;
            }
        }
    }
    Ok(best as i64)
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

/// `parse_int_list(text, delim)` — split `text` on the literal `delim`, trim
/// whitespace from each field, skip empty fields, and parse the rest as base-10
/// integers.
fn parse_int_list_dec(text: &str, delim: &str) -> Result<Array, Box<EvalAltResult>> {
    parse_int_list_radix(text, delim, 10)
}

/// `parse_int_list(text, delim, radix)` — as [`parse_int_list_dec`], parsing
/// each field in the given `radix` (2..=36, matching Rhai's own `parse_int`).
fn parse_int_list_radix(text: &str, delim: &str, radix: i64) -> Result<Array, Box<EvalAltResult>> {
    if !(2..=36).contains(&radix) {
        return Err(format!("parse_int_list: radix must be between 2 and 36, got {radix}").into());
    }
    let radix = radix as u32;
    let mut out = Array::new();
    for (i, field) in text.split(delim).enumerate() {
        let field = field.trim();
        if field.is_empty() {
            continue;
        }
        let value = i64::from_str_radix(field, radix).map_err(|e| -> Box<EvalAltResult> {
            format!(
                "parse_int_list: field {i} (\"{field}\") is not a valid base-{radix} integer: {e}"
            )
            .into()
        })?;
        out.push(Dynamic::from(value));
    }
    Ok(out)
}

/// `to_char(value)` — a one-character string for the Unicode scalar `value`,
/// e.g. `to_char(65)` → `"A"`. Lets a script build a string from bytes read out
/// of a blob, which Rhai's stdlib has no direct route for (`char.to_int()`
/// exists; nothing goes the other way).
fn to_char(value: i64) -> Result<String, Box<EvalAltResult>> {
    u32::try_from(value)
        .ok()
        .and_then(char::from_u32)
        .map(String::from)
        .ok_or_else(|| format!("to_char: {value} is not a valid Unicode scalar value").into())
}

/// `trimmed()` — a copy of the string with leading/trailing whitespace removed.
/// The stock `trim()` mutates in place and returns `()`, so `let t = s.trim();`
/// binds unit rather than the trimmed text.
fn trimmed(s: &str) -> String {
    s.trim().to_string()
}

/// `format_hex(value, width)` — assembly's own hex spelling: `$`-prefixed,
/// zero-padded to `width` digits, uppercase. E.g. `format_hex(255, 2)` →
/// `"$FF"`. `value` is masked to `width` hex digits first, so a negative or
/// oversized value wraps the way a fixed-width dump would rather than printing
/// more digits than asked for.
fn format_hex(value: i64, width: i64) -> Result<String, Box<EvalAltResult>> {
    let w = usize::try_from(width)
        .ok()
        .filter(|&w| (1..=16).contains(&w))
        .ok_or_else(|| -> Box<EvalAltResult> {
            format!("format_hex: width must be between 1 and 16, got {width}").into()
        })?;
    let bits = w * 4;
    let mask = if bits >= 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    };
    Ok(format!("${:0w$X}", (value as u64) & mask, w = w))
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

/// Note `path` in `recorder` (when there is one) and hand it back, so a
/// registration can record and resolve in one expression.
fn record(recorder: Option<&Recorder>, path: PathBuf) -> PathBuf {
    if let Some(rec) = recorder {
        rec.borrow_mut().insert(path.clone());
    }
    path
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

    /// PNG bytes for a grayscale image from row-major shade bytes (`R == G == B`,
    /// opaque) — the shape of image these scripts actually work with.
    fn gray_png(width: u32, height: u32, values: &[u8]) -> Vec<u8> {
        let rgba: Vec<u8> = values.iter().flat_map(|&v| [v, v, v, 255]).collect();
        png_bytes(width, height, &rgba)
    }

    /// A 4×4 bank of four 2×2 cells, and a 4×4 source whose cells match cell 0
    /// (twice over, since cell 2 repeats it), cell 1, cell 3, and nothing.
    fn cell_fixture(tag: &str) -> TempDir {
        let dir = TempDir::new(tag);
        #[rustfmt::skip]
        let bank = [
            0, 0,   255, 255,
            0, 0,   255, 255,
            0, 0,     0, 255,
            0, 0,   255,   0,
        ];
        // The source repeats the bank's cells with data stashed in the low
        // nibble of each byte — it snaps away, so the cells still compare equal.
        #[rustfmt::skip]
        let src = [
            0x0F, 0x0F,   0xF0, 0xFF,
            0x0F, 0x0F,   0xFF, 0xF0,
               0,  255,     85,   85,
             255,    0,     85,   85,
        ];
        std::fs::write(dir.0.join("bank.png"), gray_png(4, 4, &bank)).unwrap();
        std::fs::write(dir.0.join("src.png"), gray_png(4, 4, &src)).unwrap();
        dir
    }

    #[test]
    fn find_cell_matches_the_hand_rolled_rhai_scan() {
        // `bank.find_cell(src, …)` must agree with the loop scripts write today:
        // compare `nes_shade(bank.tile(…))` to `nes_shade(src.tile(…))` over
        // every bank cell and take the first hit.
        let dir = cell_fixture("find-cell");
        let src = r#"
            fn custom(ints, texts) {
                let bank = decode_png_file("bank.png");
                let src = decode_png_file("src.png");
                let cols = bank.width / 2;
                let out = [];
                for row in 0..2 {
                    for col in 0..2 {
                        let needle = nes_shade(src.tile(col, row, 2, 2));
                        let want = -1;
                        for i in 0..(cols * (bank.height / 2)) {
                            if want == -1 &&
                               nes_shade(bank.tile(i % cols, i / cols, 2, 2)) == needle {
                                want = i;
                            }
                        }
                        // +1 so the -1 'no match' case survives the byte encoding.
                        out.push(bank.find_cell(src, col, row, 2, 2) + 1);
                        out.push(want + 1);
                    }
                }
                out
            }
        "#;
        let out = run(src, &[], &[], &dir.0).unwrap();
        // Cell (0,0) matches bank cells 0 and 2 — the lowest index wins; (1,0)
        // matches cell 1; (0,1) matches cell 3; (1,1) matches nothing (-1 → 0).
        assert_eq!(out, vec![1, 1, 2, 2, 4, 4, 0, 0]);
    }

    #[test]
    fn cell_equals_accepts_exactly_the_matching_indices() {
        // True for every bank cell that draws the source cell (including the
        // duplicate `find_cell` passes over), false elsewhere and out of range.
        let dir = cell_fixture("cell-equals");
        let src = r#"
            fn custom(ints, texts) {
                let bank = decode_png_file("bank.png");
                let src = decode_png_file("src.png");
                let out = [];
                for i in [0, 1, 2, 3, 4, -1] {
                    out.push(if bank.cell_equals(i, src, 0, 0, 2, 2) { 1 } else { 0 });
                }
                out
            }
        "#;
        // Cells 0 and 2 are the all-dark cell; 4 and -1 are outside the bank.
        assert_eq!(run(src, &[], &[], &dir.0).unwrap(), vec![1, 0, 1, 0, 0, 0]);
    }

    #[test]
    fn nearest_cell_falls_back_to_the_closest_shade_distance() {
        let dir = cell_fixture("nearest-cell");
        let src = r#"
            fn custom(ints, texts) {
                let bank = decode_png_file("bank.png");
                let src = decode_png_file("src.png");
                [
                    bank.nearest_cell(src, 1, 1, 2, 2),  // no exact match
                    bank.nearest_cell(src, 1, 0, 2, 2),  // exact match wins
                ]
            }
        "#;
        // The unmatched cell is mid-gray (shade 1): distance 4 to the all-dark
        // cells 0/2, 6 to cell 3, 8 to the all-light cell 1 — so cell 0.
        assert_eq!(run(src, &[], &[], &dir.0).unwrap(), vec![0, 1]);
    }

    #[test]
    fn cell_matching_grids_the_bank_like_tile_does() {
        // A bank that is not a whole multiple of the cell size uses
        // floor(width/w) x floor(height/h) cells; the ragged edge is not a cell.
        let dir = TempDir::new("ragged-bank");
        #[rustfmt::skip]
        let bank = [
            0, 0,   255, 255,   255,
            0, 0,   255, 255,   255,
            9, 9,     9,   9,     9,
        ];
        std::fs::write(dir.0.join("bank.png"), gray_png(5, 3, &bank)).unwrap();
        std::fs::write(dir.0.join("src.png"), gray_png(2, 2, &[255; 4])).unwrap();
        let src = r#"
            fn custom(ints, texts) {
                let bank = decode_png_file("bank.png");
                let src = decode_png_file("src.png");
                [
                    bank.find_cell(src, 0, 0, 2, 2) + 1,
                    if bank.cell_equals(2, src, 0, 0, 2, 2) { 1 } else { 0 },
                ]
            }
        "#;
        // Two whole cells (the third column and third row are partial): the
        // light cell is index 1, and index 2 is out of range.
        assert_eq!(run(src, &[], &[], &dir.0).unwrap(), vec![2, 0]);
    }

    #[test]
    fn cell_matching_reports_bad_arguments_by_call_name() {
        let dir = cell_fixture("cell-errors");
        let cases = [
            (
                "bank.find_cell(src, 9, 0, 2, 2)",
                "find_cell",
                "out of bounds",
            ),
            (
                "bank.find_cell(src, 0, 0, 0, 2)",
                "find_cell",
                "must be positive",
            ),
            (
                "bank.nearest_cell(src, 0, 0, 8, 8)",
                "nearest_cell",
                "out of bounds",
            ),
            (
                "bank.cell_equals(0, src, -1, 0, 2, 2)",
                "cell_equals",
                "out of bounds",
            ),
        ];
        for (call, name, wanted) in cases {
            let script = format!(
                r#"fn custom(ints, texts) {{
                    let bank = decode_png_file("bank.png");
                    let src = decode_png_file("src.png");
                    [{call}]
                }}"#
            );
            let err = run(&script, &[], &[], &dir.0).unwrap_err();
            assert!(err.contains(name), "{call}: error did not name it: {err}");
            assert!(err.contains(wanted), "{call}: unexpected error: {err}");
        }

        // A bank too small to hold one cell is an error, not a silent -1.
        std::fs::write(dir.0.join("tiny.png"), gray_png(1, 1, &[0])).unwrap();
        let script = r#"
            fn custom(ints, texts) {
                [decode_png_file("tiny.png")
                    .find_cell(decode_png_file("src.png"), 0, 0, 2, 2)]
            }
        "#;
        let err = run(script, &[], &[], &dir.0).unwrap_err();
        assert!(
            err.contains("find_cell") && err.contains("no whole 2x2 cells"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn passing_an_image_to_a_function_does_not_copy_it() {
        // An image is a handle, so handing it to a helper is a refcount bump.
        // Back when it was a map of every pixel, each call cloned the whole
        // pixel array and a loop like this took minutes; the bound here is
        // deliberately loose — it is a cliff detector, not a benchmark.
        let dir = TempDir::new("img-handle");
        let side = 512;
        let values: Vec<u8> = (0..side * side).map(|i| (i % 256) as u8).collect();
        std::fs::write(
            dir.0.join("big.png"),
            gray_png(side as u32, side as u32, &values),
        )
        .unwrap();
        let src = r#"
            fn probe(img, x) { img.pixel(x, 0)[0] }
            fn custom(ints, texts) {
                let img = decode_png_file("big.png");
                let sum = 0;
                for x in 0..20000 {
                    sum += probe(img, x % 512);
                }
                [sum % 256]
            }
        "#;
        let started = std::time::Instant::now();
        let out = run(src, &[], &[], &dir.0).unwrap();
        let elapsed = started.elapsed();
        let expected: i64 = (0..20000).map(|x: i64| (x % 512) % 256).sum();
        assert_eq!(out, vec![(expected % 256) as u8]);
        assert!(
            elapsed < std::time::Duration::from_secs(30),
            "20k image reads through a helper took {elapsed:?} — images are being copied"
        );
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

    #[cfg(feature = "rand")]
    #[test]
    fn rand_functions_are_registered_and_bounded() {
        // `rhai-rand` supplies `rand(min, max)` (inclusive), `rand_bool`, and the
        // array `shuffle`/`sample` helpers. Draw several values into a fixed
        // range and assert each byte lands inside it — the values are random but
        // the bounds are not.
        let src = r"
            fn custom(ints, texts) {
                let out = [];
                for i in 0..8 {
                    out.push(rand(10, 20));     // 10..=20
                }
                out.push(if rand_bool() { 1 } else { 0 });  // 0 or 1
                out
            }
        ";
        let out = run(src, &[], &[], cwd()).unwrap();
        assert_eq!(out.len(), 9);
        for &b in &out[..8] {
            assert!(
                (10..=20).contains(&b),
                "rand(10, 20) produced {b}, out of range"
            );
        }
        assert!(out[8] <= 1, "rand_bool mapped to {}", out[8]);
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

    // ---- input recording and purity (plan 011, Phase 3) --------------------

    #[test]
    fn records_every_route_from_a_path_to_a_file() {
        // The registrations that turn a path string into a file: rhai-fs's
        // `open_file` (via the `path` hook), and the shorthands, including the
        // two structured-data parsers (plan 013 §3 — they must record exactly
        // like `read_blob`/`decode_png_file`, with no separate mechanism).
        let dir = TempDir::new("record-all");
        std::fs::write(dir.0.join("a.bin"), b"a").unwrap();
        std::fs::write(dir.0.join("b.bin"), b"b").unwrap();
        write_png_1x1(&dir.0.join("c.png"));
        std::fs::write(dir.0.join("d.xml"), b"<r/>").unwrap();
        std::fs::write(dir.0.join("e.json"), b"1").unwrap();
        let src = r#"
            fn custom(ints, texts) {
                let x = open_file("a.bin", "r").read_blob();
                let y = read_blob("b.bin");
                let img = decode_png_file("c.png");
                let doc = parse_xml_file("d.xml");
                let j = parse_json_file("e.json");
                x + y
            }
        "#;

        let out = run_with_inputs(src, &[], &[], &dir.0).unwrap();
        let names: Vec<String> = out
            .inputs
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, ["a.bin", "b.bin", "c.png", "d.xml", "e.json"]);
        // Absolute, so a recorded path means the same thing from anywhere.
        assert!(out.inputs.iter().all(|p| p.is_absolute()));
        assert_eq!(out.bytes, b"ab");
        assert!(out.cacheable);
    }

    // ---- `@/` project-root resolution (plan 013 §11.1) ---------------------

    #[test]
    fn a_root_relative_path_resolves_from_the_project_root_not_base_dir() {
        // `@/`-prefixed paths a script resolves itself join the project root,
        // not `base_dir` — the same rule `.incbin`/a declared `file://`
        // argument already follow (plan 012, plan 013 §11.1).
        let dir = TempDir::new("at-slash-root");
        let root = dir.0.join("proj");
        std::fs::create_dir_all(root.join("assets")).unwrap();
        std::fs::write(root.join("assets/logo.bin"), b"logo").unwrap();
        let sub = root.join("src/gfx");
        std::fs::create_dir_all(&sub).unwrap();

        let src = r#"fn custom(ints, texts) { read_blob("@/assets/logo.bin") }"#;
        let out = run_with_root(src, &[], &[], &sub, Some(&root)).unwrap();
        assert_eq!(out, b"logo");
    }

    #[test]
    fn every_path_taking_function_honors_at_slash() {
        // read_blob, decode_png_file, parse_xml_file, parse_json_file, and
        // rhai-fs's own open_file (via the `path` hook) all resolve `@/`
        // identically — no function is left inconsistent with another.
        let dir = TempDir::new("at-slash-every-fn");
        let root = dir.0.join("proj");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.bin"), b"a").unwrap();
        write_png_1x1(&root.join("b.png"));
        std::fs::write(root.join("c.xml"), b"<r/>").unwrap();
        std::fs::write(root.join("d.json"), b"1").unwrap();
        std::fs::write(root.join("e.txt"), b"e").unwrap();
        let src = r#"
            fn custom(ints, texts) {
                let a = read_blob("@/a.bin");
                let img = decode_png_file("@/b.png");
                let doc = parse_xml_file("@/c.xml");
                let j = parse_json_file("@/d.json");
                let e = open_file("@/e.txt", "r").read_blob();
                a + e
            }
        "#;
        let out = run_with_root(src, &[], &[], &root, Some(&root)).unwrap();
        assert_eq!(out, b"ae");
    }

    #[test]
    fn a_root_relative_path_is_recorded_as_its_resolved_absolute_path() {
        let dir = TempDir::new("at-slash-record");
        let root = dir.0.join("proj");
        std::fs::create_dir_all(root.join("assets")).unwrap();
        let asset = root.join("assets/logo.bin");
        std::fs::write(&asset, b"logo").unwrap();

        let src = r#"fn custom(ints, texts) { read_blob("@/assets/logo.bin") }"#;
        let out = run_with_inputs_and_root(src, &[], &[], &root, Some(&root)).unwrap();
        assert_eq!(out.inputs, vec![asset]);
    }

    #[test]
    fn a_root_relative_path_without_a_root_names_the_sigil() {
        // No project root could be determined (e.g. `run`'s plain, root-less
        // form) — a hard error, not a silent fallback to `base_dir`.
        let dir = TempDir::new("at-slash-no-root");
        let src = r#"fn custom(ints, texts) { read_blob("@/assets/logo.bin") }"#;
        let err = run(src, &[], &[], &dir.0).unwrap_err();
        assert!(err.contains("@/assets/logo.bin"), "unexpected error: {err}");
        assert!(err.contains("no project root"), "unexpected error: {err}");
    }

    #[test]
    fn a_root_relative_path_that_escapes_the_root_is_an_error() {
        let dir = TempDir::new("at-slash-escape");
        let root = dir.0.join("proj");
        std::fs::create_dir_all(&root).unwrap();
        let src = r#"fn custom(ints, texts) { read_blob("@/../secret.bin") }"#;
        let err = run_with_root(src, &[], &[], &root, Some(&root)).unwrap_err();
        assert!(
            err.contains("outside the project root"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn a_nested_parse_xml_file_call_is_recorded_too() {
        // A document that references another file, parsed from *inside* the
        // script rather than passed as a directive argument, is still just
        // another call through the same choke point (plan 013 §3) — the cache
        // sees both, not only the one the directive named directly.
        let dir = TempDir::new("record-nested");
        std::fs::write(dir.0.join("outer.xml"), br#"<doc ref="inner.xml"/>"#).unwrap();
        std::fs::write(dir.0.join("inner.xml"), b"<inner/>").unwrap();
        let src = r#"
            fn custom(ints, texts) {
                let outer = parse_xml_file("outer.xml");
                let inner = parse_xml_file(outer.attr("ref"));
                inner.name.to_blob()
            }
        "#;
        let out = run_with_inputs(src, &[], &[], &dir.0).unwrap();
        let names: Vec<String> = out
            .inputs
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, ["inner.xml", "outer.xml"]);
        assert_eq!(out.bytes, b"inner");
        assert!(out.cacheable);
    }

    #[test]
    fn a_script_that_reads_nothing_records_nothing() {
        let src = "fn custom(ints, texts) { [1, 2, 3] }";
        let out = run_with_inputs(src, &[], &[], cwd()).unwrap();
        assert!(out.inputs.is_empty());
        assert!(out.cacheable);
    }

    #[test]
    fn only_the_files_this_invocation_read_are_recorded() {
        // The recorded set is what the script did with *these* arguments, not
        // every path in its source — which is why the arguments are part of any
        // cache key built on top of it.
        let dir = TempDir::new("record-branch");
        std::fs::write(dir.0.join("even.bin"), b"e").unwrap();
        std::fs::write(dir.0.join("odd.bin"), b"o").unwrap();
        let src = r#"
            fn custom(ints, texts) {
                if ints[0] % 2 == 0 { read_blob("even.bin") } else { read_blob("odd.bin") }
            }
        "#;

        let even = run_with_inputs(src, &[0], &[], &dir.0).unwrap();
        let odd = run_with_inputs(src, &[1], &[], &dir.0).unwrap();
        assert_eq!(even.inputs.len(), 1);
        assert!(even.inputs[0].ends_with("even.bin"));
        assert_eq!(odd.inputs.len(), 1);
        assert!(odd.inputs[0].ends_with("odd.bin"));
    }

    #[test]
    fn randomness_makes_a_run_uncacheable() {
        let src = "fn custom(ints, texts) { [rand(0, 255)] }";
        let out = run_with_inputs(src, &[], &[], cwd()).unwrap();
        assert!(!out.cacheable);
        assert_eq!(
            out.impurity,
            Some(purity::Impurity::Calls("rand".to_string()))
        );
    }

    #[test]
    fn an_array_shuffle_makes_a_run_uncacheable() {
        let src = "fn custom(ints, texts) { let a = [1, 2, 3]; a.shuffle(); a }";
        assert!(!run_with_inputs(src, &[], &[], cwd()).unwrap().cacheable);
    }

    #[test]
    fn a_file_write_makes_a_run_uncacheable() {
        // The write is the observable effect; a cache hit would skip it.
        let dir = TempDir::new("purity-write");
        let src = r#"fn custom(ints, texts) { open_file("out.bin").write("ok"); () }"#;
        let out = run_with_inputs(src, &[], &[], &dir.0).unwrap();
        assert!(!out.cacheable);
        assert_eq!(out.impurity, Some(purity::Impurity::WritesAFile));
    }

    #[test]
    fn a_read_mode_open_stays_cacheable() {
        let dir = TempDir::new("purity-read");
        std::fs::write(dir.0.join("a.bin"), b"a").unwrap();
        let src = r#"fn custom(ints, texts) { open_file("a.bin", "r").read_blob() }"#;
        assert!(run_with_inputs(src, &[], &[], &dir.0).unwrap().cacheable);
    }

    #[test]
    fn a_computed_open_mode_is_assumed_to_write() {
        // The mode is not a literal, so it cannot be ruled a read.
        let dir = TempDir::new("purity-dyn");
        std::fs::write(dir.0.join("a.bin"), b"a").unwrap();
        let src = r#"
            fn custom(ints, texts) {
                let mode = if ints[0] == 0 { "r" } else { "w" };
                open_file("a.bin", mode).read_blob()
            }
        "#;
        assert!(!run_with_inputs(src, &[0], &[], &dir.0).unwrap().cacheable);
    }

    #[test]
    fn a_directory_listing_makes_a_run_uncacheable() {
        // A listing's contents are not described by any per-file freshness record.
        let dir = TempDir::new("purity-dir");
        let src = r#"fn custom(ints, texts) { let d = open_dir("."); [] }"#;
        let out = run_with_inputs(src, &[], &[], &dir.0).unwrap();
        assert!(!out.cacheable);
        assert_eq!(
            out.impurity,
            Some(purity::Impurity::Calls("open_dir".to_string()))
        );
    }

    #[test]
    fn an_import_makes_a_script_uncacheable() {
        // A module's source is invisible to both the script's identity and the
        // recorder, so such a script is refused rather than recorded partially.
        // Scanned rather than run: rhai resolves modules against the process
        // directory, which is exactly why the recorder cannot see them.
        let src = r#"import "helper" as h; fn custom(ints, texts) { [1] }"#;
        let ast = Engine::new().compile(src).expect("compiles");
        assert_eq!(purity::impurity(&ast), Some(purity::Impurity::Imports));
    }

    #[test]
    fn impurity_is_found_in_a_branch_this_call_does_not_take() {
        // Conservative on purpose: a wrong "uncacheable" costs one execution, a
        // wrong "cacheable" serves stale bytes. (A branch guarded by a *constant*
        // is a different case — rhai's optimizer folds it away before the scan
        // sees it, and code that cannot run cannot make the result vary.)
        let src = "fn custom(ints, texts) { if ints[0] == 0 { [7] } else { [rand(0, 9)] } }";
        let out = run_with_inputs(src, &[0], &[], cwd()).unwrap();
        assert_eq!(out.bytes, vec![7]);
        assert!(!out.cacheable);
    }

    #[test]
    fn run_still_returns_only_bytes() {
        // The original entry point is unchanged, so `nessemble-wasm` and every
        // existing caller keep working.
        let src = "fn custom(ints, texts) { [1, 2] }";
        assert_eq!(run(src, &[], &[], cwd()).unwrap(), vec![1, 2]);
    }

    // ---- structured data parsing (plan 013) --------------------------------

    #[test]
    fn parse_xml_walks_elements_attrs_and_text() {
        let src = r#"
            fn custom(ints, texts) {
                let doc = parse_xml("<map w=\"2\"><row>1,2</row><row>3,4</row></map>");
                let out = [parse_int(doc.attr("w"))];
                for row in doc.find_all("row") {
                    out += parse_int_list(row.text, ",");
                }
                out
            }
        "#;
        assert_eq!(run(src, &[], &[], cwd()).unwrap(), vec![2, 1, 2, 3, 4]);
    }

    #[test]
    fn parse_xml_file_reproduces_a_compiled_conversion_byte_for_byte() {
        // The acceptance criterion from plan 013 §10: a script driven by
        // `parse_xml_file` must match what an equivalent compiled conversion of
        // the same document would produce, not merely "look plausible".
        let dir = TempDir::new("xml-table");
        std::fs::write(
            dir.0.join("tiles.xml"),
            br#"<tiles>
                <tile id="0" bytes="10,20,30"/>
                <tile id="1" bytes="40,50"/>
                <tile id="2" bytes="60"/>
            </tiles>"#,
        )
        .unwrap();

        // The "compiled implementation" this script's output is checked against.
        let doc = xml::parse(&std::fs::read_to_string(dir.0.join("tiles.xml")).unwrap()).unwrap();
        let mut expected = Vec::new();
        for tile in doc.find_all("tile") {
            expected.push(tile.attr("id").unwrap().parse::<u8>().unwrap());
            for field in tile.attr("bytes").unwrap().split(',') {
                expected.push(field.trim().parse::<u8>().unwrap());
            }
        }

        let src = r#"
            fn custom(ints, texts) {
                let doc = parse_xml_file("tiles.xml");
                let out = [];
                for tile in doc.find_all("tile") {
                    out.push(parse_int(tile.attr("id")));
                    out += parse_int_list(tile.attr("bytes"), ",");
                }
                out
            }
        "#;
        assert_eq!(run(src, &[], &[], &dir.0).unwrap(), expected);
    }

    #[test]
    fn parse_xml_rejects_doctype_and_unknown_entities() {
        let err = run(
            r#"fn custom(ints, texts) { parse_xml("<!DOCTYPE x><r/>") }"#,
            &[],
            &[],
            cwd(),
        )
        .unwrap_err();
        assert!(err.contains("DOCTYPE"), "{err}");

        let err = run(
            r#"fn custom(ints, texts) { parse_xml("<r>&nope;</r>") }"#,
            &[],
            &[],
            cwd(),
        )
        .unwrap_err();
        assert!(err.contains("&nope;"), "{err}");
    }

    #[test]
    fn parse_xml_file_error_names_the_file_and_position() {
        let dir = TempDir::new("xml-err");
        std::fs::write(dir.0.join("bad.xml"), b"<a><b></a>").unwrap();
        let src = r#"fn custom(ints, texts) { parse_xml_file("bad.xml") }"#;
        let err = run(src, &[], &[], &dir.0).unwrap_err();
        assert!(err.contains("parse_xml_file"), "{err}");
        assert!(err.contains("bad.xml"), "{err}");
        // Position of the mismatched `</a>`, 1-indexed.
        assert!(err.contains("1:7"), "{err}");
    }

    #[test]
    fn parse_json_walks_maps_and_arrays() {
        let src = r#"
            fn custom(ints, texts) {
                let doc = parse_json("{\"rows\": [[1, 2], [3, 4]], \"count\": 2}");
                let out = [doc.count];
                for row in doc.rows {
                    out += row;
                }
                out
            }
        "#;
        assert_eq!(run(src, &[], &[], cwd()).unwrap(), vec![2, 1, 2, 3, 4]);
    }

    #[test]
    fn parse_json_file_reads_and_records_like_read_blob() {
        let dir = TempDir::new("json-file");
        std::fs::write(dir.0.join("d.json"), br#"{"v": 7}"#).unwrap();
        let src = r#"fn custom(ints, texts) { [parse_json_file("d.json").v] }"#;
        let out = run_with_inputs(src, &[], &[], &dir.0).unwrap();
        assert_eq!(out.bytes, vec![7]);
        assert_eq!(out.inputs.len(), 1);
        assert!(out.inputs[0].ends_with("d.json"));
    }

    #[test]
    fn parse_json_reports_a_syntax_error_with_position() {
        let err = run(
            r#"fn custom(ints, texts) { parse_json("{\"a\": }") }"#,
            &[],
            &[],
            cwd(),
        )
        .unwrap_err();
        assert!(err.contains("parse_json"), "{err}");
        assert!(err.contains("line 1"), "{err}");
    }

    #[test]
    fn parse_int_list_trims_skips_empties_and_honors_radix() {
        let src = r#"
            fn custom(ints, texts) {
                let out = parse_int_list(" 1, 2,,3 ,", ",");
                out += parse_int_list("ff,1A", ",", 16);
                out
            }
        "#;
        assert_eq!(run(src, &[], &[], cwd()).unwrap(), vec![1, 2, 3, 255, 26]);
    }

    #[test]
    fn parse_int_list_errors_name_the_bad_field() {
        let src = r#"fn custom(ints, texts) { parse_int_list("1,x,3", ",") }"#;
        let err = run(src, &[], &[], cwd()).unwrap_err();
        assert!(err.contains("field 1") && err.contains('x'), "{err}");
    }

    #[test]
    fn to_char_trimmed_and_format_hex_round_out_the_string_gaps() {
        let src = r#"
            fn custom(ints, texts) {
                let s = to_char(72) + to_char(73);       // "HI"
                let t = "  padded  ".trimmed();          // non-mutating trim
                let h = format_hex(255, 2) + format_hex(-1, 2) + format_hex(0x1A, 4);
                [s.len(), t.len(), h.len()]
            }
        "#;
        // "HI" (2) + "padded" (6) + "$FF$FF$001A" (11)
        assert_eq!(run(src, &[], &[], cwd()).unwrap(), vec![2, 6, 11]);
    }

    #[test]
    fn trimmed_does_not_mutate_the_original_string() {
        let src = r#"
            fn custom(ints, texts) {
                let s = "  hi  ";
                let t = s.trimmed();
                [s.len(), t.len()]
            }
        "#;
        assert_eq!(run(src, &[], &[], cwd()).unwrap(), vec![6, 2]);
    }

    #[test]
    fn stock_parse_int_radix_and_blob_as_string_already_work() {
        // plan 013 §4: these were reported missing against an older Rhai
        // release; the version this crate depends on already has them, so this
        // is a regression test for "still true", not new host code.
        let src = r#"
            fn custom(ints, texts) {
                let n = parse_int("1A", 16);
                let s = texts[0].to_blob().as_string();
                [n, s.len()]
            }
        "#;
        assert_eq!(
            run(src, &[], &["hi".to_string()], cwd()).unwrap(),
            vec![26, 2]
        );
    }

    /// Write a minimal 1×1 grayscale PNG that `decode_png` accepts.
    fn write_png_1x1(path: &Path) {
        use image::ImageEncoder as _;
        let mut bytes = Vec::new();
        image::codecs::png::PngEncoder::new(&mut bytes)
            .write_image(&[0u8], 1, 1, image::ExtendedColorType::L8)
            .unwrap();
        std::fs::write(path, bytes).unwrap();
    }
}
