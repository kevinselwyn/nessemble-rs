//! Abstract syntax for the nessemble assembly language.

/// Binary operators. All share a single precedence level and are
/// right-associative, matching the reference (bison) grammar's default
/// shift-based conflict resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Pow,
    And,
    Or,
    Xor,
    Rshift,
    Lshift,
    Mod,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
}

/// An expression tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    Num(i64),
    /// Reference to a named symbol (constant or label).
    Symbol(String),
    /// Anonymous forward label reference (`:+`, `:++`, ...): count of `+`.
    LocalForward(u32),
    /// Anonymous backward label reference (`:-`, `:--`, ...): count of `-`.
    LocalBackward(u32),
    High(Box<Expr>),
    Low(Box<Expr>),
    Bank(String),
    Binary(Box<Expr>, BinOp, Box<Expr>),
}

/// The operand form of an instruction, determined syntactically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operand {
    Implied,
    Accumulator(char),
    Immediate(Expr),
    Indirect(Expr),
    /// `[expr, X]` or `[expr], Y` — register carried separately.
    IndirectIndexed(Expr, char),
    ZeroPage(Expr),
    ZeroPageIndexed(Expr, char),
    Absolute(Expr),
    AbsoluteIndexed(Expr, char),
}

/// A single assembly instruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instruction {
    pub mnemonic: String,
    pub operand: Operand,
}

/// A `.ascii` string with an optional per-byte offset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AsciiArg {
    pub text: String,
    pub offset: Option<Expr>,
    pub negate: bool,
}

/// A pseudo-op directive (Phase 2 subset; others are `Unsupported`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pseudo {
    Org(Expr),
    Db(Vec<Expr>),
    Dw(Vec<Expr>),
    Ascii(AsciiArg),
    Hibytes(Vec<Expr>),
    Lobytes(Vec<Expr>),
    Fill(Vec<Expr>),
    InesPrg(Expr),
    InesChr(Expr),
    InesMap(Expr),
    InesMir(Expr),
    Prg(Expr),
    Chr(Expr),
    Segment(String),
    /// A directive not yet implemented in this phase.
    Unsupported(String),
}

/// A top-level statement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stmt {
    /// A label definition. Name is `":"` for an anonymous label.
    Label(String),
    /// A constant definition; value expression is `None` for a bare name (=> 1).
    Constant(String, Option<Expr>),
    Instruction(Instruction),
    Pseudo(Pseudo),
}

/// A statement together with its 1-based source line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub stmt: Stmt,
    pub line: u32,
}
