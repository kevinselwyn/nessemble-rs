//! Catalog-driven `.rhai` editor support: completion, hover, and signature
//! help built entirely from [`nessemble_script_api::SCRIPT_API`].
//!
//! This is the half of `plans/014-scripting-docs-and-tooling.md` §5 that needs
//! no Rhai dependency at all — the catalog is data — so it is compiled
//! unconditionally, unlike the `scripting` module (feature `scripting`), which
//! needs a compiled AST. A build with `--no-default-features --features lsp`
//! still completes and documents the host API through this module alone
//! (§5.8, acceptance item 8).

use lsp_types::{
    CompletionItem, CompletionItemKind, Documentation, MarkupContent, MarkupKind,
    ParameterInformation, ParameterLabel, Position, Range, SignatureHelp, SignatureInformation,
};
use nessemble_script_api::{ApiKind, ScriptApi, SCRIPT_API};

/// Completion items for a `.rhai` buffer: every catalog entry, plus every
/// script-local `fn` found by a lightweight scan of `text` (script-defined
/// functions have no catalog entry of their own).
pub(crate) fn completions(text: &str) -> Vec<CompletionItem> {
    let mut items: Vec<CompletionItem> = SCRIPT_API.iter().map(catalog_item).collect();
    items.extend(local_fn_names(text).into_iter().map(|name| CompletionItem {
        label: name,
        kind: Some(CompletionItemKind::FUNCTION),
        detail: Some("script-local function".to_string()),
        ..CompletionItem::default()
    }));
    items
}

fn catalog_item(entry: &ScriptApi) -> CompletionItem {
    CompletionItem {
        label: entry.name.to_string(),
        kind: Some(match entry.kind {
            ApiKind::Function => CompletionItemKind::FUNCTION,
            ApiKind::Method => CompletionItemKind::METHOD,
            ApiKind::Property => CompletionItemKind::PROPERTY,
            ApiKind::Type => CompletionItemKind::CLASS,
        }),
        detail: Some(entry.signature.to_string()),
        documentation: Some(Documentation::MarkupContent(MarkupContent {
            kind: MarkupKind::Markdown,
            value: entry_markdown(entry),
        })),
        ..CompletionItem::default()
    }
}

/// Hover markdown for the host-API entries named `name` — usually one, but a
/// name can be both a free function and a method (`read_blob`), in which case
/// every entry is shown. `None` when `name` is not in the catalog at all.
pub(crate) fn hover(name: &str) -> Option<String> {
    let mut entries = nessemble_script_api::lookup(name).peekable();
    entries.peek()?;
    Some(
        entries
            .map(entry_markdown)
            .collect::<Vec<_>>()
            .join("\n\n---\n\n"),
    )
}

/// Hover markdown for one catalog entry: signature, summary, an availability
/// note when the entry isn't in every build, and a link to the docs section
/// that explains it — the same sentence in the book and under the cursor
/// (`plans/014-scripting-docs-and-tooling.md` §5.4).
fn entry_markdown(entry: &ScriptApi) -> String {
    use std::fmt::Write as _;

    let mut md = format!("**`{}`**\n\n{}", entry.signature, entry.summary);
    if let Some(note) = entry.availability.note() {
        let _ = write!(md, "\n\n_{note}_");
    }
    let _ = write!(md, "\n\n[Extending docs]({})", entry.docs_url());
    md
}

/// Script-local function names in `text`, from a scan of `fn NAME(` (or
/// `private fn NAME(`) declarations — no Rhai parse required, so this works
/// even while the buffer has a syntax error and even without the `scripting`
/// feature.
pub(crate) fn local_fn_names(text: &str) -> Vec<String> {
    text.lines().filter_map(fn_name_on_line).collect()
}

/// The function name declared by a `fn NAME(` (optionally `private fn`) on
/// `line`, if any.
fn fn_name_on_line(line: &str) -> Option<String> {
    let idx = line.find("fn ")?;
    let rest = line[idx + 3..].trim_start();
    let name: String = rest
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

/// The identifier under `pos` in `text`, with its range — used for hover and
/// go-to-definition on both host-API names and script-local `fn`s. `pos`'s
/// UTF-16 `character` is treated as a char index directly: exact for the
/// ASCII identifiers Rhai scripts use, approximate around non-ASCII content
/// elsewhere on the line.
pub(crate) fn identifier_at(text: &str, pos: Position) -> Option<(String, Range)> {
    let line = text.split('\n').nth(pos.line as usize)?;
    let chars: Vec<char> = line.chars().collect();
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    let idx = (pos.character as usize).min(chars.len());
    let (start, end) = if idx < chars.len() && is_ident(chars[idx]) {
        let mut s = idx;
        let mut e = idx;
        while s > 0 && is_ident(chars[s - 1]) {
            s -= 1;
        }
        while e < chars.len() && is_ident(chars[e]) {
            e += 1;
        }
        (s, e)
    } else if idx > 0 && is_ident(chars[idx - 1]) {
        let mut s = idx - 1;
        while s > 0 && is_ident(chars[s - 1]) {
            s -= 1;
        }
        (s, idx)
    } else {
        return None;
    };
    let name: String = chars[start..end].iter().collect();
    Some((
        name,
        Range::new(
            Position::new(pos.line, start as u32),
            Position::new(pos.line, end as u32),
        ),
    ))
}

/// Signature help for the call enclosing `pos` in `text`: the callee's name is
/// found by scanning backward for its unmatched opening `(`, and the active
/// parameter is the count of top-level commas since. `None` when the cursor
/// isn't inside a call, or the callee names nothing in the catalog.
pub(crate) fn signature_help(text: &str, pos: Position) -> Option<SignatureHelp> {
    let offset = char_offset(text, pos);
    let chars: Vec<char> = text.chars().collect();
    // A comma or paren inside a `"…"` string argument (e.g. the delimiter in
    // `parse_int_list(text, ",")`) is not call syntax; the mask keeps the
    // paren/comma scan below from mistaking one for it.
    let in_string = string_literal_mask(&chars);
    let open = enclosing_call_open(&chars, &in_string, offset)?;
    let active_parameter = top_level_commas(&chars, &in_string, open + 1, offset);
    let name = identifier_before(&chars, open)?;

    let entries: Vec<&ScriptApi> = nessemble_script_api::lookup(&name)
        .filter(|e| matches!(e.kind, ApiKind::Function | ApiKind::Method))
        .collect();
    if entries.is_empty() {
        return None;
    }
    let signatures: Vec<SignatureInformation> = entries
        .iter()
        .flat_map(|e| signature_informations(e))
        .collect();
    if signatures.is_empty() {
        return None;
    }
    let active_signature = signatures
        .iter()
        .position(|s| {
            s.parameters
                .as_ref()
                .is_some_and(|p| p.len() > active_parameter)
        })
        .unwrap_or(signatures.len() - 1);

    Some(SignatureHelp {
        signatures,
        active_signature: Some(active_signature as u32),
        active_parameter: Some(active_parameter as u32),
    })
}

/// Convert an LSP `Position` (UTF-16) to a `char` offset into `text`. Rhai
/// source is overwhelmingly ASCII, so this is exact for the case that matters
/// and merely approximate around any non-ASCII content on the line.
fn char_offset(text: &str, pos: Position) -> usize {
    let mut offset = 0usize;
    for (i, line) in text.split('\n').enumerate() {
        if i as u32 == pos.line {
            let mut utf16 = 0u32;
            for (ci, ch) in line.chars().enumerate() {
                if utf16 >= pos.character {
                    return offset + ci;
                }
                utf16 += ch.len_utf16() as u32;
            }
            return offset + line.chars().count();
        }
        offset += line.chars().count() + 1; // `+1` for the newline itself
    }
    offset
}

/// The char indices of `chars` that fall inside a `"…"` string literal
/// (`\"` escapes honored), so a paren or comma written *inside* a string
/// argument — the delimiter in `parse_int_list(text, ",")` — is never
/// mistaken for call syntax by [`enclosing_call_open`]/[`top_level_commas`].
fn string_literal_mask(chars: &[char]) -> Vec<bool> {
    let mut mask = vec![false; chars.len()];
    let mut in_string = false;
    let mut escaped = false;
    for (i, &c) in chars.iter().enumerate() {
        if in_string {
            mask[i] = true;
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
        } else if c == '"' {
            mask[i] = true;
            in_string = true;
        }
    }
    mask
}

/// Walking backward from `offset`, the index of the nearest unmatched `(` —
/// the call this offset sits inside the arguments of.
fn enclosing_call_open(chars: &[char], in_string: &[bool], offset: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut i = offset.min(chars.len());
    while i > 0 {
        i -= 1;
        if in_string[i] {
            continue;
        }
        match chars[i] {
            ')' => depth += 1,
            '(' => {
                if depth == 0 {
                    return Some(i);
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    None
}

/// The number of top-level (unbracketed) commas in `chars[start..end]`.
fn top_level_commas(chars: &[char], in_string: &[bool], start: usize, end: usize) -> usize {
    let mut depth = 0i32;
    let mut commas = 0usize;
    let end = end.min(chars.len()).max(start);
    for i in start..end {
        if in_string[i] {
            continue;
        }
        match chars[i] {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => commas += 1,
            _ => {}
        }
    }
    commas
}

/// The identifier (and, for a method call, its receiver-qualified name is not
/// included — only the bare method name) immediately preceding `open`.
fn identifier_before(chars: &[char], open: usize) -> Option<String> {
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    let mut start = open;
    while start > 0 && is_ident(chars[start - 1]) {
        start -= 1;
    }
    (start != open).then(|| chars[start..open].iter().collect())
}

/// One [`ScriptApi`] entry's possible arities as [`SignatureInformation`]. A
/// `[, name]`-bracketed optional tail (§3.1's signature convention) yields
/// two: the required parameters alone, and the required parameters plus the
/// optional ones — e.g. `parse_int_list(text, delim[, radix])` offers both
/// `(text, delim)` and `(text, delim, radix)`.
fn signature_informations(entry: &ScriptApi) -> Vec<SignatureInformation> {
    let Some(params) = param_list(entry.signature) else {
        return Vec::new();
    };
    let head = entry.signature.split('(').next().unwrap_or(entry.name);
    let bracket = params.find('[');
    let required: Vec<String> = match bracket {
        Some(i) => split_params(params[..i].trim_end_matches(',').trim()),
        None => split_params(params),
    };
    let mut out = vec![make_signature(head, entry.summary, &required)];
    if let Some(i) = bracket {
        let inner = params[i..]
            .trim_start_matches('[')
            .trim_end_matches(']')
            .trim_start_matches(',')
            .trim();
        let mut full = required;
        full.extend(split_params(inner));
        out.push(make_signature(head, entry.summary, &full));
    }
    out
}

/// The text between a signature's outer parentheses, e.g. `text, delim[,
/// radix]` from `parse_int_list(text, delim[, radix])`. `None` for a
/// receiver-less `signature` (a [`ApiKind::Type`] entry, which is never
/// looked up here since [`signature_help`] filters to functions/methods).
fn param_list(signature: &str) -> Option<&str> {
    let open = signature.find('(')?;
    let close = signature.rfind(')')?;
    (close > open).then(|| &signature[open + 1..close])
}

/// Split a parameter list on top-level commas, trimming each field.
fn split_params(params: &str) -> Vec<String> {
    if params.trim().is_empty() {
        return Vec::new();
    }
    params.split(',').map(|p| p.trim().to_string()).collect()
}

fn make_signature(head: &str, summary: &str, params: &[String]) -> SignatureInformation {
    SignatureInformation {
        label: format!("{head}({})", params.join(", ")),
        documentation: Some(Documentation::String(summary.to_string())),
        parameters: Some(
            params
                .iter()
                .map(|p| ParameterInformation {
                    label: ParameterLabel::Simple(p.clone()),
                    documentation: None,
                })
                .collect(),
        ),
        active_parameter: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completions_include_catalog_entries_and_local_functions() {
        let text = "fn helper(x) { x }\nfn custom(ints, texts) { helper(1) }\n";
        let items = completions(text);
        assert!(items.iter().any(|i| i.label == "decode_png_file"));
        assert!(items.iter().any(|i| i.label == "helper"));
        assert!(items.iter().any(|i| i.label == "custom"));
    }

    #[test]
    fn hover_renders_signature_and_summary() {
        let md = hover("nes_shade").expect("catalogued");
        assert!(md.contains("nes_shade(value)"));
        assert!(md.contains("extending.html#palette-quantization"));
    }

    #[test]
    fn hover_joins_every_entry_sharing_a_name() {
        let md = hover("read_blob").expect("catalogued");
        assert!(md.contains("file.read_blob"));
        assert!(md.contains("read_blob(path)"));
        assert!(md.contains("---"));
    }

    #[test]
    fn hover_is_none_for_an_unknown_name() {
        assert!(hover("not_a_real_function").is_none());
    }

    #[test]
    fn signature_help_reports_the_active_parameter() {
        let text = "fn custom(ints, texts) { format_hex(255, ";
        let pos = Position::new(0, text.chars().count() as u32);
        let help = signature_help(text, pos).expect("inside a call");
        assert_eq!(help.active_parameter, Some(1));
        assert!(help.signatures[0].label.starts_with("format_hex("));
    }

    #[test]
    fn signature_help_offers_both_arities_of_an_optional_argument() {
        let text = "fn custom(ints, texts) { parse_int_list(texts[0], \",\", ";
        let pos = Position::new(0, text.chars().count() as u32);
        let help = signature_help(text, pos).expect("inside a call");
        assert_eq!(help.signatures.len(), 2);
        assert_eq!(help.active_signature, Some(1));
        assert_eq!(help.active_parameter, Some(2));
    }

    #[test]
    fn signature_help_is_none_outside_any_call() {
        let text = "let x = 1;";
        assert!(signature_help(text, Position::new(0, 5)).is_none());
    }

    #[test]
    fn identifier_at_finds_the_word_under_the_cursor() {
        let text = "nes_shade(1)";
        let (name, range) = identifier_at(text, Position::new(0, 3)).expect("on `nes_shade`");
        assert_eq!(name, "nes_shade");
        assert_eq!(range, Range::new(Position::new(0, 0), Position::new(0, 9)));
    }

    #[test]
    fn identifier_at_is_none_between_words() {
        assert!(identifier_at("a  b", Position::new(0, 2)).is_none());
    }
}
