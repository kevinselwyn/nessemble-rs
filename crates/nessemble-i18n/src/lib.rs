//! Internationalization for `nessemble-rs`, built on **Project Fluent**.
//!
//! Every user-facing string is looked up by a stable id from a per-locale
//! Fluent catalog (the equivalent of the reference tool's gettext `_()` calls).
//! `en-US` ships embedded and is always the fallback; additional locales can be
//! registered at runtime with [`register_locale`], and the active locale chosen
//! with [`set_locale`] (or from the `NESSEMBLE_LANG` / `LANG` / `LC_ALL`
//! environment at startup).
//!
//! Use the [`t!`] macro:
//!
//! ```
//! use nessemble_i18n::t;
//! assert_eq!(t!("invalid-mode"), "Invalid addressing mode");
//! assert_eq!(t!("unknown-opcode", mnemonic = "BLA"), "Unknown opcode `BLA`");
//! ```

use std::cell::RefCell;
use std::collections::HashMap;

use fluent_bundle::{FluentArgs, FluentBundle, FluentResource, FluentValue};
use unic_langid::LanguageIdentifier;

/// The default (and always-present fallback) locale.
pub const DEFAULT_LOCALE: &str = "en-US";

/// The embedded `en-US` catalog source.
const EN_US_FTL: &str = include_str!("../locales/en-US.ftl");

type Bundle = FluentBundle<FluentResource>;

struct Catalog {
    active: String,
    bundles: HashMap<String, Bundle>,
}

impl Catalog {
    fn new() -> Self {
        let mut catalog = Catalog {
            active: DEFAULT_LOCALE.to_string(),
            bundles: HashMap::new(),
        };
        // en-US is always available.
        catalog.insert(DEFAULT_LOCALE, EN_US_FTL);
        // Honour the environment's locale, if a catalog for it exists (only
        // en-US ships today, but a registered stub can be selected this way).
        if let Some(lang) = detect_locale() {
            catalog.active = lang;
        }
        catalog
    }

    /// Build and store a bundle for `lang` from Fluent source.
    fn insert(&mut self, lang: &str, source: &str) {
        let langid: LanguageIdentifier = lang.parse().unwrap_or_else(|_| "en-US".parse().unwrap());
        let mut bundle = FluentBundle::new(vec![langid]);
        // Do not wrap interpolated values in Unicode isolation marks — output
        // must be byte-identical to the reference's plain strings.
        bundle.set_use_isolating(false);
        if let Ok(resource) = FluentResource::try_new(source.to_string()) {
            bundle.add_resource_overriding(resource);
        }
        self.bundles.insert(lang.to_string(), bundle);
    }

    /// Format message `id` from `bundle`, if present.
    fn format_from(bundle: &Bundle, id: &str, args: Option<&FluentArgs>) -> Option<String> {
        let msg = bundle.get_message(id)?;
        let pattern = msg.value()?;
        let mut errors = Vec::new();
        let out = bundle.format_pattern(pattern, args, &mut errors);
        Some(out.into_owned())
    }

    /// Resolve `id`: try the active locale, then fall back to en-US, then the id.
    fn translate(&self, id: &str, args: Option<&FluentArgs>) -> String {
        if let Some(b) = self.bundles.get(&self.active) {
            if let Some(s) = Self::format_from(b, id, args) {
                return s;
            }
        }
        if self.active != DEFAULT_LOCALE {
            if let Some(b) = self.bundles.get(DEFAULT_LOCALE) {
                if let Some(s) = Self::format_from(b, id, args) {
                    return s;
                }
            }
        }
        id.to_string()
    }
}

thread_local! {
    static CATALOG: RefCell<Catalog> = RefCell::new(Catalog::new());
}

/// Extract a language id from the locale environment (e.g. `en_US.UTF-8` →
/// `en-US`), preferring `NESSEMBLE_LANG`.
fn detect_locale() -> Option<String> {
    let raw = ["NESSEMBLE_LANG", "LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|k| std::env::var(k).ok())
        .filter(|v| !v.is_empty())?;
    // Strip encoding/modifier, normalise separators: `en_US.UTF-8` → `en-US`.
    let base = raw.split(['.', '@']).next().unwrap_or(&raw);
    let normalized = base.replace('_', "-");
    if normalized.is_empty() || normalized == "C" || normalized == "POSIX" {
        None
    } else {
        Some(normalized)
    }
}

/// Look up `id`, interpolating `args`. Missing ids fall back to en-US, then to
/// the id itself. Prefer the [`t!`] macro over calling this directly.
#[must_use]
pub fn translate(id: &str, args: &[(&str, String)]) -> String {
    let fluent_args = if args.is_empty() {
        None
    } else {
        let mut fa = FluentArgs::new();
        for (k, v) in args {
            fa.set(*k, FluentValue::from(v.clone()));
        }
        Some(fa)
    };
    CATALOG.with(|c| c.borrow().translate(id, fluent_args.as_ref()))
}

/// Set the active locale for the current thread. Unknown locales still resolve
/// via the en-US fallback.
pub fn set_locale(lang: &str) {
    CATALOG.with(|c| c.borrow_mut().active = lang.to_string());
}

/// The active locale for the current thread.
#[must_use]
pub fn active_locale() -> String {
    CATALOG.with(|c| c.borrow().active.clone())
}

/// Register (or replace) a locale catalog from Fluent source at runtime. This is
/// how translators add a locale; it also backs the tests.
pub fn register_locale(lang: &str, source: &str) {
    CATALOG.with(|c| c.borrow_mut().insert(lang, source));
}

/// Load every `<lang>.ftl` in `dir` as a locale catalog (the file stem is the
/// locale id). Missing directories are ignored. This lets a translator drop a
/// `~/.nessemble/locales/<lang>.ftl` file and select it via `NESSEMBLE_LANG`.
pub fn load_locale_dir(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("ftl") {
            continue;
        }
        if let (Some(lang), Ok(source)) = (
            path.file_stem().and_then(|s| s.to_str()),
            std::fs::read_to_string(&path),
        ) {
            register_locale(lang, &source);
        }
    }
}

/// Translate a message by id, with optional `name = value` arguments.
///
/// ```
/// # use nessemble_i18n::t;
/// let _ = t!("no-errors");
/// let _ = t!("could-not-open", file = "game.asm");
/// ```
#[macro_export]
macro_rules! t {
    ($id:expr) => {
        $crate::translate($id, &[])
    };
    ($id:expr, $($key:ident = $value:expr),+ $(,)?) => {
        $crate::translate(
            $id,
            &[ $( (stringify!($key), ::std::string::ToString::to_string(&$value)) ),+ ],
        )
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_plain_and_parameterized() {
        assert_eq!(t!("invalid-mode"), "Invalid addressing mode");
        assert_eq!(
            t!("unknown-opcode", mnemonic = "BLA"),
            "Unknown opcode `BLA`"
        );
        assert_eq!(
            t!("symbol-not-defined", name = "test"),
            "Symbol `test` was not defined"
        );
    }

    #[test]
    fn numeric_args_have_no_grouping() {
        // A four-digit value must not gain a thousands separator.
        assert_eq!(
            t!("error-line", file = "a.asm", line = 1234, message = "boom"),
            "Error in `a.asm` on line 1234: boom"
        );
    }

    #[test]
    fn trailing_space_is_preserved() {
        assert_eq!(t!("init-prompt-filename"), "Filename: ");
    }

    #[test]
    fn unknown_id_falls_back_to_itself() {
        assert_eq!(t!("no-such-message-id"), "no-such-message-id");
    }

    #[test]
    fn stub_locale_overrides_then_falls_back_to_en_us() {
        register_locale("xx", "invalid-mode = Modo no valido\n");
        set_locale("xx");
        // Overridden message uses the stub locale...
        assert_eq!(t!("invalid-mode"), "Modo no valido");
        // ...while a message the stub omits falls back to en-US.
        assert_eq!(t!("no-errors"), "No errors");
        set_locale(DEFAULT_LOCALE);
    }
}
