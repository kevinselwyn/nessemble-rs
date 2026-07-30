//! Whether a script's output may be reused across builds.
//!
//! A cache of "what this directive emitted" is only sound for a script that is a
//! *function* of its arguments and the files it read. Three things break that:
//! randomness (the point of which is to differ), writing a file (the write is the
//! observable effect, and a cache hit would skip it), and reading something the
//! host cannot record — a directory listing, or an `import`ed module, neither of
//! which the input recorder sees.
//!
//! The scan is **static** and deliberately **conservative**: a `rand()` in a
//! branch that never runs still marks the script uncacheable. Being wrong in that
//! direction costs one script execution; being wrong in the other direction
//! serves a stale ROM.

use rhai::{ASTNode, Expr, Stmt, AST};

/// Host functions whose use makes a run's bytes unsafe to reuse. `write` is
/// rhai-fs's `File#write`; `open_dir` yields a directory listing, whose contents
/// no per-file freshness record describes.
const IMPURE: &[&str] = &[
    "rand",
    "rand_float",
    "rand_bool",
    "rand_int",
    "rand_char",
    "shuffle",
    "sample",
    "write",
    "open_dir",
];

/// Why a script cannot be cached, for a diagnostic or a log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Impurity {
    /// Calls a function whose result is not a function of its inputs, or whose
    /// effect is the point of calling it.
    Calls(String),
    /// Opens a file for writing (the one-argument `open_file`, or a mode with
    /// `w`, `a` or `+`).
    WritesAFile,
    /// Imports a module, whose source is invisible to both the script's own
    /// identity and the input recorder.
    Imports,
}

impl Impurity {
    /// A short phrase naming the cause, e.g. `` `rand` ``.
    #[must_use]
    pub fn reason(&self) -> String {
        match self {
            Impurity::Calls(name) => format!("`{name}`"),
            Impurity::WritesAFile => "a file write".to_string(),
            Impurity::Imports => "`import`".to_string(),
        }
    }
}

/// The first reason `ast` cannot be cached, or `None` if it may be.
#[must_use]
pub fn impurity(ast: &AST) -> Option<Impurity> {
    let mut found = None;
    ast.walk(&mut |path: &[ASTNode]| {
        let Some(node) = path.last() else {
            return true;
        };
        let hit = match node {
            ASTNode::Stmt(Stmt::Import(..)) => Some(Impurity::Imports),
            ASTNode::Expr(expr) => call_impurity(expr),
            _ => None,
        };
        if hit.is_some() {
            found = hit;
            return false;
        }
        true
    });
    found
}

/// Classify one expression: an impure call, a write-mode `open_file`, or nothing.
///
/// Both call shapes count: a plain `rand(0, 255)` is an [`Expr::FnCall`], while
/// `array.shuffle()` and `file.write(bytes)` are [`Expr::MethodCall`]s.
fn call_impurity(expr: &Expr) -> Option<Impurity> {
    let (Expr::FnCall(call, _) | Expr::MethodCall(call, _)) = expr else {
        return None;
    };
    let name = call.name.as_str();
    if name == "open_file" {
        return write_mode(call).then_some(Impurity::WritesAFile);
    }
    IMPURE
        .contains(&name)
        .then(|| Impurity::Calls(name.to_string()))
}

/// Whether an `open_file` call opens for writing: the one-argument form creates
/// or truncates, and any mode that is not a plain, literal read is treated as a
/// write rather than guessed at.
fn write_mode(call: &rhai::FnCallExpr) -> bool {
    match call.args.len() {
        0 | 1 => true,
        _ => match call.args[1]
            .get_literal_value(None)
            .and_then(|mode| mode.into_immutable_string().ok())
        {
            Some(mode) => mode.chars().any(|c| matches!(c, 'w' | 'a' | '+' | 'x')),
            // A computed mode could be anything; assume the unsafe case.
            None => true,
        },
    }
}
