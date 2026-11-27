//! Scoped `@local` labels (`plans/021-local-labels.md`): the name forms, and the
//! token rewrite that makes a macro body's (or emitted source's) own local
//! definitions private to one expansion.
//!
//! An `@name` label is local to the nearest global label above it in the same
//! file; the assembler keys it as `scope@name`. A *qualified* name
//! (`routine@name`) references another scope's local directly. A *private*
//! name (`\@7@name`) is a local defined inside expansion 7, which belongs to
//! that expansion alone and needs no scope.

use std::collections::HashSet;

use crate::lexer::{Tok, Token};

/// What a symbol name is, by its spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalName<'a> {
    /// An ordinary name: no `@` segment.
    Global(&'a str),
    /// `@name`: local to the current scope.
    Local(&'a str),
    /// `parent@name`: another scope's local, by its full name.
    Qualified { parent: &'a str, local: &'a str },
    /// `\@<id>@name`: a local private to expansion `id`.
    Private { id: &'a str, local: &'a str },
}

/// Classify `name` by its spelling. The `local` part of every non-global form
/// keeps its leading `@`.
#[must_use]
pub fn classify(name: &str) -> LocalName<'_> {
    if let Some(rest) = name.strip_prefix("\\@") {
        if let Some(at) = rest.find('@') {
            return LocalName::Private {
                id: &rest[..at],
                local: &rest[at..],
            };
        }
        return LocalName::Global(name);
    }
    if name.starts_with('@') {
        return LocalName::Local(name);
    }
    match name.find('@') {
        Some(at) if at > 0 => LocalName::Qualified {
            parent: &name[..at],
            local: &name[at..],
        },
        _ => LocalName::Global(name),
    }
}

/// Whether `name` is written as a local label (`@` then one or more
/// `[A-Za-z0-9_]`).
#[must_use]
pub fn is_local_name(name: &str) -> bool {
    name.strip_prefix('@').is_some_and(|rest| {
        !rest.is_empty() && rest.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
    })
}

/// Split a qualified name `parent@local` into `("parent", "@local")`.
#[must_use]
pub fn split_qualified(name: &str) -> Option<(&str, &str)> {
    match classify(name) {
        LocalName::Qualified { parent, local } => Some((parent, local)),
        _ => None,
    }
}

/// How a name is shown in a diagnostic: a private name as the `@x` the body
/// wrote, anything else as it is.
#[must_use]
pub fn display(name: &str) -> &str {
    match classify(name) {
        LocalName::Private { local, .. } => local,
        _ => name,
    }
}

/// The private key of local `local` (`@x`) in expansion `id`.
#[must_use]
pub fn private_key(id: u32, local: &str) -> String {
    format!("\\@{id}{local}")
}

/// The local labels `tokens` define at the start of a statement: `@x:` and
/// `@x\i:` (a line may start with indentation).
#[must_use]
pub fn local_defs(tokens: &[Token]) -> HashSet<String> {
    let mut defs = HashSet::new();
    let mut at_start = true;
    let mut k = 0;
    while k < tokens.len() {
        match &tokens[k].tok {
            Tok::Endl => at_start = true,
            Tok::Indent => {}
            Tok::Text(name) if at_start && is_local_name(name) => {
                let colon = match tokens.get(k + 1).map(|t| &t.tok) {
                    Some(Tok::Colon) => !matches!(
                        tokens.get(k + 2).map(|t| &t.tok),
                        Some(Tok::Plus | Tok::Minus)
                    ),
                    Some(Tok::LoopIndex(_)) => {
                        matches!(tokens.get(k + 2).map(|t| &t.tok), Some(Tok::Colon))
                    }
                    _ => false,
                };
                if colon {
                    defs.insert(name.clone());
                }
                at_start = false;
            }
            _ => at_start = false,
        }
        k += 1;
    }
    defs
}

/// Rewrite every token in `tokens` spelled like one of `defs` to its private
/// key in expansion `id`.
pub fn privatize(tokens: &mut [Token], id: u32, defs: &HashSet<String>) {
    if defs.is_empty() {
        return;
    }
    for t in tokens {
        if let Tok::Text(name) = &t.tok {
            if defs.contains(name) {
                t.tok = Tok::Text(private_key(id, name));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;

    #[test]
    fn names_classify_by_spelling() {
        assert_eq!(classify("main"), LocalName::Global("main"));
        assert_eq!(classify("@x"), LocalName::Local("@x"));
        assert_eq!(
            classify("p1@x"),
            LocalName::Qualified {
                parent: "p1",
                local: "@x"
            }
        );
        assert_eq!(
            classify("\\@7@x3"),
            LocalName::Private {
                id: "7",
                local: "@x3"
            }
        );
        assert_eq!(display("\\@7@x"), "@x");
        assert_eq!(display("p1@x"), "p1@x");
        assert!(is_local_name("@1"));
        assert!(!is_local_name("@"));
        assert!(!is_local_name("@x@y"));
        assert!(!is_local_name("x"));
        assert_eq!(split_qualified("q@x"), Some(("q", "@x")));
        assert_eq!(split_qualified("@x"), None);
    }

    #[test]
    fn local_defs_finds_statement_initial_definitions_only() {
        let toks = Lexer::new("@a:\n  BNE @b\n  @c:\n@d\\i:\n  BEQ :+\n").tokenize();
        let defs = local_defs(&toks);
        let mut v: Vec<_> = defs.into_iter().collect();
        v.sort();
        assert_eq!(v, vec!["@a", "@c", "@d"]);
    }

    #[test]
    fn privatize_rewrites_only_defined_names() {
        let mut toks = Lexer::new("@a:\n  BNE @a\n  BNE @b\n").tokenize();
        let defs = local_defs(&toks);
        privatize(&mut toks, 7, &defs);
        let texts: Vec<_> = toks
            .iter()
            .filter_map(|t| match &t.tok {
                Tok::Text(s) => Some(s.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, vec!["\\@7@a", "BNE", "\\@7@a", "BNE", "@b"]);
    }
}
