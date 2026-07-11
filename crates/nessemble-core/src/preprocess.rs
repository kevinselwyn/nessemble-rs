//! Source preprocessing: `.include`, `.inestrn`, and `.macrodef`/`.macro`.
//!
//! The reference tool implements includes and macros at the lexer level, using
//! re-entrant flex buffers (an include stack) and a `<macro>` start condition
//! that captures a macro body as raw text and re-scans it on each invocation.
//! This crate instead lexes each file up front, so the equivalent splicing is
//! done here, on the token stream, *before* parsing and assembly:
//!
//! - `.include "file"` recursively lexes the referenced file and splices its
//!   tokens in place (relative to the top-level file's directory, matching the
//!   reference's global `cwd_path`), enforcing the depth limit.
//! - `.macrodef NAME` … `.endm` captures the body tokens; `.macro NAME, a, b`
//!   re-emits that body with each `\N` replaced by the (parenthesised) tokens of
//!   argument *N*, `\#` by the argument count, and `\@` by a per-invocation
//!   unique id — the token-level analogue of re-scanning the body text.
//! - `.inestrn "file"` emits a bare marker (flipping the assembler into
//!   trainer-redirect mode) and then splices the file like `.include`.
//!
//! Conditionals (`.if`/`.ifdef`/`.ifndef`/`.else`/`.endif`) are *not* handled
//! here: they depend on assembly-time values and are evaluated by the assembler.

use std::collections::HashMap;
use std::path::PathBuf;

use nessemble_i18n::t;

use crate::lexer::{Lexer, Tok, Token};
use crate::Diag;

/// Matches the reference `MAX_INCLUDE_DEPTH`.
const MAX_INCLUDE_DEPTH: usize = 10;

/// Preprocess `source` (already read from `top_name`), resolving includes
/// relative to `base_dir` and expanding macros. Returns the final flat token
/// stream plus the table of file display names (indexed by `Token::file`).
pub fn preprocess(
    source: &str,
    base_dir: PathBuf,
    top_name: &str,
) -> Result<(Vec<Token>, Vec<String>), Diag> {
    let mut pre = Pre {
        base_dir,
        files: vec![top_name.to_string()],
        macros: HashMap::new(),
        out: Vec::new(),
        unique: 0,
    };
    pre.process_file(source, 0, 0)?;
    Ok((pre.out, pre.files))
}

struct Pre {
    base_dir: PathBuf,
    files: Vec<String>,
    macros: HashMap<String, Vec<Token>>,
    out: Vec<Token>,
    unique: u32,
}

impl Pre {
    fn diag(&self, file: usize, line: u32, message: impl Into<String>) -> Diag {
        Diag {
            file: self.files.get(file).cloned().unwrap_or_default(),
            line,
            message: message.into(),
        }
    }

    /// Lex `text` (the file identified by `file_id`) and process its tokens.
    fn process_file(&mut self, text: &str, file_id: usize, depth: usize) -> Result<(), Diag> {
        let toks = Lexer::new(text).tokenize();
        let raw: Vec<&str> = text.lines().collect();
        self.process_tokens(&toks, file_id, depth, &raw)
    }

    /// Walk a token slice, emitting tokens to `out` and handling directives.
    fn process_tokens(
        &mut self,
        toks: &[Token],
        file_id: usize,
        depth: usize,
        raw: &[&str],
    ) -> Result<(), Diag> {
        let mut i = 0;
        while i < toks.len() {
            if let Tok::Pseudo(name) = &toks[i].tok {
                if is_directive(name) {
                    let name = name.clone();
                    // A directive consumes its whole logical line; drop the
                    // leading indentation token already emitted for this line.
                    if matches!(self.out.last().map(|t| &t.tok), Some(Tok::Indent)) {
                        self.out.pop();
                    }
                    i = self.handle_directive(&name, toks, i, file_id, depth, raw)?;
                    continue;
                }
            }
            self.out.push(Token {
                tok: toks[i].tok.clone(),
                line: toks[i].line,
                file: file_id as u32,
            });
            i += 1;
        }
        Ok(())
    }

    /// Handle one directive starting at `toks[i]`, returning the index to
    /// resume at.
    fn handle_directive(
        &mut self,
        name: &str,
        toks: &[Token],
        i: usize,
        file_id: usize,
        depth: usize,
        raw: &[&str],
    ) -> Result<usize, Diag> {
        let line = toks[i].line;
        match name {
            "include" => {
                let target = self.include_target(raw, line, file_id)?;
                let next = skip_line(toks, i);
                self.do_include(&target, depth, file_id, line)?;
                Ok(next)
            }
            "inestrn" => {
                let target = self.include_target(raw, line, file_id)?;
                let next = skip_line(toks, i);
                // Emit the bare marker, then splice the trainer file.
                self.out.push(Token {
                    tok: Tok::Pseudo("inestrn".to_string()),
                    line,
                    file: file_id as u32,
                });
                self.out.push(Token {
                    tok: Tok::Endl,
                    line,
                    file: file_id as u32,
                });
                self.do_include(&target, depth, file_id, line)?;
                Ok(next)
            }
            "macrodef" => self.capture_macro(toks, i, file_id, line),
            "macro" => self.expand_macro(toks, i, file_id, depth, raw),
            _ => unreachable!("unhandled directive {name}"),
        }
    }

    /// Extract the raw include target (e.g. `"../x.asm"` or `<pkg>`) from the
    /// directive's source line, mirroring the flex `<include>` start condition.
    fn include_target(&self, raw: &[&str], line: u32, file_id: usize) -> Result<String, Diag> {
        let text = raw
            .get((line as usize).wrapping_sub(1))
            .copied()
            .unwrap_or("");
        let after = text
            .split_once(".include")
            .or_else(|| text.split_once(".inestrn"))
            .map_or("", |(_, b)| b);
        match after.split_whitespace().next() {
            Some(t) => Ok(t.to_string()),
            None => Err(self.diag(file_id, line, t!("full-path"))),
        }
    }

    /// Resolve and splice an included file.
    fn do_include(
        &mut self,
        target: &str,
        depth: usize,
        file_id: usize,
        line: u32,
    ) -> Result<(), Diag> {
        // `<pkg>` references the (out-of-scope) package library; there is no
        // package to resolve, so this always fails as the reference does.
        if target.starts_with('<') {
            return Err(self.diag(file_id, line, t!("full-path-of", target = target)));
        }

        // The reference enforces the depth limit as it pushes the include.
        if depth + 1 >= MAX_INCLUDE_DEPTH {
            return Err(self.diag(file_id, line, t!("too-many-includes")));
        }

        let name = target.trim_matches('"').to_string();
        let path = self.base_dir.join(&name);
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Err(self.diag(file_id, line, t!("could-not-include", file = name)));
        };

        let child_id = self.files.len();
        self.files.push(name);
        self.process_file(&text, child_id, depth + 1)
    }

    /// Capture a `.macrodef NAME` … `.endm` body, starting at `toks[i]`.
    fn capture_macro(
        &mut self,
        toks: &[Token],
        i: usize,
        file_id: usize,
        line: u32,
    ) -> Result<usize, Diag> {
        // toks[i] = `.macrodef`; the name is the following TEXT token.
        let name = match toks.get(i + 1).map(|t| &t.tok) {
            Some(Tok::Text(n)) => n.clone(),
            _ => return Err(self.diag(file_id, line, t!("macro-name-after-macrodef"))),
        };
        // Skip to the start of the body (past the `.macrodef` line's ENDL).
        let mut j = skip_line(toks, i);
        let mut body: Vec<Token> = Vec::new();
        loop {
            match toks.get(j) {
                None => return Err(self.diag(file_id, line, t!("macro-unterminated"))),
                Some(t) if matches!(&t.tok, Tok::Pseudo(p) if p == "endm") => break,
                Some(t) => {
                    body.push(t.clone());
                    j += 1;
                }
            }
        }
        // Drop a trailing indentation token belonging to the `.endm` line.
        if matches!(body.last().map(|t| &t.tok), Some(Tok::Indent)) {
            body.pop();
        }
        self.macros.insert(name, body);
        // Resume past `.endm` and its ENDL.
        Ok(skip_line(toks, j))
    }

    /// Expand a `.macro NAME, arg, …` invocation starting at `toks[i]`.
    fn expand_macro(
        &mut self,
        toks: &[Token],
        i: usize,
        file_id: usize,
        depth: usize,
        raw: &[&str],
    ) -> Result<usize, Diag> {
        let line = toks[i].line;
        let name = match toks.get(i + 1).map(|t| &t.tok) {
            Some(Tok::Text(n)) => n.clone(),
            _ => return Err(self.diag(file_id, line, t!("macro-name-after-macro"))),
        };
        let end = skip_line(toks, i);
        // Arguments: the `(COMMA number)*` tokens after the name (up to but not
        // including the terminating ENDL), split on top-level commas.
        let content_end = line_content_end(toks, i);
        let args = split_args(&toks[(i + 2).min(content_end)..content_end]);

        let body = match self.macros.get(&name) {
            Some(b) => b.clone(),
            None => {
                return Err(self.diag(file_id, line, t!("macro-not-defined", name = name)));
            }
        };

        self.unique += 1;
        let unique = self.unique;
        let expanded = substitute(&body, &args, unique, file_id);
        // Re-process the expansion so nested directives resolve too.
        self.process_tokens(&expanded, file_id, depth, raw)?;
        Ok(end)
    }
}

/// Directives handled by the preprocessor (rather than the parser/assembler).
fn is_directive(name: &str) -> bool {
    matches!(name, "include" | "inestrn" | "macrodef" | "macro")
}

/// Index of the ENDL that terminates `toks[i]`'s line (or `len` if none).
fn line_content_end(toks: &[Token], i: usize) -> usize {
    let mut j = i;
    while j < toks.len() && !matches!(toks[j].tok, Tok::Endl) {
        j += 1;
    }
    j
}

/// Index of the first token after the ENDL that terminates `toks[i]`'s line.
fn skip_line(toks: &[Token], i: usize) -> usize {
    let mut j = i;
    while j < toks.len() && !matches!(toks[j].tok, Tok::Endl) {
        j += 1;
    }
    // Step past the ENDL itself, if present.
    if j < toks.len() {
        j += 1;
    }
    j
}

/// Split macro-invocation argument tokens on top-level commas, dropping the
/// leading comma that separates the name from the first argument.
fn split_args(toks: &[Token]) -> Vec<Vec<Token>> {
    let mut args: Vec<Vec<Token>> = Vec::new();
    let mut current: Vec<Token> = Vec::new();
    let mut depth = 0i32;
    let mut started = false;
    for t in toks {
        match &t.tok {
            Tok::OpenParen | Tok::OpenBrack => {
                depth += 1;
                current.push(t.clone());
                started = true;
            }
            Tok::CloseParen | Tok::CloseBrack => {
                depth -= 1;
                current.push(t.clone());
                started = true;
            }
            Tok::Comma if depth == 0 => {
                if started {
                    args.push(std::mem::take(&mut current));
                }
                started = true;
            }
            _ => {
                current.push(t.clone());
                started = true;
            }
        }
    }
    if !current.is_empty() {
        args.push(current);
    }
    args
}

/// Re-emit a macro body with `\N` / `\#` / `\@` substituted. Substituted tokens
/// keep the body's definition line numbers, which diagnostics report.
fn substitute(body: &[Token], args: &[Vec<Token>], unique: u32, file_id: usize) -> Vec<Token> {
    let mut out = Vec::new();
    let mut k = 0;
    while k < body.len() {
        let t = &body[k];
        match &t.tok {
            Tok::NumberArg(n) => {
                // `\N` → the parenthesised tokens of argument N (1-based).
                out.push(mk(Tok::OpenParen, t.line, file_id));
                if let Some(arg) = args.get((*n as usize).wrapping_sub(1)) {
                    for a in arg {
                        out.push(mk(a.tok.clone(), t.line, file_id));
                    }
                } else {
                    out.push(mk(Tok::Number(0), t.line, file_id));
                }
                out.push(mk(Tok::CloseParen, t.line, file_id));
            }
            Tok::NumberArgc => out.push(mk(Tok::Number(args.len() as i64), t.line, file_id)),
            Tok::Text(name) if matches!(body.get(k + 1).map(|n| &n.tok), Some(Tok::NumberArgu)) => {
                // `label\@` → a per-invocation unique label name.
                out.push(mk(Tok::Text(format!("{name}{unique}")), t.line, file_id));
                k += 1; // consume the `\@`
            }
            Tok::NumberArgu => out.push(mk(Tok::Number(unique as i64), t.line, file_id)),
            other => out.push(mk(other.clone(), t.line, file_id)),
        }
        k += 1;
    }
    out
}

fn mk(tok: Tok, line: u32, file_id: usize) -> Token {
    Token {
        tok,
        line,
        file: file_id as u32,
    }
}
