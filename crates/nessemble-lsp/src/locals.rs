//! Scoped `@local` labels in the editor (`plans/021-local-labels.md` §3.9).
//!
//! The language server reads source as text, so a local's scope here is
//! textual: the nearest global label defined above it, up to the next one —
//! or, for a name a `.macrodef` body defines itself, that body. It ignores
//! `.if` suppression and labels a macro expansion would define (R10); the
//! assembler's diagnostics stay authoritative.

use lsp_types::{Position, Range};
use nessemble_core::tooling::{self, LexKind};

use super::{definitions, located_lexemes, DefKind, Definition, Located};

/// A global label's scope: its name and the lines `[start, end)` it covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LabelScope {
    pub parent: String,
    pub start: u32,
    pub end: u32,
}

/// The scope a local name belongs to at some line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Scope {
    /// A global label's scope.
    Label(LabelScope),
    /// A `.macrodef` body that defines the name itself (lines `[start, end)`),
    /// which makes it private to each expansion.
    Macro { start: u32, end: u32 },
}

impl Scope {
    pub fn lines(&self) -> (u32, u32) {
        match self {
            Scope::Label(s) => (s.start, s.end),
            Scope::Macro { start, end } => (*start, *end),
        }
    }
}

/// Whether `name` is an `@local` label name.
pub(crate) fn is_local(name: &str) -> bool {
    tooling::is_local_name(name)
}

/// The body lines `[start, end)` of every `.macrodef` … `.endm` in `toks`.
pub(crate) fn macro_bodies(toks: &[Located<'_>]) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    let mut open: Option<u32> = None;
    for t in toks {
        if t.kind != LexKind::Directive {
            continue;
        }
        if t.text.eq_ignore_ascii_case(".macrodef") {
            open = Some(t.range.start.line + 1);
        } else if t.text.eq_ignore_ascii_case(".endm") {
            if let Some(start) = open.take() {
                out.push((start, t.range.start.line));
            }
        }
    }
    out
}

fn in_body(bodies: &[(u32, u32)], line: u32) -> Option<(u32, u32)> {
    bodies
        .iter()
        .copied()
        .find(|&(s, e)| (s..e).contains(&line))
}

/// Whether definition `d` is a global label (one that opens a scope).
fn opens_scope(d: &Definition) -> bool {
    d.kind == DefKind::Label && !d.name.starts_with('@') && !d.name.contains('@')
}

/// Every global label's scope in `text`, in source order. A label written in
/// a macro body is defined only by expanding it, so it opens no scope here.
pub(crate) fn label_scopes(text: &str) -> Vec<LabelScope> {
    let toks = located_lexemes(text);
    let bodies = macro_bodies(&toks);
    let starts: Vec<(String, u32)> = definitions(text)
        .into_iter()
        .filter(|d| opens_scope(d) && in_body(&bodies, d.range.start.line).is_none())
        .map(|d| (d.name, d.range.start.line))
        .collect();
    starts
        .iter()
        .enumerate()
        .map(|(i, (parent, start))| LabelScope {
            parent: parent.clone(),
            start: *start,
            end: starts.get(i + 1).map_or(u32::MAX, |(_, s)| *s),
        })
        .collect()
}

/// The label scope containing `line`, if a global label is above it.
pub(crate) fn label_scope_at(text: &str, line: u32) -> Option<LabelScope> {
    label_scopes(text)
        .into_iter()
        .find(|s| (s.start..s.end).contains(&line))
}

/// Whether a definition named `def` defines the local `local` (`@x:`, or a
/// pasted `@x\i:` for the base `@x`).
fn defines(def: &str, local: &str) -> bool {
    def == local || def.split_once('\\').is_some_and(|(base, _)| base == local)
}

/// The scope the local `local` written on `line` resolves in.
pub(crate) fn scope_at(text: &str, line: u32, local: &str) -> Option<Scope> {
    let toks = located_lexemes(text);
    if let Some((start, end)) = in_body(&macro_bodies(&toks), line) {
        let own = definitions(text).iter().any(|d| {
            d.kind == DefKind::Label
                && (start..end).contains(&d.range.start.line)
                && defines(&d.name, local)
        });
        if own {
            return Some(Scope::Macro { start, end });
        }
    }
    label_scope_at(text, line).map(Scope::Label)
}

/// The definition of `local` in `scope`, in `text`.
pub(crate) fn definition_in(text: &str, scope: &Scope, local: &str) -> Option<Range> {
    let (start, end) = scope.lines();
    let private = private_lines(text, local);
    definitions(text)
        .into_iter()
        .filter(|d| d.kind == DefKind::Label && defines(&d.name, local))
        .filter(|d| (start..end).contains(&d.range.start.line))
        .find(|d| {
            matches!(scope, Scope::Macro { .. })
                || !private
                    .iter()
                    .any(|&(s, e)| (s..e).contains(&d.range.start.line))
        })
        .map(|d| d.range)
}

/// The macro bodies in `text` that define `local` themselves, where `@local`
/// is private to the expansion rather than the label scope.
fn private_lines(text: &str, local: &str) -> Vec<(u32, u32)> {
    let toks = located_lexemes(text);
    let defs = definitions(text);
    macro_bodies(&toks)
        .into_iter()
        .filter(|&(s, e)| {
            defs.iter().any(|d| {
                d.kind == DefKind::Label
                    && (s..e).contains(&d.range.start.line)
                    && defines(&d.name, local)
            })
        })
        .collect()
}

/// Every `@local` token in `text` that belongs to `scope`.
pub(crate) fn local_tokens(text: &str, scope: &Scope, local: &str) -> Vec<Range> {
    let (start, end) = scope.lines();
    let private = private_lines(text, local);
    located_lexemes(text)
        .into_iter()
        .filter(|t| t.kind == LexKind::Ident && t.text == local)
        .filter(|t| (start..end).contains(&t.range.start.line))
        .filter(|t| {
            matches!(scope, Scope::Macro { .. })
                || !private
                    .iter()
                    .any(|&(s, e)| (s..e).contains(&t.range.start.line))
        })
        .map(|t| t.range)
        .collect()
}

/// Every `parent@local` token in `text`: its whole range and the range of the
/// `@local` part.
pub(crate) fn qualified_tokens(text: &str, parent: &str, local: &str) -> Vec<(Range, Range)> {
    let qualified = format!("{parent}{local}");
    located_lexemes(text)
        .into_iter()
        .filter(|t| t.kind == LexKind::Ident && t.text == qualified)
        .map(|t| {
            let split = Position::new(
                t.range.start.line,
                t.range.start.character + parent.len() as u32,
            );
            (t.range, Range::new(split, t.range.end))
        })
        .collect()
}

/// The `name@…` prefix range of every qualified token naming global `name`.
pub(crate) fn qualified_prefixes(text: &str, name: &str) -> Vec<Range> {
    located_lexemes(text)
        .into_iter()
        .filter(|t| {
            t.kind == LexKind::Ident
                && tooling::split_qualified(t.text).is_some_and(|(p, _)| p == name)
        })
        .map(|t| {
            let end = Position::new(
                t.range.start.line,
                t.range.start.character + name.len() as u32,
            );
            Range::new(t.range.start, end)
        })
        .collect()
}

/// The locals completion offers on `line`: the label scope's, plus — inside a
/// `.macrodef` body — the body's own.
pub(crate) fn locals_in_scope(text: &str, line: u32) -> Vec<String> {
    let toks = located_lexemes(text);
    let body = in_body(&macro_bodies(&toks), line);
    let scope = label_scope_at(text, line);
    let mut out: Vec<String> = Vec::new();
    for d in definitions(text) {
        if d.kind != DefKind::Label || !d.name.starts_with('@') {
            continue;
        }
        let at = d.range.start.line;
        let in_own_body = body.is_some_and(|(s, e)| (s..e).contains(&at));
        let in_scope = scope
            .as_ref()
            .is_some_and(|s| (s.start..s.end).contains(&at))
            && in_body(&macro_bodies(&toks), at).is_none();
        if (in_own_body || in_scope) && !out.contains(&d.name) {
            out.push(d.name);
        }
    }
    out
}

/// Whether the word being typed at `pos` starts with `@` (a local label).
pub(crate) fn typing_local(text: &str, pos: Position) -> bool {
    let Some(line) = text.lines().nth(pos.line as usize) else {
        return false;
    };
    let mut col = 0u32;
    let mut end = line.len();
    for (i, c) in line.char_indices() {
        if col >= pos.character {
            end = i;
            break;
        }
        col += c.len_utf16() as u32;
    }
    let before = &line[..end];
    let word: String = before
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '@')
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    word.starts_with('@') && word.matches('@').count() == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = "p1:\n@loc:\n    BNE @loc\np2:\n@loc:\n    BNE @loc\n";

    #[test]
    fn label_scopes_run_to_the_next_global_label() {
        let scopes = label_scopes(SRC);
        assert_eq!(
            scopes,
            vec![
                LabelScope {
                    parent: "p1".into(),
                    start: 0,
                    end: 3
                },
                LabelScope {
                    parent: "p2".into(),
                    start: 3,
                    end: u32::MAX
                },
            ]
        );
    }

    #[test]
    fn a_macro_body_defining_the_name_is_its_scope() {
        let text = "p1:\n.macrodef wait\n@spin:\n    BPL @spin\n    BNE @out\n.endm\n";
        assert_eq!(
            scope_at(text, 3, "@spin"),
            Some(Scope::Macro { start: 2, end: 5 })
        );
        assert!(matches!(scope_at(text, 4, "@out"), Some(Scope::Label(_))));
    }

    #[test]
    fn typing_local_reads_the_word_before_the_cursor() {
        assert!(typing_local("    BNE @lo\n", Position::new(0, 11)));
        assert!(typing_local("    BNE @\n", Position::new(0, 9)));
        assert!(!typing_local("    BNE lo\n", Position::new(0, 10)));
        assert!(!typing_local("    BNE p1@lo\n", Position::new(0, 13)));
    }
}
