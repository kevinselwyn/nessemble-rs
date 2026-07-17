//! WebAssembly bindings for the nessemble-rs assembler.
//!
//! The centerpiece is [`assemble`], which turns 6502/NES assembly source into a
//! ROM entirely client-side, for the in-browser assembler component. It wraps
//! [`nessemble_core::assemble_with`] and runs custom pseudo-op scripts via
//! [`nessemble_script`] (built without filesystem access — see the crate's `fs`
//! feature). Alongside it, [`tokenize`]/[`token_classes`] drive the editor's
//! syntax highlighting and [`format()`] reformats source in the editor.
//!
//! # Options (`opts_json`)
//!
//! A JSON object, or the empty string for defaults:
//!
//! ```json
//! {
//!   "format": "nes" | "raw",
//!   "undocumented": false,
//!   "empty_byte": 255,
//!   "pseudo": { "ease": true, "myop": "fn custom(ints, texts) { … }" }
//! }
//! ```
//!
//! In `pseudo`, `true` enables a **built-in** script by name and a string value
//! supplies **inline** Rhai source.
//!
//! # Limitations
//!
//! There is no filesystem in the browser, so directives that read files
//! (`.include`, `.incbin`, `.incpng`, …) and scripts that use the file API fail
//! with an error rather than producing bytes.

use std::collections::HashMap;

use nessemble_core::tooling::{self, TokenClass};
use nessemble_core::{assemble_with, AssembleError, CustomResolver, Diag, Options};
use serde::Deserialize;
use wasm_bindgen::prelude::*;

/// Built-in custom pseudo-op scripts, embedded so `pseudo: { name: true }` can
/// enable them by name — mirroring the CLI's bundled scripts (shared source, to
/// avoid drift; a future phase may move these to a common location).
const BUILTIN_SCRIPTS: &[(&str, &str)] = &[(
    "ease",
    include_str!("../../nessemble-cli/src/data/scripts/ease.rhai"),
)];

/// Assembly options accepted from JavaScript (see the crate docs for the shape).
#[derive(Deserialize, Default)]
#[serde(default)]
struct WasmOptions {
    format: Option<String>,
    undocumented: bool,
    empty_byte: Option<u8>,
    pseudo: HashMap<String, PseudoSpec>,
}

/// A `pseudo` entry: `true`/`false` toggles a built-in script; a string supplies
/// inline Rhai source.
#[derive(Deserialize)]
#[serde(untagged)]
enum PseudoSpec {
    Enabled(bool),
    Source(String),
}

/// The outcome of an [`assemble`] call: the ROM bytes plus any diagnostics.
/// `errors` is empty on success; `ok` is a convenience for `errors.length === 0`.
#[wasm_bindgen]
pub struct AssembleResult {
    rom: Vec<u8>,
    errors: Vec<String>,
    warnings: Vec<String>,
}

#[wasm_bindgen]
impl AssembleResult {
    /// The assembled ROM bytes (empty when assembly failed).
    #[must_use]
    #[wasm_bindgen(getter)]
    pub fn rom(&self) -> Vec<u8> {
        self.rom.clone()
    }

    /// Error messages; empty on success.
    #[must_use]
    #[wasm_bindgen(getter)]
    pub fn errors(&self) -> Vec<String> {
        self.errors.clone()
    }

    /// Warning messages emitted during assembly.
    #[must_use]
    #[wasm_bindgen(getter)]
    pub fn warnings(&self) -> Vec<String> {
        self.warnings.clone()
    }

    /// Whether assembly succeeded (no errors).
    #[must_use]
    #[wasm_bindgen(getter)]
    pub fn ok(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Assemble `source` into a ROM. `opts_json` is a JSON options object (or the
/// empty string for defaults).
///
/// Errors are returned as data on the [`AssembleResult`] — a malformed program
/// never throws.
#[must_use]
#[wasm_bindgen]
pub fn assemble(source: &str, opts_json: &str) -> AssembleResult {
    let opts: WasmOptions = if opts_json.trim().is_empty() {
        WasmOptions::default()
    } else {
        match serde_json::from_str(opts_json) {
            Ok(opts) => opts,
            Err(e) => return AssembleResult::from_error(format!("invalid options: {e}")),
        }
    };

    let options = Options {
        nes: opts.format.as_deref() == Some("nes"),
        undocumented: opts.undocumented,
        empty_byte: opts.empty_byte.unwrap_or(0xFF),
    };

    let resolver = build_resolver(&opts.pseudo);
    match assemble_with(source, &options, resolver) {
        Ok(assembly) => AssembleResult {
            rom: assembly.rom,
            errors: Vec::new(),
            warnings: assembly.warnings.iter().map(format_diag).collect(),
        },
        Err(AssembleError::Diagnostic(diag)) => AssembleResult::from_error(format_diag(&diag)),
    }
}

impl AssembleResult {
    fn from_error(message: String) -> Self {
        AssembleResult {
            rom: Vec::new(),
            errors: vec![message],
            warnings: Vec::new(),
        }
    }
}

/// Classify `source` for syntax highlighting, as a flat, triple-packed array
/// `[start, len, class, start, len, class, …]` (a `Uint32Array` in JS):
///
/// - `start`, `len` — the token's offset and length in **UTF-16 code units**, so
///   they index directly into the JS source string.
/// - `class` — a highlight-class id, indexed into [`token_classes`] (e.g. map it
///   to a CSS class like `na-tok-<name>`).
///
/// Whitespace and newlines are not emitted — the gaps between tokens are trivia.
/// This reuses the assembler's own lexer
/// ([`nessemble_core::tooling::highlight`]), so the browser highlights tokens
/// exactly as the language server does.
#[must_use]
#[wasm_bindgen]
pub fn tokenize(source: &str) -> Vec<u32> {
    let toks = tooling::highlight(source);
    let mut out = Vec::with_capacity(toks.len() * 3);
    for t in toks {
        out.push(t.start);
        out.push(t.len);
        out.push(token_class_id(t.class));
    }
    out
}

/// The highlight-class **id** for a class. Kept explicit (not the core enum's
/// discriminant) so `tokenize`'s wire format is stable regardless of the enum's
/// layout; index-aligned with [`token_classes`].
fn token_class_id(class: TokenClass) -> u32 {
    match class {
        TokenClass::Directive => 0,
        TokenClass::Instruction => 1,
        TokenClass::Identifier => 2,
        TokenClass::Number => 3,
        TokenClass::String => 4,
        TokenClass::Comment => 5,
        TokenClass::Operator => 6,
    }
}

/// The highlight-class **names**, indexed by the class id in [`tokenize`]'s
/// output — the self-describing legend a JS consumer uses to turn an id into a
/// CSS class (e.g. `na-tok-<name>`).
#[must_use]
#[wasm_bindgen]
pub fn token_classes() -> Vec<String> {
    [
        "directive",
        "instruction",
        "identifier",
        "number",
        "string",
        "comment",
        "operator",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect()
}

/// Reformat `source` with nessemble's default formatter and return the result —
/// the same transform the CLI's `nessemble format` and the language server apply
/// (indent normalization, comma spacing, trailing-whitespace trim, …). It never
/// fails: unparsable input is returned reformatted as best it can be.
#[must_use]
#[wasm_bindgen]
pub fn format(source: &str) -> String {
    tooling::format(source)
}

/// Format a core diagnostic for display (`file: line N: message`, or just the
/// message for file-less diagnostics).
fn format_diag(diag: &Diag) -> String {
    if diag.line == 0 {
        diag.message.clone()
    } else {
        format!("{}: line {}: {}", diag.file, diag.line, diag.message)
    }
}

/// Build a custom pseudo-op resolver from the requested `pseudo` map: an inline
/// source is used directly, and `true` enables the matching built-in script.
/// Anything unresolved errors as an unknown directive.
fn build_resolver(pseudo: &HashMap<String, PseudoSpec>) -> CustomResolver {
    let mut sources: HashMap<String, String> = HashMap::new();
    for (name, spec) in pseudo {
        match spec {
            PseudoSpec::Source(src) => {
                sources.insert(name.clone(), src.clone());
            }
            PseudoSpec::Enabled(true) => {
                if let Some((_, src)) = BUILTIN_SCRIPTS.iter().find(|(n, _)| *n == name) {
                    sources.insert(name.clone(), (*src).to_string());
                }
            }
            PseudoSpec::Enabled(false) => {}
        }
    }
    Box::new(move |name, ints, texts, base_dir| match sources.get(name) {
        Some(src) => nessemble_script::run(src, ints, texts, base_dir),
        None => Err(format!("unknown custom pseudo-instruction `.{name}`")),
    })
}

/// Route Rust panics to the browser console with a readable message. Runs once
/// on module start; the [`assemble`] path returns errors as data and does not
/// panic on bad input.
#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();
}

// The `#[wasm_bindgen]` getters compile as ordinary methods on the host, so the
// assembly logic is exercised by normal `cargo test` (no wasm toolchain needed).
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_source_assembles() {
        let r = assemble("  lda #$00\n", "");
        assert!(r.ok(), "errors: {:?}", r.errors());
        assert_eq!(r.rom(), vec![0xA9, 0x00]);
    }

    #[test]
    fn a_bad_program_returns_an_error_not_a_panic() {
        let r = assemble("  notareal\n", "");
        assert!(!r.ok());
        assert_eq!(r.errors().len(), 1);
        assert!(r.rom().is_empty());
    }

    #[test]
    fn invalid_options_are_reported() {
        let r = assemble("  nop\n", "{ not json");
        assert!(!r.ok());
        assert!(r.errors()[0].contains("invalid options"));
    }

    #[test]
    fn an_inline_custom_pseudo_op_runs() {
        let opts = r#"{"pseudo":{"double":"fn custom(ints, texts) { [ints[0] * 2] }"}}"#;
        let r = assemble("  .double 5\n", opts);
        assert!(r.ok(), "errors: {:?}", r.errors());
        assert_eq!(r.rom(), vec![10]);
    }

    #[test]
    fn a_builtin_script_enabled_by_name_runs() {
        let r = assemble(
            "  .ease \"easeOutBounce\", 0, $20, $40\n",
            r#"{"pseudo":{"ease":true}}"#,
        );
        assert!(r.ok(), "errors: {:?}", r.errors());
        assert!(!r.rom().is_empty());
    }

    #[test]
    fn an_undeclared_custom_pseudo_op_errors() {
        let r = assemble("  .mystery 1\n", "");
        assert!(!r.ok());
        assert!(r.errors()[0].contains("mystery"));
    }

    #[test]
    fn tokenize_packs_class_triples() {
        // "lda #$00 ; c\n" → instruction(0,3) operator(4,1) number(5,3) comment(9,3);
        // whitespace and the newline are dropped.
        assert_eq!(
            tokenize("lda #$00 ; c\n"),
            vec![
                0, 3, 1, // lda → instruction
                4, 1, 6, // #   → operator
                5, 3, 3, // $00 → number
                9, 3, 5, // ; c → comment
            ]
        );
    }

    #[test]
    fn tokenize_offsets_are_utf16() {
        // `é` is 2 UTF-8 bytes but 1 UTF-16 unit: `nop` starts at 4, not 5.
        assert_eq!(
            tokenize("; é\nnop\n"),
            vec![
                0, 3, 5, // ; é → comment
                4, 3, 1, // nop → instruction
            ]
        );
    }

    #[test]
    fn format_reindents_source() {
        // A label stays in column 0; an instruction is normalized to the default
        // indent. `format` mirrors `tooling::format`, so this just checks the
        // export is wired up.
        assert_eq!(format("label:\nlda #$00\n"), tooling::format("label:\nlda #$00\n"));
        assert!(format("lda #$00\n").starts_with(' '));
    }

    #[test]
    fn token_classes_legend_aligns_with_ids() {
        let names = token_classes();
        assert_eq!(names.len(), 7);
        assert_eq!(names[0], "directive");
        assert_eq!(names[1], "instruction");
        assert_eq!(names[3], "number");
        assert_eq!(names[6], "operator");
    }
}
