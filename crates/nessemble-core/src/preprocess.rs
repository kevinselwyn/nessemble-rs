//! Source preprocessing: `.include`, `.inestrn`, and `.macrodef`/`.macro`.
//!
//! The reference tool implements includes and macros at the lexer level, using
//! re-entrant flex buffers (an include stack) and a `<macro>` start condition
//! that captures a macro body as raw text and re-scans it on each invocation.
//! This crate instead lexes each file up front, so the equivalent splicing is
//! done here, on the token stream, *before* parsing and assembly:
//!
//! - `.include "file"` recursively lexes the referenced file and splices its
//!   tokens in place (resolved relative to the directory of the file that
//!   contains the `.include`, so nested includes are file-relative), enforcing
//!   the depth limit.
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
use std::path::{Path, PathBuf};

use nessemble_i18n::t;

use crate::diagnostic::{locate, AsmRule, Expansion};
use crate::lexer::{Lexer, Span, Tok, Token};
use crate::Diag;

/// A source overlay for tooling: given the resolved path of a file about to be
/// spliced by `.include` / `.inestrn`, returns the in-memory text to use
/// instead of reading from disk, or `None` to fall back to
/// `std::fs::read_to_string`.
///
/// The language server builds one from its open (unsaved) buffers so a project
/// is preprocessed as the editor currently sees it. The default, no-overlay
/// path reads only from disk — exactly as the CLI does — so the assembler's
/// behavior (and ROM parity) is unchanged.
pub type FileOverlay<'a> = dyn Fn(&Path) -> Option<String> + 'a;

/// Matches the reference `MAX_INCLUDE_DEPTH`.
const MAX_INCLUDE_DEPTH: usize = 10;

/// The flattened result of preprocessing: the token stream plus two parallel
/// tables (indexed by `Token::file`) of per-file display names and directories.
pub struct Preprocessed {
    pub tokens: Vec<Token>,
    /// File display names, for diagnostics.
    pub files: Vec<String>,
    /// Directory each file resolves its filename-based directives against.
    pub dirs: Vec<PathBuf>,
    /// Resolved path of each file, parallel to `files`. `paths[0]` is the
    /// top-level file (`base_dir` joined with `top_name`); each include is the
    /// path it resolved to. Lets tooling map a diagnostic's file back to a URI.
    pub paths: Vec<PathBuf>,
    /// The project root a `@/`-prefixed filename argument resolves against
    /// (see [`crate::PROJECT_ROOT_PREFIX`]), or `None` where no root could be
    /// determined at all (only reachable with no filesystem — see
    /// [`crate::PathArgError::NoProjectRoot`]).
    pub root: Option<PathBuf>,
    /// Source text of each file, parallel to `files` (token spans index it).
    pub sources: Vec<String>,
    /// Every `.macro` expansion, indexed by `Token::expansion - 1`.
    pub expansions: Vec<Expansion>,
    /// Rule diagnostics raised while preprocessing (a redefined macro), in
    /// source order, for the caller to report at their configured severity.
    pub warnings: Vec<Diag>,
}

impl Preprocessed {
    /// The per-file tables (and pending rule diagnostics) the assembler needs.
    /// The token stream is consumed separately, by the parser.
    pub(crate) fn into_tables(self) -> crate::assemble::SourceTables {
        crate::assemble::SourceTables {
            files: self.files,
            dirs: self.dirs,
            paths: self.paths,
            root: self.root,
            expansions: self.expansions,
            pending: self.warnings,
        }
    }
}

/// A captured `.macrodef`: its body and what diagnostics need to know about it.
#[derive(Clone)]
struct MacroDef {
    body: Vec<Token>,
    /// Where the definition is (file id and line of the `.macrodef`).
    file: u32,
    line: u32,
    /// The highest `\N` the body references (0 if none).
    arity: usize,
    /// Whether the body reads `\#`, i.e. handles its own argument count.
    variadic: bool,
}

/// Preprocess `source` (already read from `top_name`), resolving includes and
/// expanding macros. Each `.include`/`.inestrn` is resolved relative to the
/// directory of the file that contains it (seeded from `base_dir` for the
/// top-level file), so the assembler can resolve filename-based directives
/// against the file that contains them. `root` is the project root for `@/`
/// arguments, computed once by the caller (see `plans/012-project-root-paths.md`
/// §4.1).
pub fn preprocess(
    source: &str,
    base_dir: PathBuf,
    top_name: &str,
    root: Option<PathBuf>,
) -> Result<Preprocessed, Diag> {
    preprocess_with(source, base_dir, top_name, root, None)
}

/// Like [`preprocess`], but consulting `overlay` before disk for each included
/// file (see [`FileOverlay`]). Passing `None` is identical to [`preprocess`].
pub fn preprocess_with(
    source: &str,
    base_dir: PathBuf,
    top_name: &str,
    root: Option<PathBuf>,
    overlay: Option<&FileOverlay>,
) -> Result<Preprocessed, Diag> {
    let top_path = base_dir.join(top_name);
    let mut pre = Pre {
        files: vec![top_name.to_string()],
        dirs: vec![base_dir],
        paths: vec![top_path],
        root,
        sources: vec![source.to_string()],
        expansions: Vec::new(),
        warnings: Vec::new(),
        macros: HashMap::new(),
        out: Vec::new(),
        unique: 0,
        for_depth: 0,
        overlay,
    };
    pre.process_file(source, 0, 0)?;
    Ok(Preprocessed {
        tokens: pre.out,
        files: pre.files,
        dirs: pre.dirs,
        paths: pre.paths,
        root: pre.root,
        sources: pre.sources,
        expansions: pre.expansions,
        warnings: pre.warnings,
    })
}

struct Pre<'a> {
    files: Vec<String>,
    /// Directory each file resolves relative includes against, parallel to
    /// `files`. `dirs[0]` is the top-level `base_dir`.
    dirs: Vec<PathBuf>,
    /// Resolved path of each file, parallel to `files` (see `Preprocessed`).
    paths: Vec<PathBuf>,
    /// The project root `@/` arguments resolve against (see `Preprocessed::root`).
    root: Option<PathBuf>,
    /// Source text of each file (see `Preprocessed::sources`).
    sources: Vec<String>,
    expansions: Vec<Expansion>,
    warnings: Vec<Diag>,
    macros: HashMap<String, MacroDef>,
    out: Vec<Token>,
    unique: u32,
    /// How many `.for` blocks are open at this point of the token stream. The
    /// loop itself runs in the assembler, but a `.macrodef` inside a body is
    /// only visible here (the definition is consumed by this pass).
    for_depth: usize,
    /// Optional tooling overlay consulted before disk when splicing includes.
    overlay: Option<&'a FileOverlay<'a>>,
}

impl Pre<'_> {
    fn diag(&self, file: usize, line: u32, message: impl Into<String>) -> Diag {
        Diag::new(
            self.files.get(file).cloned().unwrap_or_default(),
            line,
            message,
        )
    }

    /// A diagnostic on the line of `tok` (written in file `file_id`), located in
    /// its macro body and naming the calls that expanded it, if it was expanded.
    fn diag_at(&self, tok: &Token, file_id: usize, message: impl Into<String>) -> Diag {
        locate(
            &self.files,
            &self.expansions,
            file_id as u32,
            tok.line,
            tok.expansion,
            message,
        )
    }

    /// The file a diagnostic about `tok` names: its macro body's file if it was
    /// expanded, otherwise the file it was written in.
    fn shown_file(&self, tok: &Token, file_id: usize) -> u32 {
        match tok.expansion.checked_sub(1) {
            Some(i) => self.expansions[i as usize].def_file,
            None => file_id as u32,
        }
    }

    /// Lex `text` (the file identified by `file_id`) and process its tokens.
    fn process_file(&mut self, text: &str, file_id: usize, depth: usize) -> Result<(), Diag> {
        let mut toks = Lexer::new(text).tokenize();
        for t in &mut toks {
            t.span.src = file_id as u32;
        }
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
                if name == "for" {
                    self.for_depth += 1;
                } else if name == "endfor" {
                    self.for_depth = self.for_depth.saturating_sub(1);
                }
            }
            self.out.push(Token {
                tok: toks[i].tok.clone(),
                line: toks[i].line,
                file: file_id as u32,
                // Preserve the macro-origin flag: tokens re-emitted from a macro
                // body (via `substitute`) carry it, so it survives re-processing.
                from_macro: toks[i].from_macro,
                expansion: toks[i].expansion,
                span: toks[i].span,
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
                for tok in [Tok::Pseudo("inestrn".to_string()), Tok::Endl] {
                    self.out.push(Token {
                        tok,
                        line,
                        file: file_id as u32,
                        from_macro: false,
                        expansion: toks[i].expansion,
                        span: toks[i].span,
                    });
                }
                self.do_include(&target, depth, file_id, line)?;
                Ok(next)
            }
            "macrodef" => {
                // A definition is made once by this pass whatever the loop does,
                // which would surprise anyone who put one in a body.
                if self.for_depth > 0 {
                    return Err(self.diag_at(&toks[i], file_id, t!("macrodef-in-for")));
                }
                // A `[KEY]` tag (or anything else) already emitted for this
                // line would prefix the definition, which emits nothing.
                let tagged = self
                    .out
                    .iter()
                    .rev()
                    .take_while(|t| !matches!(t.tok, Tok::Endl))
                    .any(|t| !matches!(t.tok, Tok::Indent));
                if tagged {
                    return Err(self.diag_at(
                        &toks[i],
                        file_id,
                        t!("tag-before-block", directive = ".macrodef"),
                    ));
                }
                self.capture_macro(toks, i, file_id)
            }
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

        // A `file://` declaration is stripped like any other filename argument,
        // so `.include "file://defs.asm"` resolves to `defs.asm`.
        let name = crate::strip_file_url(target.trim_matches('"'))
            .0
            .to_string();
        // Resolve relative to the directory of the file that contains the
        // directive, so a nested include (or an include in a subdirectory) is
        // resolved from *its* location rather than the top-level file's — unless
        // `name` starts with `@/`, which resolves from the project root instead.
        let dir = self.dirs[file_id].clone();
        let path = crate::resolve_path_arg(self.root.as_deref(), &dir, &name)
            .map_err(|e| self.diag(file_id, line, e.message(&name)))?;
        // Prefer an overlay entry (an unsaved editor buffer) when present; a
        // buffer can even resolve a file that isn't on disk yet. Otherwise read
        // from disk, as the CLI/assembler path always does.
        let text = if let Some(text) = self.overlay.and_then(|f| f(&path)) {
            text
        } else if let Ok(text) = std::fs::read_to_string(&path) {
            text
        } else {
            return Err(self.diag(file_id, line, t!("could-not-include", file = name)));
        };

        let child_id = self.files.len();
        self.files.push(name);
        // The included file's own directory becomes the base for its includes.
        let child_dir = path
            .parent()
            .map_or_else(|| dir.clone(), std::path::Path::to_path_buf);
        self.dirs.push(child_dir);
        self.paths.push(path);
        self.sources.push(text.clone());
        self.process_file(&text, child_id, depth + 1)
    }

    /// Capture a `.macrodef NAME` … `.endm` body, starting at `toks[i]`.
    ///
    /// Redefining a macro replaces it (the last definition wins, as before) and
    /// raises a `duplicate-symbol` diagnostic naming the first definition.
    fn capture_macro(&mut self, toks: &[Token], i: usize, file_id: usize) -> Result<usize, Diag> {
        let line = toks[i].line;
        // toks[i] = `.macrodef`; the name is the following TEXT token.
        let name = match toks.get(i + 1).map(|t| &t.tok) {
            Some(Tok::Text(n)) => n.clone(),
            _ => return Err(self.diag_at(&toks[i], file_id, t!("macro-name-after-macrodef"))),
        };
        // Skip to the start of the body (past the `.macrodef` line's ENDL).
        let mut j = skip_line(toks, i);
        let mut body: Vec<Token> = Vec::new();
        loop {
            match toks.get(j) {
                None => return Err(self.diag_at(&toks[i], file_id, t!("macro-unterminated"))),
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
        let arity = body
            .iter()
            .filter_map(|t| match t.tok {
                Tok::NumberArg(n) if n >= 1 => Some(n as usize),
                _ => None,
            })
            .max()
            .unwrap_or(0);
        let variadic = body.iter().any(|t| matches!(t.tok, Tok::NumberArgc));
        let def_file = self.shown_file(&toks[i], file_id);
        if let Some(prev) = self.macros.get(&name) {
            let mut d = self.diag_at(
                &toks[i],
                file_id,
                t!(
                    "macro-redefined",
                    name = name.as_str(),
                    file = self
                        .files
                        .get(prev.file as usize)
                        .cloned()
                        .unwrap_or_default(),
                    line = prev.line
                ),
            );
            d.rule = Some(AsmRule::DuplicateSymbol);
            self.warnings.push(d);
        }
        self.macros.insert(
            name,
            MacroDef {
                body,
                file: def_file,
                line,
                arity,
                variadic,
            },
        );
        // Resume past `.endm` and its ENDL.
        Ok(skip_line(toks, j))
    }

    /// Expand a `.macro NAME, arg, …` invocation starting at `toks[i]`.
    ///
    /// A call passing a different number of arguments than the body uses (its
    /// highest `\N`) is an error at the call, unless the body reads `\#` and so
    /// handles the count itself.
    fn expand_macro(
        &mut self,
        toks: &[Token],
        i: usize,
        file_id: usize,
        depth: usize,
        raw: &[&str],
    ) -> Result<usize, Diag> {
        let call = &toks[i];
        let name = match toks.get(i + 1).map(|t| &t.tok) {
            Some(Tok::Text(n)) => n.clone(),
            _ => return Err(self.diag_at(call, file_id, t!("macro-name-after-macro"))),
        };
        let end = skip_line(toks, i);
        // Arguments: the `(COMMA number)*` tokens after the name (up to but not
        // including the terminating ENDL), split on top-level commas.
        let content_end = line_content_end(toks, i);
        let args = split_args(&toks[(i + 2).min(content_end)..content_end]);

        let Some(def) = self.macros.get(&name).cloned() else {
            return Err(self.diag_at(call, file_id, t!("macro-not-defined", name = name)));
        };

        if !def.variadic && args.len() != def.arity {
            let message = match def.arity {
                0 => t!(
                    "macro-arity-none",
                    name = name.as_str(),
                    passed = args.len()
                ),
                1 => t!("macro-arity-one", name = name.as_str(), passed = args.len()),
                n => t!(
                    "macro-arity",
                    name = name.as_str(),
                    arity = n,
                    passed = args.len()
                ),
            };
            let mut d = self.diag_at(call, file_id, message);
            d.notes.insert(
                0,
                t!(
                    "note-macro-defined",
                    name = name.as_str(),
                    file = self
                        .files
                        .get(def.file as usize)
                        .cloned()
                        .unwrap_or_default(),
                    line = def.line
                ),
            );
            return Err(d);
        }

        self.unique += 1;
        let unique = self.unique;
        self.expansions.push(Expansion {
            name,
            def_file: def.file,
            def_line: def.line,
            call_file: self.shown_file(call, file_id),
            call_line: call.line,
            parent: call.expansion,
            unique_names: Vec::new(),
        });
        let expansion = self.expansions.len() as u32;
        let (expanded, unique_names) = substitute(&def.body, &args, unique, file_id, expansion);
        if let Some(e) = self.expansions.last_mut() {
            e.unique_names = unique_names;
        }
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
/// keep the body's definition line numbers, which diagnostics report, and every
/// token is stamped with `expansion` (the id of this expansion). A substituted
/// token — an argument, or the parentheses around it — takes the span of the
/// `\N` it replaces, so a diagnostic quotes the body as written.
///
/// The body's own `@local` definitions (`@x:`, `@x\i:`) — and every body token
/// spelled like one — are first rewritten to keys private to this expansion
/// (`\@7@x`, see [`crate::locals`]); argument tokens are never rewritten, so
/// an `@x` passed in names the caller's label. Also returns the names built
/// with `\@`, which open no local-label scope.
fn substitute(
    body: &[Token],
    args: &[Vec<Token>],
    unique: u32,
    file_id: usize,
    expansion: u32,
) -> (Vec<Token>, Vec<String>) {
    let defs = crate::locals::local_defs(body);
    let mut owned;
    let body = if defs.is_empty() {
        body
    } else {
        owned = body.to_vec();
        crate::locals::privatize(&mut owned, unique, &defs);
        &owned[..]
    };
    let mut unique_names = Vec::new();
    let mut out = Vec::new();
    let mk = |tok: Tok, t: &Token, span: Span| Token {
        tok,
        line: t.line,
        file: file_id as u32,
        // Every token re-emitted from a macro body is macro-originated.
        from_macro: true,
        expansion,
        span,
    };
    let mut k = 0;
    while k < body.len() {
        let t = &body[k];
        match &t.tok {
            Tok::NumberArg(n) => {
                // `\N` → the parenthesised tokens of argument N (1-based).
                out.push(mk(Tok::OpenParen, t, t.span));
                if let Some(arg) = args.get((*n as usize).wrapping_sub(1)) {
                    for a in arg {
                        out.push(mk(a.tok.clone(), t, t.span));
                    }
                } else {
                    out.push(mk(Tok::Number(0), t, t.span));
                }
                out.push(mk(Tok::CloseParen, t, t.span));
            }
            Tok::NumberArgc => out.push(mk(Tok::Number(args.len() as i64), t, t.span)),
            Tok::Text(name) if matches!(body.get(k + 1).map(|n| &n.tok), Some(Tok::NumberArgu)) => {
                // `label\@` → a per-invocation unique label name.
                let span = Span {
                    end: body[k + 1].span.end,
                    ..t.span
                };
                let built = format!("{name}{unique}");
                unique_names.push(built.clone());
                out.push(mk(Tok::Text(built), t, span));
                k += 1; // consume the `\@`
            }
            Tok::NumberArgu => out.push(mk(Tok::Number(unique as i64), t, t.span)),
            other => out.push(mk(other.clone(), t, t.span)),
        }
        k += 1;
    }
    (out, unique_names)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(src: &str) -> (Vec<String>, Vec<Expansion>) {
        let pre = preprocess(src, PathBuf::from("."), "t.asm", None).expect("preprocesses");
        let texts = pre
            .tokens
            .iter()
            .filter_map(|t| match &t.tok {
                Tok::Text(s) => Some(s.clone()),
                _ => None,
            })
            .collect();
        (texts, pre.expansions)
    }

    #[test]
    fn privatize_rewrites_only_the_bodys_own_locals() {
        let (t, _) = texts(
            ".macrodef mm\n@a:\n    BNE @a\n    BNE @b\n    .dw \\1\n.endm\n    .macro mm, @a\n",
        );
        assert_eq!(t, vec!["\\@1@a", "BNE", "\\@1@a", "BNE", "@b", "@a"]);
    }

    #[test]
    fn nested_expansions_record_their_parent_and_unique_names() {
        let (t, ex) = texts(concat!(
            ".macrodef inner\n@x:\n.endm\n",
            ".macrodef outer\nlb\\@:\n    .macro inner\n.endm\n",
            "    .macro outer\n",
        ));
        assert_eq!(t, vec!["lb1", "\\@2@x"]);
        assert_eq!(ex.len(), 2);
        assert_eq!((ex[0].parent, ex[1].parent), (0, 1));
        assert_eq!(ex[0].unique_names, vec!["lb1".to_string()]);
        assert!(ex[1].unique_names.is_empty());
    }
}
