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

/// Indent applied to instruction lines (labels, directives, and constant
/// definitions stay at column 0), matching the corpus house style.
const INDENT: &str = "    ";

/// Reformat nessemble assembly source: normalize leading indentation, tidy
/// spacing around commas, and trim trailing whitespace, while **preserving
/// comments, other internal spacing, blank lines, and identifier case**. The
/// transform is idempotent.
#[must_use]
pub fn format(source: &str) -> String {
    let lexemes = lex(source);

    // Split into physical lines (a `Newline` ends a line; a trailing newline
    // yields a final empty line, preserving whether the file ends in `\n`).
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

    let formatted: Vec<String> = lines.iter().map(|line| format_line(source, line)).collect();
    formatted.join("\n")
}

fn text<'a>(source: &'a str, lx: &Lexeme) -> &'a str {
    &source[lx.start..lx.end]
}

fn is_punct(source: &str, lx: &Lexeme, s: &str) -> bool {
    lx.kind == LexKind::Punct && text(source, lx) == s
}

fn format_line(source: &str, line: &[Lexeme]) -> String {
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

    let indent = indent_for(source, &sig);

    // Reconstruct from the first to the last significant lexeme, preserving
    // internal whitespace except around commas (no space before, one after).
    let last_sig = line
        .iter()
        .rposition(|l| l.kind != LexKind::Whitespace)
        .unwrap();
    let body_lexemes = &line[first_sig..=last_sig];
    let mut body = String::new();
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
            if k != body_lexemes.len() - 1 {
                body.push(' ');
            }
        } else {
            body.push_str(text(source, lx));
        }
    }

    format!("{indent}{body}").trim_end().to_string()
}

/// Leading indent for a line, from its significant lexemes: labels
/// (`name:` / `:`), constant definitions (`name = …`), and directives sit at
/// column 0; everything else (instructions) is indented.
fn indent_for(source: &str, sig: &[&Lexeme]) -> &'static str {
    let first = sig[0];
    match first.kind {
        LexKind::Directive => "",
        LexKind::Ident => {
            let is_label = sig.get(1).is_some_and(|l| is_punct(source, l, ":"));
            let is_const = sig.get(1).is_some_and(|l| is_punct(source, l, "="));
            if is_label || is_const {
                ""
            } else {
                INDENT
            }
        }
        LexKind::Punct if is_punct(source, first, ":") => "",
        _ => INDENT,
    }
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
    fn format_preserves_trailing_newline_presence() {
        assert_eq!(format("nop"), "    nop");
        assert_eq!(format("nop\n"), "    nop\n");
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
