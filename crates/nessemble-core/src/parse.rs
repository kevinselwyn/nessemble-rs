//! Recursive-descent parser producing [`Line`]s from a token stream.
//!
//! Mirrors the reference (bison) grammar for the Phase 2 subset: labels,
//! constants, instructions (all addressing forms), expressions (single
//! precedence level, right-associative), and the core/data/iNES pseudo-ops.
//! Directives outside this phase parse to [`Pseudo::Unsupported`].

use crate::ast::*;
use crate::lexer::{Tok, Token};

/// A parse error with the offending 1-based line and source-file id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    pub line: u32,
    pub file: u32,
    pub message: String,
}

pub fn parse(tokens: Vec<Token>) -> Result<Vec<Line>, ParseError> {
    Parser {
        toks: tokens,
        pos: 0,
    }
    .parse_program()
}

struct Parser {
    toks: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos).map(|t| &t.tok)
    }

    fn peek_at(&self, off: usize) -> Option<&Tok> {
        self.toks.get(self.pos + off).map(|t| &t.tok)
    }

    fn cur_line(&self) -> u32 {
        self.toks
            .get(self.pos)
            .or_else(|| self.toks.last())
            .map(|t| t.line)
            .unwrap_or(1)
    }

    fn cur_file(&self) -> u32 {
        self.toks
            .get(self.pos)
            .or_else(|| self.toks.last())
            .map(|t| t.file)
            .unwrap_or(0)
    }

    fn bump(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).map(|t| t.tok.clone());
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn err<T>(&self, message: impl Into<String>) -> Result<T, ParseError> {
        Err(ParseError {
            line: self.cur_line(),
            file: self.cur_file(),
            message: message.into(),
        })
    }

    fn parse_program(&mut self) -> Result<Vec<Line>, ParseError> {
        let mut lines = Vec::new();
        loop {
            // Skip blank lines / stray end-of-line tokens.
            while matches!(self.peek(), Some(Tok::Endl)) {
                self.pos += 1;
            }
            if self.peek().is_none() {
                break;
            }
            let indented = matches!(self.peek(), Some(Tok::Indent));
            if indented {
                self.pos += 1;
                // An indent immediately followed by end-of-line is a blank line.
                if matches!(self.peek(), Some(Tok::Endl) | None) {
                    continue;
                }
            }
            let line_no = self.cur_line();
            let file_no = self.cur_file();
            if let Some(stmt) = self.parse_stmt(indented)? {
                lines.push(Line {
                    stmt,
                    line: line_no,
                    file: file_no,
                });
            }
            // Consume through end-of-line.
            while !matches!(self.peek(), Some(Tok::Endl) | None) {
                self.pos += 1;
            }
            if matches!(self.peek(), Some(Tok::Endl)) {
                self.pos += 1;
            }
        }
        Ok(lines)
    }

    fn parse_stmt(&mut self, indented: bool) -> Result<Option<Stmt>, ParseError> {
        match self.peek().cloned() {
            Some(Tok::Pseudo(name)) => {
                self.pos += 1;
                Ok(Some(Stmt::Pseudo(self.parse_pseudo(&name)?)))
            }
            Some(Tok::Colon) => {
                self.pos += 1;
                Ok(Some(Stmt::Label(":".to_string())))
            }
            Some(Tok::Text(name)) => {
                // A label declaration is `text COLON` where the colon ends the
                // line. A colon that begins a local-label operand (`:-`, `:+`)
                // belongs to an instruction, so require end-of-line after it.
                let is_label = matches!(self.peek_at(1), Some(Tok::Colon))
                    && matches!(self.peek_at(2), Some(Tok::Endl) | None);
                if is_label {
                    self.pos += 2;
                    return Ok(Some(Stmt::Label(name)));
                }
                // `<label> .rs <size>` reserves a variable.
                if matches!(self.peek_at(1), Some(Tok::Pseudo(p)) if p == "rs") {
                    self.pos += 2;
                    let size = self.parse_expr()?;
                    return Ok(Some(Stmt::Pseudo(Pseudo::Rs(name, size))));
                }
                match self.peek_at(1) {
                    Some(Tok::Equ) => {
                        self.pos += 2;
                        let e = self.parse_expr()?;
                        Ok(Some(Stmt::Constant(name, Some(e))))
                    }
                    _ if indented => {
                        self.pos += 1;
                        let operand = self.parse_operand()?;
                        Ok(Some(Stmt::Instruction(Instruction {
                            mnemonic: name,
                            operand,
                        })))
                    }
                    _ => {
                        self.pos += 1;
                        Ok(Some(Stmt::Constant(name, None)))
                    }
                }
            }
            _ => self.err("unexpected token at start of statement"),
        }
    }

    fn parse_operand(&mut self) -> Result<Operand, ParseError> {
        match self.peek().cloned() {
            None | Some(Tok::Endl) => Ok(Operand::Implied),
            Some(Tok::CharReg(r)) => {
                self.pos += 1;
                Ok(Operand::Accumulator(r))
            }
            Some(Tok::Hash) => {
                self.pos += 1;
                Ok(Operand::Immediate(self.parse_expr()?))
            }
            Some(Tok::OpenBrack) => {
                self.pos += 1;
                let e = self.parse_expr()?;
                match self.peek().cloned() {
                    Some(Tok::Comma) => {
                        self.pos += 1;
                        let r = self.expect_reg()?;
                        self.expect(Tok::CloseBrack)?;
                        Ok(Operand::IndirectIndexed(e, r))
                    }
                    Some(Tok::CloseBrack) => {
                        self.pos += 1;
                        if matches!(self.peek(), Some(Tok::Comma)) {
                            self.pos += 1;
                            let r = self.expect_reg()?;
                            Ok(Operand::IndirectIndexed(e, r))
                        } else {
                            Ok(Operand::Indirect(e))
                        }
                    }
                    _ => self.err("expected `]` or `,` in indirect operand"),
                }
            }
            Some(Tok::Lt) => {
                self.pos += 1;
                let e = self.parse_expr()?;
                if matches!(self.peek(), Some(Tok::Comma)) {
                    self.pos += 1;
                    let r = self.expect_reg()?;
                    Ok(Operand::ZeroPageIndexed(e, r))
                } else {
                    Ok(Operand::ZeroPage(e))
                }
            }
            _ => {
                let e = self.parse_expr()?;
                if matches!(self.peek(), Some(Tok::Comma)) {
                    self.pos += 1;
                    let r = self.expect_reg()?;
                    Ok(Operand::AbsoluteIndexed(e, r))
                } else {
                    Ok(Operand::Absolute(e))
                }
            }
        }
    }

    fn expect(&mut self, tok: Tok) -> Result<(), ParseError> {
        if self.peek() == Some(&tok) {
            self.pos += 1;
            Ok(())
        } else {
            self.err(format!("expected {tok:?}"))
        }
    }

    fn expect_reg(&mut self) -> Result<char, ParseError> {
        match self.bump() {
            Some(Tok::CharReg(r)) => Ok(r),
            _ => self.err("expected register"),
        }
    }

    fn parse_pseudo(&mut self, name: &str) -> Result<Pseudo, ParseError> {
        let p = match name {
            "org" => Pseudo::Org(self.parse_expr()?),
            "db" | "byte" => Pseudo::Db(self.parse_expr_list()?),
            "dw" | "word" => Pseudo::Dw(self.parse_expr_list()?),
            "hibytes" => Pseudo::Hibytes(self.parse_expr_list()?),
            "lobytes" => Pseudo::Lobytes(self.parse_expr_list()?),
            "fill" => Pseudo::Fill(self.parse_expr_list()?),
            "checksum" => Pseudo::Checksum(self.parse_expr()?),
            "color" => Pseudo::Color(self.parse_expr_list()?),
            "endenum" => Pseudo::Endenum,
            "enum" => {
                let start = self.parse_expr()?;
                let inc = if matches!(self.peek(), Some(Tok::Comma)) {
                    self.pos += 1;
                    Some(self.parse_expr()?)
                } else {
                    None
                };
                Pseudo::Enum(start, inc)
            }
            "rsset" => Pseudo::Rsset(self.parse_expr()?),
            "random" => Pseudo::Random(self.parse_rand_terms()?),
            // The trainer file is spliced in by the preprocessor; this bare
            // marker only flips the assembler into trainer-redirect mode.
            "inestrn" => Pseudo::InesTrn,
            "if" => Pseudo::If(self.parse_expr()?),
            "ifdef" => Pseudo::Ifdef(self.parse_text_name("ifdef")?),
            "ifndef" => Pseudo::Ifndef(self.parse_text_name("ifndef")?),
            "else" => Pseudo::Else,
            "endif" => Pseudo::Endif,
            "incbin" => {
                let (file, a, b) = self.parse_inc_args("incbin")?;
                Pseudo::Incbin(file, a, b)
            }
            "incpng" => {
                let (file, a, b) = self.parse_inc_args("incpng")?;
                Pseudo::Incpng(file, a, b)
            }
            "incpal" => Pseudo::Incpal(self.parse_quoted("incpal")?),
            "incrle" => Pseudo::Incrle(self.parse_quoted("incrle")?),
            "incwav" => {
                let file = self.parse_quoted("incwav")?;
                let amp = if matches!(self.peek(), Some(Tok::Comma)) {
                    self.pos += 1;
                    Some(self.parse_expr()?)
                } else {
                    None
                };
                Pseudo::Incwav(file, amp)
            }
            "font" => Pseudo::Font(self.parse_expr_list()?),
            "defchr" => Pseudo::Defchr(self.parse_defchr_list()?),
            "inesprg" => Pseudo::InesPrg(self.parse_expr()?),
            "ineschr" => Pseudo::InesChr(self.parse_expr()?),
            "inesmap" => Pseudo::InesMap(self.parse_expr()?),
            "inesmir" => Pseudo::InesMir(self.parse_expr()?),
            "prg" => Pseudo::Prg(self.parse_expr()?),
            "chr" => Pseudo::Chr(self.parse_expr()?),
            "segment" => match self.bump() {
                Some(Tok::QuotString(s)) => Pseudo::Segment(strip_quotes(&s)),
                _ => return self.err("expected string after .segment"),
            },
            "ascii" => {
                let text = match self.bump() {
                    Some(Tok::QuotString(s)) => strip_quotes(&s),
                    _ => return self.err("expected string after .ascii"),
                };
                let (offset, negate) = match self.peek() {
                    Some(Tok::Plus) => {
                        self.pos += 1;
                        (Some(self.parse_expr()?), false)
                    }
                    Some(Tok::Minus) => {
                        self.pos += 1;
                        (Some(self.parse_expr()?), true)
                    }
                    _ => (None, false),
                };
                Pseudo::Ascii(AsciiArg {
                    text,
                    offset,
                    negate,
                })
            }
            // Any other directive is a custom pseudo-op resolved to a script.
            other => Pseudo::Custom(other.to_string(), self.parse_custom_args()?),
        };
        Ok(p)
    }

    /// Parse a custom pseudo-op's comma-separated arguments (numbers or quoted
    /// strings), preserving source order.
    fn parse_custom_args(&mut self) -> Result<Vec<CustomArg>, ParseError> {
        let mut out = Vec::new();
        if matches!(self.peek(), Some(Tok::Endl) | None) {
            return Ok(out);
        }
        loop {
            match self.peek() {
                Some(Tok::QuotString(s)) => {
                    let s = strip_quotes(s);
                    self.pos += 1;
                    out.push(CustomArg::Str(s));
                }
                _ => out.push(CustomArg::Int(self.parse_expr()?)),
            }
            if matches!(self.peek(), Some(Tok::Comma)) {
                self.pos += 1;
            } else {
                break;
            }
        }
        Ok(out)
    }

    /// Parse the single symbol name argument to `.ifdef` / `.ifndef`.
    fn parse_text_name(&mut self, which: &str) -> Result<String, ParseError> {
        match self.bump() {
            Some(Tok::Text(name)) => Ok(name),
            _ => self.err(format!("expected symbol after .{which}")),
        }
    }

    /// Parse a quoted-string filename argument to an importer directive.
    fn parse_quoted(&mut self, which: &str) -> Result<String, ParseError> {
        match self.bump() {
            Some(Tok::QuotString(s)) => Ok(strip_quotes(&s)),
            _ => self.err(format!("expected string after .{which}")),
        }
    }

    /// Parse `"file"[, offset[, limit]]` for `.incbin` / `.incpng`.
    #[allow(clippy::type_complexity)]
    fn parse_inc_args(
        &mut self,
        which: &str,
    ) -> Result<(String, Option<Expr>, Option<Expr>), ParseError> {
        let file = self.parse_quoted(which)?;
        let mut offset = None;
        let mut limit = None;
        if matches!(self.peek(), Some(Tok::Comma)) {
            self.pos += 1;
            offset = Some(self.parse_expr()?);
            if matches!(self.peek(), Some(Tok::Comma)) {
                self.pos += 1;
                limit = Some(self.parse_expr()?);
            }
        }
        Ok((file, offset, limit))
    }

    /// Parse `.defchr`'s eight tile rows, which are comma-separated and may span
    /// lines (a trailing comma continues onto the next indented line).
    fn parse_defchr_list(&mut self) -> Result<Vec<Expr>, ParseError> {
        let mut out = vec![self.parse_expr()?];
        while matches!(self.peek(), Some(Tok::Comma)) {
            self.pos += 1;
            // A comma may be followed by a line break + indentation.
            while matches!(self.peek(), Some(Tok::Endl) | Some(Tok::Indent)) {
                self.pos += 1;
            }
            if matches!(self.peek(), Some(Tok::Endl) | None) {
                break;
            }
            out.push(self.parse_expr()?);
        }
        Ok(out)
    }

    fn parse_rand_terms(&mut self) -> Result<Vec<RandTerm>, ParseError> {
        let mut out = Vec::new();
        // `.random` with no arguments is valid.
        if matches!(self.peek(), Some(Tok::Endl) | None) {
            return Ok(out);
        }
        loop {
            match self.peek() {
                Some(Tok::QuotString(s)) => {
                    let s = strip_quotes(s);
                    self.pos += 1;
                    out.push(RandTerm::Str(s));
                }
                _ => out.push(RandTerm::Num(self.parse_expr()?)),
            }
            if matches!(self.peek(), Some(Tok::Comma)) {
                self.pos += 1;
            } else {
                break;
            }
        }
        Ok(out)
    }

    fn parse_expr_list(&mut self) -> Result<Vec<Expr>, ParseError> {
        let mut out = vec![self.parse_expr()?];
        while matches!(self.peek(), Some(Tok::Comma)) {
            self.pos += 1;
            // Allow a trailing comma / line continuation to simply stop.
            if matches!(self.peek(), Some(Tok::Endl) | None) {
                break;
            }
            out.push(self.parse_expr()?);
        }
        Ok(out)
    }

    fn parse_expr(&mut self) -> Result<Expr, ParseError> {
        let left = self.parse_primary()?;
        if let Some(op) = self.peek().and_then(binop) {
            self.pos += 1;
            let right = self.parse_expr()?; // right-associative
            Ok(Expr::Binary(Box::new(left), op, Box::new(right)))
        } else {
            Ok(left)
        }
    }

    fn parse_primary(&mut self) -> Result<Expr, ParseError> {
        let mut e = match self.bump() {
            Some(Tok::Number(n)) => Expr::Num(n),
            Some(Tok::NumberArg(_)) | Some(Tok::NumberArgc) | Some(Tok::NumberArgu) => Expr::Num(0),
            Some(Tok::Text(name)) => Expr::Symbol(name),
            Some(Tok::High) => {
                self.expect(Tok::OpenParen)?;
                let inner = self.parse_expr()?;
                self.expect(Tok::CloseParen)?;
                Expr::High(Box::new(inner))
            }
            Some(Tok::Low) => {
                self.expect(Tok::OpenParen)?;
                let inner = self.parse_expr()?;
                self.expect(Tok::CloseParen)?;
                Expr::Low(Box::new(inner))
            }
            Some(Tok::Bank) => {
                self.expect(Tok::OpenParen)?;
                let name = match self.bump() {
                    Some(Tok::Text(n)) => n,
                    _ => return self.err("expected symbol in BANK()"),
                };
                self.expect(Tok::CloseParen)?;
                Expr::Bank(name)
            }
            Some(Tok::OpenParen) => {
                let inner = self.parse_expr()?;
                self.expect(Tok::CloseParen)?;
                inner
            }
            Some(Tok::Colon) => {
                let mut count = 0u32;
                match self.peek() {
                    Some(Tok::Plus) => {
                        while matches!(self.peek(), Some(Tok::Plus)) {
                            count += 1;
                            self.pos += 1;
                        }
                        Expr::LocalForward(count)
                    }
                    Some(Tok::Minus) => {
                        while matches!(self.peek(), Some(Tok::Minus)) {
                            count += 1;
                            self.pos += 1;
                        }
                        Expr::LocalBackward(count)
                    }
                    _ => return self.err("expected `+`/`-` after `:` in expression"),
                }
            }
            _ => return self.err("expected expression"),
        };
        // `label -> label` chains (left-associative addition of label values).
        while matches!(self.peek(), Some(Tok::Arrow)) {
            self.pos += 1;
            match self.bump() {
                Some(Tok::Text(n)) => {
                    e = Expr::Binary(Box::new(e), BinOp::Add, Box::new(Expr::Symbol(n)));
                }
                _ => return self.err("expected symbol after `->`"),
            }
        }
        Ok(e)
    }
}

fn binop(tok: &Tok) -> Option<BinOp> {
    Some(match tok {
        Tok::Plus => BinOp::Add,
        Tok::Minus => BinOp::Sub,
        Tok::Mult => BinOp::Mul,
        Tok::Div => BinOp::Div,
        Tok::Pow => BinOp::Pow,
        Tok::And => BinOp::And,
        Tok::Or => BinOp::Or,
        Tok::Xor => BinOp::Xor,
        Tok::Rshift => BinOp::Rshift,
        Tok::Lshift => BinOp::Lshift,
        Tok::Mod => BinOp::Mod,
        Tok::DblEqu => BinOp::Eq,
        Tok::NotEqu => BinOp::Ne,
        Tok::Lt => BinOp::Lt,
        Tok::Gt => BinOp::Gt,
        Tok::Lte => BinOp::Le,
        Tok::Gte => BinOp::Ge,
        _ => return None,
    })
}

fn strip_quotes(s: &str) -> String {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"' {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}
