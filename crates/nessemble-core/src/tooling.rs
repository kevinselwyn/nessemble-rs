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
    /// Consolidate adjacent `.db`/`.dw`/`.color` lines to this many values per
    /// line. `0` disables consolidation (data lines are left as-is).
    pub data_per_line: usize,
    /// Honor `; @fmt stride=N[,N,...]` hint comments that override
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
    let mut lines: Vec<Vec<Lexeme>> = Vec::new();
    let mut current: Vec<Lexeme> = Vec::new();
    for lx in lexemes {
        if lx.kind == LexKind::Newline {
            lines.push(std::mem::take(&mut current));
        } else {
            current.push(lx);
        }
    }
    lines.push(current);

    // Pass 0 — normalize each physical line.
    let indent = opts.indent_unit();
    let mut content: Vec<String> = lines
        .iter()
        .map(|line| format_line(source, line, opts, &indent))
        .collect();

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

    let instruction_line = is_indented(source, &sig);
    let lead = if instruction_line { indent } else { "" };

    // Reconstruct from the first to the last significant lexeme, preserving
    // internal whitespace except around commas (no space before, one after when
    // `comma_spacing`, else tight). Case normalization (Pass 4) is applied here:
    // the mnemonic is the first significant token of an instruction line.
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
            let is_mnemonic = instruction_line
                && !seen_significant
                && lx.kind == LexKind::Ident
                && MNEMONICS.contains(&text(source, lx).to_ascii_lowercase());
            body.push_str(&cased_lexeme(source, lx, opts, is_mnemonic));
            seen_significant = true;
        }
    }

    format!("{lead}{body}").trim_end().to_string()
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

/// Whether a line is an indented instruction line, from its significant
/// lexemes: labels (`name:` / `:`), constant definitions (`name = …`), and
/// directives sit at column 0 (returns `false`); everything else (instructions)
/// is indented (returns `true`).
fn is_indented(source: &str, sig: &[&Lexeme]) -> bool {
    let first = sig[0];
    match first.kind {
        LexKind::Directive => false,
        LexKind::Ident => {
            let is_label = sig.get(1).is_some_and(|l| is_punct(source, l, ":"));
            let is_const = sig.get(1).is_some_and(|l| is_punct(source, l, "="));
            !(is_label || is_const)
        }
        LexKind::Punct if is_punct(source, first, ":") => false,
        _ => true,
    }
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
/// (`name = …`), matching the group-flush rule from the reference formatter.
fn is_label_or_constant(line: &str) -> bool {
    let lexemes = lex(line);
    let sig: Vec<&Lexeme> = lexemes
        .iter()
        .filter(|l| !matches!(l.kind, LexKind::Whitespace | LexKind::Newline))
        .collect();
    if sig.first().is_none_or(|l| l.kind != LexKind::Ident) {
        return false;
    }
    sig.get(1)
        .is_some_and(|l| is_punct(line, l, ":") || is_punct(line, l, "="))
}

/// Parse a `; @fmt stride=N[,N,...]` hint comment into its stride list.
fn parse_hint(line: &str) -> Option<Vec<usize>> {
    let rest = line.trim().strip_prefix(';')?.trim_start();
    let rest = rest.strip_prefix("@fmt")?;
    if !rest.starts_with([' ', '\t']) {
        return None;
    }
    let rest = rest.trim_start().strip_prefix("stride=")?;
    let spec: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == ',')
        .collect();
    // After the stride spec, only whitespace or a trailing comment may follow.
    let tail = rest[spec.len()..].trim_start();
    if !tail.is_empty() && !tail.starts_with(';') {
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
/// lines, honoring `; @fmt stride=N` hints. Mirrors the reference (thrilla)
/// formatter's grouping semantics: a directive-type change, a label/constant,
/// an instruction, a blank line, or a trailing comment all flush the current
/// group; hinted blocks buffer values and re-flow them by their strides.
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
                if let Some(hs) = hint_strides.clone() {
                    flush_hint(&mut out, &mut hint_buffer, &hs, &mut stride_idx, sep);
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
            if let Some(hs) = hint_strides.clone() {
                flush_hint(&mut out, &mut hint_buffer, &hs, &mut stride_idx, sep);
                if consecutive_blanks >= 2 {
                    hint_strides = None;
                    stride_idx = 0;
                }
            } else {
                flush_group(&mut out, &mut group, per, sep);
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
            if let Some(hs) = hint_strides.clone() {
                flush_hint(&mut out, &mut hint_buffer, &hs, &mut stride_idx, sep);
            } else {
                flush_group(&mut out, &mut group, per, sep);
            }
        } else if is_label_or_constant(line) && prev_blanks == 0 {
            // A label/constant butting against data flushes but keeps the hint.
            if let Some(hs) = hint_strides.clone() {
                flush_hint(&mut out, &mut hint_buffer, &hs, &mut stride_idx, sep);
            } else {
                flush_group(&mut out, &mut group, per, sep);
            }
        } else if let Some(hs) = hint_strides.clone() {
            flush_hint(&mut out, &mut hint_buffer, &hs, &mut stride_idx, sep);
            hint_strides = None;
            stride_idx = 0;
        } else {
            flush_group(&mut out, &mut group, per, sep);
        }
        out.push(line.clone());
    }

    flush_group(&mut out, &mut group, per, sep);
    if let Some(hs) = hint_strides.clone() {
        flush_hint(&mut out, &mut hint_buffer, &hs, &mut stride_idx, sep);
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
}
