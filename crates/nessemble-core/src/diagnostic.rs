//! Diagnostics shared by every stage of an assembly: the [`Diag`] record, the
//! configurable assembler rules ([`AsmRule`] / [`AsmRules`]), and the macro
//! expansion table that lets a diagnostic on a macro-expanded line name the
//! macro body line it came from *and* the call that expanded it.
//!
//! See `plans/017-assembler-diagnostics.md`.

use nessemble_i18n::t;

pub use crate::tooling::RuleSeverity;

/// A diagnostic (error or warning) with a source-file display name, 1-based
/// source line, and message, matching the reference tool's wording.
///
/// `rule` is set for a diagnostic a project can configure (see [`AsmRule`]);
/// always-on errors leave it `None`. `notes` carry supporting locations — the
/// macro a call names, or the chain of `.macro` calls that expanded the line —
/// each rendered on its own `  note:` line under the diagnostic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diag {
    /// Display name of the file the diagnostic refers to (the basename of the
    /// top-level input, or the raw path of an included file).
    pub file: String,
    pub line: u32,
    pub message: String,
    /// The configurable rule this diagnostic was raised under, if any.
    pub rule: Option<AsmRule>,
    /// Supporting notes, outermost last.
    pub notes: Vec<String>,
}

impl Diag {
    /// A plain diagnostic with no rule and no notes.
    #[must_use]
    pub fn new(file: impl Into<String>, line: u32, message: impl Into<String>) -> Self {
        Diag {
            file: file.into(),
            line,
            message: message.into(),
            rule: None,
            notes: Vec::new(),
        }
    }

    /// The message as reported: with the rule id appended (`… [rule-id]`) when
    /// the diagnostic was raised under one.
    #[must_use]
    pub fn display_message(&self) -> String {
        match self.rule {
            Some(rule) => format!("{}  [{}]", self.message, rule.id()),
            None => self.message.clone(),
        }
    }
}

/// An assembler diagnostic a project can configure as `off`, `warn` or `error`.
///
/// Every rule defaults to [`RuleSeverity::Warn`]. The always-on errors (a label
/// with content on its line, leftover tokens, a macro arity mismatch) are not
/// rules: they describe source that cannot mean what it says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AsmRule {
    /// A symbol or macro redefined with a different value or kind.
    DuplicateSymbol,
    /// A symbol redefined to the same value and kind (e.g. an includer and the
    /// included file both placing the same label at the same address).
    DuplicateSymbolSameValue,
    /// A value narrowed to fit a byte, word, or operand field it does not fit.
    ValueOutOfRange,
}

impl AsmRule {
    /// Every rule, in a stable order (also the [`AsmRules`] index order).
    pub const ALL: [AsmRule; 3] = [
        AsmRule::DuplicateSymbol,
        AsmRule::DuplicateSymbolSameValue,
        AsmRule::ValueOutOfRange,
    ];

    /// The stable kebab-case identifier used in `.nessemblerc`, `-W`, and
    /// reports.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            AsmRule::DuplicateSymbol => "duplicate-symbol",
            AsmRule::DuplicateSymbolSameValue => "duplicate-symbol-same-value",
            AsmRule::ValueOutOfRange => "value-out-of-range",
        }
    }

    /// Parse a rule id, returning `None` if it names no assembler rule.
    #[must_use]
    pub fn from_id(s: &str) -> Option<AsmRule> {
        AsmRule::ALL.into_iter().find(|r| r.id() == s)
    }

    /// One sentence on what the rule reports, for `nessemble reference`.
    #[must_use]
    pub fn summary(self) -> &'static str {
        match self {
            AsmRule::DuplicateSymbol => {
                "A label, constant, or macro redefined with a different value or kind; \
                 the last definition wins."
            }
            AsmRule::DuplicateSymbolSameValue => {
                "A symbol redefined with the same value and kind, such as a label placed \
                 by an includer and again by the included file at the same address."
            }
            AsmRule::ValueOutOfRange => {
                "A value narrowed to fit a byte (-128..255), word (-32768..65535), \
                 zero-page operand (0..255) or absolute operand (0..65535) it does not fit."
            }
        }
    }

    fn index(self) -> usize {
        match self {
            AsmRule::DuplicateSymbol => 0,
            AsmRule::DuplicateSymbolSameValue => 1,
            AsmRule::ValueOutOfRange => 2,
        }
    }
}

/// The configured severity of each [`AsmRule`], plus `--warnings-as-errors`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsmRules {
    severities: [RuleSeverity; AsmRule::ALL.len()],
    /// Report every assembler warning — rule warnings and the built-in ones
    /// (bank overflow, …) alike — as an error. A rule set to `off` stays off.
    pub warnings_as_errors: bool,
}

impl Default for AsmRules {
    fn default() -> Self {
        AsmRules {
            severities: [RuleSeverity::Warn; AsmRule::ALL.len()],
            warnings_as_errors: false,
        }
    }
}

impl AsmRules {
    /// The severity configured for `rule`, before `warnings_as_errors`.
    #[must_use]
    pub fn get(&self, rule: AsmRule) -> RuleSeverity {
        self.severities[rule.index()]
    }

    /// Set the severity for `rule`.
    pub fn set(&mut self, rule: AsmRule, severity: RuleSeverity) {
        self.severities[rule.index()] = severity;
    }

    /// The severity a diagnostic under `rule` is reported at: the configured
    /// one, with `warn` raised to `error` under `warnings_as_errors`.
    #[must_use]
    pub fn effective(&self, rule: AsmRule) -> RuleSeverity {
        match self.get(rule) {
            RuleSeverity::Warn if self.warnings_as_errors => RuleSeverity::Error,
            s => s,
        }
    }
}

/// One `.macro` expansion: which macro, where its definition is, and where the
/// call that expanded it was written. Tokens produced by the expansion carry its
/// 1-based id ([`crate::lexer::Token::expansion`]; `0` is "not expanded").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expansion {
    pub name: String,
    /// File id of the `.macrodef`, whose lines the body tokens carry.
    pub def_file: u32,
    /// Line of the `.macrodef`.
    pub def_line: u32,
    /// File id and line of the call, as reported (for a call inside another
    /// expansion, the enclosing macro's definition file and body line).
    pub call_file: u32,
    pub call_line: u32,
    /// The expansion the call itself was written in (`0` for none).
    pub parent: u32,
}

/// Build a diagnostic for a statement at `line` of file `file`, which came from
/// expansion `expansion` (`0` for none): a line produced by a macro is reported
/// in the file holding the macro body, with one note per enclosing call.
pub(crate) fn locate(
    files: &[String],
    expansions: &[Expansion],
    file: u32,
    line: u32,
    expansion: u32,
    message: impl Into<String>,
) -> Diag {
    let name = |id: u32| files.get(id as usize).cloned().unwrap_or_default();
    let shown_file = match expansion_at(expansions, expansion) {
        Some(e) => e.def_file,
        None => file,
    };
    let mut diag = Diag::new(name(shown_file), line, message);
    diag.notes = expansion_notes(files, expansions, expansion);
    diag
}

/// `in expansion of macro …` notes for the chain starting at `expansion`,
/// innermost first.
pub(crate) fn expansion_notes(
    files: &[String],
    expansions: &[Expansion],
    mut expansion: u32,
) -> Vec<String> {
    let mut notes = Vec::new();
    // The chain is acyclic (each parent is created before its child), but bound
    // it anyway so a corrupt table cannot loop.
    while let Some(e) = expansion_at(expansions, expansion) {
        if notes.len() > expansions.len() {
            break;
        }
        notes.push(t!(
            "note-in-expansion",
            name = e.name.as_str(),
            file = files.get(e.call_file as usize).cloned().unwrap_or_default(),
            line = e.call_line
        ));
        expansion = e.parent;
    }
    notes
}

fn expansion_at(expansions: &[Expansion], id: u32) -> Option<&Expansion> {
    id.checked_sub(1).and_then(|i| expansions.get(i as usize))
}
