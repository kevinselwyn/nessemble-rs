//! `reference` subcommand: opcode, directive, and script-API lookup backed by
//! **locally bundled data** (the `nessemble-isa` opcode table, a static
//! directive list, and the `nessemble-script-api` host API catalog), rather
//! than the reference tool's network call to the registry.
//!
//! The `script` category is deliberately not gated on the `scripting` feature:
//! the catalog is data with no Rhai behind it, so a build that cannot *run* a
//! script can still tell you what one may call.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use nessemble_i18n::t;
use nessemble_isa::{DIRECTIVES, OPCODES};
use nessemble_script_api::{in_domain, lookup_ignore_case, Domain, ScriptApi};

/// Run `reference` with 0, 1, or 2 terms. Returns `(output, exit_code)`.
pub fn run(term1: Option<&str>, term2: Option<&str>) -> (String, u8) {
    match (term1, term2) {
        (None, _) => (list_categories(), 0),
        (Some(cat), None) => match cat.to_ascii_lowercase().as_str() {
            "instructions" | "instruction" | "opcodes" => (list_instructions(), 0),
            "directives" | "pseudos" | "pseudo" => (list_directives(), 0),
            "script" | "scripts" | "scripting" => (list_script_api(), 0),
            other => (t!("reference-not-found", term = other) + "\n", 1),
        },
        (Some(cat), Some(term)) => match cat.to_ascii_lowercase().as_str() {
            "instructions" | "instruction" | "opcodes" => instruction_detail(term),
            "directives" | "pseudos" | "pseudo" => directive_detail(term),
            "script" | "scripts" | "scripting" => script_detail(term),
            other => (t!("reference-not-found", term = other) + "\n", 1),
        },
    }
}

fn list_categories() -> String {
    "Categories:\n  instructions\n  directives\n  script\n".to_string()
}

fn list_instructions() -> String {
    let mnemonics: BTreeSet<&str> = OPCODES.iter().map(|o| o.mnemonic).collect();
    let mut out = String::from("Instructions:\n");
    for (i, m) in mnemonics.iter().enumerate() {
        out.push_str(m);
        out.push(if (i + 1) % 8 == 0 { '\n' } else { ' ' });
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn instruction_detail(mnemonic: &str) -> (String, u8) {
    let mut rows: Vec<&nessemble_isa::Opcode> = OPCODES
        .iter()
        .filter(|o| o.mnemonic.eq_ignore_ascii_case(mnemonic))
        .collect();
    if rows.is_empty() {
        return (t!("reference-not-found", term = mnemonic) + "\n", 1);
    }
    rows.sort_by_key(|o| o.opcode);
    let mut out = format!("{}:\n", rows[0].mnemonic);
    for o in rows {
        let flag = if o.is_undocumented() {
            " (undocumented)"
        } else {
            ""
        };
        let _ = writeln!(
            out,
            "  {:<12} ${:02X}  {} byte(s), {} cycles{}",
            o.mode.label(),
            o.opcode,
            o.length,
            o.timing,
            flag
        );
    }
    (out, 0)
}

fn list_directives() -> String {
    let max = DIRECTIVES.iter().map(|(n, _)| n.len()).max().unwrap_or(0);
    let mut out = String::from("Directives:\n");
    for (name, desc) in DIRECTIVES {
        let _ = writeln!(out, "  {name:<max$}  {desc}");
    }
    out
}

fn directive_detail(term: &str) -> (String, u8) {
    let needle = term.trim_start_matches('.');
    for (name, desc) in DIRECTIVES {
        if name.split(['/', ' ']).any(|n| {
            n.trim()
                .trim_start_matches('.')
                .eq_ignore_ascii_case(needle)
        }) {
            return (format!("{name}\n  {desc}\n"), 0);
        }
    }
    (t!("reference-not-found", term = term) + "\n", 1)
}

/// Every host-API entry a pseudo-op script can call, grouped by domain and
/// aligned on the signature — the offline half of the Extending page's table of
/// contents.
fn list_script_api() -> String {
    let width = nessemble_script_api::SCRIPT_API
        .iter()
        .map(|e| e.signature.len())
        .max()
        .unwrap_or(0);
    // Signatures here are far wider than a directive's name, and the summaries
    // are whole sentences, so the second column is wrapped rather than left to
    // run off the terminal.
    let indent = 2 + width + 2;
    let mut out = String::from("Script API:\n");
    for domain in Domain::ALL {
        let _ = writeln!(out, "\n{}:", domain.title());
        for entry in in_domain(*domain) {
            let summary = wrap(entry.summary, LINE_WIDTH.saturating_sub(indent), indent);
            let _ = writeln!(
                out,
                "  {:<width$}  {summary}",
                entry.signature,
                width = width
            );
        }
    }
    out
}

/// Total width the wrapped `script` listing aims for.
const LINE_WIDTH: usize = 96;

/// Wrap `text` to `width` columns, indenting every line after the first by
/// `indent` spaces.
///
/// A word longer than `width` is left whole on its own line rather than broken:
/// the long words here are signatures, URLs, and back-ticked identifiers, and a
/// split one is worse than a long line.
fn wrap(text: &str, width: usize, indent: usize) -> String {
    let width = width.max(24);
    let mut out = String::new();
    let mut column = 0;
    for word in text.split_whitespace() {
        if column > 0 && column + 1 + word.len() > width {
            let _ = write!(out, "\n{:indent$}", "", indent = indent);
            column = 0;
        } else if column > 0 {
            out.push(' ');
            column += 1;
        }
        out.push_str(word);
        column += word.len();
    }
    out
}

/// One entry in full. A name can be catalogued more than once (`read_blob` is
/// both a method on a file handle and a one-call function), so every match is
/// printed rather than the first.
fn script_detail(term: &str) -> (String, u8) {
    let matches: Vec<&ScriptApi> = lookup_ignore_case(term).collect();
    if matches.is_empty() {
        return (t!("reference-not-found", term = term) + "\n", 1);
    }
    let mut out = String::new();
    for entry in matches {
        let _ = writeln!(out, "{}", entry.signature);
        let _ = writeln!(out, "  {}", entry.summary);
        let _ = writeln!(out, "  {}", entry.domain.title());
        if let Some(note) = entry.availability.note() {
            let _ = writeln!(out, "  {note}");
        }
        let _ = writeln!(out, "  {}", entry.docs_url());
    }
    (out, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_is_a_listed_category() {
        let (out, code) = run(None, None);
        assert_eq!(code, 0);
        assert!(out.contains("script"), "{out}");
    }

    #[test]
    fn listing_groups_every_domain_and_names_every_entry() {
        let (out, code) = run(Some("script"), None);
        assert_eq!(code, 0);
        for domain in Domain::ALL {
            assert!(out.contains(domain.title()), "missing group: {domain:?}");
        }
        for entry in nessemble_script_api::SCRIPT_API {
            assert!(
                out.contains(entry.signature),
                "missing entry: {}",
                entry.signature
            );
        }
    }

    #[test]
    fn detail_prints_summary_availability_and_docs_link() {
        let (out, code) = run(Some("script"), Some("nes_shade"));
        assert_eq!(code, 0);
        assert!(out.contains("nes_shade(value)"), "{out}");
        assert!(out.contains("Palette"), "{out}");
        assert!(out.contains("#palette-quantization"), "{out}");
    }

    #[test]
    fn detail_notes_a_feature_gate() {
        let (out, _) = run(Some("script"), Some("decode_png_file"));
        assert!(out.contains("WebAssembly"), "{out}");
        // An always-present entry says nothing about availability.
        let (out, _) = run(Some("script"), Some("decode_png"));
        assert!(!out.contains("WebAssembly"), "{out}");
    }

    #[test]
    fn detail_prints_every_entry_sharing_a_name() {
        // `read_blob` is both a method on a file handle and a free function.
        let (out, code) = run(Some("script"), Some("read_blob"));
        assert_eq!(code, 0);
        assert!(out.contains("file.read_blob([n])"), "{out}");
        assert!(out.contains("read_blob(path)"), "{out}");
    }

    #[test]
    fn detail_is_case_insensitive_and_misses_are_an_error() {
        let (_, code) = run(Some("script"), Some("NES_SHADE"));
        assert_eq!(code, 0);
        let (_, code) = run(Some("script"), Some("no_such_function"));
        assert_eq!(code, 1);
    }

    #[test]
    fn wrapping_indents_continuations_and_never_splits_a_word() {
        let long = "supercalifragilisticexpialidocious";
        assert_eq!(wrap(long, 5, 2), long);
        let wrapped = wrap("one two three four five six seven", 12, 3);
        for line in wrapped.lines().skip(1) {
            assert!(
                line.starts_with("   "),
                "continuation not indented: {line:?}"
            );
        }
        assert_eq!(
            wrapped.split_whitespace().collect::<Vec<_>>(),
            ["one", "two", "three", "four", "five", "six", "seven"]
        );
    }
}
