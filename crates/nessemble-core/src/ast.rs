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

/// A single `.random` argument: a literal number or a string (hashed as a seed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RandTerm {
    Num(Expr),
    Str(String),
}

/// A pseudo-op directive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pseudo {
    Org(Expr),
    /// `.phase <expr>` — override the address labels receive so it reflects the
    /// bank's run-time (post-swap) location while ROM layout keeps flowing from
    /// `.org`. Remains in effect until `.dephase` or a bank/segment switch.
    Phase(Expr),
    /// `.dephase` — end a `.phase` block; labels revert to their load address.
    Dephase,
    Db(Vec<Expr>),
    Dw(Vec<Expr>),
    Ascii(AsciiArg),
    Hibytes(Vec<Expr>),
    Lobytes(Vec<Expr>),
    Fill(Vec<Expr>),
    Checksum(Expr),
    Random(Vec<RandTerm>),
    Color(Vec<Expr>),
    Enum(Expr, Option<Expr>),
    Endenum,
    /// `<label> .rs <size>`.
    Rs(String, Expr),
    Rsset(Expr),
    /// A numeric `.inesXxx` header directive: set `field` to the evaluated
    /// value. See [`InesField`] for the per-directive field and bit layout. The
    /// two non-numeric iNES directives (`.ines2`, `.inestiming`) have their own
    /// variants below.
    Ines(InesField, Expr),
    /// `.ines2 <flag>` — emit a NES 2.0 header (Flags 7 bits 2-3 = 2).
    Ines2(Expr),
    /// `.inestiming <n>` — NES 2.0 CPU/PPU timing (byte 12: 0 NTSC, 1 PAL,
    /// 2 multi-region, 3 Dendy).
    InesTiming(Expr),
    Prg(Expr),
    Chr(Expr),
    Segment(String),
    /// `.inestrn` — mark the iNES trainer region active (the trainer file's
    /// bytes are spliced in by the preprocessor immediately after this).
    InesTrn,
    /// `.if <expr>` — begin a conditional block.
    If(Expr),
    /// `.ifdef <symbol>` — begin a conditional block on symbol existence.
    Ifdef(String),
    /// `.ifndef <symbol>` — begin a conditional block on symbol absence.
    Ifndef(String),
    /// `.else` — invert the current conditional block.
    Else,
    /// `.endif` — end the current conditional block.
    Endif,
    /// `.incbin "file"[, offset[, limit]]` — raw binary include.
    Incbin(String, Option<Expr>, Option<Expr>),
    /// `.incpng "file"[, offset[, limit]]` — PNG → CHR tiles.
    Incpng(String, Option<Expr>, Option<Expr>),
    /// `.incpal "file"` — PNG → palette.
    Incpal(String),
    /// `.incrle "file"` — run-length-encoded binary include.
    Incrle(String),
    /// `.incwav "file"[, amplitude]` — WAV → DPCM.
    Incwav(String, Option<Expr>),
    /// `.font <start>[, <end>]` — bundled font glyphs for an ASCII range.
    Font(Vec<Expr>),
    /// `.defchr <8 rows>` — an 8×8 tile defined inline (8 tile-digit rows).
    Defchr(Vec<Expr>),
    /// A custom pseudo-op (`.foo`) resolved to a script at assemble time. The
    /// name has no leading dot; arguments preserve source order.
    Custom(String, Vec<CustomArg>),
}

/// The `Ines` header field written by a numeric `.inesXxx` directive. Each
/// variant names the directive's target field and its iNES / NES 2.0 bit layout.
/// `.ines2` and `.inestiming` are not here — they parse to their own [`Pseudo`]
/// variants because they set a flag / an optional field rather than a plain
/// numeric member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InesField {
    /// `.inesprg <count>` — PRG-ROM bank count (byte 4).
    Prg,
    /// `.ineschr <count>` — CHR-ROM bank count (byte 5).
    Chr,
    /// `.inesmap <n>` — mapper number (Flags 6/7 nibbles; byte 8 in NES 2.0).
    Map,
    /// `.inesmir <n>` — nametable mirroring (Flags 6 bit 0).
    Mir,
    /// `.inesbat <flag>` — battery-backed / persistent memory (Flags 6 bit 1).
    Bat,
    /// `.ines4scr <flag>` — four-screen VRAM / alt. nametable (Flags 6 bit 3).
    FourScreen,
    /// `.inesprgram <count>` — PRG-RAM size in 8 KB units (byte 8).
    PrgRam,
    /// `.inestv <system>` — TV system (Flags 9 bit 0 / Flags 10 bits 0-1:
    /// 0 NTSC, 1 PAL).
    Tv,
    /// `.inesvs <flag>` — VS Unisystem (Flags 7 bit 0 in iNES; console type 1
    /// in NES 2.0).
    Vs,
    /// `.inespc10 <flag>` — PlayChoice-10 (Flags 7 bit 1 in iNES; console type
    /// 2 in NES 2.0).
    Pc10,
    /// `.inessubmap <n>` — NES 2.0 submapper (byte 8 bits 4-7).
    SubMap,
    /// `.inesprgnvram <bytes>` — NES 2.0 battery PRG-RAM size (byte 10 bits 4-7).
    PrgNvRam,
    /// `.ineschrram <bytes>` — NES 2.0 volatile CHR-RAM size (byte 11 bits 0-3).
    ChrRam,
    /// `.ineschrnvram <bytes>` — NES 2.0 battery CHR-RAM size (byte 11 bits 4-7).
    ChrNvRam,
    /// `.inesconsole <n>` — NES 2.0 console type (Flags 7 bits 0-1).
    Console,
    /// `.inesvsppu <n>` — NES 2.0 VS System PPU type (byte 13 bits 0-3).
    VsPpu,
    /// `.inesvshw <n>` — NES 2.0 VS System hardware type (byte 13 bits 4-7).
    VsHw,
    /// `.inesmiscrom <n>` — NES 2.0 number of miscellaneous ROMs (byte 14).
    MiscRom,
    /// `.inesexpansion <n>` — NES 2.0 default expansion device (byte 15).
    Expansion,
}

/// A single argument to a custom pseudo-op: a numeric expression or a string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustomArg {
    Int(Expr),
    Str(String),
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

/// A statement together with its 1-based source line and source-file id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub stmt: Stmt,
    pub line: u32,
    pub file: u32,
    /// Whether this statement came from expanding a `.macro` invocation. Set from
    /// the statement's first token (see [`crate::lexer::Token::from_macro`]) so
    /// the assembler can flag labels defined in macros for the list file (`-l`).
    pub from_macro: bool,
}
