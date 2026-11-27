//! Lossless, position-preserving lexing plus a source formatter, for editor
//! tooling (the language server's formatting and semantic-token highlighting).
//!
//! Unlike the parity lexer (`crate::lexer`), this scanner is **lossless**: it
//! segments the *entire* input — including whitespace and comments — into
//! [`Lexeme`]s with byte ranges, so the original text can be reconstructed and
//! trivia can be classified for highlighting. It is intentionally separate from
//! the parity lexer, which stays byte-for-byte untouched.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use nessemble_isa::RegSet;

use crate::program::{EmitKind, ProgramMap, Resolution, Site, Via};

/// The kind of a lexeme. Every byte of the input belongs to exactly one lexeme,
/// so the stream is gap-free and reversible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LexKind {
    /// A run of spaces and tabs.
    Whitespace,
    /// A single line break (`\n`, `\r`, or `\r\n`).
    Newline,
    /// A `;`-to-end-of-line comment.
    Comment,
    /// A `"…"` string literal.
    String,
    /// A `'x'` character literal.
    Char,
    /// A numeric literal (`$hex`, `%bin`, decimal, …).
    Number,
    /// A `.`-prefixed directive name.
    Directive,
    /// An identifier: mnemonic, label, constant, or register.
    Ident,
    /// Any other single token (operators, brackets, `,`, `:`, `#`, …).
    Punct,
}

pub use crate::locals::{is_local_name, split_qualified};

/// A classified span of the source, given by byte offsets `[start, end)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lexeme {
    pub kind: LexKind,
    pub start: usize,
    pub end: usize,
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b == b'@'
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Segment `source` into a gap-free stream of [`Lexeme`]s. Always terminates
/// (every branch advances at least one byte/char) and never splits a UTF-8
/// character.
#[must_use]
pub fn lex(source: &str) -> Vec<Lexeme> {
    let bytes = source.as_bytes();
    let n = bytes.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        let start = i;
        let b = bytes[i];
        let kind = match b {
            b'\n' => {
                i += 1;
                LexKind::Newline
            }
            b'\r' => {
                i += 1;
                if i < n && bytes[i] == b'\n' {
                    i += 1;
                }
                LexKind::Newline
            }
            b' ' | b'\t' => {
                while i < n && (bytes[i] == b' ' || bytes[i] == b'\t') {
                    i += 1;
                }
                LexKind::Whitespace
            }
            b';' => {
                while i < n && bytes[i] != b'\n' && bytes[i] != b'\r' {
                    i += 1;
                }
                LexKind::Comment
            }
            b'"' => {
                i += 1;
                while i < n && bytes[i] != b'"' && bytes[i] != b'\n' {
                    i += 1;
                }
                if i < n && bytes[i] == b'"' {
                    i += 1;
                }
                LexKind::String
            }
            // `'x'` character literal; a lone quote is punctuation.
            b'\'' if i + 2 < n && bytes[i + 2] == b'\'' => {
                i += 3;
                LexKind::Char
            }
            b'$' => {
                i += 1;
                while i < n && bytes[i].is_ascii_hexdigit() {
                    i += 1;
                }
                LexKind::Number
            }
            // `%` is a binary-literal prefix only when followed by 0/1.
            b'%' if i + 1 < n && (bytes[i + 1] == b'0' || bytes[i + 1] == b'1') => {
                i += 1;
                while i < n && (bytes[i] == b'0' || bytes[i] == b'1') {
                    i += 1;
                }
                LexKind::Number
            }
            b'0'..=b'9' => {
                while i < n && bytes[i].is_ascii_alphanumeric() {
                    i += 1;
                }
                LexKind::Number
            }
            // `.name` directive; a bare `.` is punctuation.
            b'.' if i + 1 < n && is_ident(bytes[i + 1]) => {
                i += 1;
                while i < n && is_ident(bytes[i]) {
                    i += 1;
                }
                LexKind::Directive
            }
            _ if is_ident_start(b) => {
                i += 1;
                while i < n && is_ident(bytes[i]) {
                    i += 1;
                }
                // A qualified local-label name (`routine@name`) is one
                // identifier, as in the assembler's lexer
                // (`plans/021-local-labels.md` §3.8).
                if b != b'@' && i + 1 < n && bytes[i] == b'@' && is_ident(bytes[i + 1]) {
                    i += 1;
                    while i < n && is_ident(bytes[i]) {
                        i += 1;
                    }
                }
                LexKind::Ident
            }
            // Catch-all: consume one whole UTF-8 char so ranges stay on
            // character boundaries even for stray non-ASCII bytes.
            _ => {
                i += utf8_char_len(b);
                LexKind::Punct
            }
        };
        out.push(Lexeme {
            kind,
            start,
            end: i,
        });
    }
    out
}

/// Length in bytes of the UTF-8 character whose leading byte is `b` (at least 1).
fn utf8_char_len(b: u8) -> usize {
    match b {
        0xF0..=0xF7 => 4,
        0xE0..=0xEF => 3,
        0xC0..=0xDF => 2,
        _ => 1,
    }
}

/// Split a lexeme stream into physical lines: every [`LexKind::Newline`] ends a
/// line (and is dropped). A trailing newline yields a final empty line, so the
/// caller can tell whether the source ended in `\n`. Shared by the formatter and
/// the linter so both see the same line structure.
fn split_lines(lexemes: &[Lexeme]) -> Vec<Vec<Lexeme>> {
    let mut lines: Vec<Vec<Lexeme>> = Vec::new();
    let mut current: Vec<Lexeme> = Vec::new();
    for &lx in lexemes {
        if lx.kind == LexKind::Newline {
            lines.push(std::mem::take(&mut current));
        } else {
            current.push(lx);
        }
    }
    lines.push(current);
    lines
}

/// Lower-cased 6502 mnemonics (documented + undocumented), for telling an
/// instruction identifier apart from a label/constant/register during
/// classification.
static MNEMONICS: LazyLock<HashSet<String>> = LazyLock::new(|| {
    nessemble_isa::OPCODES
        .iter()
        .map(|o| o.mnemonic.to_ascii_lowercase())
        .collect()
});

/// The highlight class of a lexeme. This is the language-aware classification
/// shared by the language server's semantic tokens and the wasm/editor
/// highlighter, so every surface colors tokens identically (the single source of
/// truth for *what* a token is; each consumer supplies its own position
/// encoding).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenClass {
    /// A `.`-prefixed directive.
    Directive,
    /// An identifier that names a 6502 mnemonic.
    Instruction,
    /// Any other identifier: label, constant, or register.
    Identifier,
    /// A numeric literal.
    Number,
    /// A string or character literal.
    String,
    /// A comment.
    Comment,
    /// Punctuation / operators.
    Operator,
}

impl TokenClass {
    /// Every class, in [`wire_id`](Self::wire_id) order — the legend the wasm
    /// highlighter and the language server build their id/name tables from.
    pub const ALL: [TokenClass; 7] = [
        TokenClass::Directive,
        TokenClass::Instruction,
        TokenClass::Identifier,
        TokenClass::Number,
        TokenClass::String,
        TokenClass::Comment,
        TokenClass::Operator,
    ];

    /// This class's **wire id**: a stable integer contract (not the enum's
    /// layout-dependent discriminant). The wasm highlighter packs it into its
    /// `tokenize` output and indexes its `token_classes` legend by it, and the
    /// language server orders its semantic-token legend to match. Renumbering is
    /// a breaking change to every highlighter, so keep it fixed.
    #[must_use]
    pub fn wire_id(self) -> u32 {
        match self {
            TokenClass::Directive => 0,
            TokenClass::Instruction => 1,
            TokenClass::Identifier => 2,
            TokenClass::Number => 3,
            TokenClass::String => 4,
            TokenClass::Comment => 5,
            TokenClass::Operator => 6,
        }
    }

    /// This class's stable lower-case name, index-aligned with
    /// [`wire_id`](Self::wire_id) (e.g. mapped to a CSS class `na-tok-<name>`).
    #[must_use]
    pub fn wire_name(self) -> &'static str {
        match self {
            TokenClass::Directive => "directive",
            TokenClass::Instruction => "instruction",
            TokenClass::Identifier => "identifier",
            TokenClass::Number => "number",
            TokenClass::String => "string",
            TokenClass::Comment => "comment",
            TokenClass::Operator => "operator",
        }
    }
}

/// Classify a lexeme for highlighting. Identifiers naming a 6502 mnemonic are
/// [`TokenClass::Instruction`]; all other identifiers are
/// [`TokenClass::Identifier`]. Whitespace and newlines (which highlighters drop)
/// map to [`TokenClass::Operator`].
#[must_use]
pub fn classify(kind: LexKind, piece: &str) -> TokenClass {
    match kind {
        LexKind::Directive => TokenClass::Directive,
        LexKind::Ident => {
            if MNEMONICS.contains(&piece.to_ascii_lowercase()) {
                TokenClass::Instruction
            } else {
                TokenClass::Identifier
            }
        }
        LexKind::Number => TokenClass::Number,
        LexKind::String | LexKind::Char => TokenClass::String,
        LexKind::Comment => TokenClass::Comment,
        LexKind::Punct | LexKind::Whitespace | LexKind::Newline => TokenClass::Operator,
    }
}

/// Whether `lexemes[idx]` is the counter letter of a `.for` loop index — the `i`
/// of `\i` (or `j`/`k`), directly after a `\`. Highlighters class it like the
/// digit of a macro argument `\1`: a [`TokenClass::Number`].
#[must_use]
pub fn is_loop_counter(source: &str, lexemes: &[Lexeme], idx: usize) -> bool {
    let Some(lx) = lexemes.get(idx) else {
        return false;
    };
    idx > 0
        && lx.kind == LexKind::Ident
        && matches!(text(source, lx), "i" | "j" | "k")
        && lexemes[idx - 1].end == lx.start
        && is_punct(source, &lexemes[idx - 1], "\\")
}

/// A highlight token: a classified span given as a **UTF-16 code-unit** offset and
/// length from the start of the source, so a JavaScript consumer's string indices
/// line up. Whitespace and newlines are not emitted — the gaps between tokens are
/// trivia the consumer renders verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HlToken {
    /// Start offset in UTF-16 code units.
    pub start: u32,
    /// Length in UTF-16 code units.
    pub len: u32,
    /// The token's highlight class.
    pub class: TokenClass,
}

/// Classify every significant lexeme in `source` for highlighting, with offsets in
/// **UTF-16 code units**. This is the flat-offset convenience the wasm/editor
/// highlighter consumes; the language server shares [`classify`] but keeps its own
/// line/column delta encoding.
#[must_use]
pub fn highlight(source: &str) -> Vec<HlToken> {
    let mut out = Vec::new();
    let mut off = 0u32;
    let lexemes = lex(source);
    for (idx, lx) in lexemes.iter().enumerate() {
        let piece = &source[lx.start..lx.end];
        let len = utf16_len(piece);
        if !matches!(lx.kind, LexKind::Whitespace | LexKind::Newline) {
            let class = if is_loop_counter(source, &lexemes, idx) {
                TokenClass::Number
            } else {
                classify(lx.kind, piece)
            };
            out.push(HlToken {
                start: off,
                len,
                class,
            });
        }
        off += len;
    }
    out
}

/// Length of `s` in UTF-16 code units.
fn utf16_len(s: &str) -> u32 {
    s.encode_utf16().count() as u32
}

/// How instruction lines are indented (labels, directives, and constant
/// definitions always stay at column 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndentStyle {
    /// Indent with spaces (`indent_width` per level).
    Space,
    /// Indent with a single tab per level.
    Tab,
}

/// How a token's letters are cased by the formatter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Case {
    /// Leave the original case untouched.
    Preserve,
    /// Lower-case.
    Lower,
    /// Upper-case.
    Upper,
}

impl Case {
    /// Apply this casing to every ASCII letter in `s`.
    fn apply(self, s: &str) -> String {
        match self {
            Case::Preserve => s.to_string(),
            Case::Lower => s.to_ascii_lowercase(),
            Case::Upper => s.to_ascii_uppercase(),
        }
    }

    /// Apply this casing to only the hex-digit letters (`a`–`f`) of `s`, so a
    /// `$AB` literal is re-cased without disturbing any prefix.
    fn apply_hex(self, s: &str) -> String {
        if self == Case::Preserve {
            return s.to_string();
        }
        s.chars()
            .map(|c| {
                if matches!(c, 'a'..='f' | 'A'..='F') {
                    match self {
                        Case::Lower => c.to_ascii_lowercase(),
                        Case::Upper => c.to_ascii_uppercase(),
                        Case::Preserve => c,
                    }
                } else {
                    c
                }
            })
            .collect()
    }
}

/// Options controlling [`format_with`].
///
/// [`FormatOptions::default`] is the opinionated house style: a four-space
/// instruction indent, `", "` between comma-separated values, `.db`/`.dw`/
/// `.color` data consolidated to eight values per line, a blank line after
/// `RTS`/`RTI`, runs of blank lines collapsed to two, and a single final
/// newline. The language server calls [`format`] (defaults), so on-format output
/// gains these rules too — one house style everywhere (see
/// `plans/005-formatter.md` §5/§10). Case normalization (mnemonics, hex digits)
/// defaults to preserve, so it is opt-in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatOptions {
    /// Indent instruction lines with spaces or a tab.
    pub indent_style: IndentStyle,
    /// Number of spaces per indent level (ignored for [`IndentStyle::Tab`]).
    pub indent_width: usize,
    /// Put exactly one space after each operand/data comma (never one before).
    /// When `false`, commas are tight (`$01,$02`).
    pub comma_spacing: bool,
    /// Indent directive lines (`.db`, `.dw`, `.include`, …) to the same block
    /// depth as instructions instead of pinning them to column 0. `false` (the
    /// default, house style) keeps directives flush-left; `true` treats a
    /// directive as a statement inside its block, matching codebases that indent
    /// data under labels. Labels and constant definitions stay at column 0
    /// regardless.
    pub indent_directives: bool,
    /// Align the continuation lines of a multi-line statement (an operand list
    /// spilled across physical lines by a trailing comma) under the opening
    /// line's first argument, rather than to the block indent. `true` (the
    /// default) pads each continuation line to `<opening indent> + <directive
    /// token> + one space`; `false` indents continuations to the block indent
    /// (`indent_width`). Only the leading whitespace of continuation lines is
    /// affected — operand text is untouched — so this is purely cosmetic.
    pub align_continuations: bool,
    /// Consolidate adjacent `.db`/`.dw`/`.color` lines to this many values per
    /// line. `0` disables consolidation (data lines are left as-is).
    pub data_per_line: usize,
    /// Honor `; @nessemble-format stride=N[,N,...]` hint comments (and the
    /// deprecated `; @fmt` spelling) that override
    /// [`Self::data_per_line`] for the following data block.
    pub respect_stride_hints: bool,
    /// Insert one blank line after every `RTS`/`RTI` (a routine boundary).
    pub blank_line_after_return: bool,
    /// Collapse runs of more than this many consecutive blank lines down to it.
    pub max_consecutive_blank_lines: usize,
    /// Ensure the output ends in exactly one `\n` (and no trailing blank lines).
    /// When `false`, the original trailing-newline presence is preserved.
    pub final_newline: bool,
    /// Case applied to instruction mnemonics (only the mnemonic token of an
    /// instruction line; labels, constants, and registers are untouched).
    pub mnemonic_case: Case,
    /// Case applied to the hex-digit letters of numeric literals (`$ab` vs
    /// `$AB`). Directive names are never re-cased (nessemble is case-sensitive
    /// about them).
    pub hex_digit_case: Case,
}

impl Default for FormatOptions {
    fn default() -> Self {
        Self {
            indent_style: IndentStyle::Space,
            indent_width: 4,
            comma_spacing: true,
            indent_directives: false,
            align_continuations: true,
            data_per_line: 8,
            respect_stride_hints: true,
            blank_line_after_return: true,
            max_consecutive_blank_lines: 2,
            final_newline: true,
            mnemonic_case: Case::Preserve,
            hex_digit_case: Case::Preserve,
        }
    }
}

impl FormatOptions {
    /// The separator emitted between consolidated data values.
    fn comma_sep(&self) -> &'static str {
        if self.comma_spacing {
            ", "
        } else {
            ","
        }
    }
}

impl FormatOptions {
    /// The leading indent string applied to an instruction line.
    fn indent_unit(&self) -> String {
        match self.indent_style {
            IndentStyle::Space => " ".repeat(self.indent_width),
            IndentStyle::Tab => "\t".to_string(),
        }
    }
}

/// Reformat nessemble assembly source with the default [`FormatOptions`].
///
/// Equivalent to [`format_with`] with [`FormatOptions::default`]; retained as the
/// zero-config entry point the language server calls.
#[must_use]
pub fn format(source: &str) -> String {
    format_with(source, &FormatOptions::default())
}

/// Reformat nessemble assembly source under `opts`. Runs an ordered pass
/// pipeline: line normalization (indent, comma spacing, trailing-whitespace
/// trim; comments/case preserved), then — when enabled — `.db`/`.dw`/`.color`
/// consolidation, a blank line after `RTS`/`RTI`, blank-run collapsing, and a
/// normalized final newline. The transform is idempotent.
#[must_use]
pub fn format_with(source: &str, opts: &FormatOptions) -> String {
    let lexemes = lex(source);

    // Split into physical lines (a `Newline` ends a line; a trailing newline
    // yields a final empty line, so the split records whether the file ends in
    // `\n`).
    let lines = split_lines(&lexemes);

    // Pass 0 — normalize each physical line. Continuation lines of a multi-line
    // statement (an operand list carried across lines by a trailing comma) are
    // aligned under the opening line's first argument when `align_continuations`
    // is set; the alignment prefix is computed once from the opening line.
    let indent = opts.indent_unit();
    let mut content: Vec<String> = Vec::with_capacity(lines.len());
    let mut continuation_prefix: Option<String> = None;
    for line in &lines {
        let formatted = match &continuation_prefix {
            Some(lead) => format_continuation_line(source, line, opts, lead),
            None => format_line(source, line, opts, &indent),
        };
        // The next line is a continuation iff this line ends with an operand
        // comma. Compute the alignment prefix from the opening line only (an
        // already-active prefix carries through every continuation line).
        if opts.align_continuations && ends_with_operand_comma(source, line) {
            if continuation_prefix.is_none() {
                continuation_prefix = Some(continuation_lead(&formatted));
            }
        } else {
            continuation_prefix = None;
        }
        content.push(formatted);
    }

    // The split appends a trailing empty line iff the source ended in a
    // newline; peel it off so the passes see only real content lines, and
    // remember it for reassembly.
    let had_trailing_newline = source.ends_with('\n') || source.ends_with('\r');
    if had_trailing_newline {
        content.pop();
    }

    // Passes 1–3 — the opinionated structural rules.
    content = consolidate_data(&content, opts);
    content = align_table_tags(content, opts);
    content = blank_line_after_return(content, opts);
    content = collapse_blank_lines(content, opts);

    // Reassemble, applying Pass 5 (final newline) or preserving the original
    // trailing-newline presence.
    let body = content.join("\n");
    if opts.final_newline {
        let trimmed = body.trim_end_matches('\n');
        if trimmed.is_empty() {
            String::new()
        } else {
            format!("{trimmed}\n")
        }
    } else if had_trailing_newline {
        format!("{body}\n")
    } else {
        body
    }
}

fn text<'a>(source: &'a str, lx: &Lexeme) -> &'a str {
    &source[lx.start..lx.end]
}

fn is_punct(source: &str, lx: &Lexeme, s: &str) -> bool {
    lx.kind == LexKind::Punct && text(source, lx) == s
}

fn format_line(source: &str, line: &[Lexeme], opts: &FormatOptions, indent: &str) -> String {
    let first_sig = line.iter().position(|l| l.kind != LexKind::Whitespace);
    let Some(first_sig) = first_sig else {
        // Blank or whitespace-only line.
        return String::new();
    };
    let sig: Vec<&Lexeme> = line
        .iter()
        .filter(|l| l.kind != LexKind::Whitespace)
        .collect();

    // A comment-only line keeps its original indentation (don't re-flow prose).
    if sig.len() == 1 && sig[0].kind == LexKind::Comment {
        let lead = text(source, &line[0]);
        let lead = if line[0].kind == LexKind::Whitespace {
            lead
        } else {
            ""
        };
        return format!("{lead}{}", text(source, sig[0]))
            .trim_end()
            .to_string();
    }

    let instruction_line = is_indented(source, &sig, opts, first_sig > 0);
    let lead = if instruction_line { indent } else { "" };
    let body = build_body(source, line, first_sig, opts, instruction_line);
    format!("{lead}{body}").trim_end().to_string()
}

/// Format a continuation line — a physical line whose operands belong to the
/// preceding statement (the previous line ended with a trailing operand comma).
/// Its body is rebuilt like any operand list, but its leading whitespace is
/// `lead` (computed by [`continuation_lead`] to align under the opening line's
/// first argument) instead of the block indent. A comment-only continuation
/// line keeps its own indentation, exactly as [`format_line`] treats comments.
fn format_continuation_line(
    source: &str,
    line: &[Lexeme],
    opts: &FormatOptions,
    lead: &str,
) -> String {
    let Some(first_sig) = line.iter().position(|l| l.kind != LexKind::Whitespace) else {
        return String::new();
    };
    let sig: Vec<&Lexeme> = line
        .iter()
        .filter(|l| l.kind != LexKind::Whitespace)
        .collect();
    if sig.len() == 1 && sig[0].kind == LexKind::Comment {
        let orig = if line[0].kind == LexKind::Whitespace {
            text(source, &line[0])
        } else {
            ""
        };
        return format!("{orig}{}", text(source, sig[0]))
            .trim_end()
            .to_string();
    }
    // The first token of a continuation line is an operand, never a mnemonic.
    let body = build_body(source, line, first_sig, opts, false);
    format!("{lead}{body}").trim_end().to_string()
}

/// Reconstruct a line's body from its first significant lexeme to its last,
/// preserving internal whitespace except around commas (no space before, one
/// after when `comma_spacing`, else tight). Case normalization (Pass 4) applies
/// here: when `mnemonic_line`, the first significant token is the instruction
/// mnemonic.
fn build_body(
    source: &str,
    line: &[Lexeme],
    first_sig: usize,
    opts: &FormatOptions,
    mnemonic_line: bool,
) -> String {
    let last_sig = line
        .iter()
        .rposition(|l| l.kind != LexKind::Whitespace)
        .unwrap();
    let body_lexemes = &line[first_sig..=last_sig];
    let mut body = String::new();
    let mut seen_significant = false;
    for (k, lx) in body_lexemes.iter().enumerate() {
        if lx.kind == LexKind::Whitespace {
            let prev_comma = k > 0 && is_punct(source, &body_lexemes[k - 1], ",");
            let next_comma =
                k + 1 < body_lexemes.len() && is_punct(source, &body_lexemes[k + 1], ",");
            if !prev_comma && !next_comma {
                body.push_str(text(source, lx));
            }
        } else if is_punct(source, lx, ",") {
            body.push(',');
            if opts.comma_spacing && k != body_lexemes.len() - 1 {
                body.push(' ');
            }
            seen_significant = true;
        } else {
            let is_mnemonic = mnemonic_line
                && !seen_significant
                && lx.kind == LexKind::Ident
                && MNEMONICS.contains(&text(source, lx).to_ascii_lowercase());
            body.push_str(&cased_lexeme(source, lx, opts, is_mnemonic));
            seen_significant = true;
        }
    }
    body
}

/// Whether `line`'s last significant token — ignoring a trailing comment — is a
/// comma, marking a multi-line statement whose operand list continues on the
/// next physical line.
fn ends_with_operand_comma(source: &str, line: &[Lexeme]) -> bool {
    line.iter()
        .rev()
        .find(|l| !matches!(l.kind, LexKind::Whitespace | LexKind::Comment))
        .is_some_and(|l| is_punct(source, l, ","))
}

/// The leading whitespace for the continuation lines of the multi-line statement
/// whose already-formatted opening line is `opening`: the opening line's own
/// leading whitespace (verbatim — spaces, or a tab under `IndentStyle::Tab`)
/// followed by spaces spanning the opening's first token and the single gap
/// before its first argument. The continuation's first token therefore lines up
/// directly under the opening line's first argument, in both indent styles.
fn continuation_lead(opening: &str) -> String {
    let trimmed = opening.trim_start_matches([' ', '\t']);
    let base_ws = &opening[..opening.len() - trimmed.len()];
    // The opening's first token (directive/pseudo/mnemonic) is the run of
    // non-whitespace after the indent; the gap is the whitespace before arg 1.
    let after_token = trimmed.trim_start_matches(|c: char| c != ' ' && c != '\t');
    let token_cols = trimmed.chars().count() - after_token.chars().count();
    let arg = after_token.trim_start_matches([' ', '\t']);
    let gap_cols = after_token.chars().count() - arg.chars().count();
    format!("{base_ws}{}", " ".repeat(token_cols + gap_cols))
}

/// The text of `lx`, with Pass-4 case normalization applied: the instruction
/// mnemonic per `mnemonic_case`, numeric-literal hex digits per `hex_digit_case`,
/// everything else verbatim.
fn cased_lexeme(source: &str, lx: &Lexeme, opts: &FormatOptions, is_mnemonic: bool) -> String {
    let t = text(source, lx);
    if is_mnemonic {
        opts.mnemonic_case.apply(t)
    } else if lx.kind == LexKind::Number {
        opts.hex_digit_case.apply_hex(t)
    } else {
        t.to_string()
    }
}

/// Whether a line is an indented statement line, from its significant lexemes:
/// named labels (`name:`), anonymous labels (`:`), and constant definitions
/// (`name = …`) sit at column 0 (returns `false`); instructions are indented
/// (returns `true`). Directives follow [`FormatOptions::indent_directives`]:
/// pinned to column 0 by default, indented like instructions when enabled.
fn is_indented(
    source: &str,
    sig: &[&Lexeme],
    opts: &FormatOptions,
    written_indented: bool,
) -> bool {
    let first = sig[0];
    match first.kind {
        LexKind::Directive => opts.indent_directives,
        LexKind::Ident => {
            let n = name_lexemes(source, sig);
            // A lone name (plus an optional comment) means different things in
            // column 1 (a constant, or an `.enum` member) and indented (an
            // implied-operand instruction), so it keeps the side it was
            // written on: moving it would change the assembled bytes
            // (`plans/019-value-enums.md` §3.6).
            if sig.get(n).is_none_or(|l| l.kind == LexKind::Comment) {
                return written_indented;
            }
            let is_const = sig.get(n).is_some_and(|l| is_punct(source, l, "="));
            !(is_named_label(source, sig) || is_const || is_label_with_content(source, sig, n))
        }
        LexKind::Punct if is_punct(source, first, ":") => false,
        _ => true,
    }
}

/// Whether `sig` (a line's significant lexemes, whitespace already filtered out)
/// begins a named-label definition `name:` — an identifier followed by a `:`
/// that ends the line (a trailing comment is allowed). A `:` followed by any
/// other token is the `:+`/`:-` operand of a branch such as `BEQ :+`, not a
/// label, so the line is an ordinary instruction. This mirrors the assembler's
/// own rule (`parse.rs`: a label is `TEXT COLON` only when the colon ends the
/// line) — keeping formatter and assembler in agreement so a format pass never
/// changes the assembled bytes.
fn is_named_label(source: &str, sig: &[&Lexeme]) -> bool {
    let n = name_lexemes(source, sig);
    sig.first().is_some_and(|l| l.kind == LexKind::Ident)
        && sig.get(n).is_some_and(|l| is_punct(source, l, ":"))
        && sig.get(n + 1).is_none_or(|l| l.kind == LexKind::Comment)
}

/// Whether `sig` is a `name: <more>` line — a label with content after it,
/// which the assembler rejects ("a label must be on a line of its own"). The
/// colon touches the name, and what follows is not the `+`/`-` of a `:+`/`:-`
/// branch operand. The formatter leaves such a line at column 0 rather than
/// indenting it as an instruction, so its label stays where it was written.
fn is_label_with_content(source: &str, sig: &[&Lexeme], n: usize) -> bool {
    let (Some(name), Some(colon)) = (sig.get(n - 1), sig.get(n)) else {
        return false;
    };
    is_punct(source, colon, ":")
        && name.end == colon.start
        && sig
            .get(n + 1)
            .is_some_and(|l| !is_punct(source, l, "+") && !is_punct(source, l, "-"))
}

/// How many lexemes the symbol name starting `sig[0]` spans: 1 for a plain name,
/// 3 for a pasted `.for` name (`row\i`: the identifier, the `\`, and the
/// counter letter `i`/`j`/`k`, with no whitespace between them). The `.for`
/// loop (plan 016 §2.2) defines one such symbol per iteration, so a line
/// starting `row\i:` or `ROW\i =` is a definition like any other.
fn name_lexemes(source: &str, sig: &[&Lexeme]) -> usize {
    match sig {
        [name, slash, counter, ..]
            if name.kind == LexKind::Ident
                && is_punct(source, slash, "\\")
                && counter.kind == LexKind::Ident
                && matches!(text(source, counter), "i" | "j" | "k")
                && name.end == slash.start
                && slash.end == counter.start =>
        {
            3
        }
        _ => 1,
    }
}

/// How many lexemes the `[KEY]` tag starting `sig[0]` spans, brackets
/// included, or 0 when the line does not start with one
/// (`plans/020-index-keyed-tables.md` §6). Shared by the formatter, the linter
/// and the language server, so the three agree on what a tag is.
#[must_use]
pub fn tag_lexemes(source: &str, sig: &[&Lexeme]) -> usize {
    if !sig.first().is_some_and(|l| is_punct(source, l, "[")) {
        return 0;
    }
    let mut depth = 0usize;
    for (i, l) in sig.iter().enumerate() {
        if l.kind == LexKind::Comment {
            return 0;
        }
        if is_punct(source, l, "[") {
            depth += 1;
        } else if is_punct(source, l, "]") {
            depth -= 1;
            if depth == 0 {
                return i + 1;
            }
        }
    }
    0
}

// ── Pass 1b: `.table` tag alignment ──────────────────────────────────────────

/// The first significant lexeme of `line` is the directive `name`
/// (case-insensitively, without its dot).
fn starts_with_directive(line: &str, name: &str) -> bool {
    lex(line)
        .iter()
        .find(|l| l.kind != LexKind::Whitespace)
        .filter(|l| l.kind == LexKind::Directive)
        .and_then(|l| text(line, l).strip_prefix('.'))
        .is_some_and(|d| d.eq_ignore_ascii_case(name))
}

/// A tagged line split into its normalized tag (`[KEY]`, the brackets' inner
/// whitespace trimmed) and the statement after it (possibly empty).
fn split_tag(line: &str) -> Option<(String, String)> {
    let lexemes = lex(line);
    let sig: Vec<&Lexeme> = lexemes
        .iter()
        .filter(|l| l.kind != LexKind::Whitespace)
        .collect();
    let n = tag_lexemes(line, &sig);
    if n < 3 {
        return None;
    }
    let inner = &line[sig[1].start..sig[n - 2].end];
    let rest = sig.get(n).map_or("", |l| line[l.start..].trim_end());
    Some((format!("[{inner}]"), rest.to_string()))
}

/// Align the statements of every `.table`'s tagged entries into one column
/// (§6): each tagged line becomes `lead + tag + pad + rest`, where `pad`
/// brings `rest` one space past the widest tag in that table and `lead` is what
/// the statement would get untagged. Untagged statement lines inside the table
/// (the continuation lines of a multi-line record) are padded to the same
/// column; comments, blank lines, labels and constants are left alone.
fn align_table_tags(mut lines: Vec<String>, opts: &FormatOptions) -> Vec<String> {
    let indent = opts.indent_unit();
    let lead_for = |stmt: &str| -> &str {
        let first = lex(stmt)
            .into_iter()
            .find(|l| l.kind != LexKind::Whitespace);
        match first.map(|l| l.kind) {
            Some(LexKind::Ident) => indent.as_str(),
            _ if opts.indent_directives => indent.as_str(),
            _ => "",
        }
    };
    let mut i = 0;
    while i < lines.len() {
        if !starts_with_directive(&lines[i], "table") {
            i += 1;
            continue;
        }
        let Some(end) = (i + 1..lines.len()).find(|&j| {
            starts_with_directive(&lines[j], "endtable")
                || starts_with_directive(&lines[j], "table")
        }) else {
            break;
        };
        let widest = (i + 1..end)
            .filter_map(|k| split_tag(&lines[k]))
            .map(|(tag, _)| tag.chars().count())
            .max();
        let Some(widest) = widest else {
            i = end;
            continue;
        };
        let column = " ".repeat(widest + 1);
        // The lead for the continuation lines of an operand list carried by a
        // trailing comma, under its first argument.
        let mut continuation: Option<String> = None;
        for line in &mut lines[i + 1..end] {
            let trimmed = line.trim_start().to_string();
            let lexemes = lex(&trimmed);
            if let Some(lead) = continuation.take() {
                if opts.align_continuations && !is_comment_only(&lexemes) {
                    *line = format!("{lead}{trimmed}");
                }
                if ends_with_operand_comma(&trimmed, &lexemes) {
                    continuation = Some(lead);
                }
                continue;
            }
            if is_blank(&lexemes) || is_comment_only(&lexemes) {
                continue;
            }
            let (prefix, rest) = if let Some((tag, rest)) = split_tag(&trimmed) {
                if rest.is_empty() {
                    *line = format!("{}{tag}", lead_for(""));
                    continue;
                }
                let pad = " ".repeat(widest - tag.chars().count() + 1);
                (format!("{}{tag}{pad}", lead_for(&rest)), rest)
            } else if is_label_or_constant(&trimmed) {
                continue;
            } else {
                (format!("{}{column}", lead_for(&trimmed)), trimmed)
            };
            *line = format!("{prefix}{rest}");
            let rest_lexemes = lex(&rest);
            if ends_with_operand_comma(&rest, &rest_lexemes) {
                let base: String = prefix
                    .chars()
                    .map(|c| if c == '\t' { '\t' } else { ' ' })
                    .collect();
                continuation = Some(format!("{base}{}", continuation_lead(&rest)));
            }
        }
        i = end;
    }
    lines
}

// ── Pass 1: data-block consolidation ─────────────────────────────────────────

/// Whether `name` is a consolidatable data directive (case-insensitively).
fn is_data_directive(name: &str) -> bool {
    name.eq_ignore_ascii_case("db")
        || name.eq_ignore_ascii_case("dw")
        || name.eq_ignore_ascii_case("color")
}

/// Parse a `.db`/`.dw`/`.color` line with **no** trailing comment into its
/// directive name (without the dot), leading indent, and comma-separated
/// values. Returns `None` for anything else — including a data line that
/// carries a comment (comments pin structure, so such a line is never merged).
fn parse_data_line(line: &str) -> Option<(String, String, Vec<String>)> {
    let lexemes = lex(line);
    let first = lexemes
        .iter()
        .find(|l| !matches!(l.kind, LexKind::Whitespace | LexKind::Newline))?;
    if first.kind != LexKind::Directive {
        return None;
    }
    let name = line[first.start..first.end].strip_prefix('.')?;
    if !is_data_directive(name) {
        return None;
    }
    if lexemes.iter().any(|l| l.kind == LexKind::Comment) {
        return None;
    }
    // A trailing comma continues the operand list onto the next line, so this
    // line is only the start of one statement: re-rendering it on its own would
    // drop the comma and strand the continuation as a statement of its own.
    if ends_with_operand_comma(line, &lexemes) {
        return None;
    }
    let indent = line[..first.start].to_string();
    let args = line[first.end..].trim();
    let values: Vec<String> = args
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if values.is_empty() {
        return None;
    }
    Some((name.to_string(), indent, values))
}

/// Whether a line is a `.db`/`.dw`/`.color` directive carrying a trailing
/// comment (the "pinned" data line that is emitted verbatim, never merged).
fn is_commented_data_line(line: &str) -> bool {
    let lexemes = lex(line);
    let Some(first) = lexemes
        .iter()
        .find(|l| !matches!(l.kind, LexKind::Whitespace | LexKind::Newline))
    else {
        return false;
    };
    if first.kind != LexKind::Directive {
        return false;
    }
    let Some(name) = line[first.start..first.end].strip_prefix('.') else {
        return false;
    };
    is_data_directive(name) && lexemes.iter().any(|l| l.kind == LexKind::Comment)
}

/// Whether a line is a named label (`name:`) or constant definition
/// (`name = …`), matching the group-flush rule from the reference formatter. A
/// `:+`/`:-` operand (`BEQ :+`) is not a label — see [`is_named_label`].
fn is_label_or_constant(line: &str) -> bool {
    let lexemes = lex(line);
    let sig: Vec<&Lexeme> = lexemes
        .iter()
        .filter(|l| !matches!(l.kind, LexKind::Whitespace | LexKind::Newline))
        .collect();
    if sig.first().is_none_or(|l| l.kind != LexKind::Ident) {
        return false;
    }
    is_named_label(line, &sig)
        || sig
            .get(name_lexemes(line, &sig))
            .is_some_and(|l| is_punct(line, l, "="))
}

// ── Directive binding (shared by the formatter and the linter) ───────────────
//
// Where a `; @nessemble-format` directive attaches. This lived twice before —
// once in the formatter's stride state machine, once in the
// `ineffective-comment-directive` rule — and the two skip sets drifted apart, so
// the formatter silently ignored a directive the linter called fine and warned
// about one the formatter applied. One definition now; keep it that way.

/// Whether `line` sits **transparently** between a `@nessemble-format` directive
/// and the data run it governs — the skip set both callers share:
///
/// - a **blank** line, so a directive may breathe above its run;
/// - a **comment-only** line, so an explanation may follow the directive (the
///   same courtesy `@nessemble-coverage-ignore-next-line` documents);
/// - a **label or constant definition**, because a label is how a data run is
///   named, which makes `; @nessemble-format` above the label the natural
///   spelling.
fn is_transparent_before_data(line: &str) -> bool {
    let lexemes = lex(line);
    if is_blank(&lexemes) || is_comment_only(&lexemes) {
        return true;
    }
    is_label_or_constant(line)
}

/// Whether `line`'s first significant token is a data directive (`.db`, `.dw`,
/// `.color`) — the thing a stride hint applies to. A trailing comment does not
/// matter: such a line is pinned rather than merged, but the directive still
/// binds to it.
fn is_data_line(line: &str) -> bool {
    lex(line)
        .iter()
        .find(|l| l.kind != LexKind::Whitespace)
        .filter(|l| l.kind == LexKind::Directive)
        .and_then(|l| text(line, l).strip_prefix('.'))
        .is_some_and(is_data_directive)
}

/// The offset, within the lines **following** a `@nessemble-format` directive,
/// of the data run it governs — or `None` when it governs nothing, which is what
/// makes the directive ineffective.
///
/// The single definition of "which line does this directive bind to", called by
/// both [`consolidate_data`] and `ineffective-comment-directive` so a directive
/// the formatter applies is exactly a directive the linter calls effective.
fn format_directive_target<'a>(following: impl IntoIterator<Item = &'a str>) -> Option<usize> {
    for (offset, line) in following.into_iter().enumerate() {
        if is_transparent_before_data(line) {
            continue;
        }
        return is_data_line(line).then_some(offset);
    }
    None
}

/// Parse a `; @nessemble-format stride=N[,N,...]` hint comment — or its
/// deprecated `; @fmt` spelling — into its stride list. Both spellings arrive
/// through the one directive scanner, so they cannot drift apart.
///
/// A hint in a *trailing* comment is inert (`own_line`), preserving the original
/// behavior that only a comment-only line carries a hint.
fn parse_hint(line: &str) -> Option<Vec<usize>> {
    scan_directives(line)
        .into_iter()
        .find_map(|d| match d.args {
            DirectiveArgs::Strides(strides) if d.own_line => Some(strides),
            _ => None,
        })
}

/// A single buffered value under an active stride hint: `(directive, indent,
/// value)`.
type HintValue = (String, String, String);

/// Emit `values` as lines using `strides` starting at `start_idx`. A directive
/// change forces a break (consuming a stride slot); the final stride repeats
/// once the list is exhausted. Returns the emitted lines and the next stride
/// index (so a later run continues the cycle).
fn emit_hinted_run(
    values: &[HintValue],
    strides: &[usize],
    start_idx: usize,
    sep: &str,
) -> (Vec<String>, usize) {
    let mut out = Vec::new();
    let mut si = start_idx;
    let mut i = 0;
    while i < values.len() {
        let stride = strides[si.min(strides.len() - 1)].max(1);
        let cur_type = &values[i].0;
        let indent = &values[i].1;
        let mut batch: Vec<&str> = Vec::new();
        let mut j = i;
        while j < values.len() && j - i < stride && &values[j].0 == cur_type {
            batch.push(&values[j].2);
            j += 1;
        }
        out.push(format!("{indent}.{cur_type} {}", batch.join(sep)));
        si += 1;
        i = j;
    }
    (out, si)
}

/// Emit an accumulated ungrouped data run as `per`-value lines.
fn flush_group(
    out: &mut Vec<String>,
    group: &mut Option<(String, String, Vec<String>)>,
    per: usize,
    sep: &str,
) {
    if let Some((dir, indent, values)) = group.take() {
        for chunk in values.chunks(per) {
            out.push(format!("{indent}.{dir} {}", chunk.join(sep)));
        }
    }
}

/// Emit the buffered hint values and advance the stride index.
fn flush_hint(
    out: &mut Vec<String>,
    buffer: &mut Vec<HintValue>,
    strides: &[usize],
    stride_idx: &mut usize,
    sep: &str,
) {
    if buffer.is_empty() {
        return;
    }
    let (lines, next) = emit_hinted_run(buffer, strides, *stride_idx, sep);
    out.extend(lines);
    *stride_idx = next;
    buffer.clear();
}

/// Consolidate adjacent `.db`/`.dw`/`.color` lines into `data_per_line`-value
/// lines, honoring `; @nessemble-format stride=N` hints. Grouping semantics: a directive-type
/// change, a label/constant, an instruction, a blank line, or a trailing comment
/// all flush the current group; hinted blocks buffer values and re-flow them by
/// their strides.
///
/// A hint activates only when [`format_directive_target`] finds the run it binds
/// to; the transparent lines in between (blank, comment, label/constant) pass
/// through untouched while the hint waits for its data.
fn consolidate_data(lines: &[String], opts: &FormatOptions) -> Vec<String> {
    if opts.data_per_line == 0 {
        return lines.to_vec();
    }
    let per = opts.data_per_line;
    let sep = opts.comma_sep();
    let hints_on = opts.respect_stride_hints;

    let mut out: Vec<String> = Vec::new();
    let mut group: Option<(String, String, Vec<String>)> = None;
    let mut hint_strides: Option<Vec<usize>> = None;
    let mut stride_idx = 0usize;
    let mut hint_buffer: Vec<HintValue> = Vec::new();
    let mut consecutive_blanks = 0usize;
    // Lines still to pass through between an activated hint and its data run.
    let mut pending_gap = 0usize;

    for (idx, line) in lines.iter().enumerate() {
        if hints_on {
            if let Some(strides) = parse_hint(line) {
                flush_group(&mut out, &mut group, per, sep);
                if let Some(hs) = &hint_strides {
                    flush_hint(&mut out, &mut hint_buffer, hs, &mut stride_idx, sep);
                }
                // A hint that binds to nothing is inert — the linter reports it
                // as `ineffective-comment-directive` from the same lookahead.
                let target = format_directive_target(lines[idx + 1..].iter().map(String::as_str));
                hint_strides = target.is_some().then_some(strides);
                pending_gap = target.unwrap_or(0);
                stride_idx = 0;
                consecutive_blanks = 0;
                out.push(line.clone());
                continue;
            }
        }

        // Inside the gap a hint is waiting across. Every such line is transparent
        // by construction, and nothing is buffered yet (the hint line flushed),
        // so emit it verbatim without disturbing the pending hint.
        if pending_gap > 0 {
            pending_gap -= 1;
            consecutive_blanks = 0;
            out.push(line.clone());
            continue;
        }

        if line.trim().is_empty() {
            consecutive_blanks += 1;
            match &hint_strides {
                Some(hs) => flush_hint(&mut out, &mut hint_buffer, hs, &mut stride_idx, sep),
                None => flush_group(&mut out, &mut group, per, sep),
            }
            // Two consecutive blank lines end an active stride hint.
            if hint_strides.is_some() && consecutive_blanks >= 2 {
                hint_strides = None;
                stride_idx = 0;
            }
            out.push(line.clone());
            continue;
        }

        let prev_blanks = consecutive_blanks;
        consecutive_blanks = 0;

        if let Some((dir, indent, values)) = parse_data_line(line) {
            if hint_strides.is_some() {
                for v in values {
                    hint_buffer.push((dir.clone(), indent.clone(), v));
                }
            } else {
                match &mut group {
                    Some((gdir, _, gvals)) if *gdir == dir => gvals.extend(values),
                    _ => {
                        flush_group(&mut out, &mut group, per, sep);
                        group = Some((dir, indent, values));
                    }
                }
            }
            continue;
        }

        // A non-mergeable line. Flush appropriately, then emit it verbatim.
        if is_commented_data_line(line) {
            // A pinned data line: flush but keep any active hint alive.
            match &hint_strides {
                Some(hs) => flush_hint(&mut out, &mut hint_buffer, hs, &mut stride_idx, sep),
                None => flush_group(&mut out, &mut group, per, sep),
            }
        } else if is_label_or_constant(line) && prev_blanks == 0 {
            // A label/constant butting against data flushes but keeps the hint.
            match &hint_strides {
                Some(hs) => flush_hint(&mut out, &mut hint_buffer, hs, &mut stride_idx, sep),
                None => flush_group(&mut out, &mut group, per, sep),
            }
        } else if let Some(hs) = hint_strides.take() {
            // Any other line ends an active hint (consuming it clears the state).
            flush_hint(&mut out, &mut hint_buffer, &hs, &mut stride_idx, sep);
            stride_idx = 0;
        } else {
            flush_group(&mut out, &mut group, per, sep);
        }
        out.push(line.clone());
    }

    flush_group(&mut out, &mut group, per, sep);
    if let Some(hs) = &hint_strides {
        flush_hint(&mut out, &mut hint_buffer, hs, &mut stride_idx, sep);
    }
    out
}

// ── Pass 2: blank line after RTS / RTI ───────────────────────────────────────

/// Whether a line's only instruction is `RTS`/`RTI` (an optional trailing
/// comment is allowed).
fn is_return_line(line: &str) -> bool {
    let lexemes = lex(line);
    let sig: Vec<&Lexeme> = lexemes
        .iter()
        .filter(|l| !matches!(l.kind, LexKind::Whitespace | LexKind::Newline))
        .collect();
    let Some(first) = sig.first() else {
        return false;
    };
    if first.kind != LexKind::Ident {
        return false;
    }
    let m = &line[first.start..first.end];
    if !(m.eq_ignore_ascii_case("rts") || m.eq_ignore_ascii_case("rti")) {
        return false;
    }
    sig[1..].iter().all(|l| l.kind == LexKind::Comment)
}

/// Insert one blank line after each `RTS`/`RTI` that is followed by a
/// non-blank line.
fn blank_line_after_return(mut lines: Vec<String>, opts: &FormatOptions) -> Vec<String> {
    if !opts.blank_line_after_return {
        return lines;
    }
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    for i in 0..lines.len() {
        let is_return = is_return_line(&lines[i]);
        let next_nonblank = lines.get(i + 1).is_some_and(|next| !next.trim().is_empty());
        out.push(std::mem::take(&mut lines[i]));
        if is_return && next_nonblank {
            out.push(String::new());
        }
    }
    out
}

// ── Pass 3: collapse blank-line runs ─────────────────────────────────────────

/// Collapse runs of more than `max_consecutive_blank_lines` blank lines.
fn collapse_blank_lines(lines: Vec<String>, opts: &FormatOptions) -> Vec<String> {
    let max = opts.max_consecutive_blank_lines;
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut blanks = 0usize;
    for line in lines {
        if line.trim().is_empty() {
            blanks += 1;
            if blanks <= max {
                out.push(line);
            }
        } else {
            blanks = 0;
            out.push(line);
        }
    }
    out
}

// ─── Comment directives ──────────────────────────────────────────────────────
//
// Comments addressed to a nessemble tool, in one namespaced grammar:
//
//     ; @nessemble-<name> [args]   [; trailing prose]
//
// The registry is **closed**: a comment whose first token is an unrecognized
// `@nessemble-…` name is reported as malformed rather than silently ignored,
// which is the whole point of the namespace — a mistyped directive used to be
// indistinguishable from prose. Tokens outside the namespace (`@todo`,
// `@param`) are prose and are never touched.
//
// See `plans/009-comment-directives.md`.

/// A directive's registry name — its identity, independent of its arguments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectiveName {
    /// `@nessemble-format` — a formatter hint (deprecated spelling: `@fmt`).
    Format,
    /// `@nessemble-coverage-ignore` — a coverage exclusion region boundary.
    CoverageIgnore,
    /// `@nessemble-coverage-ignore-next-line` — a one-line coverage exclusion.
    CoverageIgnoreNextLine,
    /// `@nessemble-param` — a routine input slot (see [`Signature`]).
    Param,
    /// `@nessemble-returns` — a routine output slot.
    Returns,
    /// `@nessemble-clobbers` — the slots a routine destroys.
    Clobbers,
    /// `@nessemble-lint-ignore-next-line` — suppress findings on one line.
    LintIgnoreNextLine,
    /// `@nessemble-lint-ignore` — a suppression region boundary.
    LintIgnore,
}

/// The directive registry: token → name, plus whether the token is a deprecated
/// alias. Adding a directive is an entry here, a [`DirectiveName`] variant, and
/// an arm in [`parse_args`].
///
/// Lookup is by **exact** token match, so `@nessemble-coverage-ignore-next-line`
/// can never resolve as `@nessemble-coverage-ignore` with a stray argument.
const DIRECTIVES: &[(&str, DirectiveName, bool)] = &[
    ("@nessemble-format", DirectiveName::Format, false),
    (
        "@nessemble-coverage-ignore",
        DirectiveName::CoverageIgnore,
        false,
    ),
    (
        "@nessemble-coverage-ignore-next-line",
        DirectiveName::CoverageIgnoreNextLine,
        false,
    ),
    ("@nessemble-param", DirectiveName::Param, false),
    ("@nessemble-returns", DirectiveName::Returns, false),
    ("@nessemble-clobbers", DirectiveName::Clobbers, false),
    (
        "@nessemble-lint-ignore-next-line",
        DirectiveName::LintIgnoreNextLine,
        false,
    ),
    ("@nessemble-lint-ignore", DirectiveName::LintIgnore, false),
    // Deprecated alias, honored indefinitely: `@fmt` is a shipped, documented
    // spelling, and dropping it would silently re-flow every project that uses
    // it. Reported by the linter, never by the formatter.
    ("@fmt", DirectiveName::Format, true),
];

/// The namespace every directive shares. A comment token starting with this and
/// not in [`DIRECTIVES`] is a mistake worth reporting; anything else is prose.
const DIRECTIVE_NAMESPACE: &str = "@nessemble-";

impl DirectiveName {
    /// Every directive name, in registry order.
    pub const ALL: [DirectiveName; 8] = [
        DirectiveName::Format,
        DirectiveName::CoverageIgnore,
        DirectiveName::CoverageIgnoreNextLine,
        DirectiveName::Param,
        DirectiveName::Returns,
        DirectiveName::Clobbers,
        DirectiveName::LintIgnoreNextLine,
        DirectiveName::LintIgnore,
    ];

    /// The canonical (non-deprecated) spelling, including the `@`.
    #[must_use]
    pub fn canonical(self) -> &'static str {
        match self {
            DirectiveName::Format => "@nessemble-format",
            DirectiveName::CoverageIgnore => "@nessemble-coverage-ignore",
            DirectiveName::CoverageIgnoreNextLine => "@nessemble-coverage-ignore-next-line",
            DirectiveName::Param => "@nessemble-param",
            DirectiveName::Returns => "@nessemble-returns",
            DirectiveName::Clobbers => "@nessemble-clobbers",
            DirectiveName::LintIgnoreNextLine => "@nessemble-lint-ignore-next-line",
            DirectiveName::LintIgnore => "@nessemble-lint-ignore",
        }
    }

    /// This directive's argument syntax, for diagnostics and hovers; empty when
    /// it takes none.
    #[must_use]
    pub fn arg_syntax(self) -> &'static str {
        match self {
            DirectiveName::Format => "stride=N[,N,...]",
            DirectiveName::CoverageIgnore => "start|end",
            DirectiveName::CoverageIgnoreNextLine => "",
            DirectiveName::Param | DirectiveName::Returns => "<slot> [description]",
            DirectiveName::Clobbers => "<slot>[, <slot>...] | none",
            DirectiveName::LintIgnoreNextLine => "[rule[, rule...]]",
            DirectiveName::LintIgnore => "start|end [rule[, rule...]]",
        }
    }

    /// Whether this directive is one of the routine-signature annotations, whose
    /// arguments are [`Slot`]s and whose consumer is [`resolve_signatures`].
    #[must_use]
    pub fn is_signature_tag(self) -> bool {
        matches!(
            self,
            DirectiveName::Param | DirectiveName::Returns | DirectiveName::Clobbers
        )
    }

    /// Resolve a comment's first token to a directive name, reporting whether
    /// the token was a deprecated alias. Case-sensitive: `@NESSEMBLE-FORMAT` is
    /// prose.
    fn lookup(token: &str) -> Option<(DirectiveName, bool)> {
        DIRECTIVES
            .iter()
            .find(|(t, _, _)| *t == token)
            .map(|&(_, name, deprecated)| (name, deprecated))
    }
}

/// Which end of a coverage exclusion region a
/// [`CoverageIgnore`](DirectiveName::CoverageIgnore) directive marks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionBound {
    /// `start` — open a region (an unclosed one runs to end of file).
    Start,
    /// `end` — close the open region.
    End,
}

/// One of the 6502's status flags, as a [`Slot`]. `@nessemble-returns C` is how
/// a 6502 routine returns a boolean.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Flag {
    /// Carry.
    C,
    /// Zero.
    Z,
    /// Negative.
    N,
    /// Overflow.
    V,
    /// Decimal.
    D,
    /// Interrupt disable.
    I,
}

impl Flag {
    /// The flag's one-letter name.
    #[must_use]
    pub fn letter(self) -> &'static str {
        match self {
            Flag::C => "C",
            Flag::Z => "Z",
            Flag::N => "N",
            Flag::V => "V",
            Flag::D => "D",
            Flag::I => "I",
        }
    }
}

/// A place a value can live across a call — what a routine-signature annotation
/// names.
///
/// The vocabulary is **closed**, which is what makes a typo reportable:
/// `@nessemble-clobbers AX` is bad arguments, not a memory symbol called `AX`.
/// Memory is therefore spelled with brackets (`[tmp1]`) or as a `$` address, so
/// it can never be confused with a mistyped register.
///
/// The derived ordering is the canonical rendering order (`A`, `X`, `Y`, `S`,
/// `P`, flags, symbols, addresses), so every consumer prints a slot list the
/// same way without agreeing on anything.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum Slot {
    /// The accumulator.
    A,
    /// The `X` index register.
    X,
    /// The `Y` index register.
    Y,
    /// The stack pointer.
    S,
    /// The whole status register.
    P,
    /// One status flag.
    Flag(Flag),
    /// A named memory location, written `[name]`.
    Symbol(String),
    /// An address or inclusive address range, written `$NN` or `$NN-$NN`.
    Address(u16, u16),
}

impl fmt::Display for Slot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Slot::A => f.write_str("A"),
            Slot::X => f.write_str("X"),
            Slot::Y => f.write_str("Y"),
            Slot::S => f.write_str("S"),
            Slot::P => f.write_str("P"),
            Slot::Flag(flag) => f.write_str(flag.letter()),
            Slot::Symbol(name) => write!(f, "[{name}]"),
            Slot::Address(lo, hi) if lo == hi => write!(f, "{}", hex_addr(*lo)),
            Slot::Address(lo, hi) => write!(f, "{}-{}", hex_addr(*lo), hex_addr(*hi)),
        }
    }
}

/// Render an address the width it was most likely written: two hex digits for
/// zero page, four above it.
fn hex_addr(addr: u16) -> String {
    if addr <= 0xFF {
        format!("${addr:02X}")
    } else {
        format!("${addr:04X}")
    }
}

/// Format a slot list the way every surface shows it: canonical order, comma
/// separated, or `none` when empty.
#[must_use]
pub fn format_slots(slots: &[Slot]) -> String {
    if slots.is_empty() {
        return "none".to_string();
    }
    let mut sorted: Vec<&Slot> = slots.iter().collect();
    sorted.sort();
    sorted
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

/// A directive's parsed arguments. Parsing happens once, in the scanner, so no
/// consumer re-parses argument text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectiveArgs {
    /// `@nessemble-format stride=N[,N,…]`; never empty.
    Strides(Vec<usize>),
    /// `@nessemble-coverage-ignore start` / `end`.
    Region(RegionBound),
    /// `@nessemble-param` / `@nessemble-returns`: one slot and its description
    /// (empty when none was written).
    Slot(Slot, String),
    /// `@nessemble-clobbers`: the slot list. **Empty means `none` was written**
    /// — an explicit "preserves everything" claim, not a missing argument.
    Slots(Vec<Slot>),
    /// `@nessemble-lint-ignore-next-line`: the rules to suppress. **Empty means
    /// every rule** — the bare form is the blanket one.
    Rules(Vec<RuleId>),
    /// `@nessemble-lint-ignore start|end`: the bound, and the rules to suppress
    /// (empty for every rule). Rules are read from the `start`; an `end` closes
    /// whatever is open.
    RegionRules(RegionBound, Vec<RuleId>),
    /// The directive takes no arguments.
    None,
}

/// A well-formed directive comment found in source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Directive {
    pub name: DirectiveName,
    pub args: DirectiveArgs,
    /// 1-based line of the comment carrying the directive.
    pub line: u32,
    /// 1-based character column of the `@`.
    pub column: u32,
    /// Byte range of the directive token itself (`@…`), for editors that narrow
    /// a diagnostic or rewrite the token.
    pub start: usize,
    pub end: usize,
    /// The source spelled a deprecated alias (`@fmt`).
    pub deprecated: bool,
    /// The comment is the only significant content on its line. A directive in a
    /// trailing comment is **inert** — consumers must skip it — and the linter
    /// reports it.
    pub own_line: bool,
}

/// Why a comment addressed to nessemble is not a usable directive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MalformedReason {
    /// An `@nessemble-…` token that is not in the registry (a typo, or a
    /// directive from a newer nessemble).
    UnknownName,
    /// A known directive whose arguments are missing, unparseable, or extra.
    /// Carries the name so a message can quote [`arg_syntax`](DirectiveName::arg_syntax).
    BadArgs(DirectiveName),
}

/// A comment that meant to be a directive but is not one. Byte range and
/// position match [`Directive`], so both render the same way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MalformedDirective {
    pub reason: MalformedReason,
    pub line: u32,
    pub column: u32,
    pub start: usize,
    pub end: usize,
}

/// Every well-formed directive comment in `source`, in source order.
///
/// Malformed ones are dropped; use [`scan_directives_with_errors`] to report
/// them. Note that a returned directive may be `own_line: false` (a trailing
/// comment), which consumers treat as inert.
#[must_use]
pub fn scan_directives(source: &str) -> Vec<Directive> {
    scan_directives_with_errors(source).0
}

/// [`scan_directives`], plus the comments that tried to be directives and
/// failed — the input to the linter's directive rules.
#[must_use]
pub fn scan_directives_with_errors(source: &str) -> (Vec<Directive>, Vec<MalformedDirective>) {
    let lexemes = lex(source);
    let lines = split_lines(&lexemes);
    scan_directive_lines(source, &lines)
}

/// [`scan_directives_with_errors`] over an already-split lexeme stream, so a
/// caller that has one (the linter) does not lex twice.
fn scan_directive_lines(
    source: &str,
    lines: &[Vec<Lexeme>],
) -> (Vec<Directive>, Vec<MalformedDirective>) {
    let mut found = Vec::new();
    let mut malformed = Vec::new();

    for (idx, line) in lines.iter().enumerate() {
        // A comment runs to end of line, so a line holds at most one — but scan
        // for it rather than assuming a position.
        let own_line = line
            .iter()
            .all(|l| matches!(l.kind, LexKind::Whitespace | LexKind::Comment));
        for lx in line.iter().filter(|l| l.kind == LexKind::Comment) {
            let Some((parsed, offset, len)) = parse_directive_comment(text(source, lx)) else {
                continue;
            };
            let start = lx.start + offset;
            let end = start + len;
            let line_start = line.first().map_or(lx.start, |l| l.start);
            // The prefix of a comment line is whitespace and `;`s, so counting
            // characters gives the column directly.
            let column = source[line_start..start].chars().count() as u32 + 1;
            let line_no = (idx + 1) as u32;
            match parsed {
                Parsed::Directive(name, deprecated, args) => found.push(Directive {
                    name,
                    args,
                    line: line_no,
                    column,
                    start,
                    end,
                    deprecated,
                    own_line,
                }),
                Parsed::Malformed(reason) => malformed.push(MalformedDirective {
                    reason,
                    line: line_no,
                    column,
                    start,
                    end,
                }),
            }
        }
    }

    (found, malformed)
}

/// Directive comments in a **line-comment language** whose comments open with
/// `marker` (`//` for Rhai) instead of the assembler's `;`.
///
/// Only own-line comments are scanned — all the coverage directives need, and it
/// keeps the scan free of that language's string and block-comment rules. Best
/// effort by design: this is not a parser for the other language.
#[must_use]
pub fn scan_line_comment_directives(source: &str, marker: &str) -> Vec<Directive> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    for (idx, raw) in source.split_inclusive('\n').enumerate() {
        let line = raw.trim_end_matches(['\n', '\r']);
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        if let Some(after) = trimmed.strip_prefix(marker) {
            if let Some((Parsed::Directive(name, deprecated, args), rel, len)) =
                parse_directive_tail(marker.len(), after)
            {
                let start = offset + indent + rel;
                out.push(Directive {
                    name,
                    args,
                    line: (idx + 1) as u32,
                    column: (indent + rel) as u32 + 1,
                    start,
                    end: start + len,
                    deprecated,
                    own_line: true,
                });
            }
        }
        offset += raw.len();
    }
    out
}

/// Which 1-based lines of `source` carry significant content — neither blank nor
/// comment-only. Index `i` is line `i + 1`. This is the "next line" a directive
/// targets: an explanatory comment between a directive and its subject is
/// skipped rather than swallowing the directive.
#[must_use]
pub fn significant_lines(source: &str) -> Vec<bool> {
    let lexemes = lex(source);
    split_lines(&lexemes)
        .iter()
        .map(|l| !is_blank(l) && !is_comment_only(l))
        .collect()
}

/// The outcome of reading one comment: a directive, or a namespaced token that
/// isn't a usable one.
enum Parsed {
    Directive(DirectiveName, bool, DirectiveArgs),
    Malformed(MalformedReason),
}

/// Read a single comment's text (`; …`, leading `;`s included). Returns the
/// outcome plus the directive token's offset and length **within the comment**,
/// or `None` when the comment is ordinary prose.
fn parse_directive_comment(comment: &str) -> Option<(Parsed, usize, usize)> {
    // Skip the leading `;` run (so `;;`-banner comments carry directives too).
    let after_semis = comment.trim_start_matches(';');
    parse_directive_tail(comment.len() - after_semis.len(), after_semis)
}

/// Read the text after a comment marker of `marker_len` bytes. Shared by the
/// assembler's `;` comments and [`scan_line_comment_directives`], so both
/// languages recognize exactly the same grammar.
fn parse_directive_tail(marker_len: usize, after_marker: &str) -> Option<(Parsed, usize, usize)> {
    let body = after_marker.trim_start();
    let offset = marker_len + (after_marker.len() - body.len());
    if !body.starts_with('@') {
        return None;
    }

    // The token runs to whitespace or a nested `;` (which opens trailing prose).
    let token = body
        .split(|c: char| c.is_whitespace() || c == ';')
        .next()
        .unwrap_or(body);
    // The whole remainder. Stripping trailing prose at a `;` is per-directive:
    // a `@nessemble-param` description is free text and may contain one.
    let args = &body[token.len()..];

    let parsed = match DirectiveName::lookup(token) {
        Some((name, deprecated)) => match parse_args(name, args) {
            Some(args) => Parsed::Directive(name, deprecated, args),
            None => Parsed::Malformed(MalformedReason::BadArgs(name)),
        },
        // Only report tokens inside the namespace; `@todo` and friends are prose.
        None if token.starts_with(DIRECTIVE_NAMESPACE) => {
            Parsed::Malformed(MalformedReason::UnknownName)
        }
        None => return None,
    };
    Some((parsed, offset, token.len()))
}

/// Parse a directive's argument text — everything after the name token, prose
/// included — or `None` if it does not match the directive's syntax.
///
/// Stripping trailing prose at a `;` happens here rather than in the caller,
/// because it is per-directive: a `@nessemble-param` description is free text
/// and a `;` inside it is part of the description.
fn parse_args(name: DirectiveName, args: &str) -> Option<DirectiveArgs> {
    match name {
        DirectiveName::Format => parse_strides(before_prose(args)).map(DirectiveArgs::Strides),
        DirectiveName::CoverageIgnore => match before_prose(args) {
            "start" => Some(DirectiveArgs::Region(RegionBound::Start)),
            "end" => Some(DirectiveArgs::Region(RegionBound::End)),
            _ => None,
        },
        DirectiveName::CoverageIgnoreNextLine => {
            before_prose(args).is_empty().then_some(DirectiveArgs::None)
        }
        DirectiveName::Param | DirectiveName::Returns => parse_slot_and_description(args),
        DirectiveName::Clobbers => parse_slot_list(before_prose(args)).map(DirectiveArgs::Slots),
        DirectiveName::LintIgnoreNextLine => {
            parse_rule_list(before_prose(args)).map(DirectiveArgs::Rules)
        }
        DirectiveName::LintIgnore => parse_region_rules(before_prose(args)),
    }
}

/// Parse `start|end [rule[, rule...]]`.
fn parse_region_rules(args: &str) -> Option<DirectiveArgs> {
    let (bound, rest) = match args.find(char::is_whitespace) {
        Some(i) => (&args[..i], args[i..].trim()),
        None => (args, ""),
    };
    let bound = match bound {
        "start" => RegionBound::Start,
        "end" => RegionBound::End,
        _ => return None,
    };
    parse_rule_list(rest).map(|rules| DirectiveArgs::RegionRules(bound, rules))
}

/// Parse a comma-separated list of lint rule ids. Empty is the blanket form
/// (every rule). An unknown name is `None`, so it reports as bad arguments —
/// the same treatment every other mistyped directive argument gets.
///
/// Names are validated against [`RuleId::ALL`], so the accepted set is the rule
/// registry itself and cannot drift from it.
fn parse_rule_list(args: &str) -> Option<Vec<RuleId>> {
    if args.is_empty() {
        return Some(Vec::new());
    }
    args.split(',')
        .map(|item| RuleId::from_id(item.trim()))
        .collect::<Option<Vec<_>>>()
        .filter(|rules| !rules.is_empty())
}

/// A directive's arguments up to any trailing prose comment, trimmed.
fn before_prose(args: &str) -> &str {
    args.split(';').next().unwrap_or("").trim()
}

/// Parse `<slot> [description]`. The slot is the first whitespace-delimited
/// token; everything after it is the description, verbatim — a `;` in there is
/// prose, not a separator. A leading `-`, `--`, or `:` is stripped, so
/// `A -- index` and `A index` describe identically.
fn parse_slot_and_description(args: &str) -> Option<DirectiveArgs> {
    let trimmed = args.trim();
    let (token, rest) = match trimmed.find(char::is_whitespace) {
        Some(i) => (&trimmed[..i], trimmed[i..].trim()),
        None => (trimmed, ""),
    };
    let slot = parse_slot(token)?;
    let description = rest
        .strip_prefix("--")
        .or_else(|| rest.strip_prefix('-'))
        .or_else(|| rest.strip_prefix(':'))
        .unwrap_or(rest)
        .trim();
    Some(DirectiveArgs::Slot(slot, description.to_string()))
}

/// Parse a `clobbers` argument: a comma-separated slot list, or the keyword
/// `none` (which yields the empty list — an explicit claim, §5). `none` mixed
/// with real slots is bad arguments, not a list containing nothing.
fn parse_slot_list(args: &str) -> Option<Vec<Slot>> {
    if args.is_empty() {
        return None;
    }
    if args.eq_ignore_ascii_case("none") {
        return Some(Vec::new());
    }
    args.split(',')
        .map(|item| parse_slot(item.trim()))
        .collect::<Option<Vec<_>>>()
        .filter(|slots| !slots.is_empty())
}

/// Parse one slot token (§4). Register and flag names are case-insensitive —
/// they are assembly operands, and nessemble accepts `lda` and `LDA` alike —
/// while directive *names* stay case-sensitive registry identifiers.
fn parse_slot(token: &str) -> Option<Slot> {
    if let Some(inner) = token.strip_prefix('[').and_then(|t| t.strip_suffix(']')) {
        // A memory symbol: any non-empty name without brackets or whitespace.
        if inner.is_empty() || inner.contains(['[', ']']) || inner.contains(char::is_whitespace) {
            return None;
        }
        return Some(Slot::Symbol(inner.to_string()));
    }
    if token.starts_with('$') {
        let mut bounds = token.split('-');
        let lo = parse_hex_addr(bounds.next()?)?;
        let hi = match bounds.next() {
            Some(hi) => parse_hex_addr(hi)?,
            None => lo,
        };
        // A third bound, or an inverted range, is a mistake worth reporting.
        if bounds.next().is_some() || hi < lo {
            return None;
        }
        return Some(Slot::Address(lo, hi));
    }
    match token.to_ascii_uppercase().as_str() {
        "A" => Some(Slot::A),
        "X" => Some(Slot::X),
        "Y" => Some(Slot::Y),
        "S" => Some(Slot::S),
        "P" => Some(Slot::P),
        "C" => Some(Slot::Flag(Flag::C)),
        "Z" => Some(Slot::Flag(Flag::Z)),
        "N" => Some(Slot::Flag(Flag::N)),
        "V" => Some(Slot::Flag(Flag::V)),
        "D" => Some(Slot::Flag(Flag::D)),
        "I" => Some(Slot::Flag(Flag::I)),
        _ => None,
    }
}

/// Parse a `$`-prefixed hex address of one to four digits.
fn parse_hex_addr(token: &str) -> Option<u16> {
    let digits = token.trim().strip_prefix('$')?;
    if digits.is_empty() || digits.len() > 4 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    u16::from_str_radix(digits, 16).ok()
}

/// Parse a `stride=N[,N,...]` argument into its stride list. Empty entries are
/// skipped (`stride=2,,3` is `[2, 3]`), matching the original `@fmt` parser.
fn parse_strides(args: &str) -> Option<Vec<usize>> {
    let spec = args.strip_prefix("stride=")?;
    if spec.is_empty() || !spec.bytes().all(|b| b.is_ascii_digit() || b == b',') {
        return None;
    }
    let strides: Vec<usize> = spec
        .split(',')
        .filter(|p| !p.is_empty())
        .map(|p| p.parse().ok())
        .collect::<Option<Vec<_>>>()?;
    if strides.is_empty() {
        None
    } else {
        Some(strides)
    }
}

// ─── Routine signatures ──────────────────────────────────────────────────────
//
// The `@nessemble-param` / `-returns` / `-clobbers` annotations resolved against
// the labels they document, so every consumer (hover, outline, the clobber
// verifier) reads one structure instead of re-deriving it from directives.
//
// See `plans/010-routine-signatures.md`.

/// A routine's declared calling convention, bound to the label below its
/// annotations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    /// The label the annotations bind to.
    pub name: String,
    /// Input slots, in written order, each with its description.
    pub params: Vec<(Slot, String)>,
    /// Output slots, in written order, each with its description.
    pub returns: Vec<(Slot, String)>,
    /// The declared clobber set. Empty when `none` was declared **or** when no
    /// `@nessemble-clobbers` tag was written — see [`declares_clobbers`].
    ///
    /// [`declares_clobbers`]: Signature::declares_clobbers
    pub clobbers: Vec<Slot>,
    /// Whether a `@nessemble-clobbers` tag was written at all. This is the
    /// difference between "provably preserves everything" (`none`, a claim the
    /// verifier checks) and "undeclared" (no claim, never verified).
    pub declares_clobbers: bool,
    /// 1-based line of the label.
    pub line: u32,
    /// 1-based line of the first annotation, for a diagnostic that wants to
    /// point at the block rather than the code.
    pub first_tag_line: u32,
}

impl Signature {
    /// Every slot the routine declares it writes: its clobbers plus its returns,
    /// which are clobbered by definition (§5).
    #[must_use]
    pub fn declared_writes(&self) -> Vec<Slot> {
        let mut out = self.clobbers.clone();
        for (slot, _) in &self.returns {
            if !out.contains(slot) {
                out.push(slot.clone());
            }
        }
        out.sort();
        out
    }
}

/// Why an annotation is not part of a usable signature. These are *binding*
/// problems — the syntax parsed, but the annotation does not describe a routine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureProblemKind {
    /// The annotation binds to no label: code intervenes, or the file ends.
    Unbound,
    /// The same slot is named twice in one role of one signature.
    DuplicateSlot,
    /// The annotation is directly above an `@local` label, which is never a
    /// routine (`plans/021-local-labels.md` §3.8).
    LocalTarget,
}

/// An annotation that parsed but does not contribute to a signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureProblem {
    pub kind: SignatureProblemKind,
    /// The directive token, or the repeated slot, for the message to quote.
    pub subject: String,
    /// The local label a [`SignatureProblemKind::LocalTarget`] annotation is
    /// above (empty for any other kind).
    pub target: String,
    /// 1-based line/column of the annotation.
    pub line: u32,
    pub column: u32,
}

/// Every routine signature in `source`, in source order.
#[must_use]
pub fn resolve_signatures(source: &str) -> Vec<Signature> {
    resolve_signatures_with_errors(source).0
}

/// [`resolve_signatures`], plus the annotations that parsed but bind to nothing
/// or repeat a slot — the input to the `invalid-routine-signature` rule.
#[must_use]
pub fn resolve_signatures_with_errors(source: &str) -> (Vec<Signature>, Vec<SignatureProblem>) {
    let lexemes = lex(source);
    let lines = split_lines(&lexemes);
    let (directives, _) = scan_directive_lines(source, &lines);
    resolve_signature_lines(source, &lines, &directives)
}

/// [`resolve_signatures_with_errors`] over an already-split stream and an
/// already-scanned directive list, so the linter does neither twice.
fn resolve_signature_lines(
    source: &str,
    lines: &[Vec<Lexeme>],
    directives: &[Directive],
) -> (Vec<Signature>, Vec<SignatureProblem>) {
    let mut signatures: Vec<Signature> = Vec::new();
    let mut problems = Vec::new();
    // Label line (0-based) → index into `signatures`, so tags written above one
    // label accumulate into one signature regardless of their order.
    let mut by_label: Vec<(usize, usize)> = Vec::new();

    for directive in directives
        .iter()
        // A trailing directive is inert everywhere (plan 009 §4.2);
        // `ineffective-comment-directive` is what reports it.
        .filter(|d| d.own_line && d.name.is_signature_tag())
    {
        let tag_line = directive.line as usize - 1;
        let (label_idx, name) = match binding_target(source, lines, tag_line) {
            Binding::Label(idx, name) => (idx, name),
            Binding::Local(target) => {
                problems.push(SignatureProblem {
                    kind: SignatureProblemKind::LocalTarget,
                    subject: directive.name.canonical().to_string(),
                    target: target.to_string(),
                    line: directive.line,
                    column: directive.column,
                });
                continue;
            }
            Binding::None => {
                problems.push(SignatureProblem {
                    kind: SignatureProblemKind::Unbound,
                    subject: directive.name.canonical().to_string(),
                    target: String::new(),
                    line: directive.line,
                    column: directive.column,
                });
                continue;
            }
        };

        let existing = by_label
            .iter()
            .find(|(l, _)| *l == label_idx)
            .map(|&(_, i)| i);
        let slot = existing.unwrap_or_else(|| {
            signatures.push(Signature {
                name: name.to_string(),
                params: Vec::new(),
                returns: Vec::new(),
                clobbers: Vec::new(),
                declares_clobbers: false,
                line: label_idx as u32 + 1,
                first_tag_line: directive.line,
            });
            by_label.push((label_idx, signatures.len() - 1));
            signatures.len() - 1
        });
        let sig = &mut signatures[slot];
        sig.first_tag_line = sig.first_tag_line.min(directive.line);

        match (directive.name, &directive.args) {
            (DirectiveName::Param, DirectiveArgs::Slot(slot, desc)) => {
                push_slot(&mut sig.params, slot, desc, directive, &mut problems);
            }
            (DirectiveName::Returns, DirectiveArgs::Slot(slot, desc)) => {
                push_slot(&mut sig.returns, slot, desc, directive, &mut problems);
            }
            (DirectiveName::Clobbers, DirectiveArgs::Slots(slots)) => {
                sig.declares_clobbers = true;
                for slot in slots {
                    if sig.clobbers.contains(slot) {
                        problems.push(SignatureProblem {
                            kind: SignatureProblemKind::DuplicateSlot,
                            subject: slot.to_string(),
                            target: String::new(),
                            line: directive.line,
                            column: directive.column,
                        });
                        continue;
                    }
                    sig.clobbers.push(slot.clone());
                }
            }
            // The scanner pairs each name with its own argument shape, so no
            // other combination can reach here.
            _ => {}
        }
    }

    signatures.sort_by_key(|s| s.line);
    (signatures, problems)
}

/// Record one `param`/`returns` slot, reporting a repeat rather than silently
/// keeping the first or the last.
fn push_slot(
    into: &mut Vec<(Slot, String)>,
    slot: &Slot,
    description: &str,
    directive: &Directive,
    problems: &mut Vec<SignatureProblem>,
) {
    if into.iter().any(|(s, _)| s == slot) {
        problems.push(SignatureProblem {
            kind: SignatureProblemKind::DuplicateSlot,
            subject: slot.to_string(),
            target: String::new(),
            line: directive.line,
            column: directive.column,
        });
        return;
    }
    into.push((slot.clone(), description.to_string()));
}

/// The label an annotation on line `tag_line` (0-based) binds to: scanning down,
/// blank and comment lines are transparent — so prose and blank lines may sit
/// inside an annotation block — and the first line with code must be a label
/// definition. Anything else, or end of file, means the annotation binds to
/// nothing.
///
/// Note this accepts **any** label, not only a block entry: an author who
/// annotates a routine tucked directly under the previous one's `RTS` plainly
/// meant that label. Whether a label *ought* to be documented is a separate
/// question, asked only by `require-routine-doc`.
fn binding_target<'a>(source: &'a str, lines: &[Vec<Lexeme>], tag_line: usize) -> Binding<'a> {
    let Some((idx, line)) = lines
        .iter()
        .enumerate()
        .skip(tag_line + 1)
        .find(|(_, line)| !is_blank(line) && !is_comment_only(line))
    else {
        return Binding::None;
    };
    if let Some((name, _)) = block_label(source, line) {
        return Binding::Label(idx, name);
    }
    match local_label(source, line) {
        Some(name) => Binding::Local(name),
        None => Binding::None,
    }
}

/// What an annotation binds to (see [`binding_target`]).
enum Binding<'a> {
    /// A global label: its 0-based line and name.
    Label(usize, &'a str),
    /// An `@local` label, which annotations never bind to.
    Local(&'a str),
    None,
}

/// If `line` is exactly an `@local` label definition (`@x:`, or a pasted
/// `@x\i:`), return its name as written.
fn local_label<'a>(source: &'a str, line: &[Lexeme]) -> Option<&'a str> {
    let sig: Vec<&Lexeme> = line
        .iter()
        .filter(|l| l.kind != LexKind::Whitespace && l.kind != LexKind::Comment)
        .collect();
    if sig.first().is_none_or(|l| l.kind != LexKind::Ident) {
        return None;
    }
    let n = name_lexemes(source, &sig);
    if sig.len() != n + 1 || !is_punct(source, sig[n], ":") {
        return None;
    }
    let name = &source[sig[0].start..sig[n - 1].end];
    name.starts_with('@').then_some(name)
}

/// The registers the routine named `name` writes but does not declare — what
/// `undeclared-clobber` reports, exposed so an editor can offer to add them
/// rather than making the author retype the list.
///
/// `None` when `name` names no routine, or one that declared no clobber list
/// (which makes no claim to contradict). An empty list means the declaration is
/// accurate.
#[must_use]
pub fn missing_clobbers(source: &str, name: &str) -> Option<Vec<Slot>> {
    let lexemes = lex(source);
    let lines = split_lines(&lexemes);
    let (directives, _) = scan_directive_lines(source, &lines);
    let (signatures, problems) = resolve_signature_lines(source, &lines, &directives);
    let sig = signatures
        .iter()
        .find(|s| s.name == name && s.declares_clobbers)?;

    let ctx = LintCtx {
        source,
        lines: &lines,
        directives: &directives,
        malformed: &[],
        signatures: &signatures,
        signature_problems: &problems,
    };
    let declared = declared_regs(sig);
    let written = routine_effects(&ctx, name).writes;
    let missing = RegSet::from_bits(written.bits() & !declared.bits());
    Some(
        [
            (RegSet::A, Slot::A),
            (RegSet::X, Slot::X),
            (RegSet::Y, Slot::Y),
            (RegSet::S, Slot::S),
        ]
        .into_iter()
        .filter(|&(bit, _)| missing.contains(bit))
        .map(|(_, slot)| slot)
        .collect(),
    )
}

// ─── Linting ─────────────────────────────────────────────────────────────────
//
// A read-only lint pass over the same lossless lexeme stream the formatter uses.
// Unlike `format`, `lint` never rewrites source — it reports findings. It is the
// ESLint to the formatter's Prettier (see `plans/008-linting-rules.md`).

/// A lint rule identifier. The rule registry ([`RULES`]) is keyed by this;
/// adding a rule is a new variant here plus a registry entry and an `id`
/// mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleId {
    /// A block-opening label with no comment within the configured window.
    RequireBlockComment,
    /// A `@nessemble-…` comment that names no known directive, or a known one
    /// with bad arguments.
    UnknownCommentDirective,
    /// A directive spelled with a deprecated alias (`@fmt`).
    DeprecatedCommentDirective,
    /// A well-formed directive that cannot apply where it is written.
    IneffectiveCommentDirective,
    /// A routine-signature annotation that binds to no label, or repeats a slot.
    InvalidRoutineSignature,
    /// A routine that writes a register its `@nessemble-clobbers` omits.
    UndeclaredClobber,
    /// A routine that declares a clobber its body cannot produce.
    OverdeclaredClobber,
    /// A called routine with no signature annotations. Off by default.
    RequireRoutineDoc,
    /// A routine with a clobber list that the whole-program analysis cannot
    /// follow on some path (an indirect jump, …). Program mode only.
    UnverifiableClobbers,
    /// A `@nessemble-lint-ignore…` suppression that no longer suppresses
    /// anything.
    UnusedLintIgnore,
}

impl RuleId {
    /// Every rule, in a stable order (also the [`SeverityMap`] index order).
    pub const ALL: [RuleId; 10] = [
        RuleId::RequireBlockComment,
        RuleId::UnknownCommentDirective,
        RuleId::DeprecatedCommentDirective,
        RuleId::IneffectiveCommentDirective,
        RuleId::InvalidRoutineSignature,
        RuleId::UndeclaredClobber,
        RuleId::OverdeclaredClobber,
        RuleId::RequireRoutineDoc,
        RuleId::UnverifiableClobbers,
        RuleId::UnusedLintIgnore,
    ];

    /// The stable kebab-case identifier used in `.nessemblerc` and in reports.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            RuleId::RequireBlockComment => "require-block-comment",
            RuleId::UnknownCommentDirective => "unknown-comment-directive",
            RuleId::DeprecatedCommentDirective => "deprecated-comment-directive",
            RuleId::IneffectiveCommentDirective => "ineffective-comment-directive",
            RuleId::InvalidRoutineSignature => "invalid-routine-signature",
            RuleId::UndeclaredClobber => "undeclared-clobber",
            RuleId::OverdeclaredClobber => "overdeclared-clobber",
            RuleId::RequireRoutineDoc => "require-routine-doc",
            RuleId::UnverifiableClobbers => "unverifiable-clobbers",
            RuleId::UnusedLintIgnore => "unused-lint-ignore",
        }
    }

    /// Parse a rule id string, returning `None` if it names no known rule.
    #[must_use]
    pub fn from_id(s: &str) -> Option<RuleId> {
        RuleId::ALL.into_iter().find(|r| r.id() == s)
    }

    /// This rule's severity when a project says nothing about it.
    ///
    /// Every rule is [`Warn`](RuleSeverity::Warn) except `require-routine-doc`,
    /// which is [`Off`](RuleSeverity::Off): it would fire on every called label
    /// in an existing project at once, and a rule that is loud on day one gets
    /// switched off permanently rather than adopted
    /// (`plans/010-routine-signatures.md` §13.4).
    #[must_use]
    pub fn default_severity(self) -> RuleSeverity {
        match self {
            RuleId::RequireRoutineDoc => RuleSeverity::Off,
            _ => RuleSeverity::Warn,
        }
    }

    /// This rule's index into a [`SeverityMap`].
    fn index(self) -> usize {
        match self {
            RuleId::RequireBlockComment => 0,
            RuleId::UnknownCommentDirective => 1,
            RuleId::DeprecatedCommentDirective => 2,
            RuleId::IneffectiveCommentDirective => 3,
            RuleId::InvalidRoutineSignature => 4,
            RuleId::UndeclaredClobber => 5,
            RuleId::OverdeclaredClobber => 6,
            RuleId::RequireRoutineDoc => 7,
            RuleId::UnverifiableClobbers => 8,
            RuleId::UnusedLintIgnore => 9,
        }
    }

    /// Whether this rule reads resolved [`Signature`]s, which are shared across
    /// the rules that need them and skipped when none is enabled.
    fn needs_signatures(self) -> bool {
        matches!(
            self,
            RuleId::InvalidRoutineSignature
                | RuleId::UndeclaredClobber
                | RuleId::OverdeclaredClobber
                | RuleId::RequireRoutineDoc
                | RuleId::UnverifiableClobbers
        )
    }

    /// Whether this rule uses the whole-program analysis when a program is
    /// available (`plans/023-whole-program-clobber-analysis.md` §2.1).
    #[must_use]
    pub fn needs_program(self) -> bool {
        matches!(
            self,
            RuleId::UndeclaredClobber
                | RuleId::OverdeclaredClobber
                | RuleId::RequireRoutineDoc
                | RuleId::UnverifiableClobbers
        )
    }
}

/// A rule's configured severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RuleSeverity {
    /// Do not run the rule.
    Off,
    /// Run the rule; findings are advisory warnings (do not fail the run).
    #[default]
    Warn,
    /// Run the rule; findings are errors (fail the run).
    Error,
}

/// Per-rule severities, one slot per [`RuleId`]. Defaults to each rule's
/// [`default_severity`](RuleId::default_severity) — every rule warns out of the
/// box except the opt-in `require-routine-doc`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeverityMap([RuleSeverity; RuleId::ALL.len()]);

impl Default for SeverityMap {
    fn default() -> Self {
        let mut map = SeverityMap([RuleSeverity::Warn; RuleId::ALL.len()]);
        for rule in RuleId::ALL {
            map.set(rule, rule.default_severity());
        }
        map
    }
}

impl SeverityMap {
    /// The severity configured for `rule`.
    #[must_use]
    pub fn get(&self, rule: RuleId) -> RuleSeverity {
        self.0[rule.index()]
    }

    /// Set the severity for `rule`.
    pub fn set(&mut self, rule: RuleId, severity: RuleSeverity) {
        self.0[rule.index()] = severity;
    }
}

/// A single lint finding: which rule fired, where (1-based line/column), the
/// subject (the offending label name or directive token), and a human-readable
/// message. Severity is deliberately absent — core emits raw findings tagged
/// with their [`RuleId`], and the caller maps rule → severity for display, exit
/// codes, and editor squiggles.
///
/// The message is built by the rule (which knows *why* it fired) rather than by
/// each consumer, so the CLI report and the editor say the same thing. It
/// backtick-quotes `subject`, which is how the language server narrows a
/// diagnostic to the offending token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub rule: RuleId,
    pub line: u32,
    pub column: u32,
    pub subject: String,
    pub message: String,
}

/// Configuration for [`lint`]. Plain data plus an `ignore` predicate: the regex
/// compilation backing `ignore` lives in the caller (the `nessemble-rc` config
/// layer), keeping `regex` out of this crate — the same boundary the formatter
/// draws for `serde`.
pub struct LintOptions<'a> {
    /// Per-rule severities; a rule at [`RuleSeverity::Off`] is not run.
    pub severities: SeverityMap,
    /// Comment search radius (lines above/below) for `require-block-comment`.
    pub window: usize,
    /// A label whose name matches is exempt from every rule.
    pub ignore: &'a dyn Fn(&str) -> bool,
}

/// What every rule is handed: the source, its physical lines, and — for the
/// directive rules — the one directive scan and the one signature resolution,
/// shared so the rules that need them do not each redo the work.
struct LintCtx<'a> {
    source: &'a str,
    lines: &'a [Vec<Lexeme>],
    directives: &'a [Directive],
    malformed: &'a [MalformedDirective],
    signatures: &'a [Signature],
    signature_problems: &'a [SignatureProblem],
}

/// A rule implementation: scan the context and push any findings. Runs only
/// when its severity is not [`RuleSeverity::Off`].
type RuleFn = fn(&LintCtx, &LintOptions, &mut Vec<Finding>);

/// The rule registry. Adding a rule is one entry here plus its function.
const RULES: &[(RuleId, RuleFn)] = &[
    (RuleId::RequireBlockComment, rule_require_block_comment),
    (
        RuleId::UnknownCommentDirective,
        rule_unknown_comment_directive,
    ),
    (
        RuleId::DeprecatedCommentDirective,
        rule_deprecated_comment_directive,
    ),
    (
        RuleId::IneffectiveCommentDirective,
        rule_ineffective_comment_directive,
    ),
    (
        RuleId::InvalidRoutineSignature,
        rule_invalid_routine_signature,
    ),
    (RuleId::UndeclaredClobber, rule_undeclared_clobber),
    (RuleId::OverdeclaredClobber, rule_overdeclared_clobber),
    (RuleId::RequireRoutineDoc, rule_require_routine_doc),
];

/// Lint `source`, returning findings sorted by position. Every rule whose
/// severity is not [`RuleSeverity::Off`] is run. This never mutates source.
///
/// Findings suppressed by a `@nessemble-lint-ignore…` directive are filtered out
/// **here**, so a suppressed finding does not exist as far as any caller is
/// concerned: it cannot print, cannot count toward `--max-warnings`, and cannot
/// affect an exit code, whatever its configured severity. Suppression reaches
/// rule findings only — parse and assembly errors never travel this path.
#[must_use]
pub fn lint(source: &str, opts: &LintOptions) -> Vec<Finding> {
    let lexemes = lex(source);
    let lines = split_lines(&lexemes);
    // The directive scan feeds three rules *and* the suppression pass, so it is
    // needed whenever any rule at all can produce a finding.
    let scan_needed = RuleId::ALL
        .into_iter()
        .any(|r| opts.severities.get(r) != RuleSeverity::Off);
    let (directives, malformed) = if scan_needed {
        scan_directive_lines(source, &lines)
    } else {
        (Vec::new(), Vec::new())
    };
    // Likewise the signature resolution, shared by the four routine rules.
    let signatures_needed = RuleId::ALL
        .into_iter()
        .any(|r| r.needs_signatures() && opts.severities.get(r) != RuleSeverity::Off);
    let (signatures, signature_problems) = if signatures_needed {
        resolve_signature_lines(source, &lines, &directives)
    } else {
        (Vec::new(), Vec::new())
    };
    let ctx = LintCtx {
        source,
        lines: &lines,
        directives: &directives,
        malformed: &malformed,
        signatures: &signatures,
        signature_problems: &signature_problems,
    };
    let mut findings = Vec::new();
    for &(rule, run) in RULES {
        if opts.severities.get(rule) != RuleSeverity::Off {
            run(&ctx, opts, &mut findings);
        }
    }
    let mut suppressions = LintSuppressions::resolve(source, &lines, &directives);
    suppressions.apply(
        &mut findings,
        &Judge {
            severities: &opts.severities,
            program_lines: None,
        },
    );
    findings.sort_by_key(|f| (f.line, f.column));
    findings
}

// ─── Program mode ────────────────────────────────────────────────────────────
//
// The routine rules over the assembled program, rather than over text blocks
// (`plans/023-whole-program-clobber-analysis.md`). Text rules are unchanged.

/// One file to lint in program mode.
pub struct ProgramFile<'a> {
    /// The path as reports print it.
    pub path: &'a Path,
    pub source: &'a str,
    /// This file's own lint configuration (per-glob overrides and `ignore`
    /// apply as in file mode).
    pub options: &'a LintOptions<'a>,
    /// Whether the caller reports this file's findings. A file in the include
    /// closure that is not reported still supplies its contracts.
    pub report: bool,
}

/// The result of [`lint_programs`].
#[derive(Debug, Default)]
pub struct ProgramLint {
    /// Findings per file, parallel to the `files` given (empty when not
    /// reported), sorted by position.
    pub findings: Vec<Vec<Finding>>,
    /// Per `(file index, routine name)`, the registers it writes but does not
    /// declare — what "Add `X` to clobbers" offers.
    missing: HashMap<(usize, String), Vec<Slot>>,
    /// The canonical path of each file, parallel to `findings`.
    paths: Vec<PathBuf>,
}

/// The registers the routine `name` in `path` writes but does not declare,
/// according to a [`lint_programs`] run: [`missing_clobbers`] for program
/// mode. `None` when the routine was not analysed.
#[must_use]
pub fn missing_clobbers_in(lint: &ProgramLint, path: &Path, name: &str) -> Option<Vec<Slot>> {
    let canon = canonical(path);
    let file = lint.paths.iter().position(|p| *p == canon)?;
    lint.missing.get(&(file, name.to_string())).cloned()
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// One file's text analysis, shared by every rule.
struct Prepared<'a> {
    source: &'a str,
    lines: Vec<Vec<Lexeme>>,
    directives: Vec<Directive>,
    malformed: Vec<MalformedDirective>,
    signatures: Vec<Signature>,
    problems: Vec<SignatureProblem>,
    /// 1-based lines of the block-opening labels, ascending.
    block_starts: Vec<u32>,
}

impl<'a> Prepared<'a> {
    fn new(source: &'a str) -> Prepared<'a> {
        let lexemes = lex(source);
        let lines = split_lines(&lexemes);
        let (directives, malformed) = scan_directive_lines(source, &lines);
        let (signatures, problems) = resolve_signature_lines(source, &lines, &directives);
        let block_starts = lines
            .iter()
            .enumerate()
            .filter(|(idx, line)| {
                block_label(source, line).is_some() && is_block_entry(source, &lines, *idx)
            })
            .map(|(idx, _)| idx as u32 + 1)
            .collect();
        Prepared {
            source,
            lines,
            directives,
            malformed,
            signatures,
            problems,
            block_starts,
        }
    }

    fn ctx(&self) -> LintCtx<'_> {
        LintCtx {
            source: self.source,
            lines: &self.lines,
            directives: &self.directives,
            malformed: &self.malformed,
            signatures: &self.signatures,
            signature_problems: &self.problems,
        }
    }

    /// The text block holding 1-based `line`: its opening label's line.
    fn block_of(&self, line: u32) -> Option<u32> {
        self.block_starts
            .iter()
            .rev()
            .find(|&&b| b <= line)
            .copied()
    }
}

/// One routine's merged result across every program it is assembled in.
#[derive(Default)]
struct Merged {
    /// Registers written but not declared (union), with each one's origin.
    undeclared: RegSet,
    origins: [Option<String>; 4],
    /// Declared clobbers never written, in every program (intersection); `None`
    /// before the first program.
    extra: Option<RegSet>,
    /// A save that explains an extra register, per register.
    preserved: [Option<String>; 4],
    /// Unverifiable reasons, rendered (union).
    reasons: Vec<String>,
    /// The editor did not run a pseudo-op this routine reaches.
    editor: bool,
}

/// Lint `files` in program mode against the assembled `programs`
/// (`plans/023-whole-program-clobber-analysis.md` §2.6). Text rules run per
/// file exactly as [`lint`] runs them; the routine rules run once over the
/// programs, and a file in several programs gets the merge of §2.1.
#[must_use]
pub fn lint_programs(programs: &[ProgramMap], files: &[ProgramFile]) -> ProgramLint {
    let prepared: Vec<Prepared> = files.iter().map(|f| Prepared::new(f.source)).collect();
    let paths: Vec<PathBuf> = files.iter().map(|f| canonical(f.path)).collect();
    let mut merged: HashMap<(usize, u32), Merged> = HashMap::new();
    let mut called: HashSet<(usize, u32)> = HashSet::new();
    // Per file, the label lines some program assembled: where the program
    // rules ran (for judging suppressions).
    let mut assembled: HashMap<usize, HashSet<u32>> = HashMap::new();

    for program in programs {
        analyse_program(
            program,
            files,
            &prepared,
            &paths,
            &mut merged,
            &mut called,
            &mut assembled,
        );
    }
    // A routine the editor could not fully model is not judged.
    for (&(fi, line), m) in &merged {
        if m.editor {
            if let Some(lines) = assembled.get_mut(&fi) {
                lines.remove(&line);
            }
        }
    }

    let mut out = ProgramLint {
        findings: Vec::with_capacity(files.len()),
        missing: HashMap::new(),
        paths,
    };
    for (fi, (file, prep)) in files.iter().zip(&prepared).enumerate() {
        for sig in prep.signatures.iter().filter(|s| s.declares_clobbers) {
            if let Some(m) = merged.get(&(fi, sig.line)) {
                out.missing
                    .insert((fi, sig.name.clone()), regs_to_slots(m.undeclared));
            }
        }
        if !file.report {
            out.findings.push(Vec::new());
            continue;
        }
        let opts = file.options;
        let ctx = prep.ctx();
        let mut findings = Vec::new();
        for &(rule, run) in RULES {
            if !rule.needs_program() && opts.severities.get(rule) != RuleSeverity::Off {
                run(&ctx, opts, &mut findings);
            }
        }
        program_findings(fi, prep, opts, &merged, &called, &mut findings);
        let mut suppressions =
            LintSuppressions::resolve(prep.source, &prep.lines, &prep.directives);
        let assembled = assembled.get(&fi).cloned().unwrap_or_default();
        suppressions.apply(
            &mut findings,
            &Judge {
                severities: &opts.severities,
                program_lines: Some(&assembled),
            },
        );
        findings.sort_by_key(|f| (f.line, f.column));
        out.findings.push(findings);
    }
    out
}

/// The routine-rule findings for file `fi` from the merged analysis.
fn program_findings(
    fi: usize,
    prep: &Prepared,
    opts: &LintOptions,
    merged: &HashMap<(usize, u32), Merged>,
    called: &HashSet<(usize, u32)>,
    out: &mut Vec<Finding>,
) {
    let ctx = prep.ctx();
    let on = |rule| opts.severities.get(rule) != RuleSeverity::Off;
    for sig in prep.signatures.iter().filter(|s| s.declares_clobbers) {
        if (opts.ignore)(&sig.name) {
            continue;
        }
        let Some(m) = merged.get(&(fi, sig.line)) else {
            continue;
        };
        let column = label_column(&ctx, sig.line);
        let name = &sig.name;
        if on(RuleId::UndeclaredClobber) && !m.undeclared.is_empty() {
            let details: Vec<String> = (0..4)
                .filter(|&r| m.undeclared.contains(crate::clobbers::reg_set(r)))
                .filter_map(|r| {
                    m.origins[r]
                        .as_ref()
                        .map(|o| format!("{}: {o}", crate::clobbers::reg_name(r)))
                })
                .collect();
            out.push(Finding {
                rule: RuleId::UndeclaredClobber,
                line: sig.line,
                column,
                subject: name.clone(),
                message: format!(
                    "routine `{name}` writes {} but does not declare {} clobbered ({})",
                    reg_list(m.undeclared),
                    it_or_them(m.undeclared),
                    details.join("; ")
                ),
            });
        }
        let extra = m.extra.unwrap_or(RegSet::EMPTY);
        if on(RuleId::OverdeclaredClobber) && !extra.is_empty() {
            let mut message = format!(
                "routine `{name}` declares {} clobbered but never writes {}",
                reg_list(extra),
                it_or_them(extra),
            );
            if extra.bits().is_power_of_two() {
                let reg = extra.bits().trailing_zeros() as usize;
                if let Some(p) = &m.preserved[reg] {
                    message = format!("{message} ({p})");
                }
            }
            out.push(Finding {
                rule: RuleId::OverdeclaredClobber,
                line: sig.line,
                column,
                subject: name.clone(),
                message,
            });
        }
        if on(RuleId::UnverifiableClobbers) && !m.reasons.is_empty() {
            let more = match m.reasons.len() - 1 {
                0 => String::new(),
                n => format!(" (and {n} more)"),
            };
            out.push(Finding {
                rule: RuleId::UnverifiableClobbers,
                line: sig.line,
                column,
                subject: name.clone(),
                message: format!(
                    "routine `{name}` cannot be fully verified: {}{more}",
                    m.reasons[0]
                ),
            });
        }
    }
    if on(RuleId::RequireRoutineDoc) {
        for &line in &prep.block_starts {
            if !called.contains(&(fi, line)) {
                continue;
            }
            let Some((name, column)) = block_label(prep.source, &prep.lines[line as usize - 1])
            else {
                continue;
            };
            if (opts.ignore)(name) || prep.signatures.iter().any(|s| s.name == name) {
                continue;
            }
            out.push(Finding {
                rule: RuleId::RequireRoutineDoc,
                line,
                column,
                subject: name.to_string(),
                message: format!("called routine `{name}` has no signature annotations"),
            });
        }
    }
}

/// The verifier registers in `regs`, as slots in canonical order.
fn regs_to_slots(regs: RegSet) -> Vec<Slot> {
    [
        (RegSet::A, Slot::A),
        (RegSet::X, Slot::X),
        (RegSet::Y, Slot::Y),
        (RegSet::S, Slot::S),
    ]
    .into_iter()
    .filter(|&(bit, _)| regs.contains(bit))
    .map(|(_, slot)| slot)
    .collect()
}

/// Run the analysis over one program and merge its results.
fn analyse_program(
    program: &ProgramMap,
    files: &[ProgramFile],
    prepared: &[Prepared],
    paths: &[PathBuf],
    merged: &mut HashMap<(usize, u32), Merged>,
    called: &mut HashSet<(usize, u32)>,
    assembled: &mut HashMap<usize, HashSet<u32>>,
) {
    use crate::clobbers::{analyze, Contract, Contracts, WalkOptions};

    // Program file id → index into `files`.
    let file_of: Vec<Option<usize>> = program
        .files
        .iter()
        .map(|p| {
            let c = canonical(p);
            paths.iter().position(|q| *q == c)
        })
        .collect();
    let symbols: HashMap<&str, i64> = program
        .symbols
        .iter()
        .map(|(n, v)| (n.as_str(), *v))
        .collect();

    let mut contracts = Contracts::default();
    // Contracted labels to walk: (rom offset, file, signature line).
    let mut routines: Vec<(usize, usize, u32)> = Vec::new();
    for def in program
        .labels
        .iter()
        .filter(|l| !l.in_expansion && !l.local)
    {
        let Some(fi) = file_of.get(def.site.file as usize).copied().flatten() else {
            continue;
        };
        assembled.entry(fi).or_default().insert(def.site.line);
        let Some(sig) = prepared[fi]
            .signatures
            .iter()
            .find(|s| s.line == def.site.line && s.name == def.name && s.declares_clobbers)
        else {
            continue;
        };
        let memory = sig
            .clobbers
            .iter()
            .filter_map(|slot| match slot {
                Slot::Address(lo, hi) => Some((*lo, *hi)),
                Slot::Symbol(name) => symbols
                    .get(name.as_str())
                    .and_then(|&v| u16::try_from(v).ok().map(|v| (v, v))),
                _ => None,
            })
            .collect();
        contracts.insert(
            def.rom_offset,
            Contract {
                name: sig.name.clone(),
                writes: declared_regs(sig),
                memory,
                site: def.site,
            },
        );
        routines.push((def.rom_offset, fi, sig.line));
    }

    let mut entries: Vec<usize> = routines.iter().map(|r| r.0).collect();
    entries.sort_unstable();
    entries.dedup();
    let effects = analyze(
        program,
        &contracts,
        &entries,
        WalkOptions { memory_saves: true },
    );

    let name_of = |file: u32| -> String {
        match file_of.get(file as usize).copied().flatten() {
            Some(fi) => files[fi].path.display().to_string(),
            None => program
                .files
                .get(file as usize)
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
        }
    };

    // Each routine: the union over its definitions in this program.
    let mut per_routine: HashMap<(usize, u32), crate::clobbers::RoutineEffect> = HashMap::new();
    for &(off, fi, line) in &routines {
        if let Some(effect) = effects.get(&off) {
            per_routine.entry((fi, line)).or_default().join(effect);
        }
    }
    for ((fi, line), effect) in per_routine {
        let Some(sig) = prepared[fi].signatures.iter().find(|s| s.line == line) else {
            continue;
        };
        let own = files[fi].path.display().to_string();
        let render = Render {
            own: &own,
            name_of: &name_of,
        };
        let declared = declared_regs(sig);
        let undeclared = RegSet::from_bits(effect.writes.bits() & !declared.bits());
        let mut reasons: Vec<String> = effect
            .reasons
            .iter()
            .filter(|r| !matches!(r.kind, crate::clobbers::ReasonKind::EditorNotRun(_)))
            .map(|r| render.reason(r))
            .collect();
        if let Some(pull) = &effect.pulls_return {
            if pull.text == "PLA" || pull.text == "PLP" {
                reasons.push(format!(
                    "`{}` on {} pulls the routine's own return address",
                    pull.text,
                    render.at(pull.site)
                ));
            }
        }
        let verifiable = reasons.is_empty() && !effect.editor_not_run();
        let clobbers = slots_to_regs(&sig.clobbers);
        let extra = if verifiable {
            RegSet::from_bits(clobbers.bits() & !effect.writes.bits())
        } else {
            RegSet::EMPTY
        };

        let m = merged.entry((fi, line)).or_default();
        for reg in 0..4 {
            let bit = crate::clobbers::reg_set(reg);
            if undeclared.contains(bit) && !m.undeclared.contains(bit) {
                m.origins[reg] = effect.origins[reg]
                    .as_ref()
                    .map(|o| render.origin(o, crate::clobbers::reg_name(reg)));
            }
            if m.preserved[reg].is_none() {
                if let Some(p) = effect.preserved.iter().find(|p| p.reg == reg) {
                    m.preserved[reg] = Some(format!(
                        "it is saved on {} and restored on {}",
                        render.at(p.saved),
                        render.at(p.restored)
                    ));
                }
            }
        }
        m.undeclared = m.undeclared.union(undeclared);
        m.extra = Some(match m.extra {
            None => extra,
            Some(prev) => RegSet::from_bits(prev.bits() & extra.bits()),
        });
        for r in reasons {
            if !m.reasons.contains(&r) {
                m.reasons.push(r);
            }
        }
        m.editor |= effect.editor_not_run();
    }

    // `require-routine-doc`: what a `JSR` anywhere, or a tail `JMP`, reaches.
    for emit in &program.emits {
        let EmitKind::Instruction {
            target: Some(target),
        } = &emit.kind
        else {
            continue;
        };
        let op = program.image.get(emit.rom_offset).copied();
        if op != Some(0x20) && op != Some(0x4C) {
            continue;
        }
        let Resolution::At(t) = program.resolve(target, emit.bank) else {
            continue;
        };
        for def in program.labels_at(t).filter(|l| !l.in_expansion && !l.local) {
            let Some(fi) = file_of.get(def.site.file as usize).copied().flatten() else {
                continue;
            };
            if op == Some(0x4C) {
                // A loop back to the top of the same text block is not a call.
                let same_file = file_of.get(emit.site.file as usize).copied().flatten() == Some(fi);
                if same_file && prepared[fi].block_of(emit.site.line) == Some(def.site.line) {
                    continue;
                }
            }
            called.insert((fi, def.site.line));
        }
    }
}

/// Message rendering for one routine: lines in the routine's own file are
/// bare, others name their file.
struct Render<'a> {
    own: &'a str,
    name_of: &'a dyn Fn(u32) -> String,
}

impl Render<'_> {
    /// "line 14", or "line 14 of \`util.asm\`" for another file.
    fn at(&self, site: Site) -> String {
        let file = (self.name_of)(site.file);
        if file == self.own {
            format!("line {}", site.line)
        } else {
            format!("line {} of `{file}`", site.line)
        }
    }

    fn step(&self, step: &crate::clobbers::Step) -> String {
        format!("`{}` on {}", step.text, self.at(step.site))
    }

    fn origin(&self, origin: &crate::clobbers::WriteOrigin, reg: &str) -> String {
        use crate::clobbers::OriginBase;
        let base = match &origin.base {
            OriginBase::Instr {
                mnemonic,
                site,
                via,
                data,
            } => match (data, via) {
                (Some(directive), _) => {
                    format!(
                        "hand-encoded `{mnemonic}` in `{directive}` on {}",
                        self.at(*site)
                    )
                }
                (None, Some(Via::Macro(name))) => {
                    format!("`{mnemonic}` in `.macro {name}` on {}", self.at(*site))
                }
                (None, Some(Via::Pseudo(name))) => {
                    format!("`{mnemonic}` in `.{name}` on {}", self.at(*site))
                }
                (None, None) => format!("`{mnemonic}` on {}", self.at(*site)),
            },
            OriginBase::Contract { how, name, site } => match how {
                Some(step) => format!("{}, whose contract clobbers {reg}", self.step(step)),
                None => format!(
                    "falls through into `{name}` on {}, whose contract clobbers {reg}",
                    self.at(*site)
                ),
            },
        };
        match &origin.through {
            Some(step) => format!("{base}, reached through {}", self.step(step)),
            None => base,
        }
    }

    fn reason(&self, reason: &crate::clobbers::Reason) -> String {
        use crate::clobbers::ReasonKind;
        let text = match &reason.kind {
            ReasonKind::IndirectJmp(step) => format!("indirect {} ends a path", self.step(step)),
            ReasonKind::ReturnThroughPushed(step) => format!(
                "{} returns through values the routine pushed (a computed jump)",
                self.step(step)
            ),
            ReasonKind::ReturnAfterTxs { ret, txs } => format!(
                "{} follows `TXS` on {}, so its return address is unknown",
                self.step(ret),
                self.at(*txs)
            ),
            ReasonKind::PullsReturn { call, callee } => format!(
                "{} enters `{callee}`, which pulls its own return address",
                self.step(call)
            ),
            ReasonKind::SelfPullsReturn(step) => {
                format!("{} pulls the routine's own return address", self.step(step))
            }
            ReasonKind::Ambiguous { from, addr, banks } => {
                let where_ = if banks.len() == 1 {
                    format!("which bank {} assembles code at more than once", banks[0])
                } else {
                    let names: Vec<String> = banks.iter().map(u16::to_string).collect();
                    let (last, rest) = names.split_last().expect("two or more");
                    let all = if banks.len() == 2 { "both" } else { "all" };
                    format!(
                        "which banks {} and {last} {all} assemble code at",
                        rest.join(", ")
                    )
                };
                format!("{} targets ${addr:04X}, {where_}", self.step(from))
            }
            ReasonKind::Nothing { from, addr } => format!(
                "{} targets ${addr:04X}, where nothing was assembled",
                self.step(from)
            ),
            ReasonKind::UnknownOpcode { site, byte, data } => {
                if *data {
                    format!(
                        "the path reaches data on {} that is not a known instruction (`${byte:02X}`)",
                        self.at(*site)
                    )
                } else {
                    format!(
                        "the path reaches `${byte:02X}` on {}, which is not a known instruction",
                        self.at(*site)
                    )
                }
            }
            ReasonKind::ReturnsIntoData { call, data } => {
                format!(
                    "{} returns into data on {}",
                    self.step(call),
                    self.at(*data)
                )
            }
            ReasonKind::RunsPast { bank, after } => match after {
                Some(site) => format!(
                    "the path runs past the last byte assembled in bank {bank} after {}",
                    self.at(*site)
                ),
                None => format!("the path runs past the last byte assembled in bank {bank}"),
            },
            ReasonKind::EditorNotRun(site) => {
                format!("the editor did not run the pseudo-op on {}", self.at(*site))
            }
            ReasonKind::TooComplex => "it has too many paths to follow".to_string(),
        };
        match &reason.through {
            Some(step) => format!("through {}, {text}", self.step(step)),
            None => text,
        }
    }
}

/// One `@nessemble-lint-ignore…` suppression: the lines it covers, the rules it
/// silences (empty: every rule), and its directive, for `unused-lint-ignore`.
#[derive(Debug)]
struct Suppression {
    /// Inclusive line range (`last == u32::MAX`: to the end of the file).
    first: u32,
    last: u32,
    rules: Vec<RuleId>,
    /// The directive's line and `@` column.
    line: u32,
    column: u32,
    /// The directive as messages quote it (`@nessemble-lint-ignore start`).
    token: String,
    /// What kind of suppression it is.
    extent: Extent,
    /// The rules it silenced a finding of (`None` in a bare suppression's
    /// list stands for "something").
    used: Vec<RuleId>,
    used_any: bool,
}

/// The shape of a [`Suppression`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Extent {
    /// `-next-line`.
    NextLine,
    /// A `start` … `end` region.
    Region,
    /// A `start` with no `end`: to the end of the file.
    OpenRegion,
}

/// The `@nessemble-lint-ignore…` suppressions in one file.
///
/// Matching is **by the line a finding is reported at**, which is what lets one
/// mechanism cover both line-anchored rules (`require-block-comment`, reported at
/// the label) and routine-level ones (`undeclared-clobber` and friends, also
/// reported at the label) with no special cases. Note that
/// `invalid-routine-signature` is reported at the *annotation*, not the label, so
/// only the region form can reach it — see the docs.
#[derive(Debug, Default)]
struct LintSuppressions {
    entries: Vec<Suppression>,
}

/// Where each rule ran with full information in one file, which decides
/// whether a suppression naming it can be judged unused
/// (`plans/023-whole-program-clobber-analysis.md` §2.7).
struct Judge<'a> {
    severities: &'a SeverityMap,
    /// Program mode: the lines of the labels the program assembled (where the
    /// program rules ran), minus those whose analysis the editor could not
    /// complete. `None` in file mode, where program rules are not judged.
    program_lines: Option<&'a HashSet<u32>>,
}

impl Judge<'_> {
    /// Whether `rule` ran with full information somewhere in `first..=last`.
    fn ran(&self, rule: RuleId, first: u32, last: u32) -> bool {
        if self.severities.get(rule) == RuleSeverity::Off || rule == RuleId::UnusedLintIgnore {
            return false;
        }
        if !rule.needs_program() {
            return true;
        }
        self.program_lines
            .is_some_and(|lines| lines.iter().any(|l| (first..=last).contains(l)))
    }
}

impl LintSuppressions {
    /// Resolve one file's directives into suppression ranges, mirroring
    /// [`crate::coverage::resolve_ignores`]: `-next-line` targets the next
    /// **significant** line (so the directive may sit above a whole annotation
    /// block and still land on the label), `start` opens a region and `end`
    /// closes it, an unclosed region runs to end of file, regions do not nest,
    /// and a trailing-comment directive is inert.
    fn resolve(source: &str, lines: &[Vec<Lexeme>], directives: &[Directive]) -> LintSuppressions {
        let mut out = LintSuppressions::default();
        let mut region: Option<&Directive> = None;
        let entry = |d: &Directive, first: u32, last: u32, rules: &[RuleId], extent| Suppression {
            first,
            last,
            rules: rules.to_vec(),
            line: d.line,
            column: d.column,
            token: source[d.start..d.end].to_string(),
            extent,
            used: Vec::new(),
            used_any: false,
        };
        for d in directives.iter().filter(|d| d.own_line) {
            match (d.name, &d.args) {
                (DirectiveName::LintIgnoreNextLine, DirectiveArgs::Rules(rules)) => {
                    if let Some(target) = significant_line_after(lines, d.line) {
                        let t = target as u32 + 1;
                        out.entries.push(entry(d, t, t, rules, Extent::NextLine));
                    }
                }
                (DirectiveName::LintIgnore, DirectiveArgs::RegionRules(bound, _)) => match bound {
                    RegionBound::Start => {
                        if region.is_none() {
                            region = Some(d);
                        }
                    }
                    RegionBound::End => {
                        if let Some(start) = region.take() {
                            if let DirectiveArgs::RegionRules(_, rules) = &start.args {
                                let mut e = entry(start, start.line, d.line, rules, Extent::Region);
                                e.token = format!("{} start", e.token);
                                out.entries.push(e);
                            }
                        }
                    }
                },
                _ => {}
            }
        }
        // An unclosed region runs to the end of the file.
        if let Some(start) = region {
            if let DirectiveArgs::RegionRules(_, rules) = &start.args {
                let mut e = entry(start, start.line, u32::MAX, rules, Extent::OpenRegion);
                e.token = format!("{} start", e.token);
                out.entries.push(e);
            }
        }
        out
    }

    /// Whether `rule` is suppressed at 1-based `line`, recording which
    /// suppressions silenced it. `skip` is a suppression that may not silence
    /// this finding (the one the finding is about).
    fn consume(&mut self, rule: RuleId, line: u32, skip: Option<usize>) -> bool {
        let mut hit = false;
        for (i, s) in self.entries.iter_mut().enumerate() {
            if Some(i) == skip || !(s.first..=s.last).contains(&line) {
                continue;
            }
            if s.rules.is_empty() || s.rules.contains(&rule) {
                hit = true;
                s.used_any = true;
                if !s.used.contains(&rule) {
                    s.used.push(rule);
                }
            }
        }
        hit
    }

    /// Filter `findings` through the suppressions, then add an
    /// `unused-lint-ignore` finding for each suppression (or each named rule of
    /// one) that silenced nothing where its rule ran (§2.7).
    fn apply(&mut self, findings: &mut Vec<Finding>, judge: &Judge) {
        findings.retain(|f| !self.consume(f.rule, f.line, None));
        let on = judge.severities.get(RuleId::UnusedLintIgnore) != RuleSeverity::Off;
        if !on {
            return;
        }
        // (suppression, unused rules; empty for a bare one that silenced
        // nothing)
        let mut candidates: Vec<(usize, Vec<RuleId>)> = Vec::new();
        for (i, s) in self.entries.iter().enumerate() {
            let last = s.last;
            if s.rules.is_empty() {
                // A bare suppression is judged only where every rule ran.
                let all_ran = RuleId::ALL.into_iter().all(|r| {
                    r == RuleId::UnusedLintIgnore
                        || judge.severities.get(r) == RuleSeverity::Off
                        || judge.ran(r, s.first, last)
                });
                if all_ran && !s.used_any {
                    candidates.push((i, Vec::new()));
                }
                continue;
            }
            let mut unused: Vec<RuleId> = s
                .rules
                .iter()
                .copied()
                .filter(|&r| judge.ran(r, s.first, last) && !s.used.contains(&r))
                .collect();
            unused.sort_by_key(|r| r.id());
            unused.dedup();
            if !unused.is_empty() {
                candidates.push((i, unused));
            }
        }
        // One pass: a finding about a suppression may itself be suppressed,
        // never by the suppression it is about.
        let mut kept: Vec<(usize, Vec<RuleId>)> = Vec::new();
        for (i, rules) in candidates {
            let line = self.entries[i].line;
            if !self.consume(RuleId::UnusedLintIgnore, line, Some(i)) {
                kept.push((i, rules));
            }
        }
        // A suppression naming `unused-lint-ignore` that silenced none.
        for (i, s) in self.entries.iter().enumerate() {
            if s.rules.contains(&RuleId::UnusedLintIgnore)
                && !s.used.contains(&RuleId::UnusedLintIgnore)
            {
                match kept.iter_mut().find(|(k, _)| *k == i) {
                    Some((_, rules)) => rules.push(RuleId::UnusedLintIgnore),
                    None => kept.push((i, vec![RuleId::UnusedLintIgnore])),
                }
            }
        }
        for (i, mut rules) in kept {
            let s = &self.entries[i];
            rules.sort_by_key(|r| r.id());
            let lines = match s.extent {
                Extent::NextLine => format!("line {}", s.first),
                Extent::Region => format!("lines {}\u{2013}{}", s.first, s.last),
                Extent::OpenRegion => format!("lines {} to the end of the file", s.first),
            };
            let message = if rules.is_empty() {
                format!("`{}` suppresses nothing on {lines}", s.token)
            } else {
                let names: Vec<String> = rules.iter().map(|r| format!("`{}`", r.id())).collect();
                let verb = if rules.len() == 1 {
                    "reports"
                } else {
                    "report"
                };
                format!(
                    "`{}` suppresses {}, which {verb} nothing on {lines}",
                    s.token,
                    names.join(", ")
                )
            };
            findings.push(Finding {
                rule: RuleId::UnusedLintIgnore,
                line: s.line,
                column: s.column,
                subject: s.token.clone(),
                message,
            });
        }
    }
}

/// `require-block-comment`: warn on a block-opening label with no comment within
/// `±window` lines. Internal branch-target labels (a label directly after code)
/// and anonymous labels are never flagged; a name matching `ignore` is skipped.
fn rule_require_block_comment(ctx: &LintCtx, opts: &LintOptions, out: &mut Vec<Finding>) {
    for (idx, line) in ctx.lines.iter().enumerate() {
        let Some((name, column)) = block_label(ctx.source, line) else {
            continue;
        };
        if (opts.ignore)(name) {
            continue;
        }
        if !is_block_entry(ctx.source, ctx.lines, idx) {
            continue;
        }
        if has_comment_within(ctx.lines, idx, opts.window) {
            continue;
        }
        out.push(Finding {
            rule: RuleId::RequireBlockComment,
            line: (idx + 1) as u32,
            column,
            subject: name.to_string(),
            message: format!("code block `{name}` has no nearby comment"),
        });
    }
}

/// `unknown-comment-directive`: a comment addressed to nessemble that names no
/// known directive, or names one but gets its arguments wrong. Scoped to the
/// `@nessemble-` namespace (plus the `@fmt` alias), so ordinary `@todo`-style
/// prose is never flagged.
fn rule_unknown_comment_directive(ctx: &LintCtx, _opts: &LintOptions, out: &mut Vec<Finding>) {
    for bad in ctx.malformed {
        let token = &ctx.source[bad.start..bad.end];
        let message = match bad.reason {
            MalformedReason::UnknownName => {
                format!("unknown comment directive `{token}`")
            }
            MalformedReason::BadArgs(name) => match name.arg_syntax() {
                "" => format!("comment directive `{token}` takes no arguments"),
                syntax => format!("comment directive `{token}` expects `{syntax}`"),
            },
        };
        out.push(Finding {
            rule: RuleId::UnknownCommentDirective,
            line: bad.line,
            column: bad.column,
            subject: token.to_string(),
            message,
        });
    }
}

/// `deprecated-comment-directive`: a directive written with a legacy alias
/// (`@fmt`). The directive still works — this points at the canonical spelling.
fn rule_deprecated_comment_directive(ctx: &LintCtx, _opts: &LintOptions, out: &mut Vec<Finding>) {
    for d in ctx.directives.iter().filter(|d| d.deprecated) {
        let token = &ctx.source[d.start..d.end];
        out.push(Finding {
            rule: RuleId::DeprecatedCommentDirective,
            line: d.line,
            column: d.column,
            subject: token.to_string(),
            message: format!(
                "comment directive `{token}` is deprecated; use `{}`",
                d.name.canonical()
            ),
        });
    }
}

/// `ineffective-comment-directive`: a well-formed directive that cannot apply
/// where it is written — the quiet failure the namespace exists to surface.
///
/// An **unclosed** ignore region is deliberately not flagged: running to end of
/// file is the documented way to exclude a whole file.
fn rule_ineffective_comment_directive(ctx: &LintCtx, _opts: &LintOptions, out: &mut Vec<Finding>) {
    // The coverage and lint ignore regions nest independently of one another.
    let mut region_open = false;
    let mut lint_region_open = false;
    for d in ctx.directives {
        let token = &ctx.source[d.start..d.end];
        let reason = if d.own_line {
            match (d.name, &d.args) {
                (DirectiveName::CoverageIgnoreNextLine | DirectiveName::LintIgnoreNextLine, _) => {
                    significant_line_after(ctx.lines, d.line)
                        .is_none()
                        .then(|| "has no following line to ignore".to_string())
                }
                (DirectiveName::Format, _) => {
                    // The formatter's own lookahead: effective here means the
                    // formatter will actually re-flow that run.
                    let following = ctx.lines[d.line as usize..]
                        .iter()
                        .map(|l| line_text(ctx.source, l));
                    format_directive_target(following).is_none().then(|| {
                        "is not followed by a data line (`.db`, `.dw`, `.color`)".to_string()
                    })
                }
                (DirectiveName::CoverageIgnore, DirectiveArgs::Region(bound)) => {
                    let ineffective = match bound {
                        RegionBound::Start => region_open
                            .then(|| "is already inside an open ignore region".to_string()),
                        RegionBound::End => {
                            (!region_open).then(|| "has no matching `start`".to_string())
                        }
                    };
                    region_open = *bound == RegionBound::Start;
                    ineffective
                }
                (DirectiveName::LintIgnore, DirectiveArgs::RegionRules(bound, _)) => {
                    let ineffective = match bound {
                        RegionBound::Start => lint_region_open
                            .then(|| "is already inside an open ignore region".to_string()),
                        RegionBound::End => {
                            (!lint_region_open).then(|| "has no matching `start`".to_string())
                        }
                    };
                    lint_region_open = *bound == RegionBound::Start;
                    ineffective
                }
                _ => None,
            }
        } else {
            Some("has no effect in a trailing comment (put it on its own line)".to_string())
        };
        if let Some(reason) = reason {
            out.push(Finding {
                rule: RuleId::IneffectiveCommentDirective,
                line: d.line,
                column: d.column,
                subject: token.to_string(),
                message: format!("comment directive `{token}` {reason}"),
            });
        }
    }
}

/// `invalid-routine-signature`: an annotation that parsed but does not describe
/// a routine — it binds to no label, or repeats a slot the signature already
/// names. Argument *syntax* errors are `unknown-comment-directive`'s job; these
/// are the mistakes only binding can reveal.
fn rule_invalid_routine_signature(ctx: &LintCtx, _opts: &LintOptions, out: &mut Vec<Finding>) {
    for problem in ctx.signature_problems {
        let subject = &problem.subject;
        let message = match problem.kind {
            SignatureProblemKind::Unbound => {
                format!("routine annotation `{subject}` is not followed by a label")
            }
            SignatureProblemKind::DuplicateSlot => {
                format!("routine annotation names `{subject}` twice")
            }
            SignatureProblemKind::LocalTarget => format!(
                "routine annotation `{subject}` is followed by local label `{}`; \
                 annotations bind only to global labels",
                problem.target
            ),
        };
        out.push(Finding {
            rule: RuleId::InvalidRoutineSignature,
            line: problem.line,
            column: problem.column,
            subject: subject.clone(),
            message,
        });
    }
}

/// `undeclared-clobber`: a routine whose body definitely writes a register its
/// `@nessemble-clobbers` does not list.
///
/// Only routines that **declared** a clobber list are checked — an undeclared
/// routine makes no claim, so there is nothing to contradict. Needs one definite
/// write, so unknown callees and macros cannot suppress it; they can only hide
/// writes the analysis never saw (§8.2).
fn rule_undeclared_clobber(ctx: &LintCtx, opts: &LintOptions, out: &mut Vec<Finding>) {
    for sig in ctx.signatures.iter().filter(|s| s.declares_clobbers) {
        if (opts.ignore)(&sig.name) {
            continue;
        }
        let declared = declared_regs(sig);
        let effects = routine_effects(ctx, &sig.name);
        let missing = RegSet::from_bits(effects.writes.bits() & !declared.bits());
        if missing.is_empty() {
            continue;
        }
        out.push(Finding {
            rule: RuleId::UndeclaredClobber,
            line: sig.line,
            column: label_column(ctx, sig.line),
            subject: sig.name.clone(),
            message: format!(
                "routine `{}` writes {} but does not declare {} clobbered",
                sig.name,
                reg_list(missing),
                it_or_them(missing),
            ),
        });
    }
}

/// `overdeclared-clobber`: a routine that declares a clobber its body cannot
/// produce — the opposite drift, where a routine stopped using a register and
/// still makes every caller spill it.
///
/// Over-declaring is *safe*, so this is a documentation-accuracy finding, and it
/// fires only when the body is **fully understood**: any macro invocation,
/// unannotated call, indirect jump, or data run inside the body suppresses it
/// (§8.4).
fn rule_overdeclared_clobber(ctx: &LintCtx, opts: &LintOptions, out: &mut Vec<Finding>) {
    for sig in ctx.signatures.iter().filter(|s| s.declares_clobbers) {
        if (opts.ignore)(&sig.name) {
            continue;
        }
        let effects = routine_effects(ctx, &sig.name);
        if effects.unknown {
            continue;
        }
        // Only the clobber list is checked: a `returns` slot is a claim about
        // the routine's output, answered by reading it, not by a write count.
        let declared = slots_to_regs(&sig.clobbers);
        let extra = RegSet::from_bits(declared.bits() & !effects.writes.bits());
        if extra.is_empty() {
            continue;
        }
        out.push(Finding {
            rule: RuleId::OverdeclaredClobber,
            line: sig.line,
            column: label_column(ctx, sig.line),
            subject: sig.name.clone(),
            message: format!(
                "routine `{}` declares {} clobbered but never writes {}",
                sig.name,
                reg_list(extra),
                it_or_them(extra),
            ),
        });
    }
}

/// `require-routine-doc`: a called, block-opening label with no signature
/// annotations. **Off by default** — see [`RuleId::default_severity`].
fn rule_require_routine_doc(ctx: &LintCtx, opts: &LintOptions, out: &mut Vec<Finding>) {
    let called = call_targets(ctx);
    for (idx, line) in ctx.lines.iter().enumerate() {
        let Some((name, column)) = block_label(ctx.source, line) else {
            continue;
        };
        if (opts.ignore)(name) || !is_block_entry(ctx.source, ctx.lines, idx) {
            continue;
        }
        if !called.contains(name) {
            continue;
        }
        if ctx.signatures.iter().any(|s| s.name == name) {
            continue;
        }
        out.push(Finding {
            rule: RuleId::RequireRoutineDoc,
            line: (idx + 1) as u32,
            column,
            subject: name.to_string(),
            message: format!("called routine `{name}` has no signature annotations"),
        });
    }
}

/// The registers a signature declares it writes: its clobbers plus its returns,
/// narrowed to the four the verifier tracks. Flag and memory slots are
/// documentation and are dropped here (§8.3).
fn declared_regs(sig: &Signature) -> RegSet {
    slots_to_regs(&sig.declared_writes())
}

/// The verifier-tracked registers among `slots`.
fn slots_to_regs(slots: &[Slot]) -> RegSet {
    slots.iter().fold(RegSet::EMPTY, |set, slot| {
        set.union(match slot {
            Slot::A => RegSet::A,
            Slot::X => RegSet::X,
            Slot::Y => RegSet::Y,
            Slot::S => RegSet::S,
            _ => RegSet::EMPTY,
        })
    })
}

/// The pronoun a message should use for `regs` — one register is "it", several
/// are "them".
fn it_or_them(regs: RegSet) -> &'static str {
    if regs.bits().is_power_of_two() {
        "it"
    } else {
        "them"
    }
}

/// Render a register set for a message, in canonical order.
fn reg_list(regs: RegSet) -> String {
    [
        (RegSet::A, "A"),
        (RegSet::X, "X"),
        (RegSet::Y, "Y"),
        (RegSet::S, "S"),
    ]
    .into_iter()
    .filter(|&(bit, _)| regs.contains(bit))
    .map(|(_, name)| name)
    .collect::<Vec<_>>()
    .join(", ")
}

/// The 1-based column of the label defined on 1-based `line`, for a finding that
/// points at the routine rather than at its annotations.
fn label_column(ctx: &LintCtx, line: u32) -> u32 {
    ctx.lines
        .get(line as usize - 1)
        .and_then(|l| block_label(ctx.source, l))
        .map_or(1, |(_, column)| column)
}

/// What a routine's body was observed to do.
#[derive(Debug, Clone, Copy, Default)]
struct BodyEffects {
    /// Registers definitely written on some path through the body.
    writes: RegSet,
    /// Something in the body the analysis cannot read, so `writes` is a lower
    /// bound rather than the whole story.
    unknown: bool,
}

/// The effects of the routine named `name`, following calls to other annotated
/// routines in this buffer.
///
/// A call to a routine with a signature contributes that routine's **declared**
/// writes; a call to one without a signature — or outside this buffer — is an
/// unknown. Recursion is broken by visiting each routine once, with a cycle
/// counting as an unknown.
fn routine_effects(ctx: &LintCtx, name: &str) -> BodyEffects {
    let mut visiting = Vec::new();
    routine_effects_inner(ctx, name, &mut visiting)
}

fn routine_effects_inner(ctx: &LintCtx, name: &str, visiting: &mut Vec<String>) -> BodyEffects {
    if visiting.iter().any(|n| n == name) {
        // A cycle: stop rather than recurse, and say we do not fully know.
        return BodyEffects {
            writes: RegSet::EMPTY,
            unknown: true,
        };
    }
    visiting.push(name.to_string());

    let mut effects = BodyEffects::default();
    let body = body_lines(ctx, name);
    // The routine's own `@local` labels: a jump to one stays in the body.
    let locals: Vec<&str> = body
        .clone()
        .filter_map(|idx| local_label(ctx.source, &ctx.lines[idx]))
        .collect();
    // `@x` names `@x:`; `@x3` names a pasted `@x\i:`.
    let is_own_local = |target: &str| {
        locals.iter().any(|l| match l.split_once('\\') {
            Some((base, _)) => target
                .strip_prefix(base)
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit())),
            None => *l == target,
        })
    };
    for idx in body {
        let line = &ctx.lines[idx];
        let sig: Vec<&Lexeme> = line
            .iter()
            .filter(|l| l.kind != LexKind::Whitespace && l.kind != LexKind::Comment)
            .collect();
        // A `.table` entry's `[KEY]` tag emits nothing: see through it.
        let sig = &sig[tag_lexemes(ctx.source, &sig)..];
        let Some(first) = sig.first() else {
            continue;
        };
        // A label inside the body (an internal branch target) is transparent,
        // and so is an `@local` label.
        if block_label(ctx.source, line).is_some() || local_label(ctx.source, line).is_some() {
            continue;
        }
        match first.kind {
            LexKind::Directive => {
                if !is_transparent_directive(text(ctx.source, first)) {
                    effects.unknown = true;
                }
            }
            LexKind::Ident => {
                let mnemonic = text(ctx.source, first);
                if !MNEMONICS.contains(&mnemonic.to_ascii_lowercase()) {
                    // A macro invocation or custom pseudo-instruction: its
                    // expansion is not analyzed.
                    effects.unknown = true;
                    continue;
                }
                let operand = operand_text(ctx.source, sig);
                if mnemonic.eq_ignore_ascii_case("jsr") || mnemonic.eq_ignore_ascii_case("jmp") {
                    let target = operand.trim();
                    if is_own_local(target) {
                        // A jump within the routine: its writes are the
                        // body's own, already counted.
                        continue;
                    }
                    if target.contains('@') {
                        // Another routine's local (or one this routine does
                        // not define): not a routine with a signature.
                        effects.unknown = true;
                        continue;
                    }
                    apply_call(ctx, &operand, visiting, &mut effects);
                    continue;
                }
                match instruction_writes(mnemonic, &operand) {
                    Some(writes) => effects.writes = effects.writes.union(writes),
                    None => effects.unknown = true,
                }
            }
            // A constant definition or anything else unexpected: not a write,
            // but not something to claim understanding of either.
            _ => effects.unknown = true,
        }
    }

    visiting.pop();
    effects
}

/// Fold a `JSR`/`JMP` into the caller's effects. A direct call to an annotated
/// routine contributes its declared writes; anything else — an unannotated
/// target, a target in another file, an indirect jump — is an unknown.
fn apply_call(ctx: &LintCtx, operand: &str, visiting: &mut Vec<String>, effects: &mut BodyEffects) {
    let target = operand.trim();
    if target.starts_with('(') || target.is_empty() {
        effects.unknown = true;
        return;
    }
    match ctx.signatures.iter().find(|s| s.name == target) {
        Some(callee) if callee.declares_clobbers => {
            effects.writes = effects.writes.union(declared_regs(callee));
        }
        // Annotated but with no clobber list, or not annotated at all: the call
        // makes no claim, so the body is not fully understood. Following its
        // body would double-count what a declaration is for.
        Some(_) | None => {
            let inner = routine_effects_inner(ctx, target, visiting);
            effects.writes = effects.writes.union(inner.writes);
            effects.unknown = true;
        }
    }
}

/// The 0-based line indexes of the body of the routine labelled `name`: from its
/// label to the last line before the next block-entry label, or end of file.
///
/// This **under-approximates** on purpose. A routine that falls through into the
/// next one really does clobber what the next one clobbers, and this will not see
/// it — a false negative, which costs a missed report. Following fall-through
/// would risk attributing a neighbour's writes to this routine, which costs the
/// rule its credibility (§8.1).
fn body_lines(ctx: &LintCtx, name: &str) -> std::ops::Range<usize> {
    let Some(start) = ctx.lines.iter().enumerate().position(|(idx, line)| {
        block_label(ctx.source, line).is_some_and(|(label, _)| label == name)
            && is_block_entry(ctx.source, ctx.lines, idx)
    }) else {
        return 0..0;
    };
    let end = ((start + 1)..ctx.lines.len())
        .find(|&idx| {
            block_label(ctx.source, &ctx.lines[idx]).is_some()
                && is_block_entry(ctx.source, ctx.lines, idx)
        })
        .unwrap_or(ctx.lines.len());
    (start + 1)..end
}

/// The operand text of an instruction line: everything after the mnemonic, with
/// the trailing comment already filtered out by the caller.
fn operand_text(source: &str, sig: &[&Lexeme]) -> String {
    match (sig.get(1), sig.last()) {
        (Some(first), Some(last)) if sig.len() > 1 => {
            source[first.start..last.end].trim().to_string()
        }
        _ => String::new(),
    }
}

/// The registers an instruction writes, or `None` when its effects are not
/// recorded (an undocumented opcode).
///
/// Only the parts of the addressing mode that change the register effects are
/// recovered from the operand text — accumulator mode, and indexing by `X` or
/// `Y`. That is enough to distinguish `ASL A` (writes `A`) from `ASL $00` (does
/// not) and `LDA $10,X` (reads `X`) from `LDA $10`, which is why the effect table
/// is per opcode rather than per mnemonic.
fn instruction_writes(mnemonic: &str, operand: &str) -> Option<RegSet> {
    use nessemble_isa::AddressingMode as Mode;

    let accumulator = operand.is_empty() || operand.eq_ignore_ascii_case("a");
    let indexed_x = ends_with_index(operand, 'x');
    let indexed_y = ends_with_index(operand, 'y');

    let modes: &[Mode] = if accumulator {
        &[Mode::Accumulator, Mode::Implied]
    } else if indexed_x {
        &[Mode::ZeroPageX, Mode::AbsoluteX, Mode::IndirectX]
    } else if indexed_y {
        &[Mode::ZeroPageY, Mode::AbsoluteY, Mode::IndirectY]
    } else {
        &[
            Mode::Immediate,
            Mode::ZeroPage,
            Mode::Absolute,
            Mode::Relative,
            Mode::Indirect,
            Mode::Implied,
        ]
    };
    let opcode = modes
        .iter()
        .find_map(|&mode| nessemble_isa::find(mnemonic, mode))?;
    opcode.effects_known.then_some(opcode.writes)
}

/// Whether an operand ends with `,X` / `,Y` (or `,X)` for indirect), ignoring
/// case and spacing.
fn ends_with_index(operand: &str, index: char) -> bool {
    let trimmed = operand.trim_end_matches(')').trim_end();
    match trimmed.rsplit_once(',') {
        Some((_, last)) => last.trim().eq_ignore_ascii_case(&index.to_string()),
        None => false,
    }
}

/// Whether a directive inside a routine body is transparent to the verifier:
/// it emits no bytes that could be executed and changes no register. Anything
/// else — a data run, an include, an unknown pseudo-op — makes the body not
/// fully understood.
fn is_transparent_directive(name: &str) -> bool {
    const TRANSPARENT: &[&str] = &[
        ".if",
        ".ifdef",
        ".ifndef",
        ".else",
        ".elseif",
        ".endif",
        ".rs",
        ".rsset",
        ".enum",
        ".ende",
        ".equ",
        ".table",
        ".endtable",
        ".assert",
        ".error",
        ".out",
    ];
    TRANSPARENT
        .iter()
        .any(|d| d.eq_ignore_ascii_case(name.trim()))
}

/// Every label named as the direct target of a `JSR` in this buffer.
fn call_targets<'a>(ctx: &LintCtx<'a>) -> HashSet<&'a str> {
    let mut out = HashSet::new();
    for line in ctx.lines {
        let sig: Vec<&Lexeme> = line
            .iter()
            .filter(|l| l.kind != LexKind::Whitespace && l.kind != LexKind::Comment)
            .collect();
        let (Some(first), Some(second)) = (sig.first(), sig.get(1)) else {
            continue;
        };
        if first.kind == LexKind::Ident
            && second.kind == LexKind::Ident
            && text(ctx.source, first).eq_ignore_ascii_case("jsr")
        {
            out.insert(text(ctx.source, second));
        }
    }
    out
}

/// The index of the first significant line after 1-based `line` — skipping
/// blank and comment-only lines, so an explanatory comment may sit between a
/// directive and its target.
fn significant_line_after(lines: &[Vec<Lexeme>], line: u32) -> Option<usize> {
    lines
        .iter()
        .enumerate()
        .skip(line as usize)
        .find(|(_, l)| !is_blank(l) && !is_comment_only(l))
        .map(|(idx, _)| idx)
}

/// The source text of one physical line (the span of its lexemes), so the
/// linter can hand a line to the text-based helpers the formatter uses.
fn line_text<'a>(source: &'a str, line: &[Lexeme]) -> &'a str {
    match (line.first(), line.last()) {
        (Some(first), Some(last)) => &source[first.start..last.end],
        _ => "",
    }
}

/// If `line` is exactly a named label definition (`name:`, or a pasted
/// `name\i:` from a `.for` body, returned as one name), return the label
/// name and its 1-based character column. Anonymous labels (`:`, `:++`), `@local`
/// temp labels, and labels sharing a line with code all return `None`.
fn block_label<'a>(source: &'a str, line: &[Lexeme]) -> Option<(&'a str, u32)> {
    let sig: Vec<&Lexeme> = line
        .iter()
        .filter(|l| l.kind != LexKind::Whitespace && l.kind != LexKind::Comment)
        .collect();
    if sig.first().is_none_or(|l| l.kind != LexKind::Ident) {
        return None;
    }
    let n = name_lexemes(source, &sig);
    if sig.len() != n + 1 || !is_punct(source, sig[n], ":") {
        return None;
    }
    let name = &source[sig[0].start..sig[n - 1].end];
    // Match the "document every code block" convention: a real label name starts
    // with an ASCII letter or `_`, so `@local` labels never open a block (a
    // qualified `p1@x:` cannot be defined at all).
    let first = name.as_bytes().first().copied().unwrap_or(0);
    if !(first.is_ascii_alphabetic() || first == b'_') || name.contains('@') {
        return None;
    }
    let line_start = line.first().map_or(sig[0].start, |l| l.start);
    // A label line's only prefix is optional whitespace, so char-count == column.
    let column = source[line_start..sig[0].start].chars().count() as u32 + 1;
    Some((name, column))
}

/// Whether the label at `idx` opens a new block: scanning backwards over
/// comment-only lines, the first non-comment line is blank or the top of the
/// file. A label directly following code is an internal branch target, not a
/// documented block entry.
fn is_block_entry(source: &str, lines: &[Vec<Lexeme>], idx: usize) -> bool {
    for i in (0..idx).rev() {
        if is_blank(&lines[i]) {
            return true;
        }
        // An `.assert`/`.error`/`.out` line emits nothing, so it is skipped
        // like a comment (`plans/022-assertions.md` §3).
        if is_comment_only(&lines[i]) || is_check_line(source, &lines[i]) {
            continue;
        }
        return false;
    }
    true
}

/// Whether a line's first significant lexeme is `.assert`, `.error` or
/// `.out`, which emit nothing.
fn is_check_line(source: &str, line: &[Lexeme]) -> bool {
    line.iter()
        .find(|l| l.kind != LexKind::Whitespace)
        .is_some_and(|l| {
            l.kind == LexKind::Directive
                && matches!(&source[l.start..l.end], ".assert" | ".error" | ".out")
        })
}

/// Whether a line has no significant tokens (only whitespace, or empty).
fn is_blank(line: &[Lexeme]) -> bool {
    line.iter().all(|l| l.kind == LexKind::Whitespace)
}

/// Whether a line's first significant token is a comment (a `; …`-only line,
/// possibly indented). A code line with a trailing comment is not comment-only.
fn is_comment_only(line: &[Lexeme]) -> bool {
    match line.iter().find(|l| l.kind != LexKind::Whitespace) {
        Some(l) => l.kind == LexKind::Comment,
        None => false,
    }
}

/// Whether any line within `±window` of `idx` carries a comment.
fn has_comment_within(lines: &[Vec<Lexeme>], idx: usize, window: usize) -> bool {
    let lo = idx.saturating_sub(window);
    let hi = (idx + window).min(lines.len().saturating_sub(1));
    (lo..=hi).any(|i| lines[i].iter().any(|l| l.kind == LexKind::Comment))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(source: &str) -> Vec<LexKind> {
        lex(source).into_iter().map(|l| l.kind).collect()
    }

    #[test]
    fn lex_is_gap_free_and_covers_input() {
        let src = "  lda #$00 ; go\n";
        let lexemes = lex(src);
        // Contiguous, covering [0, len).
        assert_eq!(lexemes.first().unwrap().start, 0);
        assert_eq!(lexemes.last().unwrap().end, src.len());
        for w in lexemes.windows(2) {
            assert_eq!(w[0].end, w[1].start);
        }
    }

    #[test]
    fn lex_classifies_tokens() {
        assert_eq!(
            kinds("lda #$00 ; c\n"),
            vec![
                LexKind::Ident,      // lda
                LexKind::Whitespace, //
                LexKind::Punct,      // #
                LexKind::Number,     // $00
                LexKind::Whitespace, //
                LexKind::Comment,    // ; c
                LexKind::Newline,    //
            ]
        );
        assert_eq!(kinds(".db"), vec![LexKind::Directive]);
        assert_eq!(kinds("\"hi\""), vec![LexKind::String]);
        assert_eq!(kinds("'x'"), vec![LexKind::Char]);
    }

    #[test]
    fn format_indents_instructions_and_keeps_others_at_column_0() {
        let src = "label:\nlda #$00\n.db $01\nCOUNT = 5\n";
        let out = format(src);
        assert_eq!(out, "label:\n    lda #$00\n.db $01\nCOUNT = 5\n");
    }

    #[test]
    fn format_indents_branch_with_anonymous_label_operand() {
        // R2 regression: a branch whose operand is an anonymous-label reference
        // (`:+`/`:-`/`:++`) is an instruction, not an anonymous-label
        // definition, so it indents to block depth. Misclassifying it as a
        // label de-indented it to column 0, which the assembler then parsed
        // differently — corrupting the assembled bytes.
        let src = "routine:\nBEQ :+\nBNE named\nBEQ :++\n:\nSTA <$02\n";
        assert_eq!(
            format(src),
            "routine:\n    BEQ :+\n    BNE named\n    BEQ :++\n:\n    STA <$02\n"
        );
    }

    #[test]
    fn format_keeps_bare_anonymous_and_named_labels_at_column_0() {
        // The anonymous-label definition (bare `:`) and named labels still sit
        // at column 0; only the `:+`/`:-` *operand* form is an instruction.
        let src = ":\n    LDA <$01\nloop:\n    BNE :-\n";
        assert_eq!(format(src), ":\n    LDA <$01\nloop:\n    BNE :-\n");
    }

    #[test]
    fn format_leaves_a_label_with_content_at_column_0() {
        // `foo: .db 1` is an assembler error (a label must be on a line of its
        // own). Formatting must not indent it into an instruction-looking line:
        // it stays put, and assembles to the same error before and after.
        assert_eq!(format("foo: .db 1\n"), "foo: .db 1\n");
        assert_eq!(format("    foo: NOP\n"), "foo: NOP\n");
        // A `:+`/`:-` operand is still an instruction.
        assert_eq!(format("BEQ :+\n"), "    BEQ :+\n");
        for src in ["foo: .db 1\n", "    foo: NOP\n"] {
            let before = crate::assemble(src, &crate::Options::default()).unwrap_err();
            let after = crate::assemble(&format(src), &crate::Options::default()).unwrap_err();
            assert_eq!(before.0.message, after.0.message);
        }
    }

    #[test]
    fn format_treats_labelled_colon_with_comment_as_label() {
        // A colon that ends the line (only a trailing comment after it) is a
        // named-label definition and stays flush-left.
        assert_eq!(format("done:  ; exit\n"), "done:  ; exit\n");
    }

    #[test]
    fn format_normalizes_comma_spacing() {
        assert_eq!(format(".db $01,$02 , $03\n"), ".db $01, $02, $03\n");
    }

    #[test]
    fn format_trims_trailing_whitespace_and_reindents() {
        assert_eq!(format("      lda #$00   \n"), "    lda #$00\n");
    }

    #[test]
    fn format_preserves_comments_and_blank_lines() {
        let src = "; header\n\n    nop  ; do nothing\n";
        assert_eq!(format(src), "; header\n\n    nop  ; do nothing\n");
    }

    #[test]
    fn format_preserves_case_and_tight_operators() {
        // Upper-case mnemonic and tight `+` are kept; only indent changes.
        assert_eq!(format("LDA #$33+1\n"), "    LDA #$33+1\n");
    }

    #[test]
    fn format_is_idempotent() {
        let src = "start:\n  LDX #$08\n.db 1,2,  3   \n; end\n";
        let once = format(src);
        assert_eq!(format(&once), once);
    }

    #[test]
    fn format_keeps_a_lone_name_on_the_side_it_was_written() {
        // Column 1: a constant / `.enum` member. Indented: an instruction.
        assert_eq!(format("FLAG\n"), "FLAG\n");
        assert_eq!(format("FLAG ; c\n"), "FLAG ; c\n");
        assert_eq!(format("ROW\\i\n"), "ROW\\i\n");
        assert_eq!(format(" RTS\n"), "    RTS\n");
    }

    #[test]
    fn format_leaves_the_value_enum_example_alone() {
        let src = ".enum $00, 1, OP\nOP_YIELD\nOP_DRAW ; redraw\nOP_FRAME\n.endenum\n";
        let once = format(src);
        assert_eq!(once, src);
        assert_eq!(format(&once), once);
        let raw = crate::Options {
            nes: false,
            ..crate::Options::default()
        };
        let before = crate::assemble(src, &raw).expect("assembles").symbols;
        let after = crate::assemble(&once, &raw).expect("assembles").symbols;
        assert_eq!(before, after);
    }

    #[test]
    fn format_ensures_a_final_newline_by_default() {
        // Pass 5 (finalNewline, default on) adds the missing trailing newline.
        assert_eq!(format("  nop"), "    nop\n");
        assert_eq!(format("  nop\n"), "    nop\n");
    }

    #[test]
    fn format_with_final_newline_off_preserves_presence() {
        let opts = FormatOptions {
            final_newline: false,
            ..FormatOptions::default()
        };
        assert_eq!(format_with("  nop", &opts), "    nop");
        assert_eq!(format_with("  nop\n", &opts), "    nop\n");
    }

    #[test]
    fn format_with_default_matches_format() {
        // The seam is a no-op refactor: default options reproduce `format`.
        let src = "start:\n  LDX #$08\n.db 1,2,  3   \n; end\n";
        assert_eq!(format_with(src, &FormatOptions::default()), format(src));
    }

    #[test]
    fn format_with_custom_indent_width() {
        let opts = FormatOptions {
            indent_width: 2,
            ..FormatOptions::default()
        };
        assert_eq!(
            format_with("label:\nlda #$00\n", &opts),
            "label:\n  lda #$00\n"
        );
    }

    #[test]
    fn format_with_tab_indent() {
        let opts = FormatOptions {
            indent_style: IndentStyle::Tab,
            ..FormatOptions::default()
        };
        // Instructions indented by a tab; the label stays at column 0.
        assert_eq!(
            format_with("label:\nlda #$00\n", &opts),
            "label:\n\tlda #$00\n"
        );
    }

    #[test]
    fn format_with_indent_directives_indents_directive_lines() {
        // R1 (opt-in): with indentDirectives on, directives carry the block's
        // indent like instructions; labels and constants still sit at column 0.
        let opts = FormatOptions {
            indent_directives: true,
            ..FormatOptions::default()
        };
        let src = "tbl:\n.db $01, $02\n.dw addr\n.include \"x.asm\"\nCOUNT = 5\n";
        assert_eq!(
            format_with(src, &opts),
            "tbl:\n    .db $01, $02\n    .dw addr\n    .include \"x.asm\"\nCOUNT = 5\n"
        );
    }

    #[test]
    fn format_default_keeps_directives_at_column_0() {
        // R1 default (house style): directives stay flush-left.
        assert_eq!(format("tbl:\n.db $01, $02\n"), "tbl:\n.db $01, $02\n");
    }

    #[test]
    fn format_with_indent_directives_is_idempotent() {
        let opts = FormatOptions {
            indent_directives: true,
            ..FormatOptions::default()
        };
        let src = "tbl:\n.db $01, $02, $03\n.dw addr\nloop:\n    BNE :-\n";
        let once = format_with(src, &opts);
        assert_eq!(format_with(&once, &opts), once);
    }

    // ── `.for` loops (plan 016): counters and pasted names ──────────────────

    const FOR_SRC: &str = "\
.for 0, 2
row\\i:
    LDA #\\i
    STA $0200+\\i
    RTS
.endfor
jump_table:
.for 0, 2
.dw row\\i
.endfor
.for 0, 1
.for 0, 1
.db \\i, \\j
.endfor
.endfor
SLOT\\i = \\i*4
";

    #[test]
    fn format_leaves_for_loops_and_pasted_names_unchanged() {
        // Body lines format as they would outside a loop: `.for`/`.endfor` are
        // directives (column 0 by default), `row\i:` is a label (column 0), and
        // `\i`/`\j` operands and pasted references pass through verbatim.
        let want = "\
.for 0, 2
row\\i:
    LDA #\\i
    STA $0200+\\i
    RTS

.endfor
jump_table:
.for 0, 2
.dw row\\i
.endfor
.for 0, 1
.for 0, 1
.db \\i, \\j
.endfor
.endfor
SLOT\\i = \\i*4
";
        let once = format(FOR_SRC);
        assert_eq!(once, want);
        assert_eq!(format(&once), once);
    }

    #[test]
    fn format_for_directives_follow_indent_directives() {
        let opts = FormatOptions {
            indent_directives: true,
            ..FormatOptions::default()
        };
        let once = format_with(".for 0, 1\n.db \\i\n.endfor\n", &opts);
        assert_eq!(once, "    .for 0, 1\n    .db \\i\n    .endfor\n");
        assert_eq!(format_with(&once, &opts), once);
        // Pasted labels and constants stay at column 0 even when directives indent.
        let once = format_with("  row\\i:\n  LDA #\\i\n  SLOT\\j = 1\n", &opts);
        assert_eq!(once, "row\\i:\n    LDA #\\i\nSLOT\\j = 1\n");
        assert_eq!(format_with(&once, &opts), once);
    }

    // ── Continuation-line alignment (alignContinuations) ────────────────────

    #[test]
    fn continuations_align_under_first_arg_by_default() {
        // Default (alignContinuations on, indentDirectives off): the directive
        // sits at column 0 and each continuation lines up under its first arg —
        // `.metasprite ` is 12 columns, so continuations get 12 spaces.
        let src = "sprite:\n\
                   .metasprite $FA, $02, $00, $FA,\n\
                   $FA, $03, $00, $02,\n\
                   $02, $0D, $00, $FA\n";
        let pad = " ".repeat(12);
        assert_eq!(
            format(src),
            format!(
                "sprite:\n\
                 .metasprite $FA, $02, $00, $FA,\n\
                 {pad}$FA, $03, $00, $02,\n\
                 {pad}$02, $0D, $00, $FA\n"
            )
        );
    }

    #[test]
    fn continuations_align_with_indent_directives() {
        // With indentDirectives on the opening line carries the 4-space block
        // indent, so the first arg — and every continuation — lands at column
        // 16 (4 + `.metasprite ` = 4 + 12).
        let opts = FormatOptions {
            indent_directives: true,
            ..FormatOptions::default()
        };
        let src = "sprite:\n.metasprite $FA, $02,\n$FA, $03\n";
        let pad = " ".repeat(16);
        assert_eq!(
            format_with(src, &opts),
            format!("sprite:\n    .metasprite $FA, $02,\n{pad}$FA, $03\n")
        );
    }

    #[test]
    fn continuations_block_indent_when_disabled() {
        // alignContinuations off reproduces today's behavior: continuation
        // lines fall to the block indent (indentWidth = 4).
        let opts = FormatOptions {
            align_continuations: false,
            ..FormatOptions::default()
        };
        let src = "sprite:\n.metasprite $FA, $02,\n$FA, $03\n";
        assert_eq!(
            format_with(src, &opts),
            "sprite:\n.metasprite $FA, $02,\n    $FA, $03\n"
        );
    }

    #[test]
    fn continuations_use_tab_base_then_space_pad() {
        // Under IndentStyle::Tab the continuation reproduces the opening line's
        // tab indent, then pads to the first-arg column with spaces (a tab plus
        // `.metasprite ` = tab + 12 spaces).
        let opts = FormatOptions {
            indent_style: IndentStyle::Tab,
            indent_directives: true,
            ..FormatOptions::default()
        };
        let src = "sprite:\n.metasprite $FA, $02,\n$FA, $03\n";
        let pad = format!("\t{}", " ".repeat(12));
        assert_eq!(
            format_with(src, &opts),
            format!("sprite:\n\t.metasprite $FA, $02,\n{pad}$FA, $03\n")
        );
    }

    #[test]
    fn continuation_alignment_holds_with_tight_commas() {
        // commaSpacing changes spacing between operands but not the first-arg
        // column, so continuations still align at column 12.
        let opts = FormatOptions {
            comma_spacing: false,
            ..FormatOptions::default()
        };
        let src = "sprite:\n.metasprite $FA, $02,\n$FA, $03\n";
        let pad = " ".repeat(12);
        assert_eq!(
            format_with(src, &opts),
            format!("sprite:\n.metasprite $FA,$02,\n{pad}$FA,$03\n")
        );
    }

    #[test]
    fn continuation_alignment_survives_trailing_comment() {
        // A trailing comment on the opening line doesn't shift the first-arg
        // column; the continuation still aligns at column 12.
        let src = "sprite:\n.metasprite $FA, $02,  ; row 0\n$FA, $03\n";
        let pad = " ".repeat(12);
        assert_eq!(
            format(src),
            format!("sprite:\n.metasprite $FA, $02, ; row 0\n{pad}$FA, $03\n")
        );
    }

    #[test]
    fn continuation_alignment_is_idempotent() {
        let src = "sprite:\n.metasprite $FA, $02, $00, $FA,\n$FA, $03, $00, $02\n";
        let once = format(src);
        assert_eq!(format(&once), once);
    }

    #[test]
    fn single_line_statements_are_unaffected_by_alignment() {
        // No trailing comma → no continuation; ordinary directive/instruction
        // layout is unchanged.
        assert_eq!(
            format("loop:\n    LDA $00\n.db $01, $02\n"),
            "loop:\n    LDA $00\n.db $01, $02\n"
        );
    }

    #[test]
    fn format_with_tight_commas() {
        let opts = FormatOptions {
            comma_spacing: false,
            ..FormatOptions::default()
        };
        assert_eq!(
            format_with(".db $01, $02 , $03\n", &opts),
            ".db $01,$02,$03\n"
        );
    }

    #[test]
    fn format_with_is_idempotent_for_custom_options() {
        let opts = FormatOptions {
            indent_style: IndentStyle::Tab,
            indent_width: 2,
            comma_spacing: false,
            ..FormatOptions::default()
        };
        let src = "start:\n      LDX #$08\n.db 1, 2,  3   \n; end\n";
        let once = format_with(src, &opts);
        assert_eq!(format_with(&once, &opts), once);
    }

    // ── Pass 1: data consolidation ──────────────────────────────────────────

    #[test]
    fn consolidates_adjacent_db_into_eight_per_line() {
        let src = ".db $01, $02\n.db $03, $04\n.db $05, $06, $07, $08, $09\n";
        assert_eq!(
            format(src),
            ".db $01, $02, $03, $04, $05, $06, $07, $08\n.db $09\n"
        );
    }

    #[test]
    fn does_not_merge_db_and_dw() {
        assert_eq!(format(".db $01\n.dw $8000\n"), ".db $01\n.dw $8000\n");
    }

    #[test]
    fn a_commented_data_line_is_never_merged() {
        let src = ".db $01\n.db $02 ; note\n.db $03\n";
        assert_eq!(format(src), ".db $01\n.db $02 ; note\n.db $03\n");
    }

    #[test]
    fn a_label_between_data_flushes_the_group() {
        let src = ".db $01\n.db $02\nlbl:\n.db $03\n.db $04\n";
        assert_eq!(format(src), ".db $01, $02\nlbl:\n.db $03, $04\n");
    }

    #[test]
    fn stride_hint_overrides_data_per_line() {
        let src = "; @fmt stride=2\n.db $01, $02, $03, $04\n";
        assert_eq!(format(src), "; @fmt stride=2\n.db $01, $02\n.db $03, $04\n");
    }

    #[test]
    fn stride_hint_last_value_repeats() {
        let src = "; @fmt stride=2,1\n.db $01, $02, $03, $04\n";
        assert_eq!(
            format(src),
            "; @fmt stride=2,1\n.db $01, $02\n.db $03\n.db $04\n"
        );
    }

    #[test]
    fn namespaced_stride_hint_overrides_data_per_line() {
        let src = "; @nessemble-format stride=2\n.db $01, $02, $03, $04\n";
        assert_eq!(
            format(src),
            "; @nessemble-format stride=2\n.db $01, $02\n.db $03, $04\n"
        );
    }

    #[test]
    fn namespaced_stride_hint_last_value_repeats() {
        let src = "; @nessemble-format stride=2,1\n.db $01, $02, $03, $04\n";
        assert_eq!(
            format(src),
            "; @nessemble-format stride=2,1\n.db $01, $02\n.db $03\n.db $04\n"
        );
    }

    #[test]
    fn both_stride_hint_spellings_are_disabled_together() {
        let opts = FormatOptions {
            respect_stride_hints: false,
            ..FormatOptions::default()
        };
        for hint in ["; @fmt stride=2", "; @nessemble-format stride=2"] {
            let src = format!("{hint}\n.db $01, $02, $03, $04\n");
            assert_eq!(
                format_with(&src, &opts),
                format!("{hint}\n.db $01, $02, $03, $04\n")
            );
        }
    }

    #[test]
    fn a_trailing_stride_hint_is_inert() {
        // Only a comment-only line carries a hint — unchanged from `@fmt`.
        let src = ".db $01 ; @nessemble-format stride=1\n.db $02\n";
        assert_eq!(
            format(src),
            ".db $01 ; @nessemble-format stride=1\n.db $02\n"
        );
    }

    // ── Directive binding ───────────────────────────────────────────────────
    //
    // The formatter and `ineffective-comment-directive` share one lookahead, so
    // every case here is asserted on *both* sides: the stride is applied and the
    // linter is clean, or neither. They disagreed before — the formatter skipped
    // { blank, label }, the linter { blank, comment } — which made a label a
    // false positive and a comment a silent no-op.

    /// Every line that must be transparent between a directive and its run,
    /// named and in the gap-text form the fixtures splice in.
    const GAPS: &[(&str, &str)] = &[
        ("immediate", ""),
        ("blank", "\n"),
        ("label", "foo:\n"),
        ("comment", "; explanation\n"),
        ("label then comment", "foo:\n; explanation\n"),
        ("comment then label", "; explanation\nfoo:\n"),
        ("two blanks", "\n\n"),
        ("constant", "FOO = $10\n"),
    ];

    #[test]
    fn a_stride_hint_binds_across_blank_comment_and_label_lines() {
        for (name, gap) in GAPS {
            let src = format!("; @nessemble-format stride=1\n{gap}.db $01, $02, $03\n");
            assert_eq!(
                format(&src),
                format!("; @nessemble-format stride=1\n{gap}.db $01\n.db $02\n.db $03\n"),
                "formatter, gap = {name}"
            );
            assert!(lint_directives(&src).is_empty(), "lint, gap = {name}");
            // Binding across a gap must stay idempotent — the run is already
            // strided the second time through.
            let once = format(&src);
            assert_eq!(format(&once), once, "idempotence, gap = {name}");
        }
    }

    #[test]
    fn a_stride_hint_binds_the_same_way_for_dw_and_color() {
        for (dir, values, strided) in [
            ("dw", "$8000, $8010", "$8000\n.dw $8010"),
            ("color", "$0F, $30", "$0F\n.color $30"),
        ] {
            for (name, gap) in GAPS {
                let src = format!("; @nessemble-format stride=1\n{gap}.{dir} {values}\n");
                assert_eq!(
                    format(&src),
                    format!("; @nessemble-format stride=1\n{gap}.{dir} {strided}\n"),
                    "formatter, .{dir} with gap = {name}"
                );
                assert!(
                    lint_directives(&src).is_empty(),
                    "lint, .{dir} with gap = {name}"
                );
            }
        }
    }

    #[test]
    fn the_deprecated_alias_binds_identically_and_still_reports() {
        for (name, gap) in GAPS {
            let src = format!("; @fmt stride=1\n{gap}.db $01, $02\n");
            assert_eq!(
                format(&src),
                format!("; @fmt stride=1\n{gap}.db $01\n.db $02\n"),
                "formatter, gap = {name}"
            );
            let findings = lint_directives(&src);
            assert_eq!(findings.len(), 1, "lint, gap = {name}: {findings:?}");
            assert_eq!(findings[0].rule, RuleId::DeprecatedCommentDirective);
        }
    }

    #[test]
    fn a_hint_that_binds_to_no_data_run_is_inert_on_both_sides() {
        // Each case pairs the lines after the directive with the data run that
        // must come back untouched (empty when there is no run at all).
        for (name, rest, intact) in [
            (
                "instruction",
                "    nop\n.db $01, $02, $03\n",
                ".db $01, $02, $03",
            ),
            (
                "label sharing its line with code",
                "foo: nop\n.db $01, $02, $03\n",
                ".db $01, $02, $03",
            ),
            (
                "data sharing a label's line",
                "foo: .db $01\n.db $02, $03\n",
                ".db $02, $03",
            ),
            ("end of file", "", ""),
        ] {
            let src = format!("; @nessemble-format stride=1\n{rest}");
            let formatted = format(&src);
            assert!(
                formatted.contains(intact),
                "formatter re-flowed past a barrier, {name}: {formatted}"
            );
            let findings = lint_directives(&src);
            assert_eq!(findings.len(), 1, "lint, {name}: {findings:?}");
            assert_eq!(findings[0].rule, RuleId::IneffectiveCommentDirective);
            assert!(findings[0].message.contains("data line"), "lint, {name}");
        }
    }

    #[test]
    fn the_shared_lookahead_is_the_one_definition_of_binding() {
        let target = |lines: &[&str]| format_directive_target(lines.iter().copied());
        // Transparent lines are skipped; the offset names the bound run.
        assert_eq!(target(&[".db $01"]), Some(0));
        assert_eq!(target(&["", "; why", "foo:", "    .dw $8000"]), Some(3));
        assert_eq!(target(&["foo:", ".color $0F ; pinned"]), Some(1));
        // A barrier, or nothing at all, binds to nothing.
        assert_eq!(target(&["    nop", ".db $01"]), None);
        assert_eq!(target(&[]), None);
    }

    // ── Comment directives ──────────────────────────────────────────────────

    fn scan(source: &str) -> Vec<Directive> {
        scan_directives(source)
    }

    fn bad(source: &str) -> Vec<MalformedDirective> {
        scan_directives_with_errors(source).1
    }

    #[test]
    fn scans_a_format_directive_with_strides() {
        let found = scan("; @nessemble-format stride=2,1\n");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, DirectiveName::Format);
        assert_eq!(found[0].args, DirectiveArgs::Strides(vec![2, 1]));
        assert!(!found[0].deprecated);
        assert!(found[0].own_line);
    }

    #[test]
    fn scans_both_region_bounds() {
        let found = scan("; @nessemble-coverage-ignore start\n; @nessemble-coverage-ignore end\n");
        let bounds: Vec<_> = found.iter().map(|d| d.args.clone()).collect();
        assert_eq!(
            bounds,
            vec![
                DirectiveArgs::Region(RegionBound::Start),
                DirectiveArgs::Region(RegionBound::End)
            ]
        );
        assert!(found
            .iter()
            .all(|d| d.name == DirectiveName::CoverageIgnore));
    }

    #[test]
    fn scans_an_argument_less_directive() {
        let found = scan("; @nessemble-coverage-ignore-next-line\n    LDA #$00\n");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, DirectiveName::CoverageIgnoreNextLine);
        assert_eq!(found[0].args, DirectiveArgs::None);
    }

    #[test]
    fn next_line_never_resolves_as_the_region_directive() {
        // Exact-token lookup: the longer name is its own directive, not the
        // shorter one with a stray `-next-line` argument.
        let found = scan("; @nessemble-coverage-ignore-next-line\n");
        assert_eq!(found[0].name, DirectiveName::CoverageIgnoreNextLine);
        assert_eq!(
            bad("; @nessemble-coverage-ignore-next-line\n"),
            [] as [crate::tooling::MalformedDirective; 0]
        );
    }

    #[test]
    fn a_deprecated_alias_is_flagged_but_honored() {
        let found = scan("; @fmt stride=4\n");
        assert_eq!(found[0].name, DirectiveName::Format);
        assert_eq!(found[0].args, DirectiveArgs::Strides(vec![4]));
        assert!(found[0].deprecated);
    }

    #[test]
    fn indentation_and_repeated_semicolons_still_carry_a_directive() {
        for src in [
            "    ; @nessemble-coverage-ignore start\n",
            ";; @nessemble-coverage-ignore start\n",
            ";;;   @nessemble-coverage-ignore start\n",
        ] {
            assert_eq!(scan(src).len(), 1, "{src:?}");
        }
    }

    #[test]
    fn trailing_prose_after_arguments_is_ignored() {
        let found = scan("; @nessemble-format stride=2 ; two bytes per line\n");
        assert_eq!(found[0].args, DirectiveArgs::Strides(vec![2]));
        let found = scan("; @nessemble-coverage-ignore start ; dead mapper path\n");
        assert_eq!(found[0].args, DirectiveArgs::Region(RegionBound::Start));
    }

    #[test]
    fn a_trailing_comment_directive_is_marked_not_own_line() {
        let found = scan("    LDA #$00 ; @nessemble-coverage-ignore-next-line\n");
        assert_eq!(found.len(), 1);
        assert!(!found[0].own_line);
    }

    #[test]
    fn ordinary_prose_is_never_a_directive() {
        for src in [
            "; @todo fix this\n",
            "; @param x the value\n",
            "; see @nessemble-format for details\n",
            "; @NESSEMBLE-FORMAT stride=2\n",
            "; @fmtstride=2\n",
            "    LDA #$00\n",
        ] {
            let (found, malformed) = scan_directives_with_errors(src);
            assert!(found.is_empty() && malformed.is_empty(), "{src:?}");
        }
    }

    #[test]
    fn a_directive_inside_a_string_is_not_scanned() {
        let src = "    .db \"; @nessemble-coverage-ignore start\"\n";
        let (found, malformed) = scan_directives_with_errors(src);
        assert!(found.is_empty() && malformed.is_empty());
    }

    #[test]
    fn an_unknown_namespaced_name_is_malformed() {
        let malformed = bad("; @nessemble-formt stride=2\n");
        assert_eq!(malformed.len(), 1);
        assert_eq!(malformed[0].reason, MalformedReason::UnknownName);
    }

    #[test]
    fn bad_arguments_are_malformed_and_yield_no_directive() {
        let cases = [
            ("; @nessemble-format\n", DirectiveName::Format),
            ("; @nessemble-format stride=x\n", DirectiveName::Format),
            ("; @nessemble-format stride=\n", DirectiveName::Format),
            ("; @nessemble-format 2\n", DirectiveName::Format),
            ("; @fmt\n", DirectiveName::Format),
            (
                "; @nessemble-coverage-ignore\n",
                DirectiveName::CoverageIgnore,
            ),
            (
                "; @nessemble-coverage-ignore begin\n",
                DirectiveName::CoverageIgnore,
            ),
            (
                "; @nessemble-coverage-ignore-next-line please\n",
                DirectiveName::CoverageIgnoreNextLine,
            ),
        ];
        for (src, name) in cases {
            let (found, malformed) = scan_directives_with_errors(src);
            assert!(found.is_empty(), "{src:?}");
            assert_eq!(malformed.len(), 1, "{src:?}");
            assert_eq!(
                malformed[0].reason,
                MalformedReason::BadArgs(name),
                "{src:?}"
            );
        }
    }

    #[test]
    fn directives_carry_their_position_and_token_range() {
        let src = "    LDA #$00\n  ;; @nessemble-format stride=2\n";
        let found = scan(src);
        assert_eq!((found[0].line, found[0].column), (2, 6));
        assert_eq!(&src[found[0].start..found[0].end], "@nessemble-format");
    }

    #[test]
    fn directives_come_back_in_source_order() {
        let src = "; @nessemble-coverage-ignore start\n; @nessemble-format stride=2\n.db $01\n; @nessemble-coverage-ignore end\n";
        let lines: Vec<u32> = scan(src).iter().map(|d| d.line).collect();
        assert_eq!(lines, vec![1, 2, 4]);
    }

    #[test]
    fn every_registry_name_round_trips_through_lookup() {
        for name in DirectiveName::ALL {
            assert_eq!(DirectiveName::lookup(name.canonical()), Some((name, false)));
        }
        assert_eq!(
            DirectiveName::lookup("@fmt"),
            Some((DirectiveName::Format, true))
        );
    }

    #[test]
    fn data_per_line_zero_disables_consolidation() {
        let opts = FormatOptions {
            data_per_line: 0,
            ..FormatOptions::default()
        };
        assert_eq!(
            format_with(".db $01\n.db $02\n", &opts),
            ".db $01\n.db $02\n"
        );
    }

    #[test]
    fn consolidation_respects_tight_commas() {
        let opts = FormatOptions {
            comma_spacing: false,
            ..FormatOptions::default()
        };
        assert_eq!(format_with(".db $01\n.db $02\n", &opts), ".db $01,$02\n");
    }

    // ── Routine signatures ──────────────────────────────────────────────────

    fn sigs(source: &str) -> Vec<Signature> {
        resolve_signatures(source)
    }

    fn sig_problems(source: &str) -> Vec<SignatureProblem> {
        resolve_signatures_with_errors(source).1
    }

    #[test]
    fn scans_each_signature_tag() {
        let found = scan(
            "; @nessemble-param A index\n\
             ; @nessemble-returns C set on error\n\
             ; @nessemble-clobbers A, X\n",
        );
        assert_eq!(
            found.iter().map(|d| d.name).collect::<Vec<_>>(),
            vec![
                DirectiveName::Param,
                DirectiveName::Returns,
                DirectiveName::Clobbers
            ]
        );
        assert_eq!(
            found[0].args,
            DirectiveArgs::Slot(Slot::A, "index".to_string())
        );
        assert_eq!(
            found[1].args,
            DirectiveArgs::Slot(Slot::Flag(Flag::C), "set on error".to_string())
        );
        assert_eq!(found[2].args, DirectiveArgs::Slots(vec![Slot::A, Slot::X]));
    }

    #[test]
    fn parses_every_slot_kind() {
        let cases = [
            ("A", Slot::A),
            ("X", Slot::X),
            ("Y", Slot::Y),
            ("S", Slot::S),
            ("P", Slot::P),
            ("C", Slot::Flag(Flag::C)),
            ("Z", Slot::Flag(Flag::Z)),
            ("N", Slot::Flag(Flag::N)),
            ("V", Slot::Flag(Flag::V)),
            ("D", Slot::Flag(Flag::D)),
            ("I", Slot::Flag(Flag::I)),
            ("[tmp1]", Slot::Symbol("tmp1".to_string())),
            ("$10", Slot::Address(0x10, 0x10)),
            ("$0300", Slot::Address(0x0300, 0x0300)),
            ("$10-$1F", Slot::Address(0x10, 0x1F)),
        ];
        for (text, expected) in cases {
            let found = scan(&format!("; @nessemble-param {text}\n"));
            assert_eq!(
                found[0].args,
                DirectiveArgs::Slot(expected, String::new()),
                "slot `{text}`"
            );
        }
    }

    #[test]
    fn slot_names_are_case_insensitive_and_render_canonically() {
        let found = scan("; @nessemble-clobbers a, x, c\n");
        assert_eq!(
            found[0].args,
            DirectiveArgs::Slots(vec![Slot::A, Slot::X, Slot::Flag(Flag::C)])
        );
        assert_eq!(format_slots(&[Slot::X, Slot::A]), "A, X");
    }

    #[test]
    fn slot_lists_render_in_canonical_order_whatever_the_input() {
        let slots = vec![
            Slot::Address(0x10, 0x1F),
            Slot::Y,
            Slot::Symbol("tmp".to_string()),
            Slot::Flag(Flag::C),
            Slot::A,
        ];
        assert_eq!(format_slots(&slots), "A, Y, C, [tmp], $10-$1F");
        assert_eq!(format_slots(&[]), "none");
    }

    #[test]
    fn a_description_may_contain_a_semicolon() {
        // For `param`/`returns` the description is free text to end of comment;
        // a `;` in it is prose, not the trailing-prose separator.
        let found = scan("; @nessemble-param A the index; zero-based\n");
        assert_eq!(
            found[0].args,
            DirectiveArgs::Slot(Slot::A, "the index; zero-based".to_string())
        );
    }

    #[test]
    fn description_separators_are_stripped() {
        for text in ["A -- index", "A - index", "A : index", "A index"] {
            let found = scan(&format!("; @nessemble-param {text}\n"));
            assert_eq!(
                found[0].args,
                DirectiveArgs::Slot(Slot::A, "index".to_string()),
                "`{text}`"
            );
        }
    }

    #[test]
    fn clobbers_none_is_an_empty_list_and_a_claim() {
        let found = scan("; @nessemble-clobbers none\nfoo:\n");
        assert_eq!(found[0].args, DirectiveArgs::Slots(Vec::new()));
        let sig = &sigs("; @nessemble-clobbers none\nfoo:\n")[0];
        assert_eq!(sig.clobbers, [] as [crate::tooling::Slot; 0]);
        assert!(sig.declares_clobbers, "`none` is a declaration");
        // No tag at all is *not* the same claim.
        let undeclared = &sigs("; @nessemble-param A i\nfoo:\n")[0];
        assert!(!undeclared.declares_clobbers);
    }

    #[test]
    fn bad_slots_are_bad_arguments() {
        for text in [
            "; @nessemble-clobbers AX\n",
            "; @nessemble-clobbers A, Q\n",
            "; @nessemble-clobbers\n",
            "; @nessemble-clobbers A, none\n",
            "; @nessemble-param\n",
            "; @nessemble-param [] empty\n",
            "; @nessemble-param [a b] spaced\n",
            "; @nessemble-param $1F-$10 inverted\n",
            "; @nessemble-param $12345 too wide\n",
        ] {
            let malformed = bad(text);
            assert_eq!(malformed.len(), 1, "expected bad args for `{text}`");
            assert!(
                matches!(malformed[0].reason, MalformedReason::BadArgs(_)),
                "`{text}`"
            );
        }
    }

    #[test]
    fn binds_across_blank_and_comment_lines() {
        let sig = &sigs(
            "; @nessemble-clobbers A, X\n\
             \n\
             ; called once, from the reset vector\n\
             ppu_reset:\n\
                 RTS\n",
        )[0];
        assert_eq!(sig.name, "ppu_reset");
        assert_eq!(sig.line, 4);
        assert_eq!(sig.first_tag_line, 1);
        assert_eq!(sig.clobbers, vec![Slot::A, Slot::X]);
    }

    #[test]
    fn tags_accumulate_into_one_signature_in_any_order() {
        let sig = &sigs(
            "; @nessemble-clobbers Y\n\
             ; @nessemble-returns C carry set on error\n\
             ; @nessemble-param A index\n\
             ; @nessemble-param X column\n\
             draw:\n",
        )[0];
        assert_eq!(
            sig.params,
            vec![
                (Slot::A, "index".to_string()),
                (Slot::X, "column".to_string())
            ]
        );
        assert_eq!(
            sig.returns,
            vec![(Slot::Flag(Flag::C), "carry set on error".to_string())]
        );
        assert_eq!(sig.clobbers, vec![Slot::Y]);
        // A returned slot is clobbered by definition, and joins declared writes.
        assert_eq!(sig.declared_writes(), vec![Slot::Y, Slot::Flag(Flag::C)]);
    }

    #[test]
    fn a_code_line_breaks_the_binding() {
        let problems = sig_problems("; @nessemble-clobbers A\n    LDA #$00\nfoo:\n");
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].kind, SignatureProblemKind::Unbound);
        assert_eq!(problems[0].subject, "@nessemble-clobbers");
        assert_eq!(
            sigs("; @nessemble-clobbers A\n    LDA #$00\nfoo:\n"),
            [] as [crate::tooling::Signature; 0]
        );
    }

    #[test]
    fn a_tag_at_end_of_file_binds_to_nothing() {
        let problems = sig_problems("foo:\n    RTS\n; @nessemble-clobbers A\n");
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].kind, SignatureProblemKind::Unbound);
    }

    #[test]
    fn a_trailing_tag_is_inert() {
        // `own_line: false` — reported by `ineffective-comment-directive`, and
        // never resolved into a signature (nor reported as unbound here).
        assert_eq!(
            sigs("    LDA #$00 ; @nessemble-clobbers A\nfoo:\n"),
            [] as [crate::tooling::Signature; 0]
        );
        assert_eq!(
            sig_problems("    LDA #$00 ; @nessemble-clobbers A\nfoo:\n"),
            [] as [crate::tooling::SignatureProblem; 0]
        );
    }

    #[test]
    fn a_repeated_slot_is_reported_once_and_kept_once() {
        let src = "; @nessemble-param A first\n\
                   ; @nessemble-param A second\n\
                   ; @nessemble-clobbers X, X\n\
                   foo:\n";
        let (found, problems) = resolve_signatures_with_errors(src);
        assert_eq!(found[0].params, vec![(Slot::A, "first".to_string())]);
        assert_eq!(found[0].clobbers, vec![Slot::X]);
        assert_eq!(problems.len(), 2);
        assert!(problems
            .iter()
            .all(|p| p.kind == SignatureProblemKind::DuplicateSlot));
    }

    #[test]
    fn the_same_slot_may_be_an_input_and_an_output() {
        let sig = &sigs("; @nessemble-param A in\n; @nessemble-returns A out\nfoo:\n")[0];
        assert_eq!(sig.params.len(), 1);
        assert_eq!(sig.returns.len(), 1);
        assert_eq!(
            sig_problems("; @nessemble-param A in\n; @nessemble-returns A out\nfoo:\n"),
            [] as [crate::tooling::SignatureProblem; 0]
        );
    }

    #[test]
    fn binds_to_a_label_that_is_not_a_block_entry() {
        // Tucked directly under the previous routine's RTS: the author plainly
        // meant this label, so it binds. (Whether it *ought* to be documented is
        // `require-routine-doc`'s question, not the resolver's.)
        let sig = &sigs("prev:\n    RTS\n; @nessemble-clobbers A\nnext:\n")[0];
        assert_eq!(sig.name, "next");
    }

    #[test]
    fn each_label_gets_its_own_signature() {
        let found = sigs(
            "; @nessemble-clobbers A\n\
             first:\n\
             \n\
             ; @nessemble-clobbers X\n\
             second:\n",
        );
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].name, "first");
        assert_eq!(found[1].name, "second");
        assert_eq!(found[1].clobbers, vec![Slot::X]);
    }

    #[test]
    fn prose_and_other_directives_are_left_alone() {
        // `@param` without the namespace is prose; a format hint is not a tag.
        let src = "; @param A not ours\n; @nessemble-format stride=2\n.db $01\n";
        assert_eq!(sigs(src), [] as [crate::tooling::Signature; 0]);
        assert_eq!(
            sig_problems(src),
            [] as [crate::tooling::SignatureProblem; 0]
        );
        assert_eq!(bad(src), [] as [crate::tooling::MalformedDirective; 0]);
    }

    // ── Pass 2: blank line after RTS / RTI ──────────────────────────────────

    #[test]
    fn inserts_blank_line_after_rts() {
        assert_eq!(
            format("    RTS\n    LDA #$00\n"),
            "    RTS\n\n    LDA #$00\n"
        );
    }

    #[test]
    fn no_double_blank_after_rts_when_one_follows() {
        assert_eq!(
            format("    RTS\n\n    LDA #$00\n"),
            "    RTS\n\n    LDA #$00\n"
        );
    }

    #[test]
    fn inserts_blank_after_rti_too() {
        assert!(format("    RTI\n    NOP\n").contains("RTI\n\n"));
    }

    // ── Pass 3: collapse blank-line runs ────────────────────────────────────

    #[test]
    fn collapses_more_than_two_blank_lines() {
        assert_eq!(format("    NOP\n\n\n\n    NOP\n"), "    NOP\n\n\n    NOP\n");
    }

    #[test]
    fn keeps_exactly_two_blank_lines() {
        assert_eq!(format("    NOP\n\n\n    NOP\n"), "    NOP\n\n\n    NOP\n");
    }

    #[test]
    fn structural_passes_are_idempotent() {
        let src = "start:\n.db $01\n.db $02\n.db $03\n.db $04\n.db $05\n.db $06\n.db $07\n.db $08\n.db $09\n    RTS\n    NOP\n\n\n\n; end\n";
        let once = format(src);
        assert_eq!(format(&once), once);
    }

    // ── Pass 4: case & literal normalization ────────────────────────────────

    #[test]
    fn mnemonic_case_lowers_and_uppers_only_the_mnemonic() {
        let lower = FormatOptions {
            mnemonic_case: Case::Lower,
            ..FormatOptions::default()
        };
        assert_eq!(format_with("LDA #$00\n", &lower), "    lda #$00\n");
        let upper = FormatOptions {
            mnemonic_case: Case::Upper,
            ..FormatOptions::default()
        };
        assert_eq!(format_with("lda #$00\n", &upper), "    LDA #$00\n");
    }

    #[test]
    fn mnemonic_case_leaves_labels_and_registers_alone() {
        let upper = FormatOptions {
            mnemonic_case: Case::Upper,
            ..FormatOptions::default()
        };
        // A label named like a mnemonic is not an instruction — untouched.
        assert_eq!(format_with("lda:\n", &upper), "lda:\n");
        // The index register `x` in the operand is not the mnemonic.
        assert_eq!(format_with("lda $10, x\n", &upper), "    LDA $10, x\n");
    }

    #[test]
    fn hex_digit_case_normalizes_only_hex_letters() {
        let upper = FormatOptions {
            hex_digit_case: Case::Upper,
            ..FormatOptions::default()
        };
        assert_eq!(format_with(".db $ab, $0f\n", &upper), ".db $AB, $0F\n");
        let lower = FormatOptions {
            hex_digit_case: Case::Lower,
            ..FormatOptions::default()
        };
        assert_eq!(format_with(".db $AB, $0F\n", &lower), ".db $ab, $0f\n");
    }

    #[test]
    fn case_normalization_defaults_to_preserve() {
        assert_eq!(format("LDA #$aB\n"), "    LDA #$aB\n");
    }

    #[test]
    fn case_normalization_is_idempotent() {
        let opts = FormatOptions {
            mnemonic_case: Case::Upper,
            hex_digit_case: Case::Lower,
            ..FormatOptions::default()
        };
        let once = format_with("lda #$AB\nsta $2000\n", &opts);
        assert_eq!(format_with(&once, &opts), once);
    }

    #[test]
    fn case_normalization_preserves_assembled_bytes() {
        // Mnemonics assemble case-insensitively and hex is case-insensitive, so
        // casing is cosmetic. (Instructions must be indented to parse as such.)
        let src = "start:\n    lda #$ab\n    sta $2000\n.db $0f, $a0\n";
        let opts = FormatOptions {
            mnemonic_case: Case::Upper,
            hex_digit_case: Case::Upper,
            ..FormatOptions::default()
        };
        let base = crate::Options::default();
        let original = crate::assemble(src, &base).expect("orig").rom;
        let formatted = crate::assemble(&format_with(src, &opts), &base)
            .expect("fmt")
            .rom;
        assert_eq!(original, formatted);
        // The casing actually changed the text (so the test has teeth).
        assert_ne!(format_with(src, &opts), format(src));
    }

    #[test]
    fn formatting_preserves_assembled_bytes() {
        // The load-bearing safety property: formatting is cosmetic, so the
        // assembled ROM of the formatted source is identical to the original's.
        let src = "\
start:
    LDA #$01
    STA $2000
.db $01
.db $02
.db $03
.db $04
.db $05
.db $06
.db $07
.db $08
.db $09
    RTS
table:
.dw $8000
.dw $C000
.color $0F, $00, $10, $30
";
        let opts = crate::Options::default();
        let original = crate::assemble(src, &opts).expect("original assembles").rom;
        let formatted = crate::assemble(&format(src), &opts)
            .expect("formatted assembles")
            .rom;
        assert_eq!(original, formatted);
        // And the formatter actually changed the layout (so the test has teeth).
        assert_ne!(format(src), src);
    }

    #[test]
    fn formatting_preserves_assembled_bytes_with_anonymous_label_branches() {
        // R2 regression at the assembler level: a branch to an anonymous label
        // (`BNE :-`, `BEQ :+`) must survive a format pass byte-for-byte. The old
        // classifier de-indented these to column 0, and the assembler parsed the
        // de-indented form differently — silently changing the ROM. A branch
        // here starts mis-indented so the formatter must re-indent it.
        let src = "\
start:
    LDX #$05
    LDA #$00
:
      STA $0200
    DEX
        BNE :-
    BEQ :+
    NOP
:
    RTS
";
        let opts = crate::Options::default();
        let original = crate::assemble(src, &opts).expect("original assembles").rom;
        let formatted = crate::assemble(&format(src), &opts)
            .expect("formatted assembles")
            .rom;
        assert_eq!(original, formatted);
        // The formatter re-indented the mis-indented lines, so it had teeth.
        assert_ne!(format(src), src);
        // And the branch is indented, not pinned to column 0.
        assert!(format(src).contains("    BNE :-"));
    }

    #[test]
    fn formatting_preserves_assembled_bytes_with_multiline_continuations() {
        // Continuation-line indentation is insignificant to the assembler (a
        // trailing comma continues the operand list regardless of the leading
        // whitespace), so aligning or block-indenting continuations must not
        // change the assembled bytes. Covers a custom pseudo (`.metasprite`, via
        // a resolver that emits one byte per int) and a built-in list directive
        // (`.hibytes`), with `align_continuations` both on and off.
        fn resolver() -> crate::CustomResolver {
            Box::new(
                |_name: &str,
                 ints: &[i64],
                 _texts: &[String],
                 _dir: &std::path::Path,
                 _root: Option<&std::path::Path>| {
                    Ok(crate::CustomOutput::Bytes(
                        ints.iter().map(|&i| i as u8).collect(),
                    ))
                },
            )
        }
        let src = "\
data:
    .metasprite $FA, $02, $00, $FA,
    $FA, $03, $00, $02,
    $02, $0D, $00, $FA
    .hibytes $8000, $C000,
    $1234, $ABCD
";
        let opts = crate::Options::default();
        let original = crate::assemble_with(src, &opts, resolver())
            .expect("original assembles")
            .rom;
        for align in [true, false] {
            let fopts = FormatOptions {
                align_continuations: align,
                ..FormatOptions::default()
            };
            let formatted = format_with(src, &fopts);
            let out = crate::assemble_with(&formatted, &opts, resolver())
                .expect("formatted assembles")
                .rom;
            assert_eq!(original, out, "align_continuations = {align}");
        }
    }

    #[test]
    fn token_class_wire_ids_are_contiguous_and_name_aligned() {
        // The wire ids are a cross-crate contract (wasm `tokenize`/`token_classes`
        // and the LSP legend): contiguous 0..N in `ALL` order, with stable names.
        for (i, class) in TokenClass::ALL.iter().enumerate() {
            assert_eq!(class.wire_id() as usize, i);
        }
        assert_eq!(TokenClass::Directive.wire_name(), "directive");
        assert_eq!(TokenClass::Instruction.wire_name(), "instruction");
        assert_eq!(TokenClass::Operator.wire_id(), 6);
    }

    #[test]
    fn classify_distinguishes_mnemonics_from_labels() {
        // A mnemonic identifier vs. an ordinary label, case-insensitively.
        assert_eq!(classify(LexKind::Ident, "lda"), TokenClass::Instruction);
        assert_eq!(classify(LexKind::Ident, "LDA"), TokenClass::Instruction);
        assert_eq!(classify(LexKind::Ident, "loop"), TokenClass::Identifier);
        assert_eq!(classify(LexKind::Directive, ".db"), TokenClass::Directive);
        assert_eq!(classify(LexKind::Number, "$00"), TokenClass::Number);
        assert_eq!(classify(LexKind::String, "\"hi\""), TokenClass::String);
        assert_eq!(classify(LexKind::Char, "'x'"), TokenClass::String);
        assert_eq!(classify(LexKind::Comment, "; c"), TokenClass::Comment);
        assert_eq!(classify(LexKind::Punct, "#"), TokenClass::Operator);
    }

    #[test]
    fn highlight_emits_significant_tokens_only() {
        // Whitespace and the newline are dropped; offsets are into the source.
        assert_eq!(
            highlight("lda #$00 ; c\n"),
            vec![
                HlToken {
                    start: 0,
                    len: 3,
                    class: TokenClass::Instruction
                }, // lda
                HlToken {
                    start: 4,
                    len: 1,
                    class: TokenClass::Operator
                }, // #
                HlToken {
                    start: 5,
                    len: 3,
                    class: TokenClass::Number
                }, // $00
                HlToken {
                    start: 9,
                    len: 3,
                    class: TokenClass::Comment
                }, // ; c
            ]
        );
    }

    #[test]
    fn highlight_offsets_are_utf16_not_bytes() {
        // `é` is two UTF-8 bytes but one UTF-16 unit: the token after the
        // multi-byte comment must line up in UTF-16 space (start 4, not 5).
        assert_eq!(
            highlight("; é\nnop\n"),
            vec![
                HlToken {
                    start: 0,
                    len: 3,
                    class: TokenClass::Comment
                }, // ; é
                HlToken {
                    start: 4,
                    len: 3,
                    class: TokenClass::Instruction
                }, // nop
            ]
        );
    }

    // ─── Linting ──────────────────────────────────────────────────────────────

    /// Lint with default severities, the given window, and no ignores.
    fn lint_default(source: &str, window: usize) -> Vec<Finding> {
        let no_ignore = |_: &str| false;
        let opts = LintOptions {
            severities: SeverityMap::default(),
            window,
            ignore: &no_ignore,
        };
        lint(source, &opts)
    }

    fn labels(findings: &[Finding]) -> Vec<&str> {
        findings.iter().map(|f| f.subject.as_str()).collect()
    }

    fn line_of(source: &str, idx: usize) -> Vec<Lexeme> {
        split_lines(&lex(source)).into_iter().nth(idx).unwrap()
    }

    #[test]
    fn block_entry_top_of_file() {
        let src = "label:\n";
        let lines = split_lines(&lex(src));
        assert!(is_block_entry(src, &lines, 0));
    }

    #[test]
    fn block_entry_after_blank_or_comment_run() {
        // blank directly above
        let src = "    nop\n\nlabel:\n";
        let lines = split_lines(&lex(src));
        assert!(is_block_entry(src, &lines, 2));
        // comment run back to the top
        let src = "; a\n; b\nlabel:\n";
        let lines = split_lines(&lex(src));
        assert!(is_block_entry(src, &lines, 2));
        // blank, then a comment, then the label
        let src = "\n; note\nlabel:\n";
        let lines = split_lines(&lex(src));
        assert!(is_block_entry(src, &lines, 2));
    }

    #[test]
    fn block_entry_false_when_code_precedes() {
        let src = "    nop\nlabel:\n";
        let lines = split_lines(&lex(src));
        assert!(!is_block_entry(src, &lines, 1));
        // code, then a comment, then the label → still an internal target
        let src = "    nop\n; c\nlabel:\n";
        let lines = split_lines(&lex(src));
        assert!(!is_block_entry(src, &lines, 2));
    }

    #[test]
    fn has_comment_within_respects_window_and_clamps() {
        let src = "; doc\n    nop\n    nop\nlabel:\n";
        let lines = split_lines(&lex(src));
        // label at idx 3: window 3 reaches idx 0 (comment); window 2 does not.
        assert!(has_comment_within(&lines, 3, 3));
        assert!(!has_comment_within(&lines, 3, 2));
    }

    #[test]
    fn block_label_detects_named_labels_only() {
        assert_eq!(
            block_label("label:", &line_of("label:", 0)).map(|(n, _)| n),
            Some("label")
        );
        // anonymous / temp labels and code-sharing lines are not block labels
        assert!(block_label(":", &line_of(":", 0)).is_none());
        assert!(block_label(":++", &line_of(":++", 0)).is_none());
        assert!(block_label("@local:", &line_of("@local:", 0)).is_none());
        assert!(block_label("label: nop", &line_of("label: nop", 0)).is_none());
        // a trailing comment leaves it a valid block label (Ident + `:`)
        assert_eq!(
            block_label("label: ; hi", &line_of("label: ; hi", 0)).map(|(n, _)| n),
            Some("label")
        );
    }

    #[test]
    fn highlight_classes_a_loop_counter_like_a_macro_argument() {
        let class_of = |src: &str, nth: usize| highlight(src)[nth].class;
        // `\1` is Operator + Number; `\i`/`\j`/`\k` match it.
        for src in ["\\1", "\\i", "\\j", "\\k"] {
            assert_eq!(class_of(src, 0), TokenClass::Operator, "{src}");
            assert_eq!(class_of(src, 1), TokenClass::Number, "{src}");
        }
        // A pasted name keeps its identifier; only the counter is a number.
        let toks = highlight("row\\i");
        assert_eq!(
            toks.iter().map(|t| t.class).collect::<Vec<_>>(),
            vec![
                TokenClass::Identifier,
                TokenClass::Operator,
                TokenClass::Number
            ]
        );
        // A plain `i` (or `\x`) is still an identifier.
        assert_eq!(class_of("i", 0), TokenClass::Identifier);
        assert_eq!(class_of("\\x", 1), TokenClass::Identifier);
    }

    #[test]
    fn block_label_reads_a_pasted_label_as_one_definition() {
        let src = "row\\i:";
        assert_eq!(block_label(src, &line_of(src, 0)), Some(("row\\i", 1)));
        let src = "  tile\\j: ; note";
        assert_eq!(block_label(src, &line_of(src, 0)), Some(("tile\\j", 3)));
        // Not pasted: a gap, an unknown counter letter, or a trailing operand.
        for src in [
            "row \\i:",
            "row\\x:",
            "row\\i: nop",
            "row\\i = 1",
            "row\\1:",
        ] {
            assert!(block_label(src, &line_of(src, 0)).is_none(), "{src}");
        }
    }

    #[test]
    fn lint_warns_once_on_an_undocumented_pasted_label() {
        let findings = lint_default("\n.for 0, 2\n\nrow\\i:\n    NOP\n    RTS\n.endfor\n", 0);
        assert_eq!(labels(&findings), vec!["row\\i"]);
        assert_eq!(findings[0].rule, RuleId::RequireBlockComment);
        assert_eq!(findings[0].line, 4);
        // A nearby comment satisfies the rule for the pasted label as for any.
        let findings = lint_default(
            "\n.for 0, 2\n\n; entry\nrow\\i:\n    NOP\n    RTS\n.endfor\n",
            3,
        );
        assert_eq!(findings, [] as [Finding; 0]);
    }

    #[test]
    fn lint_warns_on_undocumented_block_label() {
        let findings = lint_default("\nmy_label:\n    nop\n", 3);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].subject, "my_label");
        assert_eq!(findings[0].line, 2);
        assert_eq!(findings[0].rule, RuleId::RequireBlockComment);
    }

    #[test]
    fn lint_clean_when_comment_is_near() {
        assert_eq!(
            lint_default("\n; documents the routine\nmy_label:\n    nop\n", 3),
            [] as [crate::tooling::Finding; 0]
        );
        // comment after the label, within the window
        assert_eq!(
            lint_default("\nmy_label:\n    nop\n; trailing doc\n", 3),
            [] as [crate::tooling::Finding; 0]
        );
    }

    #[test]
    fn lint_skips_branch_targets_and_anonymous_labels() {
        // internal branch target (label immediately after code)
        assert_eq!(
            lint_default("    bne target\ntarget:\n    nop\n", 3),
            [] as [crate::tooling::Finding; 0]
        );
        // anonymous labels are never checked
        assert_eq!(
            lint_default("\n:\n    nop\n:++\n", 3),
            [] as [crate::tooling::Finding; 0]
        );
    }

    #[test]
    fn lint_reports_multiple_labels_in_order() {
        let findings = lint_default("\nalpha:\n    nop\n\nbeta:\n    rts\n", 3);
        assert_eq!(labels(&findings), vec!["alpha", "beta"]);
        assert_eq!(
            findings.iter().map(|f| f.line).collect::<Vec<_>>(),
            vec![2, 5]
        );
    }

    #[test]
    fn lint_honors_the_ignore_predicate() {
        let ignore = |name: &str| name.starts_with("loc_");
        let opts = LintOptions {
            severities: SeverityMap::default(),
            window: 3,
            ignore: &ignore,
        };
        // loc_ label is exempt; the other still warns.
        let findings = lint("\nloc_8000:\n    nop\n\nreal_label:\n    nop\n", &opts);
        assert_eq!(labels(&findings), vec!["real_label"]);
    }

    #[test]
    fn lint_off_rule_produces_nothing() {
        let no_ignore = |_: &str| false;
        let mut severities = SeverityMap::default();
        severities.set(RuleId::RequireBlockComment, RuleSeverity::Off);
        let opts = LintOptions {
            severities,
            window: 3,
            ignore: &no_ignore,
        };
        assert_eq!(
            lint("\nmy_label:\n    nop\n", &opts),
            [] as [crate::tooling::Finding; 0]
        );
    }

    // ── Directive rules ─────────────────────────────────────────────────────

    /// Lint with only the directive rules on, so block-comment findings do not
    /// crowd the fixtures.
    fn lint_directives(source: &str) -> Vec<Finding> {
        let no_ignore = |_: &str| false;
        let mut severities = SeverityMap::default();
        severities.set(RuleId::RequireBlockComment, RuleSeverity::Off);
        let opts = LintOptions {
            severities,
            window: 3,
            ignore: &no_ignore,
        };
        lint(source, &opts)
    }

    #[test]
    fn lint_flags_an_unknown_directive_name() {
        let findings = lint_directives("; @nessemble-formt stride=2\n.db $01\n");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule, RuleId::UnknownCommentDirective);
        assert_eq!(findings[0].subject, "@nessemble-formt");
        assert!(findings[0].message.contains("unknown comment directive"));
    }

    #[test]
    fn lint_flags_bad_directive_arguments() {
        let findings = lint_directives("; @nessemble-format stride=x\n.db $01\n");
        assert_eq!(findings[0].rule, RuleId::UnknownCommentDirective);
        assert!(findings[0].message.contains("stride=N[,N,...]"));

        let findings = lint_directives("; @nessemble-coverage-ignore\n    nop\n");
        assert!(findings[0].message.contains("start|end"));

        let findings = lint_directives("; @nessemble-coverage-ignore-next-line now\n    nop\n");
        assert!(findings[0].message.contains("takes no arguments"));
    }

    #[test]
    fn lint_flags_the_deprecated_alias_but_not_the_canonical_name() {
        let findings = lint_directives("; @fmt stride=2\n.db $01, $02\n");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule, RuleId::DeprecatedCommentDirective);
        assert!(findings[0].message.contains("@nessemble-format"));

        assert_eq!(
            lint_directives("; @nessemble-format stride=2\n.db $01, $02\n"),
            [] as [crate::tooling::Finding; 0]
        );
    }

    #[test]
    fn lint_flags_a_trailing_directive_as_ineffective() {
        let findings = lint_directives("    nop ; @nessemble-coverage-ignore-next-line\n    nop\n");
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].rule, RuleId::IneffectiveCommentDirective);
        assert!(findings[0].message.contains("trailing comment"));
    }

    #[test]
    fn lint_flags_a_next_line_directive_with_nothing_to_ignore() {
        let findings = lint_directives("    nop\n; @nessemble-coverage-ignore-next-line\n");
        assert_eq!(findings[0].rule, RuleId::IneffectiveCommentDirective);
        assert!(findings[0].message.contains("no following line"));
        // A comment between the directive and its target is skipped, not fatal.
        assert_eq!(
            lint_directives("; @nessemble-coverage-ignore-next-line\n; why\n    nop\n"),
            [] as [crate::tooling::Finding; 0]
        );
    }

    #[test]
    fn lint_flags_a_stride_hint_with_no_data_run() {
        let findings = lint_directives("; @nessemble-format stride=2\n    nop\n");
        assert_eq!(findings[0].rule, RuleId::IneffectiveCommentDirective);
        assert!(findings[0].message.contains("data line"));
        // `.color` counts as data, as does a blank line before the run.
        assert_eq!(
            lint_directives("; @nessemble-format stride=2\n.color $0F\n"),
            [] as [crate::tooling::Finding; 0]
        );
        assert_eq!(
            lint_directives("; @nessemble-format stride=2\n\n.db $01\n"),
            [] as [crate::tooling::Finding; 0]
        );
    }

    #[test]
    fn lint_flags_unbalanced_ignore_regions() {
        let findings = lint_directives("; @nessemble-coverage-ignore end\n    nop\n");
        assert_eq!(findings[0].rule, RuleId::IneffectiveCommentDirective);
        assert!(findings[0].message.contains("no matching `start`"));

        let src = "; @nessemble-coverage-ignore start\n    nop\n; @nessemble-coverage-ignore start\n    nop\n";
        let findings = lint_directives(src);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].line, 3);
        assert!(findings[0].message.contains("already inside"));
    }

    #[test]
    fn lint_never_flags_an_unclosed_ignore_region() {
        // Running to end of file is the documented whole-file opt-out.
        assert_eq!(
            lint_directives("; @nessemble-coverage-ignore start\n    nop\n    nop\n"),
            [] as [crate::tooling::Finding; 0]
        );
        // …and a balanced pair is clean too.
        assert_eq!(
            lint_directives(
                "; @nessemble-coverage-ignore start\n    nop\n; @nessemble-coverage-ignore end\n"
            ),
            [] as [crate::tooling::Finding; 0]
        );
    }

    #[test]
    fn lint_leaves_ordinary_comments_alone() {
        let src = "; @todo tidy this\n; @param x the value\nmy_label: ; documented\n    nop\n";
        assert_eq!(lint_directives(src), [] as [crate::tooling::Finding; 0]);
    }

    #[test]
    fn lint_directive_rules_are_individually_off_able() {
        let src = "; @fmt stride=2\n    nop\n; @nessemble-formt\n";
        assert_eq!(lint_directives(src).len(), 3);
        for rule in [
            RuleId::UnknownCommentDirective,
            RuleId::DeprecatedCommentDirective,
            RuleId::IneffectiveCommentDirective,
        ] {
            let no_ignore = |_: &str| false;
            let mut severities = SeverityMap::default();
            severities.set(RuleId::RequireBlockComment, RuleSeverity::Off);
            severities.set(rule, RuleSeverity::Off);
            let opts = LintOptions {
                severities,
                window: 3,
                ignore: &no_ignore,
            };
            let findings = lint(src, &opts);
            assert!(
                findings.iter().all(|f| f.rule != rule),
                "{rule:?} still fired"
            );
            assert_eq!(findings.len(), 2, "{rule:?}");
        }
    }

    #[test]
    fn lint_findings_from_all_rules_come_back_in_source_order() {
        let src = "; @nessemble-formt\n\nundocumented:\n    nop\n\n; @fmt stride=2\n.db $01\n";
        let findings = lint_default(src, 1);
        assert_eq!(
            findings.iter().map(|f| f.line).collect::<Vec<_>>(),
            vec![1, 3, 6]
        );
    }

    #[test]
    fn lint_rule_ids_round_trip_and_index_uniquely() {
        for (i, rule) in RuleId::ALL.into_iter().enumerate() {
            assert_eq!(RuleId::from_id(rule.id()), Some(rule));
            assert_eq!(rule.index(), i);
        }
    }

    // ── Routine-signature rules ─────────────────────────────────────────────

    /// Lint with the routine rules on and everything else off, so nothing
    /// crowds the fixtures. `require-routine-doc` follows its shipped default
    /// (off) unless a test asks for it.
    fn lint_routines(source: &str) -> Vec<Finding> {
        lint_routines_with(source, RuleSeverity::Off)
    }

    /// [`lint_routines`] with `require-routine-doc` turned on.
    fn lint_routines_with_doc_rule(source: &str) -> Vec<Finding> {
        lint_routines_with(source, RuleSeverity::Warn)
    }

    fn lint_routines_with(source: &str, doc_rule: RuleSeverity) -> Vec<Finding> {
        let no_ignore = |_: &str| false;
        let mut severities = SeverityMap([RuleSeverity::Off; RuleId::ALL.len()]);
        for rule in RuleId::ALL.into_iter().filter(|r| r.needs_signatures()) {
            severities.set(rule, RuleSeverity::Warn);
        }
        severities.set(RuleId::RequireRoutineDoc, doc_rule);
        let opts = LintOptions {
            severities,
            window: 3,
            ignore: &no_ignore,
        };
        lint(source, &opts)
    }

    /// The rules that fire on `source`, with their messages.
    fn rules_of(findings: &[Finding]) -> Vec<RuleId> {
        findings.iter().map(|f| f.rule).collect()
    }

    #[test]
    fn require_routine_doc_is_off_by_default() {
        // It would fire on every called label in an existing project at once
        // (§13.4); every other rule warns out of the box.
        let defaults = SeverityMap::default();
        assert_eq!(
            defaults.get(RuleId::RequireRoutineDoc),
            RuleSeverity::Off,
            "require-routine-doc must be opt-in"
        );
        for rule in RuleId::ALL
            .into_iter()
            .filter(|r| *r != RuleId::RequireRoutineDoc)
        {
            assert_eq!(defaults.get(rule), RuleSeverity::Warn, "{}", rule.id());
        }
    }

    #[test]
    fn lint_flags_an_unbound_annotation() {
        let findings = lint_routines("; @nessemble-clobbers A\n    LDA #$00\nfoo:\n    RTS\n");
        assert_eq!(rules_of(&findings), vec![RuleId::InvalidRoutineSignature]);
        assert!(findings[0].message.contains("is not followed by a label"));
    }

    #[test]
    fn lint_flags_a_repeated_slot() {
        let findings =
            lint_routines("; @nessemble-param A one\n; @nessemble-param A two\nfoo:\n    RTS\n");
        assert_eq!(rules_of(&findings), vec![RuleId::InvalidRoutineSignature]);
        assert!(findings[0].message.contains("twice"));
    }

    #[test]
    fn lint_flags_an_undeclared_clobber() {
        let findings = lint_routines(
            "; @nessemble-clobbers A\n\
             draw:\n\
             \x20   LDA #$00\n\
             \x20   LDX #$10\n\
             \x20   RTS\n",
        );
        assert_eq!(rules_of(&findings), vec![RuleId::UndeclaredClobber]);
        assert_eq!(findings[0].line, 2, "points at the routine");
        assert_eq!(findings[0].subject, "draw");
        assert!(
            findings[0].message.contains("writes X"),
            "{:?}",
            findings[0]
        );
    }

    #[test]
    fn lint_accepts_a_correct_clobber_list() {
        assert_eq!(
            lint_routines(
                "; @nessemble-clobbers A, X\n\
             draw:\n\
             \x20   LDA #$00\n\
             \x20   LDX #$10\n\
             \x20   RTS\n",
            ),
            [] as [crate::tooling::Finding; 0]
        );
    }

    #[test]
    fn lint_never_checks_an_undeclared_routine() {
        // No clobber list is no claim, so there is nothing to contradict —
        // even with `require-routine-doc` on, an annotated routine is documented.
        let findings = lint_routines(
            "; @nessemble-param A index\n\
             draw:\n\
             \x20   LDX #$10\n\
             \x20   RTS\n",
        );
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn lint_counts_a_returned_slot_as_declared() {
        // A `returns` slot is clobbered by definition and need not be repeated.
        assert_eq!(
            lint_routines(
                "; @nessemble-returns A the value\n\
             ; @nessemble-clobbers none\n\
             read:\n\
             \x20   LDA #$01\n\
             \x20   RTS\n",
            ),
            [] as [crate::tooling::Finding; 0]
        );
    }

    #[test]
    fn lint_catches_a_false_none_claim() {
        let findings = lint_routines(
            "; @nessemble-clobbers none\n\
             touch:\n\
             \x20   LDY #$00\n\
             \x20   RTS\n",
        );
        assert_eq!(rules_of(&findings), vec![RuleId::UndeclaredClobber]);
        assert!(findings[0].message.contains("writes Y"));
    }

    #[test]
    fn the_addressing_mode_decides_whether_a_is_written() {
        // `ASL A` writes A; `ASL $00` does not. The whole reason the effect
        // table is per opcode rather than per mnemonic.
        let shifts_a = lint_routines(
            "; @nessemble-clobbers none\n\
             shift:\n\
             \x20   ASL A\n\
             \x20   RTS\n",
        );
        assert_eq!(rules_of(&shifts_a), vec![RuleId::UndeclaredClobber]);
        assert_eq!(
            lint_routines(
                "; @nessemble-clobbers none\n\
             shift:\n\
             \x20   ASL $00\n\
             \x20   RTS\n",
            ),
            [] as [crate::tooling::Finding; 0]
        );
    }

    #[test]
    fn lint_flags_an_overdeclared_clobber() {
        let findings = lint_routines(
            "; @nessemble-clobbers A, Y\n\
             touch:\n\
             \x20   LDA #$00\n\
             \x20   RTS\n",
        );
        assert_eq!(rules_of(&findings), vec![RuleId::OverdeclaredClobber]);
        assert!(findings[0].message.contains("declares Y"));
    }

    #[test]
    fn an_unknown_body_suppresses_overdeclaration_only() {
        // A macro invocation is an unknown: it could be what writes Y, so the
        // over-declaration is not reported…
        let with_macro = lint_routines(
            "; @nessemble-clobbers A, Y\n\
             touch:\n\
             \x20   LDA #$00\n\
             \x20   do_something\n\
             \x20   RTS\n",
        );
        assert!(with_macro.is_empty(), "{with_macro:?}");
        // …while a definite write it fails to declare still is.
        let undeclared = lint_routines(
            "; @nessemble-clobbers A\n\
             touch:\n\
             \x20   LDA #$00\n\
             \x20   do_something\n\
             \x20   LDX #$00\n\
             \x20   RTS\n",
        );
        assert_eq!(rules_of(&undeclared), vec![RuleId::UndeclaredClobber]);
    }

    #[test]
    fn a_for_loop_in_a_body_is_not_transparent() {
        assert!(!is_transparent_directive(".for"));
        assert!(!is_transparent_directive(".endfor"));
        // Y and X are over-declared, which would be reported if the body were
        // fully understood; the `.for` emits code, so the verifier makes no claim.
        let src = |body: &str| {
            format!("; @nessemble-clobbers A, X, Y\ntouch:\n    LDA #$00\n{body}    RTS\n")
        };
        let plain = lint_routines(&src(""));
        assert_eq!(rules_of(&plain), vec![RuleId::OverdeclaredClobber]);
        let looped = lint_routines(&src(".for 0, 1\n    STA $2007\n.endfor\n"));
        assert_eq!(looped, [] as [Finding; 0]);
    }

    #[test]
    fn a_data_run_in_a_body_is_an_unknown() {
        let findings = lint_routines(
            "; @nessemble-clobbers A, Y\n\
             touch:\n\
             \x20   LDA #$00\n\
             .db $FF\n\
             \x20   RTS\n",
        );
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn an_annotated_callee_inflates_the_caller() {
        let src = "; @nessemble-clobbers A\n\
                   caller:\n\
                   \x20   LDA #$00\n\
                   \x20   JSR callee\n\
                   \x20   RTS\n\
                   \n\
                   ; @nessemble-clobbers X, Y\n\
                   callee:\n\
                   \x20   LDX #$00\n\
                   \x20   LDY #$00\n\
                   \x20   RTS\n";
        let findings = lint_routines(src);
        assert_eq!(rules_of(&findings), vec![RuleId::UndeclaredClobber]);
        assert_eq!(findings[0].subject, "caller");
        assert!(findings[0].message.contains("writes X, Y"));
    }

    #[test]
    fn an_unannotated_callee_is_an_unknown() {
        // The callee's writes are still folded in (they are definite), but the
        // body is not fully understood, so over-declaration stays quiet.
        let src = "; @nessemble-clobbers A, Y\n\
                   caller:\n\
                   \x20   LDA #$00\n\
                   \x20   JSR helper\n\
                   \x20   RTS\n\
                   \n\
                   helper:\n\
                   \x20   RTS\n";
        assert_eq!(lint_routines(src), [] as [crate::tooling::Finding; 0]);
    }

    #[test]
    fn an_indirect_jump_is_an_unknown() {
        let findings = lint_routines(
            "; @nessemble-clobbers A, Y\n\
             dispatch:\n\
             \x20   LDA #$00\n\
             \x20   JMP ($0200)\n",
        );
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn mutual_recursion_terminates() {
        // The cycle counts as an unknown rather than recursing forever.
        let src = "; @nessemble-clobbers A\n\
                   ping:\n\
                   \x20   JSR pong\n\
                   \x20   RTS\n\
                   \n\
                   pong:\n\
                   \x20   JSR ping\n\
                   \x20   RTS\n";
        let findings = lint_routines(src);
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn fall_through_is_a_documented_false_negative() {
        // `first` falls through into `second` and so really does clobber Y, but
        // the body walk stops at the next block entry (§8.1). Pinned so the
        // behavior is documented rather than discovered.
        let src = "; @nessemble-clobbers A\n\
                   first:\n\
                   \x20   LDA #$00\n\
                   \n\
                   second:\n\
                   \x20   LDY #$00\n\
                   \x20   RTS\n";
        let findings = lint_routines(src);
        assert!(
            findings.is_empty(),
            "fall-through is under-approximated: {findings:?}"
        );
    }

    #[test]
    fn lex_reads_a_qualified_local_as_one_identifier() {
        let src = "  JMP p1@x";
        let idents: Vec<&str> = lex(src)
            .iter()
            .filter(|l| l.kind == LexKind::Ident)
            .map(|l| &src[l.start..l.end])
            .collect();
        assert_eq!(idents, vec!["JMP", "p1@x"]);
        let src = "@x@y";
        let idents: Vec<&str> = lex(src)
            .iter()
            .filter(|l| l.kind == LexKind::Ident)
            .map(|l| &src[l.start..l.end])
            .collect();
        assert_eq!(idents, vec!["@x", "@y"]);
    }

    #[test]
    fn format_handles_local_labels() {
        let src = "p1:\n  @loop:\n    DEX\n    BNE @loop\n    JMP p2@x\n";
        let out = format(src);
        assert_eq!(out, "p1:\n@loop:\n    DEX\n    BNE @loop\n    JMP p2@x\n");
        assert_eq!(format(&out), out, "idempotent");
    }

    #[test]
    fn a_local_after_a_blank_line_is_not_a_block() {
        let src = "p1:\n    NOP\n\n@x:\n    RTS\n";
        let lines = split_lines(&lex(src));
        assert!(block_label(src, &lines[3]).is_none());
        // The writes after `@x:` belong to `p1`.
        let findings = lint_routines(
            "; @nessemble-clobbers A\n\
             p1:\n\
             \x20   LDA #$00\n\
             \n\
             @x:\n\
             \x20   LDX #$10\n\
             \x20   RTS\n",
        );
        assert_eq!(rules_of(&findings), vec![RuleId::UndeclaredClobber]);
        assert!(findings[0].message.contains("writes X"), "{findings:?}");
    }

    #[test]
    fn a_jump_to_the_routines_own_local_is_transparent() {
        // Fully understood, so the over-declared `Y` is reported.
        let findings = lint_routines(
            "; @nessemble-clobbers A, Y\n\
             p1:\n\
             @top:\n\
             \x20   LDA #$00\n\
             \x20   JMP @top\n",
        );
        assert_eq!(rules_of(&findings), vec![RuleId::OverdeclaredClobber]);
        // Another routine's local is an unknown, so nothing is over-declared.
        let findings = lint_routines(
            "; @nessemble-clobbers A, Y\n\
             p1:\n\
             \x20   LDA #$00\n\
             \x20   JMP p2@top\n",
        );
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn a_call_to_a_qualified_local_does_not_call_its_parent() {
        let src = "\n\
                   main:\n\
                   \x20   JSR helper@inner\n\
                   \x20   RTS\n\
                   \n\
                   helper:\n\
                   @inner:\n\
                   \x20   RTS\n";
        assert_eq!(lint_routines_with_doc_rule(src), [] as [Finding; 0]);
    }

    #[test]
    fn an_annotation_above_a_local_label_is_reported() {
        let findings = lint_routines("p1:\n; @nessemble-clobbers A\n@x:\n    RTS\n");
        assert_eq!(rules_of(&findings), vec![RuleId::InvalidRoutineSignature]);
        assert_eq!(
            findings[0].message,
            "routine annotation `@nessemble-clobbers` is followed by local label `@x`; \
             annotations bind only to global labels"
        );
        assert_eq!(findings[0].line, 2);
    }

    #[test]
    fn require_routine_doc_flags_only_called_undocumented_routines() {
        let src = "\n\
                   main:\n\
                   \x20   JSR helper\n\
                   \x20   RTS\n\
                   \n\
                   helper:\n\
                   \x20   RTS\n\
                   \n\
                   ; @nessemble-clobbers none\n\
                   documented:\n\
                   \x20   RTS\n";
        let findings = lint_routines_with_doc_rule(src);
        assert_eq!(rules_of(&findings), vec![RuleId::RequireRoutineDoc]);
        assert_eq!(
            findings[0].subject, "helper",
            "`main` is never called and `documented` is annotated"
        );
    }

    #[test]
    fn routine_rules_honor_the_ignore_predicate() {
        let ignore = |name: &str| name.starts_with("loc_");
        let mut severities = SeverityMap([RuleSeverity::Off; RuleId::ALL.len()]);
        severities.set(RuleId::UndeclaredClobber, RuleSeverity::Warn);
        let opts = LintOptions {
            severities,
            window: 3,
            ignore: &ignore,
        };
        let src = "; @nessemble-clobbers none\n\
                   loc_8000:\n\
                   \x20   LDX #$00\n\
                   \x20   RTS\n";
        assert_eq!(lint(src, &opts), [] as [crate::tooling::Finding; 0]);
    }

    // ── Lint suppression (`@nessemble-lint-ignore…`) ────────────────────────

    /// Lint with every rule on, including the opt-in doc rule, so a suppression
    /// fixture has something to suppress.
    fn lint_all(source: &str) -> Vec<Finding> {
        let no_ignore = |_: &str| false;
        let mut severities = SeverityMap::default();
        severities.set(RuleId::RequireRoutineDoc, RuleSeverity::Warn);
        let opts = LintOptions {
            severities,
            window: 3,
            ignore: &no_ignore,
        };
        lint(source, &opts)
    }

    #[test]
    fn a_bare_ignore_suppresses_every_rule_on_the_next_line() {
        let src = "; @nessemble-lint-ignore-next-line\n\
                   ; @nessemble-clobbers A\n\
                   draw:\n\
                   \x20   LDX #$00\n\
                   \x20   RTS\n";
        assert!(lint_routines(src).is_empty(), "{:?}", lint_routines(src));
    }

    #[test]
    fn a_named_ignore_suppresses_only_that_rule() {
        // The routine both over- and under-declares; naming one rule leaves the
        // other reported.
        let src = "; @nessemble-lint-ignore-next-line undeclared-clobber\n\
                   ; @nessemble-clobbers A\n\
                   draw:\n\
                   \x20   LDX #$00\n\
                   \x20   RTS\n";
        assert_eq!(
            rules_of(&lint_routines(src)),
            vec![RuleId::OverdeclaredClobber]
        );

        let both = "; @nessemble-lint-ignore-next-line undeclared-clobber, overdeclared-clobber\n\
                    ; @nessemble-clobbers A\n\
                    draw:\n\
                    \x20   LDX #$00\n\
                    \x20   RTS\n";
        assert_eq!(lint_routines(both), [] as [crate::tooling::Finding; 0]);
    }

    #[test]
    fn a_non_matching_named_ignore_leaves_the_finding_reported() {
        let src = "; @nessemble-lint-ignore-next-line require-block-comment\n\
                   ; @nessemble-clobbers none\n\
                   draw:\n\
                   \x20   LDX #$00\n\
                   \x20   RTS\n";
        assert_eq!(
            rules_of(&lint_routines(src)),
            vec![RuleId::UndeclaredClobber]
        );
    }

    #[test]
    fn next_line_skips_blanks_prose_and_an_annotation_block() {
        // The directive sits above a whole signature block and must still land
        // on the label, which is where the routine rules report.
        let src = "; @nessemble-lint-ignore-next-line undeclared-clobber\n\
                   \n\
                   ; Draw one metasprite.\n\
                   ; @nessemble-param A index\n\
                   ; @nessemble-returns C set on error\n\
                   ; @nessemble-clobbers none\n\
                   draw:\n\
                   \x20   LDY #$00\n\
                   \x20   RTS\n";
        assert!(lint_routines(src).is_empty(), "{:?}", lint_routines(src));
    }

    #[test]
    fn a_region_suppresses_everything_between_its_bounds() {
        let src = "; @nessemble-lint-ignore start\n\
                   ; @nessemble-clobbers A\n\
                   inside:\n\
                   \x20   LDX #$00\n\
                   \x20   RTS\n\
                   ; @nessemble-lint-ignore end\n\
                   \n\
                   ; @nessemble-clobbers A\n\
                   outside:\n\
                   \x20   LDX #$00\n\
                   \x20   RTS\n";
        let findings = lint_routines(src);
        assert!(
            findings.iter().all(|f| f.subject == "outside"),
            "only the routine past `end` reports: {findings:?}"
        );
        assert_ne!(findings, [] as [crate::tooling::Finding; 0]);
    }

    #[test]
    fn a_named_region_suppresses_only_those_rules() {
        let src = "; @nessemble-lint-ignore start overdeclared-clobber\n\
                   ; @nessemble-clobbers A\n\
                   inside:\n\
                   \x20   LDX #$00\n\
                   \x20   RTS\n\
                   ; @nessemble-lint-ignore end\n";
        assert_eq!(
            rules_of(&lint_routines(src)),
            vec![RuleId::UndeclaredClobber]
        );
    }

    #[test]
    fn an_unclosed_region_runs_to_end_of_file() {
        let src = "; @nessemble-lint-ignore start\n\
                   \n\
                   ; @nessemble-clobbers A\n\
                   first:\n\
                   \x20   LDX #$00\n\
                   \x20   RTS\n\
                   \n\
                   ; @nessemble-clobbers A\n\
                   second:\n\
                   \x20   LDY #$00\n\
                   \x20   RTS\n";
        assert!(lint_routines(src).is_empty(), "{:?}", lint_routines(src));
    }

    #[test]
    fn an_unknown_rule_name_is_bad_arguments() {
        for src in [
            "; @nessemble-lint-ignore-next-line no-such-rule\nfoo:\n",
            "; @nessemble-lint-ignore start no-such-rule\nfoo:\n",
            "; @nessemble-lint-ignore sideways\nfoo:\n",
        ] {
            let malformed = bad(src);
            assert_eq!(malformed.len(), 1, "expected bad args for `{src}`");
            assert!(
                matches!(malformed[0].reason, MalformedReason::BadArgs(_)),
                "`{src}`"
            );
        }
        // And the rule reports it, quoting the expected syntax.
        let findings = lint_all("; @nessemble-lint-ignore-next-line no-such-rule\nfoo:\n    RTS\n");
        assert!(findings
            .iter()
            .any(|f| f.rule == RuleId::UnknownCommentDirective
                && f.message.contains("[rule[, rule...]]")));
    }

    #[test]
    fn the_ignore_directives_are_not_themselves_unknown() {
        let src = "; @nessemble-lint-ignore-next-line\n\
                   ; @nessemble-lint-ignore start\n\
                   ; @nessemble-lint-ignore end\n\
                   \n\
                   ; documented\n\
                   foo:\n\
                   \x20   RTS\n";
        assert!(bad(src).is_empty(), "{:?}", bad(src));
        assert!(!lint_all(src)
            .iter()
            .any(|f| f.rule == RuleId::UnknownCommentDirective));
    }

    #[test]
    fn a_dangling_next_line_ignore_is_ineffective() {
        let findings =
            lint_all("\n; documented\nfoo:\n    RTS\n\n; @nessemble-lint-ignore-next-line\n");
        assert!(findings
            .iter()
            .any(|f| f.rule == RuleId::IneffectiveCommentDirective
                && f.message.contains("has no following line to ignore")));
    }

    #[test]
    fn an_unbalanced_ignore_region_is_ineffective() {
        let findings = lint_all("; @nessemble-lint-ignore end\n\n; documented\nfoo:\n    RTS\n");
        assert!(findings
            .iter()
            .any(|f| f.rule == RuleId::IneffectiveCommentDirective
                && f.message.contains("has no matching `start`")));
        // An unclosed `start` is *not* flagged — running to EOF is the
        // documented whole-file opt-out, exactly as for coverage.
        let unclosed = lint_all("; @nessemble-lint-ignore start\n\n; documented\nfoo:\n    RTS\n");
        assert!(!unclosed
            .iter()
            .any(|f| f.rule == RuleId::IneffectiveCommentDirective));
    }

    #[test]
    fn a_trailing_ignore_directive_is_inert() {
        // Consistent with every other directive: on a code line it does nothing,
        // and `ineffective-comment-directive` says so.
        let src = "; @nessemble-clobbers none\n\
                   draw:\n\
                   \x20   LDX #$00 ; @nessemble-lint-ignore-next-line\n\
                   \x20   RTS\n";
        let findings = lint_routines(src);
        assert_eq!(rules_of(&findings), vec![RuleId::UndeclaredClobber]);
    }

    #[test]
    fn suppression_is_independent_of_configured_severity() {
        // Severity is the user's own choice in `.nessemblerc`, so it is not a
        // reason to take away their escape hatch. A suppressed finding does not
        // reach the caller at all, which is what keeps it out of the error
        // count, the warning count, and the exit code alike.
        let src = "; @nessemble-lint-ignore-next-line\n\
                   ; @nessemble-clobbers A\n\
                   draw:\n\
                   \x20   LDX #$00\n\
                   \x20   RTS\n";
        let no_ignore = |_: &str| false;
        for severity in [RuleSeverity::Warn, RuleSeverity::Error] {
            let mut severities = SeverityMap([RuleSeverity::Off; RuleId::ALL.len()]);
            severities.set(RuleId::UndeclaredClobber, severity);
            severities.set(RuleId::OverdeclaredClobber, severity);
            let opts = LintOptions {
                severities,
                window: 3,
                ignore: &no_ignore,
            };
            assert!(
                lint(src, &opts).is_empty(),
                "not suppressed at severity {severity:?}"
            );
        }
    }

    #[test]
    fn suppression_reaches_line_anchored_rules_too() {
        // One mechanism, no special cases: `require-block-comment` is reported
        // at the label, and so are the routine rules.
        let src = "\n; @nessemble-lint-ignore-next-line require-block-comment\nwidget:\n    RTS\n";
        assert!(!lint_all(src)
            .iter()
            .any(|f| f.rule == RuleId::RequireBlockComment));
    }

    #[test]
    fn fall_through_entry_point_can_carry_its_truthful_annotation() {
        // Motivating case 1: `assign_chr_bank` falls through into the next
        // routine, which the body walk does not follow (§8.1), so its truthful
        // `A, X, Y` reads as over-declared. The suppression is what lets the
        // annotation stay truthful.
        let src = "; @nessemble-clobbers A, X, Y, [chr_bank_arg]\n\
                   assign_chr_bank:\n\
                   \x20   STA <chr_bank_arg\n\
                   \n\
                   ; @nessemble-clobbers A, X, Y\n\
                   find_or_evict_chr_slot:\n\
                   \x20   LDA #$00\n\
                   \x20   LDX #$00\n\
                   \x20   LDY #$00\n\
                   \x20   RTS\n";
        assert_eq!(
            rules_of(&lint_routines(src)),
            vec![RuleId::OverdeclaredClobber],
            "the blind spot this exists for"
        );

        let suppressed = format!("; @nessemble-lint-ignore-next-line overdeclared-clobber\n{src}");
        assert!(
            lint_routines(&suppressed).is_empty(),
            "{:?}",
            lint_routines(&suppressed)
        );
    }

    #[test]
    fn save_restore_routine_can_carry_its_truthful_annotation() {
        // Motivating case 2: the `PLA` restore is not modeled, so `A` — which
        // the routine hands back intact — reads as clobbered. Declaring it would
        // invert the tag's documented meaning.
        let src = "; @nessemble-clobbers X, Y, [draw_tmp]\n\
                   ppu_set_xy_addr:\n\
                   \x20   PHA\n\
                   \x20   LDX #$00\n\
                   \x20   LDY #$00\n\
                   \x20   PLA\n\
                   \x20   RTS\n";
        assert_eq!(
            rules_of(&lint_routines(src)),
            vec![RuleId::UndeclaredClobber],
            "the blind spot this exists for"
        );

        let suppressed = format!("; @nessemble-lint-ignore-next-line undeclared-clobber\n{src}");
        assert!(
            lint_routines(&suppressed).is_empty(),
            "{:?}",
            lint_routines(&suppressed)
        );
    }

    #[test]
    fn routine_rules_stay_quiet_on_undocumented_code() {
        // The adoption guarantee: a project that has never heard of these
        // annotations gets nothing from any of the routine rules (bar the
        // opt-in doc rule, which is off by default).
        let src = "\n\
                   ; a hand-rolled convention from before this feature existed\n\
                   ; in: A = index, clobbers X\n\
                   draw:\n\
                   \x20   LDX #$00\n\
                   \x20   RTS\n";
        let mut severities = SeverityMap::default();
        severities.set(RuleId::RequireBlockComment, RuleSeverity::Off);
        let no_ignore = |_: &str| false;
        let opts = LintOptions {
            severities,
            window: 3,
            ignore: &no_ignore,
        };
        assert_eq!(lint(src, &opts), [] as [crate::tooling::Finding; 0]);
    }

    // ─── Unary operators (plans/018-unary-operators.md) ───────────────────────

    #[test]
    fn format_leaves_unary_operators_as_written() {
        for src in [
            "    LDA #-1\n",
            ".db -4, ~$0F, !0\n",
            ".db 4 - -2\n",
            "    BNE :-\n",
        ] {
            assert_eq!(format(src), src);
            assert_eq!(format(&format(src)), src, "idempotent");
        }
    }

    #[test]
    fn format_merges_data_with_unary_values() {
        assert_eq!(format(".db -4\n.db ~1\n"), ".db -4, ~1\n");
    }

    #[test]
    fn highlight_classes_unary_operators_as_operators() {
        let classes: Vec<TokenClass> = highlight("    LDA #-1\n")
            .into_iter()
            .map(|t| t.class)
            .collect();
        assert_eq!(
            classes,
            vec![
                TokenClass::Instruction,
                TokenClass::Operator,
                TokenClass::Operator,
                TokenClass::Number
            ]
        );
        for src in [".db ~1\n", ".db !1\n"] {
            let toks = highlight(src);
            assert_eq!(toks[1].class, TokenClass::Operator, "{src}");
            assert_eq!(toks[2].class, TokenClass::Number, "{src}");
        }
    }

    #[test]
    fn lint_treats_a_negative_immediate_like_its_hex_spelling() {
        let routine = |imm: &str| {
            lint_routines(&format!(
                "; @nessemble-clobbers A\ndraw:\n    LDA #{imm}\n    LDX #{imm}\n    RTS\n"
            ))
        };
        let negative = routine("-1");
        let hex = routine("$FF");
        assert_eq!(rules_of(&negative), rules_of(&hex));
        assert_eq!(rules_of(&negative), vec![RuleId::UndeclaredClobber]);
    }
    // -- `.table` tags (plans/020-index-keyed-tables.md §6) -----------------

    #[test]
    fn format_aligns_table_tags_to_the_widest() {
        let src = ".table OP\n  [ OP_YIELD ] .dw op_yield\n      [OP_DRAW]   .dw op_draw\n\
                   [OP_FRAME_LONG] .dw op_frame\n.endtable\n";
        assert_eq!(
            format(src),
            ".table OP\n[OP_YIELD]      .dw op_yield\n[OP_DRAW]       .dw op_draw\n\
             [OP_FRAME_LONG] .dw op_frame\n.endtable\n"
        );
    }

    #[test]
    fn format_indents_tagged_directives_with_indent_directives() {
        let opts = FormatOptions {
            indent_directives: true,
            ..FormatOptions::default()
        };
        let src = ".table\n[0] .dw a\n[10] .dw b\n.endtable\n";
        assert_eq!(
            format_with(src, &opts),
            "    .table\n    [0]  .dw a\n    [10] .dw b\n    .endtable\n"
        );
    }

    #[test]
    fn format_tagged_instructions_keep_their_indent() {
        let src = ".table\n[0] JMP a\n[10] JMP b\n.endtable\n";
        assert_eq!(
            format(src),
            ".table\n    [0]  JMP a\n    [10] JMP b\n.endtable\n"
        );
    }

    #[test]
    fn format_pads_continuation_lines_of_a_record() {
        let src = ".table\n[0] .db 1\n.lobytes h_a\n; note\nmid:\n[10]\n  .db 2\n.endtable\n";
        assert_eq!(
            format(src),
            ".table\n[0]  .db 1\n     .lobytes h_a\n; note\nmid:\n[10]\n     .db 2\n.endtable\n"
        );
    }

    #[test]
    fn format_aligns_an_operand_list_continued_after_a_tag() {
        let src = ".table\n[0] .db 1,\n2\n[10] .db 3, 4\n.endtable\n";
        let out = format(src);
        assert_eq!(
            out,
            ".table\n[0]  .db 1,\n         2\n[10] .db 3, 4\n.endtable\n"
        );
        assert_eq!(format(&out), out);
    }

    #[test]
    fn format_of_a_table_is_idempotent() {
        let src =
            ".table\n  [ OP_A ] .dw a\n[OP_LONGER]   .dw b\n  .db 1\n[\\i] LDA #1\n.endtable\n";
        let once = format(src);
        assert_eq!(format(&once), once);
    }

    #[test]
    fn format_never_merges_data_across_tags() {
        let src = ".table\n[0] .db 1\n[1] .db 2\n.endtable\n";
        assert_eq!(format(src), ".table\n[0] .db 1\n[1] .db 2\n.endtable\n");
    }

    #[test]
    fn tag_lexemes_counts_the_bracketed_key() {
        let src = "[OBJ + (2)] .db 1";
        let lexemes = lex(src);
        let sig: Vec<&Lexeme> = lexemes
            .iter()
            .filter(|l| l.kind != LexKind::Whitespace)
            .collect();
        assert_eq!(tag_lexemes(src, &sig), 7);
        let src = "JMP [ptr]";
        let lexemes = lex(src);
        let sig: Vec<&Lexeme> = lexemes
            .iter()
            .filter(|l| l.kind != LexKind::Whitespace)
            .collect();
        assert_eq!(tag_lexemes(src, &sig), 0);
    }

    #[test]
    fn a_label_inside_a_table_still_follows_code() {
        let src = "\n; the table\ntbl:\n.table\n[0] .db 1\nmid:\n[1] .db 2\n.endtable\n";
        assert_eq!(lint_default(src, 3), [] as [Finding; 0]);
    }

    #[test]
    fn routine_effects_see_through_tags() {
        let findings = lint_routines(
            "; @nessemble-clobbers A\n\
             draw:\n\
             .table\n\
             [0] LDA #$00\n\
             [1] LDX #$10\n\
             .endtable\n\
             \x20   RTS\n",
        );
        assert_eq!(rules_of(&findings), vec![RuleId::UndeclaredClobber]);
        assert!(
            findings[0].message.contains("writes X"),
            "{:?}",
            findings[0]
        );
        // A tagged `JMP` to an annotated routine is a call like any other.
        let findings = lint_routines(
            "; @nessemble-clobbers Y\n\
             sub:\n\
             \x20   LDY #0\n\
             \x20   RTS\n\
             \n\
             ; @nessemble-clobbers none\n\
             go:\n\
             .table\n\
             [0] JMP sub\n\
             .endtable\n",
        );
        assert_eq!(rules_of(&findings), vec![RuleId::UndeclaredClobber]);
        assert!(
            findings[0].message.contains("writes Y"),
            "{:?}",
            findings[0]
        );
    }
    #[test]
    fn format_keeps_a_data_list_continued_onto_the_next_line() {
        // The trailing comma used to be dropped by consolidation, leaving `2, 3`
        // as a statement of its own, which no longer assembles.
        let src = "lbl:\n  .db 1,\n2, 3\n  .lobytes x\n";
        let out = format(src);
        assert_eq!(out, "lbl:\n.db 1,\n    2, 3\n.lobytes x\n");
        assert_eq!(format(&out), out);
        // Lines around it still consolidate, but never into it.
        let src = ".db 0\n.db 9\n.db 1,\n  2\n.db 3\n.db 4\n";
        assert_eq!(format(src), ".db 0, 9\n.db 1,\n    2\n.db 3, 4\n");
    }

    #[test]
    fn format_preserves_the_bytes_of_a_continued_data_list() {
        let src = "lbl:\n  .db 1,\n2, 3\n  .dw $1234,\n    $5678\n  .color $FF0000,\n$00FF00\n";
        let opts = crate::Options::default();
        let before = crate::assemble(src, &opts).expect("original assembles").rom;
        let after = crate::assemble(&format(src), &opts)
            .expect("formatted assembles")
            .rom;
        assert_eq!(before, after);
    }

    // ── Assertions (plan 022) ───────────────────────────────────────────────

    #[test]
    fn format_keeps_assertion_lines_under_both_indent_settings() {
        let src = "KIND = 1\n.assert KIND == 1 ,\"one\"\n.error \"\\1 \\i\"\n.out \"hi\"\n";
        for indent_directives in [false, true] {
            let opts = FormatOptions {
                indent_directives,
                ..FormatOptions::default()
            };
            let once = format_with(src, &opts);
            let pad = if indent_directives { "    " } else { "" };
            assert_eq!(
                once,
                format!(
                    "KIND = 1\n{pad}.assert KIND == 1, \"one\"\n{pad}.error \"\\1 \\i\"\n{pad}.out \"hi\"\n"
                )
            );
            assert_eq!(format_with(&once, &opts), once);
        }
    }

    #[test]
    fn assertion_directives_are_transparent_to_the_verifier() {
        for d in [".assert", ".error", ".out"] {
            assert!(is_transparent_directive(d));
        }
        // Over-declared Y is reported, so the body is still fully understood.
        let findings = lint_routines(
            "; @nessemble-clobbers A, Y\n\
             touch:\n\
             \x20   LDA #$00\n\
             \x20   .assert touch == $8000, \"fixed\"\n\
             \x20   .out \"touch\"\n\
             \x20   RTS\n",
        );
        assert_eq!(rules_of(&findings), vec![RuleId::OverdeclaredClobber]);
    }

    #[test]
    fn an_assertion_above_a_label_keeps_it_a_block_entry() {
        let src = "    nop\n\n    .assert next == $8000\nnext:\n";
        let lines = split_lines(&lex(src));
        assert!(is_block_entry(src, &lines, 3));
        // The first routine's body ends at the second, so the second's write
        // to X is not charged to the first.
        let two = "; @nessemble-clobbers A\n\
                   first:\n\
                   \x20   LDA #$00\n\
                   \x20   RTS\n\
                   \n\
                   \x20   .assert second == $8005\n\
                   ; @nessemble-clobbers X\n\
                   second:\n\
                   \x20   LDX #$00\n\
                   \x20   RTS\n";
        assert!(lint_routines(two).is_empty(), "{:?}", lint_routines(two));
        // `require-block-comment` sees the same blocks with or without it.
        let block = |src: &str| -> Vec<String> {
            lint_all(src)
                .into_iter()
                .filter(|f| f.rule == RuleId::RequireBlockComment)
                .map(|f| f.subject)
                .collect()
        };
        let with = "a:\n    nop\n\n    .assert 1\nb:\n    nop\n";
        let without = "a:\n    nop\n\nb:\n    nop\n";
        assert_eq!(block(with), block(without));
        assert_eq!(block(with), vec!["a".to_string(), "b".to_string()]);
    }
}
