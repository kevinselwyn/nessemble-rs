//! The compiled-AST half of `.rhai` editor support: syntax diagnostics, the
//! four script-specific lints, document symbols, folding, and script-local
//! definition/references
//! (`plans/014-scripting-docs-and-tooling.md` §5.2, §5.3, §5.5). Behind the
//! `scripting` feature — see [`crate::api`] for the catalog-driven half that
//! needs no Rhai at all.
//!
//! Diagnostics and the lints walk a compiled [`rhai::AST`], the same
//! `internals`-gated mechanism `nessemble_script::purity::impurity` already
//! uses. Everything else here (document symbols, folding, hover/definition on
//! a script-local `fn`) works from a lightweight scan of the source text
//! instead — it stays useful while the buffer has a syntax error elsewhere,
//! which a full reparse would not survive.

use std::collections::HashSet;

use lsp_types::{
    Diagnostic, DiagnosticSeverity, DocumentSymbol, FoldingRange, FoldingRangeKind, NumberOrString,
    Position, Range, SymbolKind,
};
use rhai::{ASTNode, Engine, Expr, Stmt};

use nessemble_script_api::SCRIPT_API;

/// Rule ids for the four lints (the LSP diagnostic's `code`), matching the
/// vocabulary of §5.3.
mod rule {
    pub const MISSING_CUSTOM: &str = "missing-custom";
    pub const TOP_LEVEL_STATEMENT: &str = "top-level-statement";
    pub const CUSTOM_ARITY: &str = "custom-arity";
    pub const UNKNOWN_HOST_FUNCTION: &str = "unknown-host-function";
}

/// A near-miss on a known name is flagged only within this edit distance —
/// tuned so a typo like `decode_png_fil` is caught and an unrelated Rhai
/// built-in this catalog doesn't list is not (§5.3, §11.3).
const EDIT_DISTANCE_THRESHOLD: usize = 2;

/// A bare engine for compiling a script into its AST, with optimization
/// switched off. This tool inspects the AST the author wrote — a top-level
/// `const` used only inside `custom` is exactly the case `top-level-statement`
/// exists to catch (§2.2), and Rhai's default optimizer propagates such a
/// constant into its use sites and can then treat the now-unreferenced
/// top-level declaration as dead code, which would erase the very thing the
/// lint looks for. `OptimizationLevel::None` keeps the AST a 1:1 parse tree.
fn diagnostics_engine() -> Engine {
    let mut engine = Engine::new();
    engine.set_optimization_level(rhai::OptimizationLevel::None);
    engine
}

/// Diagnostics for a `.rhai` buffer: a parse error when it doesn't compile,
/// otherwise the four lints of §5.3. `is_mapped` — whether a workspace
/// `pseudo.txt` maps some directive to this script — gates `missing-custom`,
/// which is only meaningful for a script something actually calls.
pub(crate) fn diagnostics(text: &str, is_mapped: bool) -> Vec<Diagnostic> {
    let engine = diagnostics_engine();
    let ast = match engine.compile(text) {
        Ok(ast) => ast,
        Err(err) => return vec![parse_error_diagnostic(&err)],
    };

    let mut out = Vec::new();
    let local_fns: Vec<(String, usize)> = ast
        .iter_functions()
        .map(|f| (f.name.to_string(), f.params.len()))
        .collect();

    match local_fns.iter().find(|(name, _)| name == "custom") {
        None if is_mapped => out.push(diagnostic(
            Range::new(Position::new(0, 0), Position::new(0, 0)),
            rule::MISSING_CUSTOM,
            "a pseudo-op mapping points at this script, but it defines no \
             `custom(ints, texts)` function"
                .to_string(),
        )),
        Some((_, arity)) if *arity != 2 => {
            if let Some(range) = fn_def_range(text, "custom") {
                out.push(diagnostic(
                    range,
                    rule::CUSTOM_ARITY,
                    format!(
                        "the host calls `custom` with exactly two arguments (ints, texts); this \
                         one declares {arity}"
                    ),
                ));
            }
        }
        _ => {}
    }

    let top_level: &[Stmt] = ast.as_ref();
    if let Some(first) = top_level.first() {
        out.push(diagnostic(
            rhai_pos_range(first.position(), 1),
            rule::TOP_LEVEL_STATEMENT,
            "a statement outside every `fn` never runs — `custom` is called without evaluating \
             the script body first"
                .to_string(),
        ));
    }

    let local_names: HashSet<&str> = local_fns.iter().map(|(name, _)| name.as_str()).collect();
    let mut reported: HashSet<(String, u32, u32)> = HashSet::new();
    ast.walk(&mut |path: &[ASTNode]| {
        let Some(ASTNode::Expr(expr)) = path.last() else {
            return true;
        };
        let (Expr::FnCall(call, pos) | Expr::MethodCall(call, pos)) = expr else {
            return true;
        };
        let name = call.name.as_str();
        if local_names.contains(name) || nessemble_script_api::lookup(name).next().is_some() {
            return true;
        }
        if let Some(nearest) = nearest_known_name(name) {
            let range = call_name_range(text, *pos, name);
            let key = (name.to_string(), range.start.line, range.start.character);
            if reported.insert(key) {
                out.push(diagnostic(
                    range,
                    rule::UNKNOWN_HOST_FUNCTION,
                    format!(
                        "`{name}` is not a script-local `fn` or a known host function — did you \
                         mean `{nearest}`?"
                    ),
                ));
            }
        }
        true
    });

    out
}

fn diagnostic(range: Range, rule: &str, message: String) -> Diagnostic {
    Diagnostic {
        range,
        // Gentle severity, like the assembly lints (`with_lint` in `lib.rs`):
        // these are advice, not build errors.
        severity: Some(DiagnosticSeverity::HINT),
        source: Some("nessemble-script".to_string()),
        code: Some(NumberOrString::String(rule.to_string())),
        message,
        ..Diagnostic::default()
    }
}

fn parse_error_diagnostic(err: &rhai::ParseError) -> Diagnostic {
    Diagnostic {
        range: rhai_pos_range(err.1, 1),
        severity: Some(DiagnosticSeverity::ERROR),
        source: Some("nessemble-script".to_string()),
        message: err.0.to_string(),
        ..Diagnostic::default()
    }
}

/// An LSP range for a bare rhai [`rhai::Position`]: `width` characters wide
/// from its (1-based) line/column, both converted to 0-based. A `Position`
/// with no line (`Position::NONE`, e.g. from an optimized-away node) falls
/// back to the start of the buffer.
fn rhai_pos_range(pos: rhai::Position, width: u32) -> Range {
    let line = pos.line().unwrap_or(1).saturating_sub(1) as u32;
    let col = pos.position().unwrap_or(1).saturating_sub(1) as u32;
    Range::new(Position::new(line, col), Position::new(line, col + width))
}

/// A call expression's range, widened to `name`'s own length by searching for
/// it on the reported line near the reported column — `rhai::Position` marks
/// a point, not a span, so this recovers a token-accurate range the same way
/// [`crate::diagnostic_range`] narrows an assembler diagnostic to its subject.
fn call_name_range(text: &str, pos: rhai::Position, name: &str) -> Range {
    let line_no = pos.line().unwrap_or(1).saturating_sub(1);
    let col0 = pos.position().unwrap_or(1).saturating_sub(1);
    if let Some(line) = text.lines().nth(line_no) {
        let chars: Vec<char> = line.chars().collect();
        let from = col0.min(chars.len());
        if let Some(start_char) = find_char_substring(&chars, from, name) {
            return Range::new(
                Position::new(line_no as u32, start_char as u32),
                Position::new(line_no as u32, (start_char + name.chars().count()) as u32),
            );
        }
        if let Some(start_char) = find_char_substring(&chars, 0, name) {
            return Range::new(
                Position::new(line_no as u32, start_char as u32),
                Position::new(line_no as u32, (start_char + name.chars().count()) as u32),
            );
        }
    }
    Range::new(
        Position::new(line_no as u32, col0 as u32),
        Position::new(line_no as u32, (col0 + name.chars().count()) as u32),
    )
}

/// The char index of `needle` in `haystack` at or after `from`, if present.
fn find_char_substring(haystack: &[char], from: usize, needle: &str) -> Option<usize> {
    let needle: Vec<char> = needle.chars().collect();
    if needle.is_empty() || needle.len() > haystack.len() || from > haystack.len() - needle.len() {
        return None;
    }
    (from..=haystack.len() - needle.len()).find(|&i| haystack[i..i + needle.len()] == needle[..])
}

/// The nearest catalog name to `name` within [`EDIT_DISTANCE_THRESHOLD`], or
/// `None` if nothing is close enough — the near-miss-only design of §5.3/§11.3.
fn nearest_known_name(name: &str) -> Option<&'static str> {
    let mut seen = HashSet::new();
    let mut best: Option<(&'static str, usize)> = None;
    for entry in SCRIPT_API {
        if !seen.insert(entry.name) {
            continue;
        }
        let d = levenshtein(name, entry.name);
        if d == 0 || d > EDIT_DISTANCE_THRESHOLD {
            continue;
        }
        if best.is_none_or(|(_, bd)| d < bd) {
            best = Some((entry.name, d));
        }
    }
    best.map(|(name, _)| name)
}

/// Levenshtein edit distance between two strings.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, &ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, &cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// The range of the function name in a `fn NAME(` (optionally `private fn
/// NAME(`) declaration in `text`, from a line scan — `rhai::ScriptFnMetadata`
/// carries no position (see `plans/014-scripting-docs-and-tooling.md` §12.4).
fn fn_def_range(text: &str, name: &str) -> Option<Range> {
    for (i, line) in text.lines().enumerate() {
        if let Some(col) = fn_name_column(line, name) {
            return Some(Range::new(
                Position::new(i as u32, col as u32),
                Position::new(i as u32, (col + name.chars().count()) as u32),
            ));
        }
    }
    None
}

/// The char column of `name` in a `fn NAME(` declaration on `line`, if that
/// line declares exactly that function (word-bounded, so `fn customize(`
/// does not match `custom`).
fn fn_name_column(line: &str, name: &str) -> Option<usize> {
    let idx = line.find("fn ")?;
    let after_kw = &line[idx + 3..];
    let rest = after_kw.trim_start();
    let skip_chars = after_kw.chars().count() - rest.chars().count();
    let stripped = rest.strip_prefix(name)?;
    let boundary = stripped
        .chars()
        .next()
        .is_none_or(|c| !c.is_alphanumeric() && c != '_');
    boundary.then_some(idx + 3 + skip_chars)
}

/// `fn NAME(params)`, read straight from the source text (see the module doc
/// for why this doesn't compile the AST).
fn fn_signature_text(text: &str, name: &str) -> Option<String> {
    for line in text.lines() {
        if fn_name_column(line, name).is_some() {
            let after = line.split_once(name)?.1;
            let open = after.find('(')?;
            let close = after[open..].find(')')?;
            return Some(format!("fn {name}{}", &after[open..=open + close]));
        }
    }
    None
}

/// The run of `//`-prefixed lines immediately above the `fn NAME(` line in
/// `text`, joined and stripped of their comment markers — the same "comment
/// run above the definition" convention `crate::preceding_doc` uses for
/// assembly symbols. `None` when there is no such run.
fn doc_comment_above_fn(text: &str, name: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let def_line = lines
        .iter()
        .position(|l| fn_name_column(l, name).is_some())?;
    let mut start = def_line;
    while start > 0 && lines[start - 1].trim_start().starts_with("//") {
        start -= 1;
    }
    (start != def_line).then(|| {
        lines[start..def_line]
            .iter()
            .map(|l| l.trim_start().trim_start_matches("//").trim_start())
            .collect::<Vec<_>>()
            .join("\n")
    })
}

/// The doc comment above `fn custom` in `text`, for a `.foo` directive's
/// hover in an `.asm` buffer (which shows the script it maps to).
pub(crate) fn doc_comment_above_custom(text: &str) -> Option<String> {
    doc_comment_above_fn(text, "custom")
}

/// Hover markdown for the script-local function `name` in `text`: its
/// signature and doc comment. `None` when `text` defines no such function.
pub(crate) fn local_fn_hover(text: &str, name: &str) -> Option<String> {
    let sig = fn_signature_text(text, name)?;
    let mut md = format!("**{sig}**");
    if let Some(doc) = doc_comment_above_fn(text, name) {
        md.push_str("\n\n");
        md.push_str(&doc);
    }
    Some(md)
}

/// The definition location of the script-local function `name` in `text`.
pub(crate) fn local_definition(text: &str, name: &str) -> Option<Range> {
    fn_def_range(text, name)
}

/// Every occurrence of `name` as a whole identifier in `text` (word-bounded,
/// so `custom` inside `customize` doesn't match).
pub(crate) fn local_references(text: &str, name: &str) -> Vec<Range> {
    let is_ident = |c: char| c.is_alphanumeric() || c == '_';
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let chars: Vec<char> = line.chars().collect();
        let mut from = 0usize;
        while let Some(rel) = find_char_substring(&chars, from, name) {
            let end = rel + name.chars().count();
            let before_ok = rel == 0 || !is_ident(chars[rel - 1]);
            let after_ok = end >= chars.len() || !is_ident(chars[end]);
            if before_ok && after_ok {
                out.push(Range::new(
                    Position::new(i as u32, rel as u32),
                    Position::new(i as u32, end as u32),
                ));
            }
            from = rel + name.chars().count().max(1);
        }
    }
    out
}

/// An outline of `text`'s script-local functions, `custom` first (matching
/// §5.5's "custom first" ordering) and then in declaration order.
pub(crate) fn document_symbols(text: &str) -> Vec<DocumentSymbol> {
    let engine = diagnostics_engine();
    let Ok(ast) = engine.compile(text) else {
        return Vec::new();
    };
    let mut metas: Vec<_> = ast.iter_functions().collect();
    metas.sort_by_key(|f| u8::from(f.name != "custom"));

    metas
        .into_iter()
        .filter_map(|f| {
            let range = fn_def_range(text, f.name)?;
            #[allow(deprecated)] // `deprecated` field is required but unused.
            Some(DocumentSymbol {
                name: f.name.to_string(),
                detail: Some(format!("fn {}({})", f.name, f.params.join(", "))),
                kind: SymbolKind::FUNCTION,
                tags: None,
                deprecated: None,
                range,
                selection_range: range,
                children: None,
            })
        })
        .collect()
}

/// Foldable regions in a `.rhai` buffer: each `fn`'s brace-delimited body, and
/// runs of two or more consecutive `//` comment lines.
pub(crate) fn folding_ranges(text: &str) -> Vec<FoldingRange> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = fn_body_folds(&lines);
    out.extend(comment_run_folds(&lines));
    out
}

/// Brace-matched `fn { … }` bodies: a stack of open-brace line indices, each
/// popped and folded when its line opened right after a `fn` header.
fn fn_body_folds(lines: &[&str]) -> Vec<FoldingRange> {
    let mut out = Vec::new();
    let mut stack: Vec<(usize, bool)> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        for ch in line.chars() {
            match ch {
                '{' => stack.push((i, line.contains("fn "))),
                '}' => {
                    if let Some((start, is_fn)) = stack.pop() {
                        if is_fn && i > start {
                            out.push(FoldingRange {
                                start_line: start as u32,
                                start_character: None,
                                end_line: i as u32,
                                end_character: None,
                                kind: Some(FoldingRangeKind::Region),
                                collapsed_text: None,
                            });
                        }
                    }
                }
                _ => {}
            }
        }
    }
    out
}

fn comment_run_folds(lines: &[&str]) -> Vec<FoldingRange> {
    let mut out = Vec::new();
    let mut run_start: Option<usize> = None;
    for (i, line) in lines.iter().enumerate() {
        if line.trim_start().starts_with("//") {
            run_start.get_or_insert(i);
        } else if let Some(start) = run_start.take() {
            push_comment_fold(&mut out, start, i - 1);
        }
    }
    if let Some(start) = run_start {
        push_comment_fold(&mut out, start, lines.len().saturating_sub(1));
    }
    out
}

fn push_comment_fold(out: &mut Vec<FoldingRange>, start: usize, end: usize) {
    if end > start {
        out.push(FoldingRange {
            start_line: start as u32,
            start_character: None,
            end_line: end as u32,
            end_character: None,
            kind: Some(FoldingRangeKind::Comment),
            collapsed_text: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(diags: &[Diagnostic]) -> Vec<String> {
        diags
            .iter()
            .filter_map(|d| match &d.code {
                Some(NumberOrString::String(s)) => Some(s.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_syntax_error_becomes_a_diagnostic() {
        let diags = diagnostics("fn custom(ints, texts) {", false);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Some(DiagnosticSeverity::ERROR));
    }

    #[test]
    fn a_clean_two_arg_custom_has_no_findings() {
        let src = "fn custom(ints, texts) { [] }";
        assert!(diagnostics(src, true).is_empty());
    }

    #[test]
    fn missing_custom_fires_only_when_mapped() {
        let src = "fn helper() { 1 }";
        assert!(diagnostics(src, false).is_empty());
        let diags = diagnostics(src, true);
        assert_eq!(codes(&diags), vec![rule::MISSING_CUSTOM]);
    }

    #[test]
    fn custom_arity_fires_on_the_wrong_parameter_count() {
        let src = "fn custom(ints) { [] }";
        let diags = diagnostics(src, true);
        assert!(codes(&diags).contains(&rule::CUSTOM_ARITY.to_string()));
    }

    #[test]
    fn top_level_statement_fires_outside_every_fn() {
        let src = "const SCALE = 3;\nfn custom(ints, texts) { [SCALE] }";
        let diags = diagnostics(src, true);
        assert!(codes(&diags).contains(&rule::TOP_LEVEL_STATEMENT.to_string()));
    }

    #[test]
    fn unknown_host_function_flags_a_near_miss_only() {
        let src = r#"fn custom(ints, texts) { decode_png_fil("x.png") }"#;
        let diags = diagnostics(src, true);
        assert!(codes(&diags).contains(&rule::UNKNOWN_HOST_FUNCTION.to_string()));
        assert!(diags.iter().any(|d| d.message.contains("decode_png_file")));
    }

    #[test]
    fn unrelated_calls_are_not_flagged() {
        // Neither a script-local helper nor a Rhai built-in far from any
        // catalog name should trip the near-miss lint.
        let src = "fn helper(x) { x.to_string() }\n\
                   fn custom(ints, texts) { helper(1); [] }";
        let diags = diagnostics(src, true);
        assert!(codes(&diags).is_empty());
    }

    #[test]
    fn document_symbols_list_custom_first() {
        let src = "fn helper(x) { x }\nfn custom(ints, texts) { [] }";
        let syms = document_symbols(src);
        assert_eq!(syms.len(), 2);
        assert_eq!(syms[0].name, "custom");
        assert_eq!(syms[1].name, "helper");
    }

    #[test]
    fn local_fn_hover_includes_the_doc_comment() {
        let src = "// Doubles x.\nfn helper(x) { x * 2 }\n";
        let md = local_fn_hover(src, "helper").expect("defined");
        assert!(md.contains("fn helper(x)"));
        assert!(md.contains("Doubles x."));
    }

    #[test]
    fn local_references_are_word_bounded() {
        let src = "fn custom(ints, texts) { customize(1) }\nfn customize(n) { n }\n";
        let refs = local_references(src, "customize");
        assert_eq!(refs.len(), 2);
    }

    #[test]
    fn folding_covers_fn_bodies_and_comment_runs() {
        let src = "// one\n// two\nfn custom(ints, texts) {\n  []\n}\n";
        let folds = folding_ranges(src);
        assert!(folds
            .iter()
            .any(|f| f.kind == Some(FoldingRangeKind::Comment) && f.start_line == 0));
        assert!(folds
            .iter()
            .any(|f| f.kind == Some(FoldingRangeKind::Region) && f.start_line == 2));
    }
}
