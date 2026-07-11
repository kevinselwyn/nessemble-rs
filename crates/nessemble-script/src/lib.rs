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

use rhai::{Array, Blob, Dynamic, Engine, EvalAltResult};

/// Run `source`'s `custom(ints, texts)` function and return the emitted bytes,
/// or a human-readable error message (a thrown string, or an engine error).
pub fn run(source: &str, ints: &[i64], texts: &[String]) -> Result<Vec<u8>, String> {
    let engine = engine();
    let ast = engine.compile(source).map_err(|e| e.to_string())?;

    let int_arr: Array = ints.iter().map(|&i| Dynamic::from(i)).collect();
    let text_arr: Array = texts.iter().map(|t| Dynamic::from(t.clone())).collect();

    let mut scope = rhai::Scope::new();
    let result: Dynamic = engine
        .call_fn(&mut scope, &ast, "custom", (int_arr, text_arr))
        .map_err(|e| error_message(&e))?;

    dynamic_to_bytes(result)
}

/// A sandboxed engine with guards against runaway scripts. Rhai has no ambient
/// filesystem or network access, so this is safe to run untrusted directives.
fn engine() -> Engine {
    let mut engine = Engine::new();
    engine.set_max_operations(10_000_000);
    engine.set_max_call_levels(64);
    engine.set_max_string_size(1_000_000);
    engine.set_max_array_size(1_000_000);
    // Allow deeply-nested arithmetic expressions (e.g. easing polynomials).
    engine.set_max_expr_depths(0, 0);
    engine
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

    #[test]
    fn sums_integer_arguments() {
        let src = "fn custom(ints, texts) { let s = 0; for i in ints { s += i; } [s % 256] }";
        assert_eq!(run(src, &[1, 2, 3], &[]).unwrap(), vec![6]);
    }

    #[test]
    fn float_math_matches_expectations() {
        // Integer args used in float math must be converted explicitly.
        let src = "fn custom(ints, texts) { \
                   let t = ints[0].to_float() / ints[1].to_float(); \
                   [(t * 16.0).floor().to_int() % 256] }";
        // (3 / 4) * 16 = 12
        assert_eq!(run(src, &[3, 4], &[]).unwrap(), vec![12]);
    }

    #[test]
    fn thrown_string_becomes_the_error() {
        let src = "fn custom(ints, texts) { throw \"bad thing\" }";
        assert_eq!(run(src, &[], &[]).unwrap_err(), "bad thing");
    }

    #[test]
    fn receives_string_arguments() {
        let src = "fn custom(ints, texts) { texts[0].to_blob() }";
        assert_eq!(run(src, &[], &["Hi".to_string()]).unwrap(), b"Hi");
    }
}
