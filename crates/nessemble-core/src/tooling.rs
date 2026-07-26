//! Lossless, position-preserving lexing plus a source formatter, for editor
//! tooling (the language server's formatting and semantic-token highlighting).
//!
//! Unlike the parity lexer (`crate::lexer`), this scanner is **lossless**: it
//! segments the *entire* input — including whitespace and comments — into
//! [`Lexeme`]s with byte ranges, so the original text can be reconstructed and
//! trivia can be classified for highlighting. It is intentionally separate from
//! the parity lexer, which stays byte-for-byte untouched.

use std::collections::HashSet;
use std::sync::LazyLock;

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
    for lx in lex(source) {
        let piece = &source[lx.start..lx.end];
        let len = utf16_len(piece);
        if !matches!(lx.kind, LexKind::Whitespace | LexKind::Newline) {
            out.push(HlToken {
                start: off,
                len,
                class: classify(lx.kind, piece),
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

    let instruction_line = is_indented(source, &sig, opts);
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
fn is_indented(source: &str, sig: &[&Lexeme], opts: &FormatOptions) -> bool {
    let first = sig[0];
    match first.kind {
        LexKind::Directive => opts.indent_directives,
        LexKind::Ident => {
            let is_const = sig.get(1).is_some_and(|l| is_punct(source, l, "="));
            !(is_named_label(source, sig) || is_const)
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
    sig.first().is_some_and(|l| l.kind == LexKind::Ident)
        && sig.get(1).is_some_and(|l| is_punct(source, l, ":"))
        && sig.get(2).is_none_or(|l| l.kind == LexKind::Comment)
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
    is_named_label(line, &sig) || sig.get(1).is_some_and(|l| is_punct(line, l, "="))
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

    for line in lines {
        if hints_on {
            if let Some(strides) = parse_hint(line) {
                flush_group(&mut out, &mut group, per, sep);
                if let Some(hs) = &hint_strides {
                    flush_hint(&mut out, &mut hint_buffer, hs, &mut stride_idx, sep);
                }
                hint_strides = Some(strides);
                stride_idx = 0;
                consecutive_blanks = 0;
                out.push(line.clone());
                continue;
            }
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
    pub const ALL: [DirectiveName; 3] = [
        DirectiveName::Format,
        DirectiveName::CoverageIgnore,
        DirectiveName::CoverageIgnoreNextLine,
    ];

    /// The canonical (non-deprecated) spelling, including the `@`.
    #[must_use]
    pub fn canonical(self) -> &'static str {
        match self {
            DirectiveName::Format => "@nessemble-format",
            DirectiveName::CoverageIgnore => "@nessemble-coverage-ignore",
            DirectiveName::CoverageIgnoreNextLine => "@nessemble-coverage-ignore-next-line",
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
        }
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

/// A directive's parsed arguments. Parsing happens once, in the scanner, so no
/// consumer re-parses argument text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectiveArgs {
    /// `@nessemble-format stride=N[,N,…]`; never empty.
    Strides(Vec<usize>),
    /// `@nessemble-coverage-ignore start` / `end`.
    Region(RegionBound),
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
    // Arguments are what remains before any trailing prose comment.
    let args = body[token.len()..].split(';').next().unwrap_or("").trim();

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

/// Parse a directive's argument text (trailing prose already stripped), or
/// `None` if it does not match the directive's syntax.
fn parse_args(name: DirectiveName, args: &str) -> Option<DirectiveArgs> {
    match name {
        DirectiveName::Format => parse_strides(args).map(DirectiveArgs::Strides),
        DirectiveName::CoverageIgnore => match args {
            "start" => Some(DirectiveArgs::Region(RegionBound::Start)),
            "end" => Some(DirectiveArgs::Region(RegionBound::End)),
            _ => None,
        },
        DirectiveName::CoverageIgnoreNextLine => args.is_empty().then_some(DirectiveArgs::None),
    }
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
}

impl RuleId {
    /// Every rule, in a stable order (also the [`SeverityMap`] index order).
    pub const ALL: [RuleId; 4] = [
        RuleId::RequireBlockComment,
        RuleId::UnknownCommentDirective,
        RuleId::DeprecatedCommentDirective,
        RuleId::IneffectiveCommentDirective,
    ];

    /// The stable kebab-case identifier used in `.nessemblerc` and in reports.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            RuleId::RequireBlockComment => "require-block-comment",
            RuleId::UnknownCommentDirective => "unknown-comment-directive",
            RuleId::DeprecatedCommentDirective => "deprecated-comment-directive",
            RuleId::IneffectiveCommentDirective => "ineffective-comment-directive",
        }
    }

    /// Parse a rule id string, returning `None` if it names no known rule.
    #[must_use]
    pub fn from_id(s: &str) -> Option<RuleId> {
        RuleId::ALL.into_iter().find(|r| r.id() == s)
    }

    /// This rule's index into a [`SeverityMap`].
    fn index(self) -> usize {
        match self {
            RuleId::RequireBlockComment => 0,
            RuleId::UnknownCommentDirective => 1,
            RuleId::DeprecatedCommentDirective => 2,
            RuleId::IneffectiveCommentDirective => 3,
        }
    }

    /// Whether this rule reads the directive scan rather than the lexeme lines.
    fn is_directive_rule(self) -> bool {
        !matches!(self, RuleId::RequireBlockComment)
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

/// Per-rule severities, one slot per [`RuleId`]. Defaults to every rule at
/// [`RuleSeverity::Warn`] — the linter is on out of the box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeverityMap([RuleSeverity; RuleId::ALL.len()]);

impl Default for SeverityMap {
    fn default() -> Self {
        SeverityMap([RuleSeverity::Warn; RuleId::ALL.len()])
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
/// directive rules — the one directive scan, shared so the rules that need it
/// do not each re-scan the buffer.
struct LintCtx<'a> {
    source: &'a str,
    lines: &'a [Vec<Lexeme>],
    directives: &'a [Directive],
    malformed: &'a [MalformedDirective],
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
];

/// Lint `source`, returning findings sorted by position. Every rule whose
/// severity is not [`RuleSeverity::Off`] is run. This never mutates source.
#[must_use]
pub fn lint(source: &str, opts: &LintOptions) -> Vec<Finding> {
    let lexemes = lex(source);
    let lines = split_lines(&lexemes);
    // The directive scan is shared by three rules, and skipped entirely when all
    // of them are off.
    let scan_needed = RuleId::ALL
        .into_iter()
        .any(|r| r.is_directive_rule() && opts.severities.get(r) != RuleSeverity::Off);
    let (directives, malformed) = if scan_needed {
        scan_directive_lines(source, &lines)
    } else {
        (Vec::new(), Vec::new())
    };
    let ctx = LintCtx {
        source,
        lines: &lines,
        directives: &directives,
        malformed: &malformed,
    };
    let mut findings = Vec::new();
    for &(rule, run) in RULES {
        if opts.severities.get(rule) != RuleSeverity::Off {
            run(&ctx, opts, &mut findings);
        }
    }
    findings.sort_by_key(|f| (f.line, f.column));
    findings
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
        if !is_block_entry(ctx.lines, idx) {
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
    let mut region_open = false;
    for d in ctx.directives {
        let token = &ctx.source[d.start..d.end];
        let reason = if d.own_line {
            match (d.name, &d.args) {
                (DirectiveName::CoverageIgnoreNextLine, _) => {
                    significant_line_after(ctx.lines, d.line)
                        .is_none()
                        .then(|| "has no following line to ignore".to_string())
                }
                (DirectiveName::Format, _) => significant_line_after(ctx.lines, d.line)
                    .filter(|&idx| is_data_line(ctx.source, &ctx.lines[idx]))
                    .is_none()
                    .then(|| "is not followed by a data line (`.db`, `.dw`, `.color`)".to_string()),
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

/// Whether a line's first significant token is a data directive (`.db`, `.dw`,
/// `.color`) — the thing a stride hint applies to.
fn is_data_line(source: &str, line: &[Lexeme]) -> bool {
    line.iter()
        .find(|l| l.kind != LexKind::Whitespace)
        .filter(|l| l.kind == LexKind::Directive)
        .and_then(|l| text(source, l).strip_prefix('.'))
        .is_some_and(is_data_directive)
}

/// If `line` is exactly a named label definition (`name:`), return the label
/// name and its 1-based character column. Anonymous labels (`:`, `:++`), `@local`
/// temp labels, and labels sharing a line with code all return `None`.
fn block_label<'a>(source: &'a str, line: &[Lexeme]) -> Option<(&'a str, u32)> {
    let sig: Vec<&Lexeme> = line
        .iter()
        .filter(|l| l.kind != LexKind::Whitespace && l.kind != LexKind::Comment)
        .collect();
    if sig.len() != 2 || sig[0].kind != LexKind::Ident || !is_punct(source, sig[1], ":") {
        return None;
    }
    let name = text(source, sig[0]);
    // Match the "document every code block" convention: a real label name starts
    // with an ASCII letter or `_`, so `@local` temp labels read as anonymous.
    let first = name.as_bytes().first().copied().unwrap_or(0);
    if !(first.is_ascii_alphabetic() || first == b'_') {
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
fn is_block_entry(lines: &[Vec<Lexeme>], idx: usize) -> bool {
    for i in (0..idx).rev() {
        if is_blank(&lines[i]) {
            return true;
        }
        if is_comment_only(&lines[i]) {
            continue;
        }
        return false;
    }
    true
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
    fn format_ensures_a_final_newline_by_default() {
        // Pass 5 (finalNewline, default on) adds the missing trailing newline.
        assert_eq!(format("nop"), "    nop\n");
        assert_eq!(format("nop\n"), "    nop\n");
    }

    #[test]
    fn format_with_final_newline_off_preserves_presence() {
        let opts = FormatOptions {
            final_newline: false,
            ..FormatOptions::default()
        };
        assert_eq!(format_with("nop", &opts), "    nop");
        assert_eq!(format_with("nop\n", &opts), "    nop\n");
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
        assert!(bad("; @nessemble-coverage-ignore-next-line\n").is_empty());
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
                |_name: &str, ints: &[i64], _texts: &[String], _dir: &std::path::Path| {
                    Ok(ints.iter().map(|&i| i as u8).collect())
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
        let lines = split_lines(&lex("label:\n"));
        assert!(is_block_entry(&lines, 0));
    }

    #[test]
    fn block_entry_after_blank_or_comment_run() {
        // blank directly above
        let lines = split_lines(&lex("    nop\n\nlabel:\n"));
        assert!(is_block_entry(&lines, 2));
        // comment run back to the top
        let lines = split_lines(&lex("; a\n; b\nlabel:\n"));
        assert!(is_block_entry(&lines, 2));
        // blank, then a comment, then the label
        let lines = split_lines(&lex("\n; note\nlabel:\n"));
        assert!(is_block_entry(&lines, 2));
    }

    #[test]
    fn block_entry_false_when_code_precedes() {
        let lines = split_lines(&lex("    nop\nlabel:\n"));
        assert!(!is_block_entry(&lines, 1));
        // code, then a comment, then the label → still an internal target
        let lines = split_lines(&lex("    nop\n; c\nlabel:\n"));
        assert!(!is_block_entry(&lines, 2));
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
    fn lint_warns_on_undocumented_block_label() {
        let findings = lint_default("\nmy_label:\n    nop\n", 3);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].subject, "my_label");
        assert_eq!(findings[0].line, 2);
        assert_eq!(findings[0].rule, RuleId::RequireBlockComment);
    }

    #[test]
    fn lint_clean_when_comment_is_near() {
        assert!(lint_default("\n; documents the routine\nmy_label:\n    nop\n", 3).is_empty());
        // comment after the label, within the window
        assert!(lint_default("\nmy_label:\n    nop\n; trailing doc\n", 3).is_empty());
    }

    #[test]
    fn lint_skips_branch_targets_and_anonymous_labels() {
        // internal branch target (label immediately after code)
        assert!(lint_default("    bne target\ntarget:\n    nop\n", 3).is_empty());
        // anonymous labels are never checked
        assert!(lint_default("\n:\n    nop\n:++\n", 3).is_empty());
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
        assert!(lint("\nmy_label:\n    nop\n", &opts).is_empty());
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

        assert!(lint_directives("; @nessemble-format stride=2\n.db $01, $02\n").is_empty());
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
        assert!(
            lint_directives("; @nessemble-coverage-ignore-next-line\n; why\n    nop\n").is_empty()
        );
    }

    #[test]
    fn lint_flags_a_stride_hint_with_no_data_run() {
        let findings = lint_directives("; @nessemble-format stride=2\n    nop\n");
        assert_eq!(findings[0].rule, RuleId::IneffectiveCommentDirective);
        assert!(findings[0].message.contains("data line"));
        // `.color` counts as data, as does a blank line before the run.
        assert!(lint_directives("; @nessemble-format stride=2\n.color $0F\n").is_empty());
        assert!(lint_directives("; @nessemble-format stride=2\n\n.db $01\n").is_empty());
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
        assert!(
            lint_directives("; @nessemble-coverage-ignore start\n    nop\n    nop\n").is_empty()
        );
        // …and a balanced pair is clean too.
        assert!(lint_directives(
            "; @nessemble-coverage-ignore start\n    nop\n; @nessemble-coverage-ignore end\n"
        )
        .is_empty());
    }

    #[test]
    fn lint_leaves_ordinary_comments_alone() {
        let src = "; @todo tidy this\n; @param x the value\nmy_label: ; documented\n    nop\n";
        assert!(lint_directives(src).is_empty());
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
}
