//! Hand-written lexer for the nessemble assembly language.
//!
//! Token kinds and lexing rules mirror the reference `nessemble.l` (flex)
//! grammar, including its longest-match / rule-order tie-breaking, so the token
//! stream is behaviorally identical. A hand-written lexer (rather than `logos`)
//! is used here for precise control over the context-sensitive pieces
//! (leading-indentation, `<` as a zero-page prefix vs. comparison operator, and
//! bracket-based indirect addressing).

/// A lexical token kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tok {
    Endl,
    Indent,

    Plus,
    Minus,
    Pow,
    Mult,
    Div,
    And,
    Or,
    Xor,
    Rshift,
    Lshift,
    Mod,
    DblEqu,
    NotEqu,

    Hash,
    Comma,
    Equ,
    Colon,
    OpenParen,
    CloseParen,
    OpenBrack,
    CloseBrack,
    Lt,
    Gt,
    Lte,
    Gte,
    Arrow,

    High,
    Low,
    Bank,

    /// A pseudo-op directive, e.g. `.db`; stored without the leading dot,
    /// lower-cased is *not* applied (kept verbatim after the dot).
    Pseudo(String),

    Number(i64),
    /// Macro argument reference `\N` (Phase 4).
    NumberArg(i64),
    /// Macro argument count `\#` (Phase 4).
    NumberArgc,
    /// Macro unique id `\@` (Phase 4).
    NumberArgu,

    Text(String),
    QuotString(String),
    CharReg(char),
}

/// A token together with its 1-based source line and source-file id.
///
/// The `file` id indexes a table of file display names maintained by the
/// preprocessor (see `preprocess`); the lexer itself always emits `0` and the
/// preprocessor rewrites it as it splices includes and expands macros.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub tok: Tok,
    pub line: u32,
    pub file: u32,
}

/// Known pseudo-op names (without the leading dot).
fn is_ident_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

fn is_hex(c: u8) -> bool {
    c.is_ascii_hexdigit()
}

/// The lexer over a byte slice.
pub struct Lexer<'a> {
    src: &'a [u8],
    pos: usize,
    line: u32,
    at_line_start: bool,
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str) -> Self {
        Lexer {
            src: src.as_bytes(),
            pos: 0,
            line: 1,
            at_line_start: true,
        }
    }

    /// Tokenize the whole input.
    pub fn tokenize(mut self) -> Vec<Token> {
        let mut out = Vec::new();
        while let Some(t) = self.next_token() {
            out.push(t);
        }
        out
    }

    fn peek(&self, off: usize) -> Option<u8> {
        self.src.get(self.pos + off).copied()
    }

    fn emit(&self, tok: Tok) -> Option<Token> {
        Some(Token {
            tok,
            line: self.line,
            file: 0,
        })
    }

    fn next_token(&mut self) -> Option<Token> {
        loop {
            let c = self.peek(0)?;

            // Leading indentation at the start of a line.
            if self.at_line_start && (c == b' ' || c == b'\t') {
                while matches!(self.peek(0), Some(b' ') | Some(b'\t')) {
                    self.pos += 1;
                }
                self.at_line_start = false;
                return self.emit(Tok::Indent);
            }
            self.at_line_start = false;

            match c {
                b'\n' => {
                    let line = self.line;
                    self.pos += 1;
                    self.line += 1;
                    self.at_line_start = true;
                    return Some(Token {
                        tok: Tok::Endl,
                        line,
                        file: 0,
                    });
                }
                b'\r' => {
                    // Mirror the reference: a lone CR is treated as end-of-line.
                    let line = self.line;
                    self.pos += 1;
                    return Some(Token {
                        tok: Tok::Endl,
                        line,
                        file: 0,
                    });
                }
                b' ' | b'\t' => {
                    self.pos += 1;
                    continue;
                }
                b';' => {
                    while !matches!(self.peek(0), None | Some(b'\n')) {
                        self.pos += 1;
                    }
                    continue;
                }
                _ => {}
            }

            return Some(self.lex_significant());
        }
    }

    fn lex_significant(&mut self) -> Token {
        // Multi-character operators first (longest match).
        if let Some(t) = self.lex_operator() {
            return t;
        }
        // Numbers, identifiers, strings, registers — flex longest-match order.
        if let Some(t) = self.lex_number_or_word() {
            return t;
        }
        // Unknown single character: skip it and treat as end-of-line only for CR
        // (handled above). Everything else is consumed as an isolated char-reg
        // fallback to avoid infinite loops.
        let c = self.peek(0).unwrap_or(b' ');
        self.pos += 1;
        Token {
            tok: Tok::CharReg(c.to_ascii_uppercase() as char),
            line: self.line,
            file: 0,
        }
    }

    fn lex_operator(&mut self) -> Option<Token> {
        let two = (self.peek(0), self.peek(1));
        let (tok, len) = match two {
            (Some(b'*'), Some(b'*')) => (Tok::Pow, 2),
            (Some(b'>'), Some(b'>')) => (Tok::Rshift, 2),
            (Some(b'<'), Some(b'<')) => (Tok::Lshift, 2),
            (Some(b'='), Some(b'=')) => (Tok::DblEqu, 2),
            (Some(b'!'), Some(b'=')) => (Tok::NotEqu, 2),
            (Some(b'<'), Some(b'=')) => (Tok::Lte, 2),
            (Some(b'>'), Some(b'=')) => (Tok::Gte, 2),
            (Some(b'-'), Some(b'>')) => (Tok::Arrow, 2),
            _ => {
                let one = self.peek(0)?;
                let single = match one {
                    b'+' => Tok::Plus,
                    b'-' => Tok::Minus,
                    b'*' => Tok::Mult,
                    b'/' => Tok::Div,
                    b'&' => Tok::And,
                    b'|' => Tok::Or,
                    b'^' => Tok::Xor,
                    // `%` is a binary-literal prefix when followed by 0/1,
                    // otherwise the modulo operator.
                    b'%' => {
                        if matches!(self.peek(1), Some(b'0') | Some(b'1')) {
                            return None;
                        }
                        Tok::Mod
                    }
                    b'#' => Tok::Hash,
                    b',' => Tok::Comma,
                    b'=' => Tok::Equ,
                    b':' => Tok::Colon,
                    b'(' => Tok::OpenParen,
                    b')' => Tok::CloseParen,
                    b'[' => Tok::OpenBrack,
                    b']' => Tok::CloseBrack,
                    b'<' => Tok::Lt,
                    b'>' => Tok::Gt,
                    _ => return None,
                };
                (single, 1)
            }
        };
        let line = self.line;
        self.pos += len;
        Some(Token { tok, line, file: 0 })
    }

    /// Longest-match over the numeric/identifier/string/register rules, with
    /// flex rule-order tie-breaking.
    fn lex_number_or_word(&mut self) -> Option<Token> {
        let start = self.pos;
        let rest = &self.src[start..];

        // Each candidate: (length, token). We pick the longest; on ties the
        // earliest rule (in flex order) wins, which we get by only replacing on
        // strictly-greater length.
        let mut best: Option<(usize, Tok)> = None;
        let consider = |len: usize, tok: Tok, best: &mut Option<(usize, Tok)>| {
            if len == 0 {
                return;
            }
            match best {
                Some((blen, _)) if *blen >= len => {}
                _ => *best = Some((len, tok)),
            }
        };

        // Rule order matters for ties: evaluate earliest-first, and only replace
        // on strictly greater length (so earlier rules win ties).
        // 1. $hex
        if rest.first() == Some(&b'$') {
            let n = rest[1..].iter().take_while(|c| is_hex(**c)).count();
            if n > 0 {
                let v = i64::from_str_radix(std::str::from_utf8(&rest[1..1 + n]).unwrap(), 16)
                    .unwrap_or(0);
                consider(1 + n, Tok::Number(v), &mut best);
            }
        }
        // 2. hex + 'h'
        {
            let n = rest.iter().take_while(|c| is_hex(**c)).count();
            if n > 0 && rest.get(n) == Some(&b'h') {
                let v =
                    i64::from_str_radix(std::str::from_utf8(&rest[..n]).unwrap(), 16).unwrap_or(0);
                consider(n + 1, Tok::Number(v), &mut best);
            }
        }
        // 3. %bin
        if rest.first() == Some(&b'%') {
            let n = rest[1..]
                .iter()
                .take_while(|c| **c == b'0' || **c == b'1')
                .count();
            if n > 0 {
                let v = i64::from_str_radix(std::str::from_utf8(&rest[1..1 + n]).unwrap(), 2)
                    .unwrap_or(0);
                consider(1 + n, Tok::Number(v), &mut best);
            }
        }
        // 4. bin + 'b'
        {
            let n = rest
                .iter()
                .take_while(|c| **c == b'0' || **c == b'1')
                .count();
            if n > 0 && rest.get(n) == Some(&b'b') {
                let v =
                    i64::from_str_radix(std::str::from_utf8(&rest[..n]).unwrap(), 2).unwrap_or(0);
                consider(n + 1, Tok::Number(v), &mut best);
            }
        }
        // 5. 0[0-7]+  (octal, leading zero)
        if rest.first() == Some(&b'0') {
            let n = rest[1..]
                .iter()
                .take_while(|c| (b'0'..=b'7').contains(*c))
                .count();
            if n > 0 {
                let v = i64::from_str_radix(std::str::from_utf8(&rest[1..1 + n]).unwrap(), 8)
                    .unwrap_or(0);
                consider(1 + n, Tok::Number(v), &mut best);
            }
        }
        // 6. [0-7]+ 'o'
        {
            let n = rest
                .iter()
                .take_while(|c| (b'0'..=b'7').contains(c))
                .count();
            if n > 0 && rest.get(n) == Some(&b'o') {
                let v =
                    i64::from_str_radix(std::str::from_utf8(&rest[..n]).unwrap(), 8).unwrap_or(0);
                consider(n + 1, Tok::Number(v), &mut best);
            }
        }
        // 7. [0-9]+ decimal
        {
            let n = rest.iter().take_while(|c| c.is_ascii_digit()).count();
            if n > 0 {
                let v: i64 = std::str::from_utf8(&rest[..n])
                    .unwrap()
                    .parse()
                    .unwrap_or(0);
                consider(n, Tok::Number(v), &mut best);
            }
        }
        // 8. [0-9]+ 'd'
        {
            let n = rest.iter().take_while(|c| c.is_ascii_digit()).count();
            if n > 0 && rest.get(n) == Some(&b'd') {
                let v: i64 = std::str::from_utf8(&rest[..n])
                    .unwrap()
                    .parse()
                    .unwrap_or(0);
                consider(n + 1, Tok::Number(v), &mut best);
            }
        }
        // 9. '.' char literal
        if rest.first() == Some(&b'\'') && rest.len() >= 3 && rest[2] == b'\'' {
            consider(3, Tok::Number(rest[1] as i64), &mut best);
        }
        // 10. \N macro arg  11. \#  12. \@
        if rest.first() == Some(&b'\\') {
            let n = rest[1..].iter().take_while(|c| c.is_ascii_digit()).count();
            if n > 0 {
                let v: i64 = std::str::from_utf8(&rest[1..1 + n])
                    .unwrap()
                    .parse()
                    .unwrap_or(0);
                consider(1 + n, Tok::NumberArg(v), &mut best);
            } else if rest.get(1) == Some(&b'#') {
                consider(2, Tok::NumberArgc, &mut best);
            } else if rest.get(1) == Some(&b'@') {
                consider(2, Tok::NumberArgu, &mut best);
            }
        }
        // 13. defchr [.0-3]{8}
        if rest.len() >= 8
            && rest[..8]
                .iter()
                .all(|c| *c == b'.' || (b'0'..=b'3').contains(c))
        {
            let digits: String = rest[..8]
                .iter()
                .map(|c| if *c == b'.' { '0' } else { *c as char })
                .collect();
            let v: i64 = digits.parse().unwrap_or(0);
            consider(8, Tok::Number(v), &mut best);
        }
        // 14. identifier / pseudo / keyword: [.@a-zA-Z_][a-zA-Z0-9_]+
        {
            let first = rest.first().copied();
            let starts = matches!(first, Some(b'.') | Some(b'@') | Some(b'_'))
                || first.map(|c| c.is_ascii_alphabetic()).unwrap_or(false);
            if starts {
                let n = 1 + rest[1..].iter().take_while(|c| is_ident_char(**c)).count();
                if n >= 2 {
                    let word = std::str::from_utf8(&rest[..n]).unwrap();
                    let tok = classify_word(word);
                    consider(n, tok, &mut best);
                }
            }
        }
        // 15. quoted string
        if rest.first() == Some(&b'"') {
            if let Some(close) = rest[1..].iter().position(|c| *c == b'"') {
                let s = std::str::from_utf8(&rest[..close + 2]).unwrap().to_string();
                consider(close + 2, Tok::QuotString(s), &mut best);
            }
        }
        // 16. single char register
        {
            let first = rest.first().copied();
            if first.map(|c| c.is_ascii_alphabetic()).unwrap_or(false) {
                consider(
                    1,
                    Tok::CharReg((first.unwrap() as char).to_ascii_uppercase()),
                    &mut best,
                );
            }
        }
        // A lone '.' that starts a custom pseudo like `.x` (single letter) — flex
        // `\.[a-zA-Z]+` allows a one-letter directive; the identifier rule needs
        // >=2 chars, so handle a `.` + letters directive of length >= 2 already
        // covered above; a bare `.` is not valid input.

        let (len, tok) = best?;
        let line = self.line;
        self.pos += len;
        Some(Token { tok, line, file: 0 })
    }
}

/// Classify a `[.@a-zA-Z_][a-zA-Z0-9_]+` word into a keyword, pseudo, or text.
fn classify_word(word: &str) -> Tok {
    match word {
        "HIGH" => Tok::High,
        "LOW" => Tok::Low,
        "BANK" => Tok::Bank,
        _ => {
            if let Some(name) = word.strip_prefix('.') {
                Tok::Pseudo(name.to_string())
            } else {
                Tok::Text(word.to_string())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<Tok> {
        Lexer::new(src)
            .tokenize()
            .into_iter()
            .map(|t| t.tok)
            .collect()
    }

    #[test]
    fn number_bases() {
        assert_eq!(kinds("165"), vec![Tok::Number(165)]);
        assert_eq!(kinds("$A5"), vec![Tok::Number(165)]);
        assert_eq!(kinds("A5h"), vec![Tok::Number(165)]);
        assert_eq!(kinds("%10100101"), vec![Tok::Number(165)]);
        assert_eq!(kinds("10100101b"), vec![Tok::Number(165)]);
        assert_eq!(kinds("0245"), vec![Tok::Number(165)]);
        assert_eq!(kinds("245o"), vec![Tok::Number(165)]);
        assert_eq!(kinds("'A'"), vec![Tok::Number(65)]);
    }

    #[test]
    fn instruction_line() {
        assert_eq!(
            kinds("    LDA #$c0"),
            vec![
                Tok::Indent,
                Tok::Text("LDA".into()),
                Tok::Hash,
                Tok::Number(0xC0)
            ]
        );
    }

    #[test]
    fn zeropage_vs_comparison() {
        assert_eq!(
            kinds("    LDA <$44, X"),
            vec![
                Tok::Indent,
                Tok::Text("LDA".into()),
                Tok::Lt,
                Tok::Number(0x44),
                Tok::Comma,
                Tok::CharReg('X'),
            ]
        );
    }

    #[test]
    fn pseudo_and_label() {
        assert_eq!(
            kinds(".inesprg 1"),
            vec![Tok::Pseudo("inesprg".into()), Tok::Number(1)]
        );
        assert_eq!(kinds("test:"), vec![Tok::Text("test".into()), Tok::Colon]);
    }
}
