//! JSON parsing for pseudo-op scripts: converts a document into native Rhai
//! values (map/array/string/int/float/bool/`()`) so a script walks it the way
//! it would walk one it built by hand.
//!
//! Unlike XML ([`crate::xml`]), this needs no opaque handle type: a JSON value
//! has no per-node behavior worth deferring (no attributes, no `.find`), so
//! converting eagerly costs nothing a script wouldn't have paid anyway. See
//! `plans/013-structured-data-parsing.md` §2.3–§2.4.
//!
//! `serde_json` is already a workspace dependency (`nessemble-cli`,
//! `nessemble-core`'s dev-dependencies), so this adds no dependency the
//! workspace does not already build.

use rhai::{Array, Dynamic, Map};

/// Parse `src` as JSON, returning the document as a Rhai value.
///
/// # Errors
/// Returns `serde_json::Error`'s own message, which already names the line and
/// column of the failure.
pub fn parse(src: &str) -> Result<Dynamic, serde_json::Error> {
    let value: serde_json::Value = serde_json::from_str(src)?;
    Ok(to_dynamic(value))
}

fn to_dynamic(value: serde_json::Value) -> Dynamic {
    match value {
        serde_json::Value::Null => Dynamic::UNIT,
        serde_json::Value::Bool(b) => Dynamic::from(b),
        serde_json::Value::Number(n) => n
            .as_i64()
            .map_or_else(|| Dynamic::from(n.as_f64().unwrap_or(0.0)), Dynamic::from),
        serde_json::Value::String(s) => Dynamic::from(s),
        serde_json::Value::Array(arr) => {
            let out: Array = arr.into_iter().map(to_dynamic).collect();
            Dynamic::from(out)
        }
        serde_json::Value::Object(obj) => {
            let mut out = Map::new();
            for (k, v) in obj {
                out.insert(k.into(), to_dynamic(v));
            }
            Dynamic::from(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalars_convert_to_their_rhai_equivalents() {
        assert!(parse("null").unwrap().is_unit());
        assert!(parse("true").unwrap().as_bool().unwrap());
        assert_eq!(parse("42").unwrap().as_int().unwrap(), 42);
        assert!((parse("1.5").unwrap().as_float().unwrap() - 1.5).abs() < f64::EPSILON);
        assert_eq!(
            parse("\"hi\"").unwrap().into_string().unwrap(),
            "hi".to_string()
        );
    }

    #[test]
    fn a_large_integer_still_converts_as_an_int() {
        // Within i64 range but past i32 -- must not silently become a float.
        assert_eq!(
            parse("9007199254740993").unwrap().as_int().unwrap(),
            9_007_199_254_740_993
        );
    }

    #[test]
    fn arrays_and_objects_convert_recursively() {
        let value = parse(r#"{"a": [1, 2, {"b": true}], "c": null}"#).unwrap();
        let map = value.cast::<Map>();
        let arr = map.get("a").unwrap().clone().cast::<Array>();
        assert_eq!(arr[0].as_int().unwrap(), 1);
        assert_eq!(arr[1].as_int().unwrap(), 2);
        let inner = arr[2].clone().cast::<Map>();
        assert!(inner.get("b").unwrap().as_bool().unwrap());
        assert!(map.get("c").unwrap().is_unit());
    }

    #[test]
    fn a_syntax_error_names_its_position() {
        let err = parse("{\"a\": }").unwrap_err();
        // serde_json's own Display already carries line/column.
        let msg = err.to_string();
        assert!(msg.contains("line 1"), "{msg}");
    }
}
