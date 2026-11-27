//! Two-pass assembler mirroring the reference `nessemble` semantics for the
//! Phase 2 (non-iNES-output) subset: instructions, symbols, expressions,
//! `.org`, data directives, and error reporting.
//!
//! Full iNES ROM output (header, banking, CHR/trainer padding) is Phase 3;
//! here we set the iNES-related state (so address math and `.org` validation
//! match) but emit the raw written region.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use nessemble_i18n::t;
use nessemble_isa::{AddressingMode, Opcode, META_UNDOCUMENTED, OPCODES};

use crate::ast::{
    BinOp, CustomArg, Expr, InesField, Instruction, Line, Name, Operand, Pseudo, Stmt, UnOp,
};
use crate::diagnostic::{AsmRule, AsmRules, Diag, Expansion, RuleSeverity};
use crate::locals::{self, LocalName};
use crate::program::{Emit, EmitKind, LabelDef, ProgramBuilder, ProgramMap, Site, Unmodelled, Via};

const BANK_PRG: i64 = 0x4000;
const BANK_CHR: i64 = 0x2000;
const MAX_BANKS: usize = 256;
/// iNES trainer region size (matches the reference `TRAINER_MAX`).
const TRAINER_MAX: usize = 512;
/// Matches the reference `MAX_NESTED_IFS`.
const MAX_NESTED_IFS: usize = 10;
/// Bound on `emit_source` expanding source that itself emits source,
/// recursively — the same depth `MAX_INCLUDE_DEPTH` (`preprocess.rs`) uses for
/// `.include`/`.macro` recursion, applied to this analogous case so a
/// pathological script errors instead of overflowing the stack.
const MAX_EMIT_SOURCE_DEPTH: usize = 10;
/// Most iterations a single `.for` may run (`plans/016-unrolled-loops.md` §5.2).
const MAX_FOR_ITERATIONS: u128 = 4096;
/// Most iterations a nest of `.for` blocks may run in total: the product of the
/// iteration counts of every loop open at the innermost body.
const MAX_FOR_NEST_ITERATIONS: u128 = 65536;

/// One open `.for` block on the assembler's loop stack.
#[derive(Debug, Clone, Copy)]
struct LoopFrame {
    /// Index (into the line slice being walked) of the first body line, which
    /// `.endfor` jumps back to.
    body_start: usize,
    /// The counter's value in the current iteration.
    current: i64,
    /// The counter's value in the last iteration.
    end: i64,
    /// `1` when counting up, `-1` when counting down.
    step: i64,
    /// Total iterations this loop runs.
    count: u128,
}

/// Convert a NES 2.0 RAM size in bytes to its logarithmic shift count, where a
/// present size is `64 << shift` bytes. Returns `Some(0)` for no RAM (0 bytes),
/// `Some(1..=15)` for a representable size (128 B … 2 MiB), or `None` when the
/// byte count is not `0` or a power-of-two multiple of 64 in range.
fn ram_shift_checked(bytes: i64) -> Option<i64> {
    if bytes == 0 {
        return Some(0);
    }
    if bytes < 0 || bytes % 64 != 0 {
        return None;
    }
    let units = (bytes / 64) as u64;
    if !units.is_power_of_two() {
        return None;
    }
    let shift = units.trailing_zeros() as i64;
    (1..=15).contains(&shift).then_some(shift)
}

/// Like [`ram_shift_checked`] but yields `0` for out-of-range input; callers
/// validate with [`ram_shift_checked`] before the header is built.
fn ram_shift(bytes: i64) -> i64 {
    ram_shift_checked(bytes).unwrap_or(0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SymType {
    Undefined,
    Label,
    Constant,
    /// A `.rs` reservation outside an `.enum` block (listed as a label).
    Rs,
    /// A `.rs` reservation inside an `.enum` block (listed as a constant).
    Enum,
}

/// A field a value is narrowed to fit when it is encoded, for the
/// `value-out-of-range` check ([`Assembler::check_range`]).
#[derive(Debug, Clone, Copy)]
enum Field {
    /// A data byte (`.db`, `.fill`, `.ascii`) or an immediate operand.
    Byte,
    /// A data word (`.dw`).
    Word,
    /// A zero-page address operand (`<x`, `[x, X]`, `[x], Y`).
    ZeroPage,
    /// An absolute address operand.
    Absolute,
}

impl Field {
    /// The values accepted without a warning. Data fields take
    /// two's-complement negatives too; address fields do not.
    fn range(self) -> (i64, i64) {
        match self {
            Field::Byte => (-128, 255),
            Field::Word => (-32768, 65535),
            Field::ZeroPage => (0, 255),
            Field::Absolute => (0, 65535),
        }
    }

    fn name(self) -> String {
        t!(match self {
            Field::Byte => "field-byte",
            Field::Word => "field-word",
            Field::ZeroPage => "field-zero-page",
            Field::Absolute => "field-absolute",
        })
    }
}

/// A contiguous run of emitted ROM bytes attributed to a single source line —
/// one entry in a [`SourceMap`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceSpan {
    /// Display name of the source file the bytes came from (the same name used
    /// in diagnostics; `stdin` for the top-level source when assembling text).
    pub file: Arc<str>,
    /// 1-based line within `file`.
    pub line: u32,
    /// Byte offset into the assembled ROM image. This is the PRG-then-CHR layout
    /// with **no iNES header** (the header is prepended only in the final NES
    /// file), so it is the same coordinate space a CDL file uses.
    pub rom_offset: usize,
    /// Number of consecutive ROM bytes this span covers.
    pub len: usize,
}

/// A byte-exact map from source lines to the ROM bytes they emitted, recorded in
/// emission order over the real post-macro / post-conditional program. Produced
/// only when [`crate::Options::source_map`] is set; otherwise the assembler does
/// no span bookkeeping and normal output is untouched.
///
/// Every recorded offset is one the assembler actually wrote (the same set the
/// internal write bitmap marks), and consecutive same-line bytes are coalesced
/// into one span.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceMap {
    /// Spans in emission order.
    pub spans: Vec<SourceSpan>,
}

/// A defined symbol exposed for the list file (`-l`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListSymbol {
    pub name: String,
    pub value: i64,
    pub bank: usize,
    /// Whether this is a label (vs. a constant), which selects its list
    /// section and formatting.
    pub label: bool,
    /// Whether the symbol was defined by expanding a `.macro`. The list file
    /// (`-l`) hides macro-defined labels unless `--mlist` is given.
    pub from_macro: bool,
}

#[derive(Debug, Clone)]
struct Symbol {
    name: String,
    value: i64,
    kind: SymType,
    bank: usize,
    /// Set when the symbol was defined on a macro-expanded source line.
    from_macro: bool,
    /// Where the symbol was last defined (display file, line), for naming it
    /// when a later definition replaces it. `None` until it is defined.
    defined_at: Option<(String, u32)>,
    /// The source site of that definition — (file id, line, expansion) — so a
    /// file included twice, which re-runs the very same definition, is not
    /// mistaken for a second one.
    site: (u32, u32, u32),
    /// The `@local` scope (global label) open where the symbol was defined, if
    /// any. Names a private local's parent in the list file.
    parent: Option<String>,
    /// The value may be wrong in the editor: a label after an unmodeled custom
    /// op in its bank, or a constant computed from such a symbol
    /// (`plans/022-assertions.md` §3).
    approx: bool,
    /// The ROM offset the definition was at, so a control transfer naming a
    /// label can be pinned to one ROM location
    /// (`plans/023-whole-program-clobber-analysis.md` §2.2). Never read by
    /// evaluation or the list file.
    rom_offset: usize,
}

#[derive(Debug, Clone, Copy)]
struct Ines {
    chr: i64,
    map: i64,
    /// Nametable mirroring (Flags 6 bit 0): `false` horizontal, `true` vertical.
    mir: bool,
    prg: i64,
    /// A trainer region is present (Flags 6 bit 2).
    trn: bool,
    /// Battery-backed / persistent memory (Flags 6 bit 1).
    bat: bool,
    /// Four-screen VRAM / alternative nametable layout (Flags 6 bit 3).
    fsc: bool,
    /// PRG-RAM size in 8 KB units (byte 8).
    prgram: i64,
    /// TV system (Flags 9 bit 0 / Flags 10 bits 0-1: 0 NTSC, 1 PAL).
    tv: i64,
    /// VS Unisystem (Flags 7 bit 0 in iNES; console type 1 in NES 2.0).
    vs: i64,
    /// PlayChoice-10 (Flags 7 bit 1 in iNES; console type 2 in NES 2.0).
    pc10: i64,
    /// Emit a NES 2.0 header rather than iNES 1.0 (`.ines2`).
    nes2: bool,
    /// NES 2.0 submapper (byte 8 bits 4-7).
    submap: i64,
    /// NES 2.0 battery PRG-RAM size in bytes (byte 10 bits 4-7).
    prgnvram: i64,
    /// NES 2.0 volatile CHR-RAM size in bytes (byte 11 bits 0-3).
    chrram: i64,
    /// NES 2.0 battery CHR-RAM size in bytes (byte 11 bits 4-7).
    chrnvram: i64,
    /// NES 2.0 CPU/PPU timing (byte 12). `None` falls back to `tv`.
    timing: Option<i64>,
    /// NES 2.0 console type (Flags 7 bits 0-1), when set via `.inesconsole`.
    console: i64,
    /// NES 2.0 VS System PPU type (byte 13 bits 0-3).
    vsppu: i64,
    /// NES 2.0 VS System hardware type (byte 13 bits 4-7).
    vshw: i64,
    /// NES 2.0 number of miscellaneous ROMs (byte 14).
    miscrom: i64,
    /// NES 2.0 default expansion device (byte 15).
    expansion: i64,
}

impl Default for Ines {
    fn default() -> Self {
        // Matches the reference initializer { chr:1, map:0, mir:0, prg:1, trn:0 };
        // the header extensions (bat/fsc/prgram/tv/vs/pc10) default to off/zero.
        Ines {
            chr: 1,
            map: 0,
            mir: false,
            prg: 1,
            trn: false,
            bat: false,
            fsc: false,
            prgram: 0,
            tv: 0,
            vs: 0,
            pc10: 0,
            nes2: false,
            submap: 0,
            prgnvram: 0,
            chrram: 0,
            chrnvram: 0,
            timing: None,
            console: 0,
            vsppu: 0,
            vshw: 0,
            miscrom: 0,
            expansion: 0,
        }
    }
}

/// One named `.enum` (`plans/019-value-enums.md` §3.3). Recorded on pass 1;
/// read-only on pass 2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumInfo {
    pub name: String,
    /// Where the `.enum` was written (a file id and line, for diagnostics that
    /// point at it).
    pub file: u32,
    pub line: u32,
    /// Value of the first member, or the evaluated `START` when empty.
    pub start: i64,
    pub step: i64,
    /// Recorded members in declaration order.
    pub members: Vec<EnumMember>,
    /// `members[i].value == start + i*step` for every `i`.
    pub contiguous: bool,
}

/// One recorded member of a named `.enum`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumMember {
    pub name: String,
    pub value: i64,
    /// Counter units consumed: 1 for a bare name, `N` for `.rs N`, 1 for `=`.
    pub size: i64,
    pub file: u32,
    pub line: u32,
}

impl EnumInfo {
    /// The number of recorded members (the value of `NAME_COUNT`).
    #[must_use]
    pub fn count(&self) -> usize {
        self.members.len()
    }
}

/// The `.enum` block currently open, while the assembler walks its body.
struct OpenEnum {
    name: Option<String>,
    /// The `.enum` line's site, so diagnostics and `NAME_COUNT` point at it.
    file: u32,
    line: u32,
    expansion: u32,
    from_macro: bool,
    /// Opened inside a false conditional: records nothing.
    suppressed: bool,
    /// The evaluated `START` argument.
    start: i64,
    step: i64,
    members: Vec<EnumMember>,
    /// Member names defined so far, and whether any definition was bare.
    seen: HashMap<String, bool>,
}

/// Where a deferred check reports: the line the author wrote
/// (`plans/020-index-keyed-tables.md` §5.2).
#[derive(Debug, Clone)]
pub(crate) struct Origin {
    pub file: u32,
    pub line: u32,
    /// `Line.expansion`; `locate` turns it into the body file and notes.
    pub expansion: u32,
    /// The `.for` iteration counters captured at record time, if any.
    pub suffix: Option<String>,
}

/// A check that needs settled symbol values and runs exactly once per build.
pub(crate) struct Deferred {
    pub origin: Origin,
    pub check: DeferredCheck,
}

pub(crate) enum DeferredCheck {
    Table(OpenTable),
    /// `.assert` (`plans/022-assertions.md` §2.2).
    Assert(Box<AssertCheck>),
    /// `.error`: the message (counters substituted) and the message as
    /// written, which de-duplicates it across `.for` iterations.
    Error {
        message: String,
        template: String,
    },
}

/// A queued `.assert`.
pub(crate) struct AssertCheck {
    expr: Expr,
    /// The expression as written.
    text: String,
    /// The message with `\i`/`\j`/`\k` substituted.
    message: Option<String>,
    /// The message as written, for de-duplication across iterations.
    template: Option<String>,
    ctx: EvalCtx,
}

/// What an assertion's expression needs from the line it was written on,
/// captured at queue time so the drain can evaluate it later.
pub(crate) struct EvalCtx {
    /// Loop counter values, innermost first (`\i`, `\j`, `\k`).
    loops: Vec<i64>,
    /// `address_offset()` and `prg_index`, for `:+` / `:-`.
    address: i64,
    bank: usize,
    /// The `@local` scope open on the line.
    scope: Option<String>,
    /// The line sits after an unmodeled custom op in its bank (§4).
    approx: bool,
}

/// Why the drain's evaluator could not settle an expression (§2.7).
enum Unresolved {
    Symbol(String),
    Loop { depth: u8, open: usize },
    Local(i32),
}

/// The `.table` being assembled (pass 2 only).
pub(crate) struct OpenTable {
    /// The `.table` line.
    origin: Origin,
    spec: TableSpec,
    entries: Vec<EntryRec>,
    untagged_reported: bool,
    /// The `COUNT`/`FIRST` arguments read an approximate symbol.
    spec_approx: bool,
}

/// What a `.table`'s arguments resolved to.
enum TableSpec {
    /// `.table`: keys from 0, any count.
    Order { first: i64 },
    /// `.table COUNT[, FIRST]`.
    Count { count: i64, first: i64 },
    /// `.table ENUM[, FIRST]`: one entry per member from `FIRST` on.
    KeySet {
        name: String,
        first: i64,
        step: i64,
        /// The members checked: `(name, value)` from `FIRST` on.
        members: Vec<(String, i64)>,
    },
    /// The arguments were in error (already reported): check sizes only.
    Invalid,
}

impl TableSpec {
    fn first_and_step(&self) -> Option<(i64, i64)> {
        match self {
            TableSpec::Order { first } | TableSpec::Count { first, .. } => Some((*first, 1)),
            TableSpec::KeySet { first, step, .. } => Some((*first, *step)),
            TableSpec::Invalid => None,
        }
    }
}

/// One recorded `[KEY]` entry.
struct EntryRec {
    origin: Origin,
    key_text: String,
    /// The key was a numeric literal, so messages don't repeat its value.
    literal: bool,
    key: i64,
    bytes: usize,
    /// The entry holds an unmodeled custom op, so its size is not known (§4
    /// of plan 022; editor only).
    approx: bool,
    /// The key read a symbol whose value is approximate.
    key_approx: bool,
}

/// One checked `.table` entry, for tooling (the language server's hovers).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableEntryInfo {
    /// Display file and line of the entry's tag.
    pub file: String,
    pub line: u32,
    /// Display file and line of its `.table`.
    pub table_file: String,
    pub table_line: u32,
    /// The tag's value.
    pub key: i64,
    /// The key the entry's position calls for (`FIRST + n·STEP`), or the
    /// tag's value when the table's arguments were in error.
    pub index: i64,
    /// Bytes the entry emitted.
    pub size: usize,
    /// The table's entry size (entry 0's).
    pub table_size: usize,
    /// How many entries the table has.
    pub table_entries: usize,
    /// The named enum the table is keyed by, if any.
    pub keyed_by: Option<String>,
    /// The entry passed its index and size checks.
    pub ok: bool,
}

/// A value in an assertion failure (§2.5): decimal for 0–9 and negatives,
/// else `$`-hex padded to 2 or 4 digits (wider when needed).
fn assert_value(v: i64) -> String {
    if (0..=9).contains(&v) || v < 0 {
        v.to_string()
    } else if v <= 0xFF {
        format!("${v:02X}")
    } else {
        format!("${v:04X}")
    }
}

/// The operator a comparison is written with, for an assertion's detail.
fn comparison(op: BinOp) -> Option<&'static str> {
    Some(match op {
        BinOp::Eq => "==",
        BinOp::Ne => "!=",
        BinOp::Lt => "<",
        BinOp::Gt => ">",
        BinOp::Le => "<=",
        BinOp::Ge => ">=",
        _ => return None,
    })
}

/// Most `name = VALUE` entries an assertion failure lists.
const ASSERT_DETAIL_NAMES: usize = 8;

/// `$NN` for 0–255, `$NNNN` above that, decimal when negative.
fn hex_value(v: i64) -> String {
    if v < 0 {
        v.to_string()
    } else if v <= 0xFF {
        format!("${v:02X}")
    } else {
        format!("${v:04X}")
    }
}

/// The assembler state for a single run.
pub struct Assembler {
    nes: bool,
    undocumented: bool,
    empty_byte: u8,

    ines: Ines,
    prg_offsets: Vec<i64>,
    chr_offsets: Vec<i64>,
    prg_index: usize,
    chr_index: usize,
    segment_prg: bool,

    /// Delta added to the physical load address (`address_offset`) to yield the
    /// address labels receive, set by `.phase` and cleared by `.dephase` (or a
    /// bank/segment switch). Zero when no phase is active. See [`Pseudo::Phase`].
    phase_delta: i64,

    rom: Vec<u8>,
    /// Per-ROM-byte write bitmap (which bytes were written). Used to bound
    /// second-pass writes and as the basis for the source map's recording.
    coverage: Vec<bool>,
    offset_max: usize,

    /// When set, record a byte-exact source map (see [`SourceMap`]) as bytes are
    /// emitted. Off by default so ordinary assembly does no extra work.
    record_source_map: bool,
    /// Coalesced source spans collected during pass 2 when `record_source_map`
    /// is set. Stored with a file *index* (into `files`) and resolved to
    /// `Arc<str>` names only when [`Assembler::source_map`] is called.
    spans: Vec<RawSpan>,

    /// When set, record what pass 2 emitted for whole-program analysis (see
    /// [`ProgramMap`]). Off by default; it only reads state.
    record_program: bool,
    program: ProgramBuilder,

    symbols: Vec<Symbol>,

    enum_active: bool,
    enum_value: i64,
    enum_inc: i64,
    /// The `.enum` being walked (see [`OpenEnum`]); `None` outside one.
    open_enum: Option<OpenEnum>,
    /// The enum registry: every named `.enum`, in source order. Built on pass
    /// 1 and never cleared, like `symbols`.
    enums: Vec<EnumInfo>,
    enum_index: HashMap<String, usize>,
    rsset: i64,

    // Conditional-assembly state (`.if`/`.ifdef`/`.ifndef`/`.else`/`.endif`):
    // one entry per open block, holding its (possibly `.else`-flipped)
    // condition. Empty means no conditional is open.
    if_stack: Vec<bool>,

    // `.for` blocks currently open, innermost last (`\i` reads the last frame,
    // `\j` the one before it, ...). Empty outside any loop.
    loop_stack: Vec<LoopFrame>,
    /// Diagnostics already reported from inside a loop body this pass, keyed by
    /// (file, line, message) so a per-iteration error is reported once.
    loop_err_seen: std::collections::HashSet<(u32, u32, String)>,
    /// The first symbol an expression evaluation touched that was not yet
    /// known (undefined, or a local-label reference on pass 1). Reset by the
    /// caller before evaluating a `.for` bound, which must be known on pass 1.
    eval_unknown: Option<String>,
    /// Each `.for` execution's evaluated (file, line, start, end) in execution
    /// order, recorded on pass 1 and replayed on pass 2 so the iteration count
    /// (which sizes everything after the loop) is fixed on pass 1.
    for_log: Vec<(u32, u32, i64, i64)>,
    /// Next `for_log` entry pass 2 will replay; reset at the start of a pass.
    for_cursor: usize,

    // iNES trainer redirection (`.inestrn`).
    trainer: Vec<u8>,
    offset_trainer: usize,

    pass: u8,
    errors: Vec<Diag>,
    warnings: Vec<Diag>,
    /// Configured severity of each [`AsmRule`].
    rules: AsmRules,
    /// Rule diagnostics already reported, keyed by (display file, line, rule),
    /// so each source location reports a rule once — whichever pass, loop
    /// iteration, or macro expansion reaches it again.
    rule_seen: std::collections::HashSet<(String, u32, AsmRule)>,
    /// Rule diagnostics raised before assembly (by the preprocessor), reported
    /// at their configured severity when the run starts.
    pending: Vec<Diag>,
    /// The `.macro` expansion table (see [`Expansion`]), for locating
    /// diagnostics on macro-expanded lines.
    expansions: Vec<Expansion>,
    /// The expansion the current source line came from (`0` for none).
    cur_expansion: u32,
    aborted: bool,
    /// When set, a hard error records a diagnostic but does **not** abort the
    /// run, so multiple problems can be collected in one pass (for tooling
    /// diagnostics). The normal `assemble` path leaves this `false` and stops at
    /// the first hard error, exactly as before.
    collect_all: bool,
    cur_line: u32,
    cur_file: u32,
    /// Whether the current source line came from a macro expansion, propagated
    /// from [`Line::from_macro`] so symbols defined here are flagged.
    cur_from_macro: bool,
    files: Vec<String>,
    /// Directory each source file resolves filename-based directives
    /// (`.incbin`/`.incpng`/… and custom pseudo-ops) against, parallel to
    /// `files`. Indexed by `cur_file`, so a directive is resolved relative to
    /// the file that contains it rather than the top-level file.
    dirs: Vec<PathBuf>,
    /// Resolved filesystem path of each source file, parallel to `files`. Used
    /// to give the source map real, uniformly-rooted paths (the `files` display
    /// names are per-file-relative and lose the top-level directory).
    paths: Vec<PathBuf>,
    /// The project root a `@/`-prefixed filename argument resolves against. See
    /// [`crate::PROJECT_ROOT_PREFIX`] and `plans/012-project-root-paths.md`.
    root: Option<PathBuf>,
    /// Resolver for custom pseudo-ops (`.foo`): given the directive name, its
    /// numeric and string arguments, the base directory, and the project root
    /// (if any), it returns what the directive resolves to (or an error
    /// message).
    custom: CustomResolver,
    /// What each custom-directive invocation in this run resolved to, so a
    /// directive's script executes **once** rather than once per pass. See
    /// [`Assembler::exec_custom`]; kept for the whole run (not cleared between
    /// passes), since sharing across passes is the point. Memoizing the
    /// *source* of an `emit_source` result (rather than only its eventual
    /// bytes) is what keeps the expansion deterministic across both passes —
    /// see [`Assembler::exec_emitted_source`].
    custom_memo: HashMap<CustomKey, Result<CustomOutput, String>>,
    /// Recursion depth of `emit_source` expansion currently in progress — an
    /// expansion whose own source contains a directive that itself emits
    /// source increments this around the nested call. See
    /// [`MAX_EMIT_SOURCE_DEPTH`].
    custom_expansion_depth: usize,
    /// The `.table` open on pass 2 (`None` outside one, or when suppressed).
    table: Option<OpenTable>,
    /// Checks queued on the final pass, run once by [`Self::run_deferred`].
    deferred: Vec<Deferred>,
    /// Deferred diagnostics already reported, keyed by (file, line, expansion,
    /// message without its `.for` suffix).
    deferred_seen: std::collections::HashSet<(u32, u32, u32, String)>,
    /// Every checked `.table` entry, for tooling.
    table_entries: Vec<TableEntryInfo>,
    /// The `@local` scope of each source file: the last global label defined
    /// in it so far this pass (`plans/021-local-labels.md` §2.3). Indexed by
    /// file id; reset at the start of each pass.
    scopes: Vec<Option<String>>,
    /// The private-local id of each emitted-source expansion, keyed by where
    /// it runs: (enclosing emitted id, file, line, expansion, index of the
    /// directive in the enclosing emitted source). Kept across passes so an
    /// expansion's id is the same on both. Ids continue after the `.macro`
    /// expansion ids, so the two never collide.
    emitted_ids: HashMap<(u32, u32, u32, u32, usize), u32>,
    /// The directive name of each emitted-source expansion, by private id.
    emitted_names: HashMap<u32, String>,
    /// The emitted-source expansion running now (`0` for none) and the index of
    /// the line it is executing.
    emit_ctx: (u32, usize),
    /// `.out` lines recorded on the final pass, in execution order.
    out_lines: Vec<Diag>,
    /// Per bank (PRG `0..MAX_BANKS`, then CHR), whether an unmodeled custom op
    /// has been written since the bank's last `.org`, so later addresses there
    /// are approximate (the editor's lenient resolver only).
    approx_bank: Vec<bool>,
    /// Set when an evaluation reads an approximate symbol; reset by callers.
    eval_taint: bool,
    /// The constant being defined was computed from an approximate symbol.
    def_taint: bool,
}

/// A source span as collected during assembly: like [`SourceSpan`] but keyed by
/// a file *index* into `Assembler::files`, so no string is cloned per byte.
#[derive(Clone)]
struct RawSpan {
    file: u32,
    line: u32,
    rom_offset: usize,
    len: usize,
}

/// Resolves a custom pseudo-op to what it produces. See [`Assembler::custom`].
///
/// The fourth argument is the base directory (the directive's own source
/// file's directory); the fifth is the project root a `@/`-prefixed path a
/// script resolves itself should join against, `None` when no root could be
/// determined (`plans/013-structured-data-parsing.md` §11.1). It is the same
/// value [`crate::resolve_path_arg`] takes for every other path-taking
/// argument, so a script's own file API resolves `@/` identically to
/// `.incbin`/a declared `file://` argument.
pub type CustomResolver = Box<
    dyn Fn(
        &str,
        &[i64],
        &[String],
        &std::path::Path,
        Option<&std::path::Path>,
    ) -> Result<CustomOutput, String>,
>;

/// What a custom directive's script resolves to.
///
/// `Source` is what `emit_source(text)` produces (`plans/013-structured-data-parsing.md`
/// §6): assembly source expanded inline at the directive's own call site, like
/// a `.macro` invocation whose body is not known until the script runs —
/// distinct from `Bytes`, which is what a plain returned string/array/blob
/// already meant (emit these bytes verbatim).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustomOutput {
    /// Bytes to write through the normal emission path.
    Bytes(Vec<u8>),
    /// Assembly source to lex, parse, and execute inline
    /// ([`Assembler::exec_emitted_source`]).
    Source(String),
    /// What the directive emits is not known (tooling did not run its
    /// script). Writes nothing, and marks later addresses in the bank as
    /// approximate so checks that read them are not reported
    /// (`plans/022-assertions.md` §4).
    Unmodeled,
}

/// Identifies one custom-directive invocation: the directive name, its evaluated
/// integer and string arguments, and the **site** it was written at (file index
/// into `Assembler::files` and line). The site keeps two identical-looking calls
/// distinct, so a script that deliberately returns something different each time
/// (`rand`, per the extending docs' `.noise` example) still varies *between* call
/// sites; the base directory is implied by the file index, so it is not stored
/// separately. See [`Assembler::custom_memo`].
type CustomKey = (String, Vec<i64>, Vec<String>, u32, u32);

/// Per-file resolution tables threaded from preprocessing into the assembler:
/// display names, resolution directories, resolved paths, and the project root
/// `@/` arguments resolve against (see [`crate::PROJECT_ROOT_PREFIX`]). These
/// travel together — they come from [`crate::preprocess::Preprocessed`] as a
/// unit and are consumed together here — so they're a struct rather than four
/// positional [`Assembler::new`] parameters (`plans/012-project-root-paths.md`
/// §4.2).
pub(crate) struct SourceTables {
    pub files: Vec<String>,
    pub dirs: Vec<PathBuf>,
    pub paths: Vec<PathBuf>,
    pub root: Option<PathBuf>,
    /// The preprocessor's `.macro` expansion table.
    pub expansions: Vec<Expansion>,
    /// Rule diagnostics the preprocessor raised (see [`Assembler::pending`]).
    pub pending: Vec<Diag>,
}

/// Why a media importer (`.incbin`/`.incpng`/…) could not produce bytes: either
/// the filename argument could not be resolved at all (a `@/` name with no
/// project root, or one that escapes it — [`crate::PathArgError`]) or it
/// resolved fine but the file at that path could not be read. The two are
/// reported with different diagnostics — see
/// `plans/012-project-root-paths.md` §6.
enum MediaError {
    Path(crate::PathArgError),
    NotFound,
}

impl Assembler {
    pub fn new(
        nes: bool,
        undocumented: bool,
        empty_byte: u8,
        tables: SourceTables,
        rules: AsmRules,
        custom: CustomResolver,
    ) -> Self {
        let SourceTables {
            files,
            dirs,
            paths,
            root,
            expansions,
            pending,
        } = tables;
        Assembler {
            nes,
            undocumented,
            empty_byte,
            ines: Ines::default(),
            prg_offsets: vec![0; MAX_BANKS],
            chr_offsets: vec![0; MAX_BANKS],
            prg_index: 0,
            chr_index: 0,
            segment_prg: true,
            phase_delta: 0,
            rom: Vec::new(),
            coverage: Vec::new(),
            offset_max: 0,
            record_source_map: false,
            spans: Vec::new(),
            record_program: false,
            program: ProgramBuilder::default(),
            symbols: Vec::new(),
            enum_active: false,
            enum_value: 0,
            enum_inc: 0,
            open_enum: None,
            enums: Vec::new(),
            enum_index: HashMap::new(),
            rsset: 0,
            if_stack: Vec::new(),
            loop_stack: Vec::new(),
            loop_err_seen: std::collections::HashSet::new(),
            eval_unknown: None,
            for_log: Vec::new(),
            for_cursor: 0,
            trainer: Vec::new(),
            offset_trainer: 0,
            pass: 1,
            errors: Vec::new(),
            warnings: Vec::new(),
            rules,
            rule_seen: std::collections::HashSet::new(),
            pending,
            expansions,
            cur_expansion: 0,
            aborted: false,
            collect_all: false,
            cur_line: 1,
            cur_file: 0,
            cur_from_macro: false,
            files,
            dirs,
            paths,
            root,
            custom,
            custom_memo: HashMap::new(),
            custom_expansion_depth: 0,
            table: None,
            deferred: Vec::new(),
            deferred_seen: std::collections::HashSet::new(),
            table_entries: Vec::new(),
            scopes: Vec::new(),
            emitted_ids: HashMap::new(),
            emitted_names: HashMap::new(),
            emit_ctx: (0, 0),
            out_lines: Vec::new(),
            approx_bank: vec![false; MAX_BANKS * 2],
            eval_taint: false,
            def_taint: false,
        }
    }

    /// Run both passes in collect mode and return every distinct error and
    /// warning (deduplicated by file/line/message, preserving first-seen order).
    /// The output bytes are discarded — this is for diagnostics only.
    pub fn diagnostics(&mut self, lines: &[Line]) -> (Vec<Diag>, Vec<Diag>) {
        self.collect_all = true;
        self.report_pending();
        // Pass 1: build symbols and size the ROM (best effort past errors).
        self.pass = 1;
        self.reset_state();
        self.run_pass(lines);
        if self.nes {
            self.offset_max = (self.ines.prg * BANK_PRG + self.ines.chr * BANK_CHR).max(0) as usize;
        }
        // Pass 2: surface symbol-resolution errors; write_byte is bounds-checked.
        self.rom = vec![self.empty_byte; self.offset_max];
        self.coverage = vec![false; self.offset_max];
        self.trainer = vec![self.empty_byte; TRAINER_MAX];
        self.pass = 2;
        self.reset_state();
        self.run_pass(lines);
        self.validate_ines();
        self.run_deferred();
        (dedup(&self.errors), dedup(&self.warnings))
    }

    /// Enable or disable byte-exact source-map recording for the next [`run`].
    /// Off by default; when off, [`source_map`] returns `None` and no span
    /// bookkeeping happens.
    ///
    /// [`run`]: Assembler::run
    /// [`source_map`]: Assembler::source_map
    pub fn set_record_source_map(&mut self, on: bool) {
        self.record_source_map = on;
    }

    /// Enable or disable program recording (see [`ProgramMap`]) for the next
    /// run. Off by default; it never changes the emitted bytes.
    pub fn set_record_program(&mut self, on: bool) {
        self.record_program = on;
    }

    /// What the last run's pass 2 emitted, or `None` when recording was off.
    pub fn take_program(&mut self) -> Option<ProgramMap> {
        if !self.record_program {
            return None;
        }
        let builder = std::mem::take(&mut self.program);
        let files = self.paths.clone();
        let symbols = self
            .symbols
            .iter()
            .filter(|s| s.kind != SymType::Undefined && s.name != ":")
            .map(|s| (s.name.clone(), s.value))
            .collect();
        Some(builder.finish(
            self.rom.clone(),
            self.coverage.clone(),
            files,
            self.undocumented,
            symbols,
        ))
    }

    /// The byte-exact source map collected during the last [`run`], or `None`
    /// when recording was not enabled (see [`set_record_source_map`]).
    ///
    /// Each file is identified by its **resolved filesystem path**
    /// (canonicalized to an absolute path), so a consumer can locate the source
    /// on disk — the per-file display names lose the top-level directory and are
    /// each relative to a different include base. Files with no real path on disk
    /// (e.g. `stdin` text assembly) fall back to their display name. One
    /// `Arc<str>` per file is shared across all its spans.
    ///
    /// [`run`]: Assembler::run
    /// [`set_record_source_map`]: Assembler::set_record_source_map
    pub fn source_map(&self) -> Option<SourceMap> {
        if !self.record_source_map {
            return None;
        }
        // Resolve each file index once. Prefer the canonical absolute path; fall
        // back to the raw resolved path, then to the display name.
        let file_ids: Vec<Arc<str>> = (0..self.files.len())
            .map(|i| {
                let resolved = self
                    .paths
                    .get(i)
                    .and_then(|p| p.canonicalize().ok())
                    .map(|p| p.to_string_lossy().into_owned())
                    .or_else(|| self.files.get(i).cloned());
                Arc::from(resolved.unwrap_or_default().as_str())
            })
            .collect();
        let empty: Arc<str> = Arc::from("");
        let spans = self
            .spans
            .iter()
            .map(|r| SourceSpan {
                file: file_ids.get(r.file as usize).unwrap_or(&empty).clone(),
                line: r.line,
                rom_offset: r.rom_offset,
                len: r.len,
            })
            .collect();
        Some(SourceMap { spans })
    }

    /// Symbols eligible for the list file (`-l`), excluding those only
    /// referenced but never defined.
    pub fn list_symbols(&self) -> Vec<ListSymbol> {
        self.symbols
            .iter()
            .filter(|s| s.kind != SymType::Undefined)
            .map(|s| ListSymbol {
                name: list_name(s),
                value: s.value,
                bank: s.bank,
                // `.rs` reservations list as labels; `.enum` entries and plain
                // constants list as constants.
                label: matches!(s.kind, SymType::Label | SymType::Rs),
                from_macro: s.from_macro,
            })
            .collect()
    }

    /// Warnings collected during assembly (in source order), each with the
    /// reference-compatible message.
    pub fn take_warnings(&mut self) -> Vec<Diag> {
        std::mem::take(&mut self.warnings)
    }

    /// The `.out` lines the last run printed, in execution order
    /// (`plans/022-assertions.md` §2.6).
    pub fn take_out(&mut self) -> Vec<Diag> {
        std::mem::take(&mut self.out_lines)
    }

    /// Run both passes over the parsed program, returning the output bytes
    /// (including the iNES header in NES mode) or the diagnostic to report.
    pub fn run(&mut self, lines: &[Line]) -> Result<Vec<u8>, Diag> {
        self.report_pending();
        if let Some(d) = self.errors.last() {
            return Err(d.clone());
        }
        // Pass 1: build symbols, size the ROM.
        self.pass = 1;
        self.reset_state();
        self.run_pass(lines);
        self.check_unterminated_enum();
        if let Some(d) = self.errors.last() {
            return Err(d.clone());
        }

        // In NES mode the ROM is a fixed size (PRG banks + CHR banks); in raw
        // mode it is the high-water mark computed during pass 1.
        if self.nes {
            self.offset_max = (self.ines.prg * BANK_PRG + self.ines.chr * BANK_CHR).max(0) as usize;
        }

        // Allocate ROM (and trainer, filled with the empty byte) and run pass 2
        // to emit.
        self.rom = vec![self.empty_byte; self.offset_max];
        self.coverage = vec![false; self.offset_max];
        self.trainer = vec![self.empty_byte; TRAINER_MAX];
        self.pass = 2;
        self.aborted = false;
        self.reset_state();
        self.run_pass(lines);
        self.validate_ines();
        if self.errors.is_empty() {
            self.run_deferred();
        }
        if let Some(d) = self.errors.last() {
            return Err(d.clone());
        }

        Ok(self.build_output())
    }

    fn reset_state(&mut self) {
        self.prg_offsets.fill(0);
        self.chr_offsets.fill(0);
        self.prg_index = 0;
        self.chr_index = 0;
        self.segment_prg = true;
        self.phase_delta = 0;
        self.ines = Ines::default();
        self.enum_active = false;
        self.enum_value = 0;
        self.enum_inc = 0;
        self.open_enum = None;
        self.rsset = 0;
        self.if_stack.clear();
        self.loop_stack.clear();
        self.loop_err_seen.clear();
        if self.pass == 1 {
            self.for_log.clear();
            self.deferred.clear();
            self.deferred_seen.clear();
            self.table_entries.clear();
        }
        self.table = None;
        self.out_lines.clear();
        self.approx_bank.fill(false);
        self.eval_taint = false;
        self.def_taint = false;
        self.for_cursor = 0;
        self.offset_trainer = 0;
        self.scopes.clear();
        // Spans are collected fresh on pass 2; clearing here drops any from an
        // earlier pass so the final map reflects only the emitting pass.
        self.spans.clear();
        self.program.clear();
    }

    /// Whether the current statement is suppressed by a false conditional
    /// block. Mirrors the reference guard used in `write_byte`/`add_symbol`,
    /// which checks the current level and (when nested) its parent.
    fn if_suppressed(&self) -> bool {
        let depth = self.if_stack.len();
        if depth == 0 {
            return false;
        }
        // Defensive bound: unbalanced `.if` nesting past the limit can only
        // happen on malformed input (which produces no golden ROM); matching the
        // reference, nothing past the limit is suppressed.
        if depth >= MAX_NESTED_IFS {
            return false;
        }
        // Suppressed when the current level is false, or — when nested — its
        // immediate parent is. The reference checks only the current level and
        // one level up, not the whole stack; preserve that exactly.
        if !self.if_stack[depth - 1] {
            return true;
        }
        depth >= 2 && !self.if_stack[depth - 2]
    }

    /// Whether any open conditional level is false: the strict test `.assert`,
    /// `.error` and `.out` use (`plans/022-assertions.md` §2.3).
    fn if_any_false(&self) -> bool {
        self.if_stack.iter().any(|c| !c)
    }

    /// Assemble the final output bytes: raw ROM, or an iNES / NES 2.0 file in
    /// NES mode.
    fn build_output(&self) -> Vec<u8> {
        if !self.nes {
            return self.rom.clone();
        }
        let mut out = Vec::with_capacity(16 + self.rom.len());
        out.extend_from_slice(b"NES");
        out.push(0x1A);
        // Bytes 4-6 are identical in iNES 1.0 and NES 2.0.
        out.push((self.ines.prg & 0xFF) as u8);
        out.push((self.ines.chr & 0xFF) as u8);
        let byte6 = i64::from(self.ines.mir)
            | (i64::from(self.ines.bat) << 1)
            | (i64::from(self.ines.trn) << 2)
            | (i64::from(self.ines.fsc) << 3)
            | ((self.ines.map & 0x0F) << 4);
        out.push((byte6 & 0xFF) as u8);
        if self.ines.nes2 {
            self.build_header_nes2(&mut out);
        } else {
            self.build_header_ines(&mut out);
        }
        // A trainer, when present, sits between the header and the PRG/CHR data.
        if self.ines.trn {
            out.extend_from_slice(&self.trainer);
        }
        out.extend_from_slice(&self.rom);
        out
    }

    /// Bytes 7-15 of an iNES 1.0 header (bytes 0-6 already emitted).
    fn build_header_ines(&self, out: &mut Vec<u8>) {
        let byte7 = (self.ines.vs & 0x01) | ((self.ines.pc10 & 0x01) << 1) | (self.ines.map & 0xF0);
        out.push((byte7 & 0xFF) as u8);
        // Byte 8: PRG-RAM size (8 KB units). Byte 9: TV system (bit 0).
        out.push((self.ines.prgram & 0xFF) as u8);
        out.push((self.ines.tv & 0x01) as u8);
        // Byte 10: unofficial TV-system field (0: NTSC; 2: PAL), mirroring byte 9.
        let byte10 = if self.ines.tv & 0x01 != 0 { 0b10 } else { 0b00 };
        out.push(byte10);
        // iNES header bytes 11..15 are zero.
        out.resize(out.len() + 5, 0x00);
    }

    /// Bytes 7-15 of a NES 2.0 header (bytes 0-6 already emitted).
    fn build_header_nes2(&self, out: &mut Vec<u8>) {
        let i = &self.ines;
        // Byte 7: console type (D0-1), NES 2.0 identifier (D2-3 = 0b10), mapper
        // nibble 1 (D4-7).
        let byte7 = (self.console_value() & 0x03) | 0x08 | (((i.map >> 4) & 0x0F) << 4);
        out.push((byte7 & 0xFF) as u8);
        // Byte 8: mapper nibble 2 (D0-3) + submapper (D4-7).
        out.push((((i.map >> 8) & 0x0F) | ((i.submap & 0x0F) << 4)) as u8);
        // Byte 9: PRG-ROM size MSB (D0-3) + CHR-ROM size MSB (D4-7).
        out.push((((i.prg >> 8) & 0x0F) | (((i.chr >> 8) & 0x0F) << 4)) as u8);
        // Byte 10: PRG-RAM (D0-3) + PRG-NVRAM (D4-7) shift counts.
        out.push((ram_shift(i.prgram) | (ram_shift(i.prgnvram) << 4)) as u8);
        // Byte 11: CHR-RAM (D0-3) + CHR-NVRAM (D4-7) shift counts.
        out.push((ram_shift(i.chrram) | (ram_shift(i.chrnvram) << 4)) as u8);
        // Byte 12: CPU/PPU timing; `.inestv` provides the NTSC/PAL fallback.
        out.push((i.timing.unwrap_or(i.tv & 0x01) & 0x03) as u8);
        // Byte 13: VS System PPU (D0-3) + hardware (D4-7) type (console type 1).
        let byte13 = if self.console_value() == 1 {
            (i.vsppu & 0x0F) | ((i.vshw & 0x0F) << 4)
        } else {
            0
        };
        out.push((byte13 & 0xFF) as u8);
        // Byte 14: number of miscellaneous ROMs (D0-1).
        out.push((i.miscrom & 0x03) as u8);
        // Byte 15: default expansion device (D0-5).
        out.push((i.expansion & 0x3F) as u8);
    }

    /// The resolved NES 2.0 console type (Flags 7 bits 0-1) from the explicit
    /// `.inesconsole` value and the `.inesvs` / `.inespc10` sugar. Any conflict
    /// is reported separately by [`Self::validate_ines`]; here the explicit
    /// value wins, then VS, then PlayChoice-10.
    fn console_value(&self) -> i64 {
        if self.ines.console != 0 {
            self.ines.console
        } else if self.ines.vs != 0 {
            1
        } else if self.ines.pc10 != 0 {
            2
        } else {
            0
        }
    }

    /// Whether `.inesconsole`, `.inesvs`, and `.inespc10` disagree about the
    /// NES 2.0 console type.
    fn console_conflict(&self) -> bool {
        let mut chosen: Option<i64> = None;
        let intents = [
            (self.ines.console != 0).then_some(self.ines.console),
            (self.ines.vs != 0).then_some(1),
            (self.ines.pc10 != 0).then_some(2),
        ];
        for v in intents.into_iter().flatten() {
            match chosen {
                Some(c) if c != v => return true,
                _ => chosen = Some(v),
            }
        }
        false
    }

    fn validate_range(&mut self, field: &str, value: i64, min: i64, max: i64) {
        if !(min..=max).contains(&value) {
            self.hard_error(t!(
                "nes2-range",
                field = field,
                value = value,
                min = min,
                max = max
            ));
        }
    }

    /// Validate the iNES / NES 2.0 header state after both passes, reporting
    /// out-of-range fields, malformed RAM sizes, console-type conflicts, and
    /// NES 2.0-only directives used without `.ines2`.
    fn validate_ines(&mut self) {
        if !self.nes {
            return;
        }
        let i = self.ines;
        if i.nes2 {
            self.validate_range("Mapper number", i.map, 0, 4095);
            self.validate_range("PRG bank count", i.prg, 0, 4095);
            self.validate_range("CHR bank count", i.chr, 0, 4095);
            self.validate_range(".inessubmap", i.submap, 0, 15);
            if let Some(t) = i.timing {
                self.validate_range(".inestiming", t, 0, 3);
            }
            self.validate_range(".inesconsole", i.console, 0, 3);
            self.validate_range(".inesmiscrom", i.miscrom, 0, 3);
            self.validate_range(".inesexpansion", i.expansion, 0, 63);
            self.validate_range(".inesvsppu", i.vsppu, 0, 15);
            self.validate_range(".inesvshw", i.vshw, 0, 15);
            for (val, field) in [
                (i.prgram, ".inesprgram"),
                (i.prgnvram, ".inesprgnvram"),
                (i.chrram, ".ineschrram"),
                (i.chrnvram, ".ineschrnvram"),
            ] {
                if ram_shift_checked(val).is_none() {
                    self.hard_error(t!("nes2-ram-size", field = field, value = val));
                }
            }
            if self.console_conflict() {
                self.hard_error(t!("nes2-console-conflict"));
            }
            if self.console_value() == 3 {
                self.hard_error(t!("nes2-extended-console"));
            }
            if (i.vsppu != 0 || i.vshw != 0) && self.console_value() != 1 {
                self.warning(t!("nes2-vs-ignored"));
            }
        } else if i.map > 255 {
            self.hard_error(t!(
                "nes2-required",
                what = format!("Mapper number {}", i.map)
            ));
        } else if i.prg > 255 {
            self.hard_error(t!(
                "nes2-required",
                what = format!("PRG bank count {}", i.prg)
            ));
        } else if i.chr > 255 {
            self.hard_error(t!(
                "nes2-required",
                what = format!("CHR bank count {}", i.chr)
            ));
        } else {
            // NES 2.0-only fields must not be set without `.ines2`.
            for (used, name) in [
                (i.submap != 0, ".inessubmap"),
                (i.prgnvram != 0, ".inesprgnvram"),
                (i.chrram != 0, ".ineschrram"),
                (i.chrnvram != 0, ".ineschrnvram"),
                (i.timing.is_some(), ".inestiming"),
                (i.console != 0, ".inesconsole"),
                (i.vsppu != 0, ".inesvsppu"),
                (i.vshw != 0, ".inesvshw"),
                (i.miscrom != 0, ".inesmiscrom"),
                (i.expansion != 0, ".inesexpansion"),
            ] {
                if used {
                    self.hard_error(t!("nes2-required", what = name));
                    break;
                }
            }
        }
    }

    fn run_pass(&mut self, lines: &[Line]) {
        self.run_lines(lines, true);
    }

    /// Walk `lines` in order, jumping back at each `.endfor` so a `.for` body
    /// runs once per counter value. With `attribute` set each line sets the
    /// current file/line/macro flag (the top-level program); emitted source
    /// leaves them as its call site set them.
    ///
    /// Frames this call pushes are discarded on the way out, so an abort inside
    /// a body cannot leave the caller's loop stack holding frames whose
    /// `body_start` indexes a different slice.
    fn run_lines(&mut self, lines: &[Line], attribute: bool) {
        let base = self.loop_stack.len();
        let mut i = 0;
        while i < lines.len() {
            if self.aborted {
                break;
            }
            let line = &lines[i];
            if attribute {
                self.cur_line = line.line;
                self.cur_file = line.file;
                self.cur_from_macro = line.from_macro;
                self.cur_expansion = line.expansion;
            } else {
                self.emit_ctx.1 = i;
            }
            match &line.stmt {
                Stmt::Pseudo(Pseudo::For(start, end)) => i = self.exec_for(lines, i, start, end),
                Stmt::Pseudo(Pseudo::EndFor) => i = self.exec_endfor(i),
                stmt => {
                    self.exec_stmt(stmt);
                    i += 1;
                }
            }
        }
        self.loop_stack.truncate(base);
    }

    /// Index of the `.endfor` that closes the `.for` at `lines[open]`. The
    /// parser guarantees one exists; `lines.len()` is the defensive fallback.
    fn matching_endfor(lines: &[Line], open: usize) -> usize {
        let mut depth = 0usize;
        for (j, l) in lines.iter().enumerate().skip(open + 1) {
            match l.stmt {
                Stmt::Pseudo(Pseudo::For(..)) => depth += 1,
                Stmt::Pseudo(Pseudo::EndFor) if depth == 0 => return j,
                Stmt::Pseudo(Pseudo::EndFor) => depth -= 1,
                _ => {}
            }
        }
        lines.len()
    }

    /// Execute the `.for START, END` at `lines[i]`: evaluate and check the
    /// bounds, then either push a frame and enter the body, or skip past the
    /// matching `.endfor`. Returns the index to continue from.
    fn exec_for(&mut self, lines: &[Line], i: usize, start: &Expr, end: &Expr) -> usize {
        let skip = |lines: &[Line]| Self::matching_endfor(lines, i) + 1;
        // In a false conditional the loop runs zero times (its bounds are not
        // even evaluated), and skipping to the `.endfor` keeps the pairing.
        if self.if_suppressed() {
            return skip(lines);
        }
        self.eval_unknown = None;
        let replay = if self.pass == 1 {
            None
        } else {
            self.for_log
                .get(self.for_cursor)
                .filter(|e| e.0 == self.cur_file && e.1 == self.cur_line)
                .map(|e| (e.2, e.3))
        };
        let (first, last) = if let Some(bounds) = replay {
            self.for_cursor += 1;
            bounds
        } else {
            let bounds = (self.eval(start), self.eval(end));
            if self.pass == 1 {
                self.for_log
                    .push((self.cur_file, self.cur_line, bounds.0, bounds.1));
            }
            bounds
        };
        let unknown = self.eval_unknown.take();
        // The count decides every later address, so on pass 1 a bound that is
        // not yet known (a forward reference) is an error, not a guess.
        if self.pass == 1 {
            if let Some(name) = unknown {
                self.error(t!("for-bound-unknown", name = name));
                return skip(lines);
            }
        }
        let count = (i128::from(last) - i128::from(first)).unsigned_abs() + 1;
        if count > MAX_FOR_ITERATIONS {
            self.error(t!(
                "for-too-many",
                count = count.to_string(),
                limit = MAX_FOR_ITERATIONS.to_string()
            ));
            return skip(lines);
        }
        let nest = self
            .loop_stack
            .iter()
            .fold(count, |total, f| total.saturating_mul(f.count));
        if nest > MAX_FOR_NEST_ITERATIONS {
            self.error(t!(
                "for-nest-too-many",
                count = nest.to_string(),
                limit = MAX_FOR_NEST_ITERATIONS.to_string()
            ));
            return skip(lines);
        }
        self.loop_stack.push(LoopFrame {
            body_start: i + 1,
            current: first,
            end: last,
            step: if first <= last { 1 } else { -1 },
            count,
        });
        i + 1
    }

    /// Execute `.endfor` at `lines[i]`: run the next iteration, or pop the
    /// frame and fall through. Returns the index to continue from.
    fn exec_endfor(&mut self, i: usize) -> usize {
        let Some(frame) = self.loop_stack.last_mut() else {
            return i + 1;
        };
        if frame.current == frame.end {
            self.loop_stack.pop();
            i + 1
        } else {
            frame.current += frame.step;
            frame.body_start
        }
    }

    /// The value of the loop counter `depth` levels out (0 = `\i`), or an error
    /// (and `0`) when that many loops are not open.
    fn loop_value(&mut self, depth: u8) -> i64 {
        let open = self.loop_stack.len();
        let d = usize::from(depth);
        if d < open {
            return self.loop_stack[open - 1 - d].current;
        }
        let counter = ["\\i", "\\j", "\\k"][d.min(2)];
        if open == 0 {
            self.error(t!("loop-index-outside", counter = counter));
        } else {
            self.error(t!(
                "loop-index-too-deep",
                counter = counter,
                needed = d + 1,
                open = open
            ));
        }
        0
    }

    /// Build the name `base` + the decimal value of the counter `depth` levels
    /// out (`row` + `\i` = `row2`), or report why it can't be built.
    fn paste_name(&mut self, base: &str, depth: u8) -> Option<String> {
        let open = self.loop_stack.len();
        if usize::from(depth) >= open {
            self.loop_value(depth);
            return None;
        }
        let value = self.loop_stack[open - 1 - usize::from(depth)].current;
        if value < 0 {
            self.error(t!("for-paste-negative", name = base, value = value));
            return None;
        }
        Some(format!("{base}{value}"))
    }

    /// Counter values for an in-body diagnostic's suffix: `\i` = 5, `\j` = 1.
    fn loop_counters(&self) -> String {
        self.loop_stack
            .iter()
            .rev()
            .zip(["\\i", "\\j", "\\k"])
            .map(|(f, name)| format!("`{name}` = {}", f.current))
            .collect::<Vec<_>>()
            .join(", ")
    }

    // -- enums (`plans/019-value-enums.md`) ----------------------------------

    /// Execute `.enum START[, STEP[, NAME]]`: start the counter and open a
    /// fresh [`OpenEnum`] frame. An unnamed `.enum` inside another replaces
    /// the open frame (the flat model: the first `.endenum` closes both).
    fn exec_enum(&mut self, start: &Expr, step: Option<&Expr>, name: Option<&String>) {
        let suppressed = self.if_suppressed();
        if self.pass == 1 && !suppressed {
            if let Some(open) = &self.open_enum {
                if name.is_some() || open.name.is_some() {
                    let line = open.line;
                    self.error(t!("enum-nested-named", line = line));
                }
            }
            if let Some(n) = name {
                if let Some(&i) = self.enum_index.get(n) {
                    let (file, line) = (self.enums[i].file, self.enums[i].line);
                    let at = self.diag_at(file, line, 0);
                    self.error(t!(
                        "enum-name-twice",
                        name = n.as_str(),
                        line = at.line,
                        file = at.file
                    ));
                }
            }
        }
        let start = self.eval(start);
        let step = step.map_or(1, |e| self.eval(e));
        self.enum_active = true;
        self.enum_value = start;
        self.enum_inc = step;
        self.open_enum = Some(OpenEnum {
            name: name.cloned(),
            file: self.cur_file,
            line: self.cur_line,
            expansion: self.cur_expansion,
            from_macro: self.cur_from_macro,
            suppressed,
            start,
            step,
            members: Vec::new(),
            seen: HashMap::new(),
        });
    }

    /// Execute `.endenum`: close the counter, and on pass 1 record a named
    /// enum in the registry and define its `NAME_COUNT`.
    fn exec_endenum(&mut self) {
        self.enum_active = false;
        self.enum_value = 0;
        self.enum_inc = 0;
        let Some(open) = self.open_enum.take() else {
            return;
        };
        let Some(name) = open.name else {
            return;
        };
        if self.pass != 1 || open.suppressed || self.enum_index.contains_key(&name) {
            return;
        }
        let start = open.members.first().map_or(open.start, |m| m.value);
        let contiguous = (0i64..)
            .zip(&open.members)
            .all(|(i, m)| m.value == start + i * open.step);
        let count = open.members.len() as i64;
        self.enum_index.insert(name.clone(), self.enums.len());
        self.enums.push(EnumInfo {
            name: name.clone(),
            file: open.file,
            line: open.line,
            start,
            step: open.step,
            members: open.members,
            contiguous,
        });
        // `NAME_COUNT` is defined where `NAME` is written, so a collision
        // (`duplicate-symbol`) points at the `.enum` line.
        let saved = (self.cur_file, self.cur_line, self.cur_expansion);
        (self.cur_file, self.cur_line, self.cur_expansion) = (open.file, open.line, open.expansion);
        self.define_symbol(
            &format!("{name}_COUNT"),
            count,
            SymType::Constant,
            open.from_macro,
        );
        (self.cur_file, self.cur_line, self.cur_expansion) = saved;
    }

    /// Define one `.enum` member `name` with `value`, consuming `size` counter
    /// units: bare names (`bare`), `NAME .rs N` and `NAME = expr` all come
    /// here. The counter moves to `value + size * STEP` on both passes, even in
    /// a false conditional, as `.rs` always has.
    fn enum_member(&mut self, name: &Name, value: i64, size: i64, bare: bool) {
        let resolved = match name {
            Name::Plain(n) => {
                self.add_symbol(n, value, SymType::Enum);
                Some(n.clone())
            }
            Name::Pasted(base, depth) => {
                let pasted = self.paste_name(base, *depth);
                if let Some(n) = &pasted {
                    self.define_pasted(n, SymType::Enum, Some(value));
                }
                pasted
            }
        };
        self.enum_value = value + size * self.enum_inc;
        let Some(member) = resolved else {
            return;
        };
        if self.pass != 1 || self.if_suppressed() {
            return;
        }
        let (file, line) = (self.cur_file, self.cur_line);
        let Some(open) = self.open_enum.as_mut() else {
            return;
        };
        if open.suppressed {
            return;
        }
        let named = open.name.is_some();
        let twice = if let Some(prev_bare) = open.seen.get_mut(&member) {
            let dup = named || bare || *prev_bare;
            *prev_bare |= bare;
            dup
        } else {
            open.seen.insert(member.clone(), bare);
            false
        };
        if named {
            open.members.push(EnumMember {
                name: member.clone(),
                value,
                size,
                file,
                line,
            });
        }
        if twice {
            self.error(t!("enum-member-twice", name = member.as_str()));
        }
    }

    /// At the end of pass 1, report a named `.enum` left open.
    fn check_unterminated_enum(&mut self) {
        let Some(open) = self.open_enum.take() else {
            return;
        };
        let Some(name) = open.name else {
            return;
        };
        if open.suppressed {
            return;
        }
        self.error_at(
            open.file,
            open.line,
            open.expansion,
            t!("enum-unterminated", name = name.as_str()),
        );
    }

    /// The enum registry entry for `name`, for `.table NAME` (plan 020).
    pub(crate) fn enum_info(&self, name: &str) -> Option<&EnumInfo> {
        self.enum_index.get(name).map(|&i| &self.enums[i])
    }

    /// Every named `.enum`, in source order.
    #[must_use]
    pub fn enums(&self) -> &[EnumInfo] {
        &self.enums
    }

    // -- tables (`plans/020-index-keyed-tables.md`) --------------------------

    /// The current origin: file, line and expansion, plus the `.for`
    /// counters when inside a loop body.
    fn origin(&self) -> Origin {
        Origin {
            file: self.cur_file,
            line: self.cur_line,
            expansion: self.cur_expansion,
            suffix: (!self.loop_stack.is_empty()).then(|| self.loop_counters()),
        }
    }

    /// Queue a check for [`Self::run_deferred`]. A no-op except on the final
    /// pass, so each check is recorded once per build; the caller applies its
    /// own suppression rule first.
    fn defer(&mut self, origin: Origin, check: DeferredCheck) {
        if self.pass == 2 {
            self.deferred.push(Deferred { origin, check });
        }
    }

    /// Run every queued check once, in queue order. In non-collect mode the
    /// first error stops reporting, so `run()`'s `errors.last()` is that
    /// first error.
    fn run_deferred(&mut self) {
        for d in std::mem::take(&mut self.deferred) {
            match d.check {
                DeferredCheck::Table(t) => self.check_table(&d.origin, &t),
                DeferredCheck::Assert(a) => self.check_assert(&d.origin, &a),
                DeferredCheck::Error { message, template } => {
                    let text = if message.is_empty() {
                        t!("error-directive-empty")
                    } else {
                        t!("error-directive", message = message)
                    };
                    self.report_keyed(&d.origin, format!("error\0{template}"), text);
                }
            }
            if !self.collect_all && !self.errors.is_empty() {
                break;
            }
        }
    }

    /// Report `message` at a stored origin (an expanded line names its body
    /// file and the calls), with its `.for` suffix, once per (site, message).
    fn report_at(&mut self, origin: &Origin, message: String) {
        self.report_keyed(origin, message.clone(), message);
    }

    /// [`Self::report_at`], de-duplicated by `key` rather than the message
    /// (an assertion's message carries per-iteration values).
    fn report_keyed(&mut self, origin: &Origin, key: String, message: String) {
        if !self.collect_all && !self.errors.is_empty() {
            return;
        }
        if !self
            .deferred_seen
            .insert((origin.file, origin.line, origin.expansion, key))
        {
            return;
        }
        let message = match &origin.suffix {
            Some(counters) => t!(
                "for-iteration-suffix",
                message = message,
                counters = counters.as_str()
            ),
            None => message,
        };
        let diag = crate::diagnostic::locate(
            &self.files,
            &self.expansions,
            origin.file,
            origin.line,
            origin.expansion,
            message,
        );
        self.errors.push(diag);
    }

    /// Execute `.table [COUNT | ENUM][, FIRST]`. Pass 1 only evaluates the
    /// arguments (so an undefined name is reported like any other); pass 2
    /// resolves the form and opens the table.
    fn exec_table(&mut self, count: Option<&Expr>, first: Option<&Expr>) {
        if self.if_suppressed() {
            return;
        }
        if self.pass != 2 {
            if let Some(e) = count.filter(|e| !matches!(e, Expr::Symbol(_))) {
                self.eval(e);
            }
            if let Some(e) = first {
                self.eval(e);
            }
            return;
        }
        self.eval_taint = false;
        let spec = self.resolve_table_spec(count, first);
        let spec_approx = std::mem::take(&mut self.eval_taint);
        self.table = Some(OpenTable {
            origin: self.origin(),
            spec,
            entries: Vec::new(),
            untagged_reported: false,
            spec_approx,
        });
    }

    /// Resolve a `.table`'s arguments (§2.2), reporting any error at the
    /// `.table` line.
    fn resolve_table_spec(&mut self, count: Option<&Expr>, first: Option<&Expr>) -> TableSpec {
        let Some(count) = count else {
            return TableSpec::Order { first: 0 };
        };
        if let Expr::Symbol(name) = count {
            if let Some(info) = self.enum_info(name) {
                let info = info.clone();
                return self.resolve_key_set(&info, first);
            }
            let known = self
                .resolve_ref(name)
                .and_then(|key| self.find_symbol(&key))
                .is_some_and(|id| self.symbols[id].kind != SymType::Undefined);
            if !known {
                self.error(t!("table-arg-unknown", name = name.as_str()));
                return TableSpec::Invalid;
            }
        }
        let n = self.eval(count);
        let first = first.map_or(0, |e| self.eval(e));
        if n < 1 {
            self.error(t!("table-count-invalid", value = n));
            return TableSpec::Invalid;
        }
        TableSpec::Count { count: n, first }
    }

    /// Resolve `.table ENUM[, FIRST]` against plan 019's registry (§5.3).
    fn resolve_key_set(&mut self, info: &EnumInfo, first: Option<&Expr>) -> TableSpec {
        if !info.contiguous || info.step == 0 {
            self.error(t!("table-not-contiguous", set = info.name.as_str()));
            return TableSpec::Invalid;
        }
        let (first, from) = match first {
            None => (info.start, 0),
            Some(e) => {
                let v = self.eval(e);
                let Some(i) = info.members.iter().position(|m| m.value == v) else {
                    self.error(t!(
                        "table-first-not-member",
                        set = info.name.as_str(),
                        value = hex_value(v)
                    ));
                    return TableSpec::Invalid;
                };
                (v, i)
            }
        };
        TableSpec::KeySet {
            name: info.name.clone(),
            first,
            step: info.step,
            members: info.members[from..]
                .iter()
                .map(|m| (m.name.clone(), m.value))
                .collect(),
        }
    }

    /// Execute a `[KEY]` tag: start a new entry of the open table.
    fn exec_tag(&mut self, key: &Expr, text: &str) {
        if self.if_suppressed() {
            return;
        }
        if self.pass != 2 {
            // Register any name the key uses, so an undefined one is reported.
            self.eval(key);
            return;
        }
        if self.table.is_none() {
            return;
        }
        self.eval_taint = false;
        let value = self.eval(key);
        let key_approx = std::mem::take(&mut self.eval_taint);
        let origin = self.origin();
        if let Some(t) = self.table.as_mut() {
            t.entries.push(EntryRec {
                origin,
                key_text: text.to_string(),
                literal: matches!(key, Expr::Num(_)),
                key: value,
                bytes: 0,
                approx: false,
                key_approx,
            });
        }
    }

    /// Count one emitted byte toward the open table's current entry, or
    /// report it as untagged when no entry has started.
    fn count_table_byte(&mut self) {
        let Some(t) = self.table.as_mut() else {
            return;
        };
        if let Some(last) = t.entries.last_mut() {
            last.bytes += 1;
        } else if !t.untagged_reported {
            t.untagged_reported = true;
            self.error(t!("table-untagged"));
        }
    }

    /// Execute `.endtable`: queue the open table's checks.
    fn exec_endtable(&mut self) {
        let Some(t) = self.table.take() else {
            return;
        };
        if t.entries.is_empty() {
            self.error(t!("table-empty"));
            return;
        }
        let origin = t.origin.clone();
        self.defer(origin, DeferredCheck::Table(t));
    }

    /// Report a layout directive used inside an open table (§3.4).
    fn check_table_layout(&mut self, directive: &str) {
        if self.table.is_some() && !self.if_suppressed() {
            self.error(t!("table-layout", directive = directive));
        }
    }

    /// The final-pass checks of one table (§5.1).
    fn check_table(&mut self, origin: &Origin, t: &OpenTable) {
        let step_info = t.spec.first_and_step();
        // In the editor, values that read an unmodeled custom op's layout are
        // not checked (`plans/022-assertions.md` §4).
        let lenient = self.collect_all;
        let spec_approx = lenient && t.spec_approx;
        let size_unknown = |e: &EntryRec| lenient && e.approx;
        // 1. Index: only the first mismatch is reported.
        if let Some((first, step)) = step_info {
            let mut expected = first;
            for e in &t.entries {
                if e.key != expected {
                    if spec_approx || (lenient && e.key_approx) {
                        break;
                    }
                    let message = if e.literal {
                        t!(
                            "table-tag-index-literal",
                            value = hex_value(e.key),
                            index = hex_value(expected)
                        )
                    } else {
                        t!(
                            "table-tag-index",
                            key = e.key_text.as_str(),
                            value = hex_value(e.key),
                            index = hex_value(expected)
                        )
                    };
                    self.report_at(&e.origin, message);
                    // Later entries are not marked: only the first is known
                    // to be wrong, the rest may just be shifted.
                    break;
                }
                expected = expected.wrapping_add(step);
            }
        }
        // 2. Size, against the first entry whose size is known.
        let size = t
            .entries
            .iter()
            .find(|e| !size_unknown(e))
            .map_or(t.entries[0].bytes, |e| e.bytes);
        let index_of = |n: usize, key: i64| {
            step_info.map_or(key, |(first, step)| {
                first.wrapping_add((n as i64).wrapping_mul(step))
            })
        };
        let mut size_ok = vec![true; t.entries.len()];
        for (n, e) in t.entries.iter().enumerate() {
            let index = hex_value(index_of(n, e.key));
            if size_unknown(e) {
                continue;
            }
            if e.bytes == 0 {
                size_ok[n] = false;
                self.report_at(&e.origin, t!("table-entry-empty", index = index));
            } else if t.entries.len() > 1 && size > 0 && e.bytes != size {
                size_ok[n] = false;
                self.report_at(
                    &e.origin,
                    t!(
                        "table-entry-size",
                        index = index,
                        size = e.bytes,
                        expected = size
                    ),
                );
            }
        }
        // 3. Count.
        let len = t.entries.len();
        let mut keyed_by = None;
        let keys_approx = spec_approx || (lenient && t.entries.iter().any(|e| e.key_approx));
        match &t.spec {
            _ if keys_approx => {
                if let TableSpec::KeySet { name, .. } = &t.spec {
                    keyed_by = Some(name.clone());
                }
            }
            TableSpec::Count { count, .. } if len as i64 != *count => {
                self.report_at(origin, t!("table-count", count = len, expected = *count));
            }
            TableSpec::KeySet { name, members, .. } => {
                keyed_by = Some(name.clone());
                for (member, value) in members {
                    if !t.entries.iter().any(|e| e.key == *value) {
                        self.report_at(
                            origin,
                            t!(
                                "table-missing-member",
                                set = name.as_str(),
                                member = member.as_str(),
                                value = hex_value(*value)
                            ),
                        );
                    }
                }
                if len > members.len() {
                    self.report_at(
                        origin,
                        t!(
                            "table-count-enum",
                            count = len,
                            set = name.as_str(),
                            expected = members.len()
                        ),
                    );
                }
            }
            _ => {}
        }
        // 4. Record for tooling.
        let table_at = self.diag_at(origin.file, origin.line, origin.expansion);
        for (n, e) in t.entries.iter().enumerate() {
            let at = self.diag_at(e.origin.file, e.origin.line, e.origin.expansion);
            let index = index_of(n, e.key);
            self.table_entries.push(TableEntryInfo {
                file: at.file,
                line: at.line,
                table_file: table_at.file.clone(),
                table_line: table_at.line,
                key: e.key,
                index,
                size: e.bytes,
                table_size: size,
                table_entries: len,
                keyed_by: keyed_by.clone(),
                ok: e.key == index && size_ok[n],
            });
        }
    }

    /// Every `.table` entry the last [`Self::diagnostics`] run checked.
    #[must_use]
    pub fn table_entries(&self) -> &[TableEntryInfo] {
        &self.table_entries
    }

    // -- assertions (`plans/022-assertions.md`) -----------------------------

    /// The `approx_bank` slot of the bank being written.
    fn layout_bank(&self) -> usize {
        if self.segment_prg {
            self.prg_index
        } else {
            MAX_BANKS + self.chr_index
        }
    }

    /// A custom op whose bytes are not modeled ran here: later addresses in
    /// this bank, and the open table entry's size, are approximate.
    fn mark_unmodeled(&mut self) {
        if self.if_suppressed() {
            return;
        }
        let bank = self.layout_bank();
        self.approx_bank[bank] = true;
        if self.recording() {
            let (site, _) = self.author_site();
            self.program.unmodelled.push(Unmodelled {
                site,
                rom_offset: self.rom_index(),
                cpu_addr: self.address_offset() as u16,
                bank: self.prg_index as u16,
            });
        }
        if let Some(e) = self.table.as_mut().and_then(|t| t.entries.last_mut()) {
            e.approx = true;
        }
    }

    /// `text` with each `\i`, `\j` and `\k` replaced by its counter's value
    /// when that many loops are open; anything else is left as written.
    fn substitute_counters(&self, text: &str) -> String {
        let open = self.loop_stack.len();
        let mut out = String::with_capacity(text.len());
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\\' {
                let depth = match chars.peek() {
                    Some('i') => Some(0),
                    Some('j') => Some(1),
                    Some('k') => Some(2),
                    _ => None,
                };
                if let Some(d) = depth.filter(|d| *d < open) {
                    chars.next();
                    out.push_str(&self.loop_stack[open - 1 - d].current.to_string());
                    continue;
                }
            }
            out.push(c);
        }
        out
    }

    /// Whether a final-pass `.assert`/`.error`/`.out` here is assembled.
    fn assembles_check(&self) -> bool {
        self.pass == 2 && !self.if_any_false()
    }

    /// What an assertion's expression needs from this line.
    fn eval_ctx(&self) -> EvalCtx {
        EvalCtx {
            loops: self.loop_stack.iter().rev().map(|f| f.current).collect(),
            address: self.address_offset(),
            bank: self.prg_index,
            scope: self.scope().map(str::to_string),
            approx: self.approx_bank[self.layout_bank()],
        }
    }

    /// Execute `.assert`: on the final pass, queue it for the drain.
    fn exec_assert(&mut self, expr: &Expr, text: &str, message: Option<&str>) {
        if !self.assembles_check() {
            return;
        }
        let check = AssertCheck {
            expr: expr.clone(),
            text: text.to_string(),
            message: message.map(|m| self.substitute_counters(m)),
            template: message.map(str::to_string),
            ctx: self.eval_ctx(),
        };
        let origin = self.origin();
        self.defer(origin, DeferredCheck::Assert(Box::new(check)));
    }

    /// Execute `.error`: on the final pass, queue its failure.
    fn exec_error(&mut self, message: &str) {
        if !self.assembles_check() {
            return;
        }
        let check = DeferredCheck::Error {
            message: self.substitute_counters(message),
            template: message.to_string(),
        };
        let origin = self.origin();
        self.defer(origin, check);
    }

    /// Execute `.out`: on the final pass, record the line.
    fn exec_out(&mut self, message: &str) {
        if !self.assembles_check() {
            return;
        }
        let text = self.substitute_counters(message);
        let diag = self.diag_here(text);
        self.out_lines.push(diag);
    }

    /// Drain one assertion against the final symbol table (§2.2).
    fn check_assert(&mut self, origin: &Origin, a: &AssertCheck) {
        let mut taint = false;
        let message = match self.eval_final(&a.ctx, &a.expr, &mut taint) {
            Ok(v) if v != 0 => return,
            // In the editor a failure that reads an unmodeled layout is not
            // trusted (§4).
            Ok(_) if taint && self.collect_all => return,
            Ok(_) => {
                let detail = self.assert_detail(&a.ctx, &a.expr);
                let detail = if detail.is_empty() {
                    String::new()
                } else {
                    format!(" ({detail})")
                };
                match &a.message {
                    Some(m) => t!(
                        "assert-failed",
                        message = m.as_str(),
                        expr = a.text.as_str(),
                        detail = detail
                    ),
                    None => t!(
                        "assert-failed-bare",
                        expr = a.text.as_str(),
                        detail = detail
                    ),
                }
            }
            Err(Unresolved::Symbol(name)) => t!("assert-undefined", name = name),
            Err(Unresolved::Loop { depth, open }) => {
                let counter = ["\\i", "\\j", "\\k"][usize::from(depth).min(2)];
                if open == 0 {
                    t!("loop-index-outside", counter = counter)
                } else {
                    t!(
                        "loop-index-too-deep",
                        counter = counter,
                        needed = usize::from(depth) + 1,
                        open = open
                    )
                }
            }
            Err(Unresolved::Local(direction)) => {
                let sign = if direction > 0 { "+" } else { "-" };
                let dir = format!(":{}", sign.repeat(direction.unsigned_abs() as usize));
                t!("assert-no-local", dir = dir)
            }
        };
        let key = format!(
            "assert\0{}\0{}",
            a.text,
            a.template.as_deref().unwrap_or_default()
        );
        self.report_keyed(origin, key, message);
    }

    /// The DETAIL of an assertion failure (§2.5): both sides of a top-level
    /// comparison, then the value of each name the expression reads.
    fn assert_detail(&self, ctx: &EvalCtx, expr: &Expr) -> String {
        let mut taint = false;
        let mut parts = Vec::new();
        if let Expr::Binary(a, op, b) = expr {
            if let Some(op) = comparison(*op) {
                if !(matches!(**a, Expr::Num(_)) && matches!(**b, Expr::Num(_))) {
                    if let (Ok(l), Ok(r)) = (
                        self.eval_final(ctx, a, &mut taint),
                        self.eval_final(ctx, b, &mut taint),
                    ) {
                        parts.push(format!("{} {op} {}", assert_value(l), assert_value(r)));
                    }
                }
            }
        }
        let mut names: Vec<(String, &Expr)> = Vec::new();
        Self::collect_names(ctx, expr, &mut names);
        let mut listed: Vec<String> = Vec::new();
        for (name, e) in names.iter().take(ASSERT_DETAIL_NAMES) {
            if let Ok(v) = self.eval_final(ctx, e, &mut taint) {
                listed.push(format!("{name} = {}", assert_value(v)));
            }
        }
        if names.len() > ASSERT_DETAIL_NAMES {
            listed.push("…".to_string());
        }
        if !listed.is_empty() {
            parts.push(listed.join(", "));
        }
        parts.join("; ")
    }

    /// Each distinct name `expr` reads, in first-appearance order, with the
    /// sub-expression that reads it.
    fn collect_names<'e>(ctx: &EvalCtx, expr: &'e Expr, out: &mut Vec<(String, &'e Expr)>) {
        let name = match expr {
            Expr::Symbol(n) => Some(n.clone()),
            Expr::PastedSymbol(base, depth) => Some(match ctx.loops.get(usize::from(*depth)) {
                Some(v) => format!("{base}{v}"),
                None => base.clone(),
            }),
            Expr::Bank(n) => Some(format!("BANK({n})")),
            Expr::High(e) | Expr::Low(e) | Expr::Unary(_, e) => {
                Self::collect_names(ctx, e, out);
                None
            }
            Expr::Binary(a, _, b) => {
                Self::collect_names(ctx, a, out);
                Self::collect_names(ctx, b, out);
                None
            }
            _ => None,
        };
        if let Some(n) = name {
            if !out.iter().any(|(seen, _)| *seen == n) {
                out.push((n, expr));
            }
        }
    }

    /// The defined symbol `name` resolves to under `scope`, for the drain.
    fn final_symbol(&self, scope: Option<&str>, name: &str) -> Result<&Symbol, Unresolved> {
        let key = match locals::classify(name) {
            LocalName::Local(local) => scope.map(|s| format!("{s}{local}")),
            _ => Some(name.to_string()),
        };
        key.and_then(|k| self.find_symbol(&k))
            .map(|id| &self.symbols[id])
            .filter(|s| s.kind != SymType::Undefined)
            .ok_or_else(|| Unresolved::Symbol(locals::display(name).to_string()))
    }

    /// Evaluate `e` against the final symbol table, without side effects: no
    /// symbol is registered and nothing is guessed (§2.7). Sets `taint` when
    /// a value read is approximate.
    fn eval_final(&self, ctx: &EvalCtx, e: &Expr, taint: &mut bool) -> Result<i64, Unresolved> {
        Ok(match e {
            Expr::Num(n) => *n,
            Expr::Symbol(name) => {
                let s = self.final_symbol(ctx.scope.as_deref(), name)?;
                *taint |= s.approx;
                s.value
            }
            Expr::PastedSymbol(base, depth) => {
                let v = Self::final_loop(ctx, *depth)?;
                let s = self.final_symbol(ctx.scope.as_deref(), &format!("{base}{v}"))?;
                *taint |= s.approx;
                s.value
            }
            Expr::Bank(name) => self.final_symbol(ctx.scope.as_deref(), name)?.bank as i64,
            Expr::LocalForward(n) | Expr::LocalBackward(n) => {
                let direction = if matches!(e, Expr::LocalForward(_)) {
                    *n as i32
                } else {
                    -(*n as i32)
                };
                let id = self
                    .get_symbol_local_at(direction, ctx.address, ctx.bank)
                    .ok_or(Unresolved::Local(direction))?;
                *taint |= ctx.approx || self.symbols[id].approx;
                self.symbols[id].value
            }
            Expr::LoopIndex(depth) => Self::final_loop(ctx, *depth)?,
            Expr::High(e) => (self.eval_final(ctx, e, taint)? >> 8) & 0xFF,
            Expr::Low(e) => self.eval_final(ctx, e, taint)? & 0xFF,
            Expr::Unary(op, e) => Self::apply_unary(*op, self.eval_final(ctx, e, taint)?),
            Expr::Binary(a, op, b) => {
                let lhs = self.eval_final(ctx, a, taint)?;
                let rhs = self.eval_final(ctx, b, taint)?;
                Self::apply(lhs, *op, rhs)
            }
        })
    }

    /// The captured value of the loop counter `depth` levels out.
    fn final_loop(ctx: &EvalCtx, depth: u8) -> Result<i64, Unresolved> {
        ctx.loops
            .get(usize::from(depth))
            .copied()
            .ok_or(Unresolved::Loop {
                depth,
                open: ctx.loops.len(),
            })
    }

    // -- error helpers ------------------------------------------------------

    fn error(&mut self, message: impl Into<String>) {
        let mut message = message.into();
        // A line in a `.for` body that fails does so once per iteration it
        // fails in; report it once, naming the first such iteration.
        if !self.loop_stack.is_empty() {
            if !self
                .loop_err_seen
                .insert((self.cur_file, self.cur_line, message.clone()))
            {
                return;
            }
            message = t!(
                "for-iteration-suffix",
                message = message,
                counters = self.loop_counters()
            );
        }
        let diag = self.diag_here(message);
        self.errors.push(diag);
    }

    /// A diagnostic at the current source line: in the macro body it was
    /// expanded from (if any), with the expanding calls as notes.
    fn diag_here(&self, message: impl Into<String>) -> Diag {
        crate::diagnostic::locate(
            &self.files,
            &self.expansions,
            self.cur_file,
            self.cur_line,
            self.cur_expansion,
            message,
        )
    }

    /// Report `message` at a stored site rather than the current line.
    fn error_at(&mut self, file: u32, line: u32, expansion: u32, message: impl Into<String>) {
        let saved = (self.cur_file, self.cur_line, self.cur_expansion);
        (self.cur_file, self.cur_line, self.cur_expansion) = (file, line, expansion);
        self.error(message);
        (self.cur_file, self.cur_line, self.cur_expansion) = saved;
    }

    /// The diagnostic location of a stored site (its display file and line).
    fn diag_at(&self, file: u32, line: u32, expansion: u32) -> Diag {
        crate::diagnostic::locate(
            &self.files,
            &self.expansions,
            file,
            line,
            expansion,
            String::new(),
        )
    }

    fn hard_error(&mut self, message: impl Into<String>) {
        self.error(message);
        // In collect mode, keep going so more problems can be reported.
        if !self.collect_all {
            self.aborted = true;
        }
    }

    fn warning(&mut self, message: impl Into<String>) {
        if self.rules.warnings_as_errors {
            self.error(message);
            return;
        }
        let diag = self.diag_here(message);
        self.warnings.push(diag);
    }

    /// Report `message` under `rule` at the current line, at the rule's
    /// configured severity: dropped when `off`, a warning when `warn`, and a
    /// hard error when `error`. Each source location reports a rule once.
    fn rule_diag(&mut self, rule: AsmRule, message: impl Into<String>) {
        let mut diag = self.diag_here(message);
        diag.rule = Some(rule);
        self.report_rule(diag);
    }

    /// Report a diagnostic already tagged with its rule (see [`Self::rule_diag`]).
    fn report_rule(&mut self, diag: Diag) {
        let Some(rule) = diag.rule else {
            return;
        };
        let severity = self.rules.effective(rule);
        if severity == RuleSeverity::Off
            || !self.rule_seen.insert((diag.file.clone(), diag.line, rule))
        {
            return;
        }
        if severity == RuleSeverity::Error {
            self.errors.push(diag);
            if !self.collect_all {
                self.aborted = true;
            }
        } else {
            self.warnings.push(diag);
        }
    }

    /// Report the rule diagnostics the preprocessor raised, once per run.
    fn report_pending(&mut self) {
        for diag in std::mem::take(&mut self.pending) {
            self.report_rule(diag);
        }
    }

    /// Warn (`value-out-of-range`) when `value` does not fit `field` and is
    /// about to be narrowed to fit it. Checked on the final pass only, where
    /// every value is known, and never in a false conditional (nothing is
    /// emitted there).
    fn check_range(&mut self, value: i64, field: Field) {
        if self.pass != 2 || self.if_suppressed() {
            return;
        }
        let (min, max) = field.range();
        if (min..=max).contains(&value) {
            return;
        }
        let (name, emitted) = match field {
            Field::Byte | Field::ZeroPage => (field.name(), format!("${:02X}", value & 0xFF)),
            Field::Word | Field::Absolute => (field.name(), format!("${:04X}", value & 0xFFFF)),
        };
        let message = if value < 0 {
            t!(
                "value-out-of-range-negative",
                value = value,
                field = name,
                emitted = emitted
            )
        } else {
            t!(
                "value-out-of-range",
                value = value,
                hex = format!("${value:04X}"),
                field = name,
                emitted = emitted
            )
        };
        self.rule_diag(AsmRule::ValueOutOfRange, message);
    }

    // -- symbol table -------------------------------------------------------

    fn find_symbol(&self, name: &str) -> Option<usize> {
        self.symbols.iter().position(|s| s.name == name)
    }

    fn add_symbol(&mut self, name: &str, value: i64, kind: SymType) {
        self.define_symbol(name, value, kind, self.cur_from_macro);
    }

    /// Define a symbol built by pasting a loop counter onto `base`, flagged
    /// `from_macro` (its name is not in the file's text) and rejected on pass 1
    /// if the name is already defined — a collision between pasted names is
    /// almost certainly a mistake, unlike an ordinary redefinition.
    fn add_pasted_symbol(&mut self, base: &str, depth: u8, kind: SymType, value: Option<i64>) {
        let Some(name) = self.paste_name(base, depth) else {
            return;
        };
        let Some(key) = self.resolve_def(&name) else {
            return;
        };
        self.define_pasted(&key, kind, value);
        if kind == SymType::Label {
            self.open_scope(&name);
        }
    }

    /// Define the already-pasted name `name` (see [`Self::add_pasted_symbol`]).
    fn define_pasted(&mut self, name: &str, kind: SymType, value: Option<i64>) {
        // A local's duplicate is reported by `define_symbol` (`local-duplicate`).
        if self.pass == 1
            && !name.contains('@')
            && !self.if_suppressed()
            && self
                .find_symbol(name)
                .is_some_and(|id| self.symbols[id].kind != SymType::Undefined)
        {
            self.error(t!("for-paste-defined", name = name));
            return;
        }
        let is_label = value.is_none() && kind == SymType::Label;
        let value = value.unwrap_or_else(|| self.address_offset());
        self.define_symbol(name, value, kind, true);
        if is_label {
            self.record_label(name);
        }
    }

    fn define_symbol(&mut self, name: &str, value: i64, kind: SymType, from_macro: bool) {
        if self.pass != 1 {
            return;
        }
        // Symbols inside a false conditional block are not recorded.
        if self.if_suppressed() {
            return;
        }
        let bank = self.prg_index;
        let existing = if name == ":" {
            None
        } else {
            self.find_symbol(name)
        };
        // Where this definition is, recorded only for a real definition (an
        // undefined forward reference registers a placeholder, not a site).
        let here = (kind != SymType::Undefined).then(|| {
            let d = self.diag_here(String::new());
            (d.file, d.line)
        });
        let parent = self.scope().map(str::to_string);
        let site = self.cur_site();
        let rom_offset = self.rom_index();
        let approx = match kind {
            SymType::Label => self.approx_bank[self.layout_bank()],
            SymType::Constant => self.def_taint,
            _ => false,
        };
        // A local or private key (`p1@x`, `\@7@x`): global names never hold `@`.
        let scoped = name.contains('@');
        match existing {
            Some(id) => {
                if kind != SymType::Undefined && self.symbols[id].kind != SymType::Undefined {
                    if scoped {
                        // The same definition seen again (a file included
                        // twice) is not a second one.
                        if !self.same_site(self.symbols[id].site, site) {
                            self.report_local_duplicate(id);
                            return;
                        }
                    } else {
                        self.report_redefinition(id, value, kind, bank, site);
                    }
                }
                let sym = &mut self.symbols[id];
                sym.value = value;
                sym.bank = bank;
                sym.kind = kind;
                sym.from_macro = from_macro;
                sym.approx = approx;
                if here.is_some() {
                    sym.defined_at = here;
                    sym.site = site;
                    sym.parent = parent;
                    sym.rom_offset = rom_offset;
                }
            }
            None => self.symbols.push(Symbol {
                name: name.to_string(),
                value,
                kind,
                bank,
                from_macro,
                defined_at: here,
                site,
                parent,
                approx,
                rom_offset,
            }),
        }
    }

    /// Report the `local-duplicate` error for a second definition of the local
    /// (or private) symbol `id` at the current line.
    fn report_local_duplicate(&mut self, id: usize) {
        let prev = &self.symbols[id];
        let (file, line) = prev.defined_at.clone().unwrap_or_default();
        let message = match locals::classify(&prev.name) {
            LocalName::Private { id: eid, local } => {
                let eid: u32 = eid.parse().unwrap_or(0);
                match self.emitted_names.get(&eid) {
                    Some(pseudo) => t!(
                        "local-duplicate-emitted",
                        name = local,
                        pseudo = format!(".{pseudo}"),
                        line = line
                    ),
                    None => t!(
                        "local-duplicate-macro",
                        name = local,
                        macro = self
                            .expansions
                            .get((eid as usize).wrapping_sub(1))
                            .map(|e| e.name.clone())
                            .unwrap_or_default(),
                        line = line
                    ),
                }
            }
            LocalName::Qualified { parent, local } => {
                if file == self.diag_here(String::new()).file {
                    t!(
                        "local-duplicate",
                        name = local,
                        parent = parent,
                        line = line
                    )
                } else {
                    t!(
                        "local-duplicate-file",
                        name = local,
                        parent = parent,
                        file = file,
                        line = line
                    )
                }
            }
            _ => return,
        };
        self.error(message);
    }

    /// The current file's `@local` scope, if one is open.
    fn scope(&self) -> Option<&str> {
        self.scopes
            .get(self.cur_file as usize)
            .and_then(Option::as_deref)
    }

    /// Open the `@local` scope `name` in the current file, if a label named
    /// `name` defined here opens one (§2.2): a global label that is executed
    /// (not in a false conditional) and not built with a macro's `\@`.
    fn open_scope(&mut self, name: &str) {
        if name == ":"
            || self.if_suppressed()
            || !matches!(locals::classify(name), LocalName::Global(_))
        {
            return;
        }
        let unique = self
            .cur_expansion
            .checked_sub(1)
            .and_then(|i| self.expansions.get(i as usize))
            .is_some_and(|e| e.unique_names.iter().any(|n| n == name));
        if unique {
            return;
        }
        let file = self.cur_file as usize;
        if self.scopes.len() <= file {
            self.scopes.resize(file + 1, None);
        }
        self.scopes[file] = Some(name.to_string());
    }

    /// The symbol key a definition of `name` uses: `scope@x` for a local,
    /// the name itself otherwise. A local with no scope is reported
    /// (`local-no-scope`, on pass 1) and yields `None`.
    fn resolve_def(&mut self, name: &str) -> Option<String> {
        match locals::classify(name) {
            LocalName::Local(local) => {
                if let Some(scope) = self.scope() {
                    return Some(format!("{scope}{local}"));
                }
                if self.pass == 1 && !self.if_suppressed() {
                    self.error(t!("local-no-scope", name = local));
                }
                None
            }
            _ => Some(name.to_string()),
        }
    }

    /// The symbol key a reference to `name` resolves to, or `None` for a
    /// local with no scope open (never registers anything).
    fn resolve_ref(&self, name: &str) -> Option<String> {
        match locals::classify(name) {
            LocalName::Local(local) => self.scope().map(|scope| format!("{scope}{local}")),
            _ => Some(name.to_string()),
        }
    }

    /// The pass-2 message for a reference to `name` (resolved to `key`) that
    /// is not defined.
    fn undefined_message(&self, name: &str, key: &str) -> String {
        match locals::classify(name) {
            LocalName::Local(local) => {
                let parent = self.scope().unwrap_or_default();
                match self.local_hint(local, key) {
                    Some(hint) => t!(
                        "local-undefined-hint",
                        name = local,
                        parent = parent,
                        hint = hint
                    ),
                    None => t!("local-undefined", name = local, parent = parent),
                }
            }
            LocalName::Qualified { parent, local } => {
                let parent_known = self
                    .find_symbol(parent)
                    .is_some_and(|id| self.symbols[id].kind != SymType::Undefined);
                if parent_known {
                    t!("local-not-in-parent", parent = parent, local = local)
                } else {
                    t!("symbol-not-defined", name = parent)
                }
            }
            _ => t!("symbol-not-defined", name = locals::display(name)),
        }
    }

    /// A defined local named `local` in another scope, to suggest for an
    /// undefined reference (resolved to `key`): the one in this file defined
    /// nearest above the reference, else the first one defined.
    fn local_hint(&self, local: &str, key: &str) -> Option<String> {
        let (file, line, _) = self.cur_site();
        let candidates = self.symbols.iter().filter(|s| {
            s.kind != SymType::Undefined
                && s.name != key
                && matches!(locals::classify(&s.name), LocalName::Qualified { local: l, .. } if l == local)
        });
        let above = candidates
            .clone()
            .filter(|s| s.site.0 == file && s.site.1 <= line)
            .max_by_key(|s| s.site.1);
        above
            .or_else(|| candidates.min_by_key(|s| s.defined_at.is_none()))
            .map(|s| s.name.clone())
    }

    /// Report that the definition at the current line replaces symbol `id`'s:
    /// `duplicate-symbol-same-value` when the new one has the same kind, value,
    /// and bank (harmless), `duplicate-symbol` otherwise. Resolution is
    /// unchanged either way — the last definition still wins.
    ///
    /// The same definition seen again — the same line of the same file, from
    /// the same expansion (a file `.include`d twice) — with the same value is
    /// one definition, not two, and is not reported.
    fn report_redefinition(
        &mut self,
        id: usize,
        value: i64,
        kind: SymType,
        bank: usize,
        site: (u32, u32, u32),
    ) {
        let prev = &self.symbols[id];
        // A label's bank is part of its value (`BANK(name)`); a constant's is
        // only where it happened to be written.
        let located = matches!(kind, SymType::Label | SymType::Rs);
        let same = prev.kind == kind && prev.value == value && (!located || prev.bank == bank);
        if same && self.same_site(prev.site, site) {
            return;
        }
        let (file, line) = prev.defined_at.clone().unwrap_or_default();
        let shown = if prev.value < 0 {
            prev.value.to_string()
        } else {
            format!("${:04X}", prev.value)
        };
        let id_key = match prev.kind {
            SymType::Label => "duplicate-symbol-label",
            SymType::Rs => "duplicate-symbol-rs",
            _ => "duplicate-symbol-constant",
        };
        let message = t!(
            id_key,
            name = prev.name.as_str(),
            file = file,
            line = line,
            value = shown
        );
        let rule = if same {
            AsmRule::DuplicateSymbolSameValue
        } else {
            AsmRule::DuplicateSymbol
        };
        self.rule_diag(rule, message);
    }

    /// The source site of the current line: the file id its text is in (the
    /// macro body's file for an expanded line), its line, and its expansion.
    fn cur_site(&self) -> (u32, u32, u32) {
        let file = match self.cur_expansion.checked_sub(1) {
            Some(i) => self
                .expansions
                .get(i as usize)
                .map_or(self.cur_file, |e| e.def_file),
            None => self.cur_file,
        };
        (file, self.cur_line, self.cur_expansion)
    }

    /// Whether two sites are the same text: the same line and expansion of
    /// the same file on disk (file ids differ when a file is included twice).
    fn same_site(&self, a: (u32, u32, u32), b: (u32, u32, u32)) -> bool {
        if a.1 != b.1 || a.2 != b.2 {
            return false;
        }
        if a.0 == b.0 {
            return true;
        }
        // Only reached for a same-value redefinition, so resolving the two
        // paths on disk here is rare.
        let path = |id: u32| {
            self.paths
                .get(id as usize)
                .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()))
        };
        path(a.0).is_some() && path(a.0) == path(b.0)
    }

    fn add_label(&mut self, name: &str) {
        let offset = self.address_offset();
        self.add_symbol(name, offset, SymType::Label);
        self.record_label(name);
    }

    // -- program recording (`plans/023-whole-program-clobber-analysis.md`) --

    /// Whether this statement's effects are being recorded.
    fn recording(&self) -> bool {
        self.record_program
            && self.pass == 2
            && self.segment_prg
            && !self.ines.trn
            && !self.if_suppressed()
    }

    /// The author's line for the current statement and what produced it: the
    /// outermost `.macro` call for an expanded line, the pseudo-op's line for
    /// emitted source.
    fn author_site(&self) -> (Site, Option<Via>) {
        let mut via = None;
        let mut site = Site {
            file: self.cur_file,
            line: self.cur_line,
        };
        let mut id = self.cur_expansion;
        let mut root = None;
        while let Some(e) = id
            .checked_sub(1)
            .and_then(|i| self.expansions.get(i as usize))
        {
            root = Some(e);
            id = e.parent;
        }
        if let Some(e) = root {
            site = Site {
                file: e.call_file,
                line: e.call_line,
            };
            via = Some(Via::Macro(e.name.clone()));
        }
        if self.custom_expansion_depth > 0 {
            if let Some(name) = self.emitted_names.get(&self.emit_ctx.0) {
                via = Some(Via::Pseudo(name.clone()));
            }
        }
        (site, via)
    }

    /// Record the definition of label `key` here (pass 2).
    fn record_label(&mut self, key: &str) {
        if !self.recording() || key == ":" {
            return;
        }
        let (site, via) = self.author_site();
        let local = matches!(locals::classify(key), LocalName::Local(_)) || key.contains('@');
        self.program.labels.push(LabelDef {
            name: key.to_string(),
            cpu_addr: self.address_offset() as u16,
            bank: self.prg_index as u16,
            rom_offset: self.rom_index(),
            site,
            in_expansion: via.is_some() || self.cur_from_macro,
            local,
        });
    }

    /// The label an instruction's operand names, as `(rom_offset, value)`:
    /// the first label symbol (or anonymous-label reference) in the operand.
    fn operand_anchor(&self, instr: &Instruction) -> Option<(usize, i64)> {
        let (Operand::Absolute(expr) | Operand::ZeroPage(expr)) = &instr.operand else {
            return None;
        };
        self.expr_anchor(expr)
    }

    fn expr_anchor(&self, expr: &Expr) -> Option<(usize, i64)> {
        let label = |id: usize| {
            let s = &self.symbols[id];
            (s.kind == SymType::Label).then_some((s.rom_offset, s.value))
        };
        match expr {
            Expr::Symbol(name) => {
                let key = self.resolve_ref(name)?;
                label(self.find_symbol(&key)?)
            }
            Expr::PastedSymbol(base, depth) => {
                let open = self.loop_stack.len();
                let d = usize::from(*depth);
                if d >= open {
                    return None;
                }
                let name = format!("{base}{}", self.loop_stack[open - 1 - d].current);
                let key = self.resolve_ref(&name)?;
                label(self.find_symbol(&key)?)
            }
            Expr::LocalForward(n) => label(self.get_symbol_local(*n as i32)?),
            Expr::LocalBackward(n) => label(self.get_symbol_local(-(*n as i32))?),
            Expr::Binary(a, _, b) => self.expr_anchor(a).or_else(|| self.expr_anchor(b)),
            Expr::Unary(_, e) => self.expr_anchor(e),
            _ => None,
        }
    }

    fn get_symbol_local(&self, direction: i32) -> Option<usize> {
        self.get_symbol_local_at(direction, self.address_offset(), self.prg_index)
    }

    /// The anonymous label `direction` steps from `offset` in `bank`.
    fn get_symbol_local_at(&self, direction: i32, offset: i64, bank: usize) -> Option<usize> {
        use std::cmp::Ordering;
        match direction.cmp(&0) {
            Ordering::Greater => {
                let mut remaining = direction;
                for (i, s) in self.symbols.iter().enumerate() {
                    if s.name == ":" && s.bank == bank && s.value > offset {
                        remaining -= 1;
                        if remaining == 0 {
                            return Some(i);
                        }
                    }
                }
                None
            }
            Ordering::Less => {
                let mut remaining = direction;
                for (i, s) in self.symbols.iter().enumerate().rev() {
                    if s.name == ":" && s.bank == bank && s.value < offset {
                        remaining += 1;
                        if remaining == 0 {
                            return Some(i);
                        }
                    }
                }
                None
            }
            Ordering::Equal => None,
        }
    }

    // -- location / emission ------------------------------------------------

    fn rom_index(&self) -> usize {
        let index: i64 = if self.segment_prg {
            self.prg_offsets[self.prg_index] + (self.prg_index as i64) * BANK_PRG
        } else {
            let mut idx = self.chr_offsets[self.chr_index] + self.ines.prg * BANK_PRG;
            if self.chr_index > 0 {
                idx += (self.chr_index as i64) * BANK_CHR;
            }
            idx
        };
        index.max(0) as usize
    }

    /// The address a label defined at the current location receives. This is the
    /// physical load address ([`Self::address_offset_raw`]) shifted by any active
    /// `.phase` delta, so within a `.phase` block labels read as their run-time
    /// (post-swap) address while ROM layout is unchanged.
    fn address_offset(&self) -> i64 {
        self.address_offset_raw() + self.phase_delta
    }

    /// The physical load address of the current location, ignoring any `.phase`
    /// override. This is what drives `.org` bounds and ROM placement.
    fn address_offset_raw(&self) -> i64 {
        if self.segment_prg {
            if self.nes {
                if self.ines.prg < 2 {
                    self.prg_offsets[self.prg_index] + BANK_PRG * 3
                } else {
                    self.prg_offsets[self.prg_index]
                        + (BANK_PRG * 2 + ((self.prg_index as i64) % 2) * BANK_PRG)
                }
            } else {
                self.prg_offsets[self.prg_index]
            }
        } else {
            let mut offset = self.chr_offsets[self.chr_index] + self.ines.prg * BANK_PRG;
            if self.chr_index > 0 {
                offset += (self.chr_index as i64) * BANK_CHR;
            }
            offset
        }
    }

    fn write_byte(&mut self, byte: u8) {
        // A byte suppressed by a false conditional is dropped entirely — it does
        // not advance the location counter (matching the reference).
        if self.if_suppressed() {
            return;
        }
        if self.table.is_some() {
            self.count_table_byte();
        }

        // While a trainer is active every emitted byte is redirected into the
        // 512-byte trainer region and does not advance the ROM counters.
        if self.ines.trn {
            if self.pass == 2 && self.offset_trainer < self.trainer.len() {
                self.trainer[self.offset_trainer] = byte;
            }
            self.offset_trainer += 1;
            return;
        }

        let offset = self.rom_index();
        // In raw (non-NES) mode the ROM grows to fit; in NES mode the size is
        // fixed by the header, so the high-water mark is not tracked.
        if !self.nes && offset + 1 > self.offset_max {
            self.offset_max = offset + 1;
        }
        if self.pass == 2 && offset < self.rom.len() {
            self.rom[offset] = byte;
            self.coverage[offset] = true;
            if self.record_source_map {
                self.record_span(offset);
            }
            if self.record_program && self.segment_prg {
                let cpu = self.address_offset() as u16;
                self.program.byte(offset, self.prg_index as u16, cpu);
            }
        }
        if self.segment_prg {
            if self.pass == 1 && self.prg_offsets[self.prg_index] >= BANK_PRG {
                self.warning(t!("overflow-prg", bank = self.prg_index));
            }
            self.prg_offsets[self.prg_index] += 1;
        } else {
            if self.pass == 1 && self.chr_offsets[self.chr_index] >= BANK_CHR {
                // The reference message uses prg_index here (matched for parity).
                self.warning(t!("overflow-chr", bank = self.prg_index));
            }
            self.chr_offsets[self.chr_index] += 1;
        }
    }

    /// Record that `offset` was just written on the current source line,
    /// extending the previous span when it is the same line and the write is
    /// contiguous, or starting a new span otherwise. Called only when
    /// `record_source_map` is set.
    fn record_span(&mut self, offset: usize) {
        if let Some(last) = self.spans.last_mut() {
            if last.file == self.cur_file
                && last.line == self.cur_line
                && last.rom_offset + last.len == offset
            {
                last.len += 1;
                return;
            }
        }
        self.spans.push(RawSpan {
            file: self.cur_file,
            line: self.cur_line,
            rom_offset: offset,
            len: 1,
        });
    }

    // -- statement execution -----------------------------------------------

    fn exec_stmt(&mut self, stmt: &Stmt) {
        if !(self.record_program && self.pass == 2) {
            self.exec_stmt_inner(stmt);
            return;
        }
        let (anchor, operand) = match stmt {
            Stmt::Instruction(instr) => (self.operand_anchor(instr), operand_text(&instr.operand)),
            _ => (None, None),
        };
        let (site, via) = self.author_site();
        let outer = std::mem::take(&mut self.program.cur);
        self.exec_stmt_inner(stmt);
        let pieces = std::mem::replace(&mut self.program.cur, outer);
        for piece in pieces {
            let kind = match stmt {
                Stmt::Instruction(_) => EmitKind::Instruction { target: None },
                Stmt::Pseudo(p) => EmitKind::Data {
                    directive: pseudo_directive(p),
                },
                _ => EmitKind::Data {
                    directive: String::new(),
                },
            };
            self.program.emits.push(Emit {
                rom_offset: piece.rom_offset,
                len: piece.len,
                bank: piece.bank,
                cpu_addr: piece.cpu_addr,
                site,
                via: via.clone(),
                kind,
                operand: operand.clone(),
            });
            self.program.anchors.push(anchor);
        }
    }

    fn exec_stmt_inner(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Label(Name::Plain(name)) => {
                // The parser rejects a plain label in a written-out `.for` body;
                // this catches one that arrives later, in a custom pseudo-op's
                // emitted source expanded inside a loop. An `@local` is guarded
                // by `local-duplicate` instead (its scope may be per-iteration).
                if !self.loop_stack.is_empty()
                    && name != ":"
                    && !matches!(locals::classify(name), LocalName::Local(_))
                {
                    self.error(t!("label-in-for", name = locals::display(name)));
                }
                if let Some(key) = self.resolve_def(name) {
                    self.add_label(&key);
                }
                self.open_scope(name);
            }
            Stmt::Label(Name::Pasted(base, depth)) => {
                self.add_pasted_symbol(base, *depth, SymType::Label, None);
            }
            Stmt::Constant(name, expr) if self.enum_active => {
                // Inside an `.enum`, a bare name takes the counter (like
                // `NAME .rs 1`) and `NAME = expr` resets it to `expr + STEP`.
                let (value, bare) = match expr {
                    Some(e) => (self.eval(e), false),
                    None => (self.enum_value, true),
                };
                self.enum_member(name, value, 1, bare);
            }
            Stmt::Constant(name, expr) => {
                self.eval_taint = false;
                let value = match expr {
                    Some(e) => self.eval(e),
                    None => 1,
                };
                self.def_taint = std::mem::take(&mut self.eval_taint);
                match name {
                    Name::Plain(name) => self.add_symbol(name, value, SymType::Constant),
                    Name::Pasted(base, depth) => {
                        self.add_pasted_symbol(base, *depth, SymType::Constant, Some(value));
                    }
                }
                self.def_taint = false;
            }
            Stmt::Instruction(instr) => self.exec_instruction(instr),
            Stmt::Pseudo(p) => self.exec_pseudo(p),
            Stmt::Tag(key, text) => self.exec_tag(key, text),
        }
    }

    fn exec_pseudo(&mut self, p: &Pseudo) {
        match p {
            Pseudo::Org(e) => {
                self.check_table_layout(".org");
                let addr = self.eval(e);
                self.exec_org(addr);
                // A fixed address ends any approximation in this bank.
                let bank = self.layout_bank();
                self.approx_bank[bank] = false;
            }
            Pseudo::Phase(e) => {
                // Store the delta against the *un-phased* load address so the run
                // address stays correct as the location counter (and later `.org`s
                // within the block) advance, and so re-phasing recomputes cleanly.
                let run = self.eval(e);
                self.phase_delta = run - self.address_offset_raw();
            }
            Pseudo::Dephase => {
                self.phase_delta = 0;
            }
            Pseudo::Db(list) => {
                let vals: Vec<i64> = list.iter().map(|e| self.eval(e)).collect();
                for v in vals {
                    self.check_range(v, Field::Byte);
                    self.write_byte((v & 0xFF) as u8);
                }
            }
            // `.lobytes` / `.hibytes` are the explicit way to narrow: never checked.
            Pseudo::Lobytes(list) => {
                let vals: Vec<i64> = list.iter().map(|e| self.eval(e)).collect();
                for v in vals {
                    self.write_byte((v & 0xFF) as u8);
                }
            }
            Pseudo::Hibytes(list) => {
                let vals: Vec<i64> = list.iter().map(|e| self.eval(e)).collect();
                for v in vals {
                    self.write_byte(((v >> 8) & 0xFF) as u8);
                }
            }
            Pseudo::Dw(list) => {
                let vals: Vec<i64> = list.iter().map(|e| self.eval(e)).collect();
                for v in vals {
                    self.check_range(v, Field::Word);
                    self.write_byte((v & 0xFF) as u8);
                    self.write_byte(((v >> 8) & 0xFF) as u8);
                }
            }
            Pseudo::Ascii(arg) => {
                let off = match &arg.offset {
                    Some(e) => {
                        let v = self.eval(e);
                        if arg.negate {
                            -v
                        } else {
                            v
                        }
                    }
                    None => 0,
                };
                for b in arg.text.bytes() {
                    let v = b as i64 + off;
                    self.check_range(v, Field::Byte);
                    self.write_byte((v & 0xFF) as u8);
                }
            }
            Pseudo::Fill(list) => {
                let vals: Vec<i64> = list.iter().map(|e| self.eval(e)).collect();
                if vals.is_empty() {
                    self.hard_error(t!("fill-args"));
                    return;
                }
                let count = vals[0];
                let value = if vals.len() < 2 { 0xFF } else { vals[1] };
                if count > 0 {
                    self.check_range(value, Field::Byte);
                }
                for _ in 0..count.max(0) {
                    self.write_byte((value & 0xFF) as u8);
                }
            }
            Pseudo::Checksum(e) => {
                let address = self.eval(e).max(0) as usize;
                let index = self.rom_index();
                let crc = if self.pass == 2 {
                    if index < address {
                        self.hard_error(t!("checksum-preceding"));
                        return;
                    }
                    crc_32(&self.rom[address..index])
                } else {
                    0
                };
                self.write_byte(((crc >> 24) & 0xFF) as u8);
                self.write_byte(((crc >> 16) & 0xFF) as u8);
                self.write_byte(((crc >> 8) & 0xFF) as u8);
                self.write_byte((crc & 0xFF) as u8);
            }
            Pseudo::Random(terms) => {
                let ints: Vec<i64> = terms
                    .iter()
                    .map(|t| match t {
                        crate::ast::RandTerm::Num(e) => self.eval(e),
                        crate::ast::RandTerm::Str(s) => str2hash(s) as i64,
                    })
                    .collect();
                let seed = ints.first().copied().unwrap_or(0);
                let count = if ints.len() < 2 { 1 } else { ints[1] };
                let mut next = seed as u64;
                for _ in 0..count.max(0) {
                    next = next.wrapping_mul(1103515245).wrapping_add(12345);
                    let r = ((next / 65536) as u32) % 32768;
                    self.write_byte((r & 0xFF) as u8);
                }
            }
            Pseudo::Color(list) => {
                let vals: Vec<i64> = list.iter().map(|e| self.eval(e)).collect();
                for v in vals {
                    let color = (v & 0xFFFFFF) as u32;
                    let idx = nessemble_media::match_nes_color(
                        ((color >> 16) & 0xFF) as u8,
                        ((color >> 8) & 0xFF) as u8,
                        (color & 0xFF) as u8,
                    );
                    self.write_byte(idx);
                }
            }
            Pseudo::Enum { start, step, name } => {
                self.exec_enum(start, step.as_ref(), name.as_ref());
            }
            Pseudo::Endenum => self.exec_endenum(),
            Pseudo::Rsset(e) => {
                self.rsset = self.eval(e);
            }
            Pseudo::Rs(label, size) => {
                let size = self.eval(size);
                if self.enum_active {
                    let value = self.enum_value;
                    self.enum_member(&Name::Plain(label.clone()), value, size, false);
                } else {
                    let value = self.rsset;
                    self.add_symbol(label, value, SymType::Rs);
                    self.rsset += size;
                }
            }
            Pseudo::Ines(field, e) => {
                self.nes = true;
                let value = self.eval(e);
                self.set_ines_field(*field, value);
            }
            Pseudo::Ines2(e) => {
                self.nes = true;
                self.ines.nes2 = self.eval(e) != 0;
            }
            Pseudo::InesTiming(e) => {
                self.nes = true;
                self.ines.timing = Some(self.eval(e));
            }
            Pseudo::Prg(e) => {
                self.check_table_layout(".prg");
                self.segment_prg = true;
                self.prg_index = self.eval(e).max(0) as usize % MAX_BANKS;
                self.phase_delta = 0;
            }
            Pseudo::Chr(e) => {
                self.check_table_layout(".chr");
                self.segment_prg = false;
                self.chr_index = self.eval(e).max(0) as usize % MAX_BANKS;
                self.phase_delta = 0;
            }
            Pseudo::InesTrn => {
                self.check_table_layout(".inestrn");
                self.nes = true;
                self.ines.trn = true;
            }
            Pseudo::If(e) => {
                let cond = self.eval(e);
                self.if_stack.push(cond != 0);
            }
            Pseudo::Ifdef(name) => {
                let defined = self.symbol_exists(name);
                self.if_stack.push(defined);
            }
            Pseudo::Ifndef(name) => {
                let defined = self.symbol_exists(name);
                self.if_stack.push(!defined);
            }
            Pseudo::Else => {
                // Invert the innermost open block; a stray `.else` with none open
                // is a no-op (as before).
                if let Some(top) = self.if_stack.last_mut() {
                    *top = !*top;
                }
            }
            Pseudo::Endif => {
                // Close the innermost block; a stray `.endif` is a no-op.
                self.if_stack.pop();
            }
            Pseudo::Segment(name) => {
                self.check_table_layout(".segment");
                if let Some(rest) = name.strip_prefix("PRG") {
                    self.segment_prg = true;
                    self.prg_index = rest.parse::<usize>().unwrap_or(0) % MAX_BANKS;
                    self.phase_delta = 0;
                } else if let Some(rest) = name.strip_prefix("CHR") {
                    self.segment_prg = false;
                    self.chr_index = rest.parse::<usize>().unwrap_or(0) % MAX_BANKS;
                    self.phase_delta = 0;
                }
            }
            Pseudo::Incbin(file, offset, limit) => {
                let off = offset.as_ref().map_or(0, |e| self.eval(e)).max(0) as usize;
                let lim = limit.as_ref().map(|e| self.eval(e).max(0) as usize);
                match self.read_media_file(file) {
                    Ok(bytes) => {
                        let out = nessemble_media::incbin_slice(&bytes, off, lim);
                        self.write_all(&out);
                    }
                    Err(MediaError::Path(e)) => {
                        self.hard_error(e.message(crate::strip_file_url(file).0));
                    }
                    Err(MediaError::NotFound) => {
                        self.hard_error(t!("could-not-read", file = crate::strip_file_url(file).0));
                    }
                }
            }
            Pseudo::Incpng(file, offset, limit) => {
                let off = offset.as_ref().map_or(0, |e| self.eval(e)) as i32;
                let lim = limit.as_ref().map(|e| self.eval(e) as i32);
                match self.decode_media_png(file) {
                    Ok(png) => {
                        let out = nessemble_media::png_to_tiles(&png, off, lim);
                        self.write_all(&out);
                    }
                    Err(MediaError::Path(e)) => {
                        self.hard_error(e.message(crate::strip_file_url(file).0));
                    }
                    Err(MediaError::NotFound) => self.hard_error(t!("could-not-load-png")),
                }
            }
            Pseudo::Incpal(file) => match self.decode_media_png(file) {
                Ok(png) => {
                    let out = nessemble_media::png_to_palette(&png);
                    self.write_all(&out);
                }
                Err(MediaError::Path(e)) => {
                    self.hard_error(e.message(crate::strip_file_url(file).0));
                }
                Err(MediaError::NotFound) => self.hard_error(t!("could-not-load-png")),
            },
            Pseudo::Incrle(file) => match self.read_media_file(file) {
                Ok(bytes) => {
                    let out = nessemble_media::rle_encode(&bytes);
                    self.write_all(&out);
                }
                Err(MediaError::Path(e)) => {
                    self.hard_error(e.message(crate::strip_file_url(file).0));
                }
                Err(MediaError::NotFound) => {
                    self.hard_error(t!("could-not-read", file = crate::strip_file_url(file).0));
                }
            },
            Pseudo::Incwav(file, amp) => {
                let amplitude = amp.as_ref().map_or(24, |e| self.eval(e)) as i32;
                self.exec_incwav(file, amplitude);
            }
            Pseudo::Font(list) => {
                let ints: Vec<i64> = list.iter().map(|e| self.eval(e)).collect();
                self.exec_font(&ints);
            }
            Pseudo::Defchr(list) => {
                let ints: Vec<i64> = list.iter().map(|e| self.eval(e)).collect();
                self.exec_defchr(&ints);
            }
            Pseudo::Custom(name, args) => self.exec_custom(name, args),
            // `.for` / `.endfor` move the line cursor, so `run_lines` handles
            // them before a statement ever reaches here.
            Pseudo::For(..) | Pseudo::EndFor => {}
            Pseudo::Table(count, first) => self.exec_table(count.as_ref(), first.as_ref()),
            Pseudo::EndTable => self.exec_endtable(),
            Pseudo::Assert(expr, text, message) => self.exec_assert(expr, text, message.as_deref()),
            Pseudo::Error(message) => self.exec_error(message),
            Pseudo::Out(message) => self.exec_out(message),
        }
    }

    /// Assign an evaluated numeric `.inesXxx` value to its iNES header field.
    /// The non-numeric directives (`.ines2`, `.inestiming`) are handled inline
    /// in [`Self::exec_pseudo`] and are not routed here.
    fn set_ines_field(&mut self, field: InesField, value: i64) {
        use InesField as F;
        match field {
            F::Prg => self.ines.prg = value,
            F::Chr => self.ines.chr = value,
            F::Map => self.ines.map = value,
            // Single-bit Flags-6 toggles: keep only bit 0, exactly as the header
            // emission's former `& 0x01` did.
            F::Mir => self.ines.mir = (value & 1) != 0,
            F::Bat => self.ines.bat = (value & 1) != 0,
            F::FourScreen => self.ines.fsc = (value & 1) != 0,
            F::PrgRam => self.ines.prgram = value,
            F::Tv => self.ines.tv = value,
            F::Vs => self.ines.vs = value,
            F::Pc10 => self.ines.pc10 = value,
            F::SubMap => self.ines.submap = value,
            F::PrgNvRam => self.ines.prgnvram = value,
            F::ChrRam => self.ines.chrram = value,
            F::ChrNvRam => self.ines.chrnvram = value,
            F::Console => self.ines.console = value,
            F::VsPpu => self.ines.vsppu = value,
            F::VsHw => self.ines.vshw = value,
            F::MiscRom => self.ines.miscrom = value,
            F::Expansion => self.ines.expansion = value,
        }
    }

    /// Resolve and run a custom pseudo-op via the injected resolver, writing the
    /// bytes it returns (or reporting the resolver's error).
    ///
    /// The resolver runs **at most once per invocation site** ([`CustomKey`]):
    /// both passes visit every directive, so without this a script executes twice
    /// per assembly — and one returning non-deterministic bytes (say via `rand`)
    /// could return a different *length* on each pass and mis-size the ROM. An
    /// invocation whose arguments genuinely differ between passes (an `Int`
    /// argument referencing a forward-declared symbol, undefined on pass 1) has a
    /// different key on each, and so is resolved on each, as it must be.
    ///
    /// Two *separate* directives with identical arguments each still resolve
    /// once, because the site is part of the key — deduplicating those would
    /// change what a deliberately non-deterministic script emits, which is a
    /// different thing from the pass skew above.
    ///
    /// The result is memoized either way: a resolver error is reported on both
    /// passes, as before, without asking the resolver twice.
    ///
    /// A string argument carrying a `file://` declaration
    /// ([`crate::strip_file_url`]) reaches the resolver as the bare path, and is
    /// checked for existence *first*: a missing declared file is reported against
    /// this directive and the script is not run at all.
    ///
    /// A declared argument that is also `@/`-prefixed resolves from the project
    /// root *before* both the existence check and the resolver call, so `texts`
    /// carries the resolved path rather than the literal `@/…` spelling — this is
    /// what lets a `.tilemap "file://@/art/map.png"` reach the script the same
    /// way `.include "file://@/lib/defs.asm"` does. An *undeclared* argument that
    /// happens to start with `@/` is untouched: without `file://` the assembler
    /// can't tell it's a path at all (`plans/012-project-root-paths.md` §5.1).
    fn exec_custom(&mut self, name: &str, args: &[CustomArg]) {
        let mut ints = Vec::new();
        let mut texts = Vec::new();
        // (path as named in the source, resolved path to existence-check) for
        // each declared argument, in order.
        let mut declared: Vec<(String, PathBuf)> = Vec::new();
        for arg in args {
            match arg {
                CustomArg::Int(e) => ints.push(self.eval(e)),
                CustomArg::Str(s) => {
                    match resolve_str_arg(s, self.cur_dir(), self.root.as_deref()) {
                        Ok((text, declared_path)) => {
                            texts.push(text);
                            if let Some(resolved) = declared_path {
                                declared.push((crate::strip_file_url(s).0.to_string(), resolved));
                            }
                        }
                        Err(e) => {
                            self.hard_error(e.message(crate::strip_file_url(s).0));
                            return;
                        }
                    }
                }
            }
        }
        // A declared input that is not there is this directive's error, reported
        // before the script gets a chance to fail on its own terms. Named by what
        // the source wrote, not the resolved path.
        for (arg, resolved) in &declared {
            if !resolved.exists() {
                self.hard_error(t!("could-not-open", file = arg));
                return;
            }
        }
        let key = (
            name.to_string(),
            ints.clone(),
            texts.clone(),
            self.cur_file,
            self.cur_line,
        );
        let result = if let Some(memoized) = self.custom_memo.get(&key) {
            memoized.clone()
        } else {
            let resolved = (self.custom)(name, &ints, &texts, self.cur_dir(), self.root.as_deref());
            self.custom_memo.insert(key, resolved.clone());
            resolved
        };
        match result {
            Ok(CustomOutput::Bytes(bytes)) => self.write_all(&bytes),
            Ok(CustomOutput::Source(src)) => self.exec_emitted_source(name, &src),
            Ok(CustomOutput::Unmodeled) => self.mark_unmodeled(),
            Err(msg) => self.hard_error(msg),
        }
    }

    /// Expand `src` — the assembly source `.name`'s script returned via
    /// `emit_source` (`plans/013-structured-data-parsing.md` §6) — inline at
    /// the directive's own call site: lex, parse, and execute each resulting
    /// statement exactly as if it had been written there.
    ///
    /// Every diagnostic, symbol, and emitted byte is attributed to the
    /// directive's own file and line — `cur_file`/`cur_line` are left exactly
    /// as `exec_custom`'s caller (`run_pass`, or an enclosing
    /// `exec_emitted_source`) set them, deliberately not repointed at a
    /// position inside `src`, because there is no file for an editor to open
    /// at that position (§2.6's reasoning for `parse_xml_file` errors, applied
    /// here to generated source instead of a parsed document). A label or
    /// constant the expansion defines is flagged
    /// [`from_macro`](crate::ast::Line::from_macro) like one from a `.macro`
    /// body, for the same reason: it did not appear in the file's own text.
    ///
    /// `.include`, `.inestrn`, `.macro`, and `.macrodef` are preprocessor-only
    /// constructs — preprocessing has already finished by the time a script
    /// runs — and are rejected with a directive-specific error rather than
    /// whatever a bare parse of them would otherwise produce (`.include` with
    /// no preprocessor to run it parses as an unrecognized custom pseudo-op,
    /// which would be a confusing error for a builtin directive to give).
    fn exec_emitted_source(&mut self, name: &str, src: &str) {
        if self.custom_expansion_depth >= MAX_EMIT_SOURCE_DEPTH {
            self.hard_error(t!("emit-source-too-deep", pseudo = format!(".{name}")));
            return;
        }

        let mut tokens = crate::lexer::Lexer::new(src).tokenize();
        // The source's own `@local` definitions are private to this
        // invocation (`plans/021-local-labels.md` §2.6), under an id fixed by
        // where it runs, so it is the same on both passes.
        let site = (
            self.emit_ctx.0,
            self.cur_file,
            self.cur_line,
            self.cur_expansion,
            self.emit_ctx.1,
        );
        let next_id = (self.expansions.len() + self.emitted_ids.len() + 1) as u32;
        let emit_id = *self.emitted_ids.entry(site).or_insert(next_id);
        self.emitted_names.insert(emit_id, name.to_string());
        let defs = locals::local_defs(&tokens);
        locals::privatize(&mut tokens, emit_id, &defs);
        let lines = match crate::parse::parse_fragment(tokens, &[src]) {
            Ok(lines) => lines,
            Err(e) => {
                self.hard_error(t!(
                    "emit-source-parse-error",
                    pseudo = format!(".{name}"),
                    line = e.line,
                    message = e.message
                ));
                return;
            }
        };

        for line in &lines {
            let unsupported = match &line.stmt {
                Stmt::Pseudo(Pseudo::Custom(n, _))
                    if matches!(n.as_str(), "include" | "macro" | "macrodef") =>
                {
                    Some(format!(".{n}"))
                }
                Stmt::Pseudo(Pseudo::InesTrn) => Some(".inestrn".to_string()),
                Stmt::Pseudo(Pseudo::Table(..)) => Some(".table".to_string()),
                Stmt::Pseudo(Pseudo::EndTable) => Some(".endtable".to_string()),
                Stmt::Tag(..) => Some("[KEY] tag".to_string()),
                _ => None,
            };
            if let Some(directive) = unsupported {
                self.hard_error(t!(
                    "emit-source-unsupported-directive",
                    pseudo = format!(".{name}"),
                    directive = directive
                ));
                return;
            }
        }

        let saved_from_macro = self.cur_from_macro;
        let saved_ctx = self.emit_ctx;
        self.cur_from_macro = true;
        self.emit_ctx = (emit_id, 0);
        self.custom_expansion_depth += 1;
        self.run_lines(&lines, false);
        self.custom_expansion_depth -= 1;
        self.emit_ctx = saved_ctx;
        self.cur_from_macro = saved_from_macro;
    }

    /// Directory of the file the current line came from, which filename-based
    /// directives resolve against. Falls back to the current directory if the
    /// file id has no recorded directory (should not happen in practice).
    fn cur_dir(&self) -> &Path {
        self.dirs
            .get(self.cur_file as usize)
            .map_or_else(|| Path::new("."), PathBuf::as_path)
    }

    // -- media importers ----------------------------------------------------

    /// Write every byte in `bytes` through the normal emission path.
    fn write_all(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_byte(b);
        }
    }

    /// Read a media file resolved against the directory of the file that
    /// contains the directive (see [`Self::cur_dir`]), or from the project root
    /// for a `@/`-prefixed name.
    ///
    /// A `file://` declaration is stripped first ([`crate::strip_file_url`]), so
    /// `.incbin "file://logo.chr"` reads the same file `.incbin "logo.chr"` does.
    fn read_media_file(&self, name: &str) -> Result<Vec<u8>, MediaError> {
        let (path, _declared) = crate::strip_file_url(name);
        let resolved = crate::resolve_path_arg(self.root.as_deref(), self.cur_dir(), path)
            .map_err(MediaError::Path)?;
        std::fs::read(resolved).map_err(|_| MediaError::NotFound)
    }

    /// Read and decode a media PNG (open failure and decode failure are
    /// indistinguishable, matching the reference's `stbi_load`).
    fn decode_media_png(&self, name: &str) -> Result<nessemble_media::Png, MediaError> {
        let bytes = self.read_media_file(name)?;
        nessemble_media::decode_png(&bytes).map_err(|_| MediaError::NotFound)
    }

    fn exec_incwav(&mut self, file: &str, amplitude: i32) {
        let bytes = match self.read_media_file(file) {
            Ok(bytes) => bytes,
            Err(MediaError::Path(e)) => {
                self.hard_error(e.message(crate::strip_file_url(file).0));
                return;
            }
            Err(MediaError::NotFound) => {
                self.hard_error(t!("could-not-open", file = crate::strip_file_url(file).0));
                return;
            }
        };
        match nessemble_media::wav_to_dpcm(&bytes, amplitude) {
            Ok(out) => self.write_all(&out),
            Err(nessemble_media::WavError::ShortRead) => {
                self.hard_error(t!("could-not-read", file = file));
            }
            Err(nessemble_media::WavError::NotWav) => self.hard_error(t!("not-a-wav", file = file)),
            Err(nessemble_media::WavError::NotMono) => self.hard_error(t!("wav-not-mono")),
        }
    }

    fn exec_font(&mut self, ints: &[i64]) {
        if ints.is_empty() {
            self.hard_error(t!("font-args"));
            return;
        }
        let start = ints[0];
        if start >= 0x80 {
            self.hard_error(t!("value-too-high"));
            return;
        }
        let end = if ints.len() < 2 { start } else { ints[1] };
        let (lo, hi) = if start <= end {
            (start, end)
        } else {
            (end, start)
        };
        for ch in lo..=hi {
            let glyph = nessemble_media::font_glyph(ch.max(0) as usize).to_vec();
            self.write_all(&glyph);
        }
    }

    fn exec_defchr(&mut self, ints: &[i64]) {
        if ints.len() != 8 {
            self.error(t!("defchr-args", count = ints.len()));
        }
        // Two bitplanes (low bit then high bit), one byte per tile row.
        for bit in 0..2 {
            for &row in ints {
                let mut byte = 0u8;
                for k in (0..8).rev() {
                    let digit = (row as f64 / 10f64.powi(k)) as i64 % 10;
                    byte |= (((digit >> bit) & 1) << k) as u8;
                }
                self.write_byte(byte);
            }
        }
    }

    fn exec_org(&mut self, address: i64) {
        if self.segment_prg {
            if self.ines.prg < 2 {
                if address < 0xC000 {
                    self.hard_error(t!("prg-start-c000", bank = self.prg_index));
                    return;
                }
                if address >= 0xC000 + BANK_PRG {
                    self.hard_error(t!("address-too-high"));
                    return;
                }
                self.prg_offsets[self.prg_index] = address - 0xC000;
            } else {
                if self.prg_index % 2 == 0 {
                    if address < 0x8000 {
                        self.hard_error(t!("prg-start-8000", bank = self.prg_index));
                        return;
                    }
                    if address >= 0x8000 + BANK_PRG {
                        self.hard_error(t!("address-too-high"));
                        return;
                    }
                } else {
                    if address < 0xC000 {
                        self.hard_error(t!("prg-start-c000", bank = self.prg_index));
                        return;
                    }
                    if address >= 0xC000 + BANK_PRG {
                        self.hard_error(t!("address-too-high"));
                        return;
                    }
                }
                self.prg_offsets[self.prg_index] =
                    address - 0x8000 - ((self.prg_index as i64) % 2) * BANK_PRG;
            }
        } else {
            if address >= BANK_CHR {
                self.hard_error(t!("address-too-high"));
                return;
            }
            self.chr_offsets[self.chr_index] = address;
        }
    }

    // -- instruction encoding ----------------------------------------------

    fn get_opcode(&self, mnemonic: &str, mode: AddressingMode) -> Option<&'static Opcode> {
        OPCODES.iter().find(|o| {
            o.mode == mode
                && o.mnemonic.eq_ignore_ascii_case(mnemonic)
                && if self.undocumented {
                    o.meta & META_UNDOCUMENTED != 0
                } else {
                    o.meta & META_UNDOCUMENTED == 0
                }
        })
    }

    /// Records the appropriate error for a missing opcode; returns whether the
    /// mnemonic exists at all (for callers that skip emission when unknown).
    fn mnemonic_exists(&mut self, mnemonic: &str) -> bool {
        let exists = OPCODES
            .iter()
            .any(|o| o.mnemonic.eq_ignore_ascii_case(mnemonic));
        if exists {
            self.error(t!("invalid-mode"));
        } else {
            self.error(t!("unknown-opcode", mnemonic = mnemonic));
        }
        exists
    }

    fn register_exists(&mut self, reg: char, allowed: &str) -> bool {
        if allowed.contains(reg) {
            true
        } else {
            self.error(t!("unknown-register", reg = reg));
            false
        }
    }

    /// Resolve `mnem` in `mode` to its opcode byte, recording the reference
    /// diagnostic and returning `None` when it can't be emitted: "unknown
    /// opcode" if the mnemonic exists in no mode, or "invalid addressing mode"
    /// if it exists but not in this one. Callers emit nothing on `None`.
    fn resolve_opcode(&mut self, mnem: &str, mode: AddressingMode) -> Option<u8> {
        if let Some(op) = self.get_opcode(mnem, mode) {
            Some(op.opcode)
        } else {
            // Records "invalid addressing mode" (mnemonic exists elsewhere) or
            // "unknown opcode" (it doesn't); the bool is unused here.
            self.mnemonic_exists(mnem);
            None
        }
    }

    /// Validate an `X`/`Y` index register and select the matching addressing
    /// mode. Records an unknown-register error and returns `None` when `reg` is
    /// neither `X` nor `Y`.
    fn indexed_mode(
        &mut self,
        reg: char,
        x: AddressingMode,
        y: AddressingMode,
    ) -> Option<AddressingMode> {
        if !self.register_exists(reg, "XY") {
            return None;
        }
        Some(if reg == 'X' { x } else { y })
    }

    fn exec_instruction(&mut self, instr: &Instruction) {
        let mnem = instr.mnemonic.clone();
        match instr.operand.clone() {
            Operand::Implied => {
                if self.enum_active
                    && !OPCODES
                        .iter()
                        .any(|o| o.mnemonic.eq_ignore_ascii_case(&mnem))
                {
                    self.error(t!("enum-member-indented", mnemonic = mnem.as_str()));
                    return;
                }
                if let Some(op) = self.resolve_opcode(&mnem, AddressingMode::Implied) {
                    self.write_byte(op);
                }
            }
            Operand::Accumulator(reg) => {
                if !self.register_exists(reg, "A") {
                    return;
                }
                if let Some(op) = self.resolve_opcode(&mnem, AddressingMode::Accumulator) {
                    self.write_byte(op);
                }
            }
            Operand::Immediate(e) => {
                let value = self.eval(&e);
                if let Some(op) = self.resolve_opcode(&mnem, AddressingMode::Immediate) {
                    self.check_range(value, Field::Byte);
                    self.write_byte(op);
                    self.write_byte((value & 0xFF) as u8);
                }
            }
            Operand::Indirect(e) => {
                let value = self.eval(&e);
                if let Some(op) = self.resolve_opcode(&mnem, AddressingMode::Indirect) {
                    self.check_range(value, Field::Absolute);
                    self.write_byte(op);
                    self.write_byte((value & 0xFF) as u8);
                    self.write_byte(((value >> 8) & 0xFF) as u8);
                }
            }
            Operand::IndirectIndexed(e, reg) => {
                let value = self.eval(&e);
                let Some(mode) =
                    self.indexed_mode(reg, AddressingMode::IndirectX, AddressingMode::IndirectY)
                else {
                    return;
                };
                if let Some(op) = self.resolve_opcode(&mnem, mode) {
                    self.check_range(value, Field::ZeroPage);
                    self.write_byte(op);
                    self.write_byte((value & 0xFF) as u8);
                }
            }
            Operand::ZeroPage(e) => {
                let value = self.eval(&e);
                // Mirrors the reference: zeropage emits without an existence
                // check, falling back to the 0xFF sentinel for an unknown opcode
                // (C's `(unsigned int)(-1)` low byte).
                let op = self.get_opcode(&mnem, AddressingMode::ZeroPage);
                self.check_range(value, Field::ZeroPage);
                self.write_byte(op.map_or(0xFF, |o| o.opcode));
                self.write_byte((value & 0xFF) as u8);
            }
            Operand::ZeroPageIndexed(e, reg) => {
                let value = self.eval(&e);
                let Some(mode) =
                    self.indexed_mode(reg, AddressingMode::ZeroPageX, AddressingMode::ZeroPageY)
                else {
                    return;
                };
                if let Some(op) = self.resolve_opcode(&mnem, mode) {
                    self.check_range(value, Field::ZeroPage);
                    self.write_byte(op);
                    self.write_byte((value & 0xFF) as u8);
                }
            }
            Operand::Absolute(e) => {
                let value = self.eval(&e);
                if let Some(op) = self.get_opcode(&mnem, AddressingMode::Absolute) {
                    self.check_range(value, Field::Absolute);
                    self.write_byte(op.opcode);
                    self.write_byte((value & 0xFF) as u8);
                    self.write_byte(((value >> 8) & 0xFF) as u8);
                } else if self.get_opcode(&mnem, AddressingMode::Relative).is_some() {
                    self.emit_relative(&mnem, value);
                } else {
                    // Records "unknown opcode" / "invalid addressing mode".
                    self.mnemonic_exists(&mnem);
                }
            }
            Operand::AbsoluteIndexed(e, reg) => {
                let value = self.eval(&e);
                let Some(mode) =
                    self.indexed_mode(reg, AddressingMode::AbsoluteX, AddressingMode::AbsoluteY)
                else {
                    return;
                };
                if let Some(op) = self.resolve_opcode(&mnem, mode) {
                    self.check_range(value, Field::Absolute);
                    self.write_byte(op);
                    self.write_byte((value & 0xFF) as u8);
                    self.write_byte(((value >> 8) & 0xFF) as u8);
                }
            }
        }
    }

    fn emit_relative(&mut self, mnemonic: &str, target: i64) {
        let op = self.get_opcode(mnemonic, AddressingMode::Relative);
        let offset = self.address_offset() + 1;
        let mut address: i64;
        if offset > target {
            address = 0xFF - (offset - target);
            if self.pass == 2 && address <= 0x7F {
                self.error(t!("branch-out-of-range"));
            }
        } else {
            address = target - offset - 1;
            if self.pass == 2 && address >= 0x80 {
                self.error(t!("branch-out-of-range"));
            }
        }
        address &= 0xFF;
        // `op` is `Some` here (the caller checked Relative exists); the sentinel
        // preserves the reference's behavior defensively.
        self.write_byte(op.map_or(0xFF, |o| o.opcode));
        self.write_byte((address & 0xFF) as u8);
    }

    // -- expression evaluation ---------------------------------------------

    fn eval(&mut self, expr: &Expr) -> i64 {
        match expr {
            Expr::Num(n) => *n,
            Expr::Symbol(name) => self.eval_symbol(name),
            Expr::LocalForward(n) => self.eval_local(*n as i32),
            Expr::LocalBackward(n) => self.eval_local(-(*n as i32)),
            Expr::High(e) => (self.eval(e) >> 8) & 0xFF,
            Expr::Low(e) => self.eval(e) & 0xFF,
            Expr::Bank(name) => self.eval_bank(name),
            Expr::LoopIndex(depth) => self.loop_value(*depth),
            Expr::PastedSymbol(base, depth) => match self.paste_name(base, *depth) {
                Some(name) => self.eval_symbol(&name),
                None => 0,
            },
            Expr::Binary(a, op, b) => {
                let lhs = self.eval(a);
                let rhs = self.eval(b);
                Self::apply(lhs, *op, rhs)
            }
            Expr::Unary(op, e) => Self::apply_unary(*op, self.eval(e)),
        }
    }

    fn apply_unary(op: UnOp, v: i64) -> i64 {
        match op {
            UnOp::Neg => v.wrapping_neg(),
            UnOp::Compl => !v,
            UnOp::Not => i64::from(v == 0),
        }
    }

    fn eval_symbol(&mut self, name: &str) -> i64 {
        let Some(key) = self.resolve_ref(name) else {
            self.note_unknown(name);
            if self.pass == 2 {
                self.error(t!("local-no-scope", name = name));
            }
            return 1;
        };
        if let Some(id) = self.find_symbol(&key) {
            if self.symbols[id].kind == SymType::Undefined {
                self.note_unknown(locals::display(name));
                if self.pass == 2 {
                    let msg = self.undefined_message(name, &key);
                    self.error(msg);
                }
            }
            self.eval_taint |= self.symbols[id].approx;
            self.symbols[id].value
        } else {
            self.note_unknown(locals::display(name));
            // Reference behavior: unknown symbols are registered as
            // undefined (value 1) during pass 1.
            self.add_symbol(&key, 1, SymType::Undefined);
            1
        }
    }

    /// Whether `.ifdef name` holds: the name resolves to a symbol entry (an
    /// earlier reference's placeholder counts, as it always has). A local with
    /// no scope open is reported on pass 2 and does not hold.
    fn symbol_exists(&mut self, name: &str) -> bool {
        if let Some(key) = self.resolve_ref(name) {
            return self.find_symbol(&key).is_some();
        }
        if self.pass == 2 && !self.if_suppressed() {
            self.error(t!("local-no-scope", name = name));
        }
        false
    }

    /// Remember the first thing an expression touched whose value is not known
    /// yet, for the pass-1 check on `.for` bounds.
    fn note_unknown(&mut self, name: &str) {
        if self.eval_unknown.is_none() {
            self.eval_unknown = Some(name.to_string());
        }
    }

    fn eval_local(&mut self, direction: i32) -> i64 {
        if self.pass != 2 {
            let name = if direction > 0 { ":+" } else { ":-" };
            self.note_unknown(name);
            return 1;
        }
        self.eval_taint |= self.approx_bank[self.layout_bank()];
        match self.get_symbol_local(direction) {
            Some(id) => {
                self.eval_taint |= self.symbols[id].approx;
                self.symbols[id].value
            }
            None => 1,
        }
    }

    fn eval_bank(&mut self, name: &str) -> i64 {
        let Some(key) = self.resolve_ref(name) else {
            self.note_unknown(name);
            if self.pass == 2 {
                self.error(t!("local-no-scope", name = name));
            }
            return 1;
        };
        if let Some(id) = self.find_symbol(&key) {
            self.symbols[id].bank as i64
        } else {
            self.note_unknown(name);
            1
        }
    }

    fn apply(a: i64, op: BinOp, b: i64) -> i64 {
        match op {
            BinOp::Add => a + b,
            BinOp::Sub => a - b,
            BinOp::Mul => a * b,
            BinOp::Div => {
                if b == 0 {
                    0
                } else {
                    a / b
                }
            }
            BinOp::Pow => (a as f64).powf(b as f64) as i64,
            BinOp::And => a & b,
            BinOp::Or => a | b,
            BinOp::Xor => a ^ b,
            BinOp::Rshift => a >> (b & 63),
            BinOp::Lshift => a << (b & 63),
            BinOp::Mod => {
                if b == 0 {
                    0
                } else {
                    a % b
                }
            }
            BinOp::Eq => (a == b) as i64,
            BinOp::Ne => (a != b) as i64,
            BinOp::Lt => (a < b) as i64,
            BinOp::Gt => (a > b) as i64,
            BinOp::Le => (a <= b) as i64,
            BinOp::Ge => (a >= b) as i64,
        }
    }
}

/// An instruction's operand rendered back to source, for messages: the forms a
/// control transfer can take (`helper`, `[vec_ptr]`, `:+`).
fn operand_text(op: &Operand) -> Option<String> {
    match op {
        Operand::Absolute(e) | Operand::ZeroPage(e) => Some(expr_text(e)),
        Operand::Indirect(e) => Some(format!("[{}]", expr_text(e))),
        _ => None,
    }
}

/// `expr` rendered back to source text (normalized spacing; numbers in hex).
fn expr_text(expr: &Expr) -> String {
    match expr {
        Expr::Num(n) if (0..10).contains(n) => n.to_string(),
        Expr::Num(n) if (0..=0xFF).contains(n) => format!("${n:02X}"),
        Expr::Num(n) if (0..=0xFFFF).contains(n) => format!("${n:04X}"),
        Expr::Num(n) => n.to_string(),
        Expr::Symbol(name) => name.clone(),
        Expr::LocalForward(n) => format!(":{}", "+".repeat(*n as usize)),
        Expr::LocalBackward(n) => format!(":{}", "-".repeat(*n as usize)),
        Expr::High(e) => format!(">{}", expr_text(e)),
        Expr::Low(e) => format!("<{}", expr_text(e)),
        Expr::Bank(name) => format!("BANK({name})"),
        Expr::LoopIndex(d) => ["\\i", "\\j", "\\k"][usize::from(*d).min(2)].to_string(),
        Expr::PastedSymbol(base, d) => {
            format!("{base}{}", ["\\i", "\\j", "\\k"][usize::from(*d).min(2)])
        }
        Expr::Unary(op, e) => {
            let op = match op {
                UnOp::Neg => "-",
                UnOp::Not => "!",
                UnOp::Compl => "~",
            };
            format!("{op}{}", expr_text(e))
        }
        Expr::Binary(a, op, b) => {
            let op = match op {
                BinOp::Add => "+",
                BinOp::Sub => "-",
                BinOp::Mul => "*",
                BinOp::Div => "/",
                BinOp::Pow => "**",
                BinOp::And => "&",
                BinOp::Or => "|",
                BinOp::Xor => "^",
                BinOp::Rshift => ">>",
                BinOp::Lshift => "<<",
                BinOp::Mod => "%",
                BinOp::Eq => "==",
                BinOp::Ne => "!=",
                BinOp::Lt => "<",
                BinOp::Gt => ">",
                BinOp::Le => "<=",
                BinOp::Ge => ">=",
            };
            format!("{}{op}{}", expr_text(a), expr_text(b))
        }
    }
}

/// The directive a data statement was written with, for messages.
fn pseudo_directive(p: &Pseudo) -> String {
    let name = match p {
        Pseudo::Db(_) => "db",
        Pseudo::Dw(_) => "dw",
        Pseudo::Ascii(_) => "ascii",
        Pseudo::Hibytes(_) => "hibytes",
        Pseudo::Lobytes(_) => "lobytes",
        Pseudo::Fill(_) => "fill",
        Pseudo::Checksum(_) => "checksum",
        Pseudo::Random(_) => "random",
        Pseudo::Color(_) => "color",
        Pseudo::Incbin(..) => "incbin",
        Pseudo::Incpng(..) => "incpng",
        Pseudo::Incpal(_) => "incpal",
        Pseudo::Incrle(_) => "incrle",
        Pseudo::Incwav(..) => "incwav",
        Pseudo::Font(_) => "font",
        Pseudo::Defchr(_) => "defchr",
        Pseudo::Custom(name, _) => return format!(".{name}"),
        _ => "data",
    };
    format!(".{name}")
}

/// CRC-32 (poly 0xEDB88320), matching the reference `crc_32`.
///
/// Used by `.checksum`, and re-exported (as [`crate::crc_32`]) for naming
/// content-addressed files — never as a correctness boundary: a CRC collision
/// must be *detected* by whatever compares the real contents, not relied upon
/// not to happen.
#[must_use]
pub fn crc_32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (i, entry) in table.iter_mut().enumerate() {
        let mut rem = i as u32;
        for _ in 0..8 {
            if rem & 1 != 0 {
                rem = (rem >> 1) ^ 0xEDB88320;
            } else {
                rem >>= 1;
            }
        }
        *entry = rem;
    }
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in data {
        crc = (crc >> 8) ^ table[((crc & 0xFF) ^ b as u32) as usize];
    }
    !crc
}

/// djb2-style hash over the inner characters of a string, matching the
/// reference `str2hash` (which skips the surrounding quotes).
fn str2hash(inner: &str) -> u32 {
    let mut hash: u32 = 5381;
    for b in inner.bytes() {
        hash = hash
            .wrapping_shl(5)
            .wrapping_add(hash)
            .wrapping_add(b as u32);
    }
    hash
}

/// How symbol `s` is named in the list file: a private local as
/// `parent@x\@id` (or `@x\@id` with no parent), anything else by its key.
fn list_name(s: &Symbol) -> String {
    match locals::classify(&s.name) {
        LocalName::Private { id, local } => match &s.parent {
            Some(parent) => format!("{parent}{local}\\@{id}"),
            None => format!("{local}\\@{id}"),
        },
        _ => s.name.clone(),
    }
}

/// Deduplicate diagnostics by (file, line, message), preserving first-seen
/// order. Collect mode runs both passes, so the same error can be recorded
/// twice; this collapses those.
fn dedup(diags: &[Diag]) -> Vec<Diag> {
    let mut seen = std::collections::HashSet::new();
    diags
        .iter()
        .filter(|d| seen.insert((d.file.clone(), d.line, d.message.clone())))
        .cloned()
        .collect()
}

/// Resolve one custom-directive string argument to what `texts` should carry,
/// and — if it was declared (`file://`) — the path to existence-check. Shared
/// by [`Assembler::exec_custom`] and [`prewarm_candidates`] so the two cannot
/// drift: a prewarm scan that resolved a declared argument differently from
/// the real pass would prewarm the wrong cache entry.
fn resolve_str_arg(
    s: &str,
    dir: &Path,
    root: Option<&Path>,
) -> Result<(String, Option<PathBuf>), crate::PathArgError> {
    let (path, is_declared) = crate::strip_file_url(s);
    if !is_declared {
        return Ok((path.to_string(), None));
    }
    let resolved = crate::resolve_path_arg(root, dir, path)?;
    let is_root_relative = path.starts_with(crate::PROJECT_ROOT_PREFIX);
    let text = if is_root_relative {
        resolved.to_string_lossy().into_owned()
    } else {
        path.to_string()
    };
    Ok((text, Some(resolved)))
}

/// A custom-directive invocation whose arguments are knowable without running
/// either assembly pass: every string argument resolves the same way
/// [`Assembler::exec_custom`] resolves it (declared/`@/` paths are looked up
/// against static per-file tables, no assembler state involved), and every
/// integer argument is a [`literal_eval`]-able expression — no symbol, local
/// label, or bank reference, so its value cannot depend on anything a pass
/// discovers. Safe to resolve ahead of time, e.g. to prewarm a cache
/// concurrently before the sequential emission passes read from it
/// (`plans/013-structured-data-parsing.md` §7).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PrewarmCandidate {
    pub name: String,
    pub ints: Vec<i64>,
    pub texts: Vec<String>,
    pub base_dir: PathBuf,
    pub root: Option<PathBuf>,
}

/// Evaluate `expr` if it references no symbol, anonymous local label, or
/// bank — a compile-time-literal expression whose value cannot differ between
/// passes (or depend on anything a pass discovers). `None` otherwise. Shares
/// [`Assembler::apply`]'s arithmetic so a literal expression evaluates
/// identically here and in the real pass.
fn literal_eval(expr: &Expr) -> Option<i64> {
    match expr {
        Expr::Num(n) => Some(*n),
        Expr::Symbol(_)
        | Expr::LocalForward(_)
        | Expr::LocalBackward(_)
        | Expr::Bank(_)
        | Expr::LoopIndex(_)
        | Expr::PastedSymbol(..) => None,
        Expr::High(e) => Some((literal_eval(e)? >> 8) & 0xFF),
        Expr::Low(e) => Some(literal_eval(e)? & 0xFF),
        Expr::Binary(a, op, b) => Some(Assembler::apply(literal_eval(a)?, *op, literal_eval(b)?)),
        Expr::Unary(op, e) => Some(Assembler::apply_unary(*op, literal_eval(e)?)),
    }
}

/// Scan `lines` for [`PrewarmCandidate`]s: every `Pseudo::Custom` invocation
/// whose arguments resolve without running a pass. A directive with even one
/// symbol-dependent integer argument, or a declared string argument that
/// doesn't resolve or names a file that isn't there, is simply left out — it
/// runs sequentially as it always has, through the real pass's own
/// `exec_custom`, which remains the sole authority on errors (a prewarm scan
/// never reports one of its own).
#[must_use]
pub(crate) fn prewarm_candidates(
    lines: &[Line],
    dirs: &[PathBuf],
    root: Option<&Path>,
) -> Vec<PrewarmCandidate> {
    let mut out = Vec::new();
    for line in lines {
        let Stmt::Pseudo(Pseudo::Custom(name, args)) = &line.stmt else {
            continue;
        };
        let dir = dirs
            .get(line.file as usize)
            .map_or_else(|| Path::new("."), PathBuf::as_path);
        let mut ints = Vec::new();
        let mut texts = Vec::new();
        let mut safe = true;
        for arg in args {
            match arg {
                CustomArg::Int(e) => {
                    if let Some(v) = literal_eval(e) {
                        ints.push(v);
                    } else {
                        safe = false;
                        break;
                    }
                }
                CustomArg::Str(s) => match resolve_str_arg(s, dir, root) {
                    Ok((_, Some(resolved))) if !resolved.exists() => {
                        safe = false;
                        break;
                    }
                    Ok((text, _)) => texts.push(text),
                    Err(_) => {
                        safe = false;
                        break;
                    }
                },
            }
        }
        if safe {
            out.push(PrewarmCandidate {
                name: name.clone(),
                ints,
                texts,
                base_dir: dir.to_path_buf(),
                root: root.map(Path::to_path_buf),
            });
        }
    }
    out
}
