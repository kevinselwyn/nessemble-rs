//! Two-pass assembler mirroring the reference `nessemble` semantics for the
//! Phase 2 (non-iNES-output) subset: instructions, symbols, expressions,
//! `.org`, data directives, and error reporting.
//!
//! Full iNES ROM output (header, banking, CHR/trainer padding) is Phase 3;
//! here we set the iNES-related state (so address math and `.org` validation
//! match) but emit the raw written region.

use std::path::{Path, PathBuf};

use nessemble_i18n::t;
use nessemble_isa::{AddressingMode, Opcode, META_UNDOCUMENTED, OPCODES};

use crate::ast::{BinOp, CustomArg, Expr, InesField, Instruction, Line, Operand, Pseudo, Stmt};

const BANK_PRG: i64 = 0x4000;
const BANK_CHR: i64 = 0x2000;
const MAX_BANKS: usize = 256;
/// iNES trainer region size (matches the reference `TRAINER_MAX`).
const TRAINER_MAX: usize = 512;
/// Matches the reference `MAX_NESTED_IFS`.
const MAX_NESTED_IFS: usize = 10;

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

/// A diagnostic (error) with a source-file display name, 1-based source line,
/// and message, matching the reference tool's wording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diag {
    /// Display name of the file the diagnostic refers to (the basename of the
    /// top-level input, or the raw path of an included file).
    pub file: String,
    pub line: u32,
    pub message: String,
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

/// Per-bank write coverage for `-C`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageReport {
    /// Covered byte count for each PRG bank.
    pub prg: Vec<u32>,
    /// Covered byte count for each CHR bank.
    pub chr: Vec<u32>,
    /// Total bytes in a PRG bank (denominator).
    pub prg_bank_size: u32,
    /// Total bytes in a CHR bank (denominator).
    pub chr_bank_size: u32,
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
}

#[derive(Debug, Clone)]
struct Symbol {
    name: String,
    value: i64,
    kind: SymType,
    bank: usize,
}

#[derive(Debug, Clone, Copy)]
struct Ines {
    chr: i64,
    map: i64,
    mir: i64,
    prg: i64,
    trn: i64,
    /// Battery-backed / persistent memory (Flags 6 bit 1).
    bat: i64,
    /// Four-screen VRAM / alternative nametable layout (Flags 6 bit 3).
    fsc: i64,
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
            mir: 0,
            prg: 1,
            trn: 0,
            bat: 0,
            fsc: 0,
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

    rom: Vec<u8>,
    /// Per-ROM-byte coverage bitmap (which bytes were written), for `-C`.
    coverage: Vec<bool>,
    offset_max: usize,

    symbols: Vec<Symbol>,

    enum_active: bool,
    enum_value: i64,
    enum_inc: i64,
    rsset: i64,

    // Conditional-assembly state (`.if`/`.ifdef`/`.ifndef`/`.else`/`.endif`):
    // one entry per open block, holding its (possibly `.else`-flipped)
    // condition. Empty means no conditional is open.
    if_stack: Vec<bool>,

    // iNES trainer redirection (`.inestrn`).
    trainer: Vec<u8>,
    offset_trainer: usize,

    pass: u8,
    errors: Vec<Diag>,
    warnings: Vec<Diag>,
    aborted: bool,
    /// When set, a hard error records a diagnostic but does **not** abort the
    /// run, so multiple problems can be collected in one pass (for tooling
    /// diagnostics). The normal `assemble` path leaves this `false` and stops at
    /// the first hard error, exactly as before.
    collect_all: bool,
    cur_line: u32,
    cur_file: u32,
    files: Vec<String>,
    /// Directory each source file resolves filename-based directives
    /// (`.incbin`/`.incpng`/… and custom pseudo-ops) against, parallel to
    /// `files`. Indexed by `cur_file`, so a directive is resolved relative to
    /// the file that contains it rather than the top-level file.
    dirs: Vec<PathBuf>,
    /// Resolver for custom pseudo-ops (`.foo`): given the directive name, its
    /// numeric and string arguments, and the base directory, it returns the
    /// bytes to emit (or an error message).
    custom: CustomResolver,
}

/// Resolves a custom pseudo-op to the bytes it emits. See [`Assembler::custom`].
pub type CustomResolver =
    Box<dyn Fn(&str, &[i64], &[String], &std::path::Path) -> Result<Vec<u8>, String>>;

impl Assembler {
    pub fn new(
        nes: bool,
        undocumented: bool,
        empty_byte: u8,
        files: Vec<String>,
        dirs: Vec<PathBuf>,
        custom: CustomResolver,
    ) -> Self {
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
            rom: Vec::new(),
            coverage: Vec::new(),
            offset_max: 0,
            symbols: Vec::new(),
            enum_active: false,
            enum_value: 0,
            enum_inc: 0,
            rsset: 0,
            if_stack: Vec::new(),
            trainer: Vec::new(),
            offset_trainer: 0,
            pass: 1,
            errors: Vec::new(),
            warnings: Vec::new(),
            aborted: false,
            collect_all: false,
            cur_line: 1,
            cur_file: 0,
            files,
            dirs,
            custom,
        }
    }

    /// Run both passes in collect mode and return every distinct error and
    /// warning (deduplicated by file/line/message, preserving first-seen order).
    /// The output bytes are discarded — this is for diagnostics only.
    pub fn diagnostics(&mut self, lines: &[Line]) -> (Vec<Diag>, Vec<Diag>) {
        self.collect_all = true;
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
        (dedup(&self.errors), dedup(&self.warnings))
    }

    /// The per-bank coverage summary (`-C`), or `None` when not in iNES mode
    /// (coverage is reported over PRG/CHR banks). Mirrors the reference
    /// `get_coverage` output.
    pub fn coverage_report(&self) -> Option<CoverageReport> {
        if !self.nes {
            return None;
        }
        let count = |start: usize, len: usize| -> u32 {
            self.coverage
                .get(start..start + len)
                .map_or(0, |s| s.iter().filter(|&&c| c).count() as u32)
        };
        let mut prg = Vec::new();
        for i in 0..self.ines.prg.max(0) as usize {
            prg.push(count(i * BANK_PRG as usize, BANK_PRG as usize));
        }
        let mut chr = Vec::new();
        let prg_bytes = self.ines.prg.max(0) as usize * BANK_PRG as usize;
        for i in 0..self.ines.chr.max(0) as usize {
            chr.push(count(prg_bytes + i * BANK_CHR as usize, BANK_CHR as usize));
        }
        Some(CoverageReport {
            prg,
            chr,
            prg_bank_size: BANK_PRG as u32,
            chr_bank_size: BANK_CHR as u32,
        })
    }

    /// Symbols eligible for the list file (`-l`), excluding those only
    /// referenced but never defined.
    pub fn list_symbols(&self) -> Vec<ListSymbol> {
        self.symbols
            .iter()
            .filter(|s| s.kind != SymType::Undefined)
            .map(|s| ListSymbol {
                name: s.name.clone(),
                value: s.value,
                bank: s.bank,
                // `.rs` reservations list as labels; `.enum` entries and plain
                // constants list as constants.
                label: matches!(s.kind, SymType::Label | SymType::Rs),
            })
            .collect()
    }

    /// Warnings collected during assembly (in source order), each with the
    /// reference-compatible message.
    pub fn take_warnings(&mut self) -> Vec<Diag> {
        std::mem::take(&mut self.warnings)
    }

    /// Run both passes over the parsed program, returning the output bytes
    /// (including the iNES header in NES mode) or the diagnostic to report.
    pub fn run(&mut self, lines: &[Line]) -> Result<Vec<u8>, Diag> {
        // Pass 1: build symbols, size the ROM.
        self.pass = 1;
        self.reset_state();
        self.run_pass(lines);
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
        if let Some(d) = self.errors.last() {
            return Err(d.clone());
        }

        Ok(self.build_output())
    }

    fn reset_state(&mut self) {
        for v in &mut self.prg_offsets {
            *v = 0;
        }
        for v in &mut self.chr_offsets {
            *v = 0;
        }
        self.prg_index = 0;
        self.chr_index = 0;
        self.segment_prg = true;
        self.ines = Ines::default();
        self.enum_active = false;
        self.enum_value = 0;
        self.enum_inc = 0;
        self.rsset = 0;
        self.if_stack.clear();
        self.offset_trainer = 0;
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
        let byte6 = (self.ines.mir & 0x01)
            | ((self.ines.bat & 0x01) << 1)
            | ((self.ines.trn & 0x01) << 2)
            | ((self.ines.fsc & 0x01) << 3)
            | ((self.ines.map & 0x0F) << 4);
        out.push((byte6 & 0xFF) as u8);
        if self.ines.nes2 {
            self.build_header_nes2(&mut out);
        } else {
            self.build_header_ines(&mut out);
        }
        // A trainer, when present, sits between the header and the PRG/CHR data.
        if self.ines.trn == 1 {
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
        for line in lines {
            if self.aborted {
                break;
            }
            self.cur_line = line.line;
            self.cur_file = line.file;
            self.exec_stmt(&line.stmt);
        }
    }

    // -- error helpers ------------------------------------------------------

    fn error(&mut self, message: impl Into<String>) {
        self.errors.push(Diag {
            file: self.file_name(),
            line: self.cur_line,
            message: message.into(),
        });
    }

    fn file_name(&self) -> String {
        self.files
            .get(self.cur_file as usize)
            .cloned()
            .unwrap_or_default()
    }

    fn hard_error(&mut self, message: impl Into<String>) {
        self.error(message);
        // In collect mode, keep going so more problems can be reported.
        if !self.collect_all {
            self.aborted = true;
        }
    }

    fn warning(&mut self, message: impl Into<String>) {
        self.warnings.push(Diag {
            file: self.file_name(),
            line: self.cur_line,
            message: message.into(),
        });
    }

    // -- symbol table -------------------------------------------------------

    fn find_symbol(&self, name: &str) -> Option<usize> {
        self.symbols.iter().position(|s| s.name == name)
    }

    fn add_symbol(&mut self, name: &str, value: i64, kind: SymType) {
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
        match existing {
            Some(id) => {
                self.symbols[id].value = value;
                self.symbols[id].bank = bank;
                self.symbols[id].kind = kind;
            }
            None => self.symbols.push(Symbol {
                name: name.to_string(),
                value,
                kind,
                bank,
            }),
        }
    }

    fn add_label(&mut self, name: &str) {
        let offset = self.address_offset();
        self.add_symbol(name, offset, SymType::Label);
    }

    fn get_symbol_local(&self, direction: i32) -> Option<usize> {
        use std::cmp::Ordering;
        let offset = self.address_offset();
        match direction.cmp(&0) {
            Ordering::Greater => {
                let mut remaining = direction;
                for (i, s) in self.symbols.iter().enumerate() {
                    if s.name == ":" && s.bank == self.prg_index && s.value > offset {
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
                    if s.name == ":" && s.bank == self.prg_index && s.value < offset {
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

    fn address_offset(&self) -> i64 {
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

        // While a trainer is active every emitted byte is redirected into the
        // 512-byte trainer region and does not advance the ROM counters.
        if self.ines.trn == 1 {
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

    // -- statement execution -----------------------------------------------

    fn exec_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Label(name) => self.add_label(name),
            Stmt::Constant(name, expr) => {
                let value = match expr {
                    Some(e) => self.eval(e),
                    None => 1,
                };
                self.add_symbol(name, value, SymType::Constant);
            }
            Stmt::Instruction(instr) => self.exec_instruction(instr),
            Stmt::Pseudo(p) => self.exec_pseudo(p),
        }
    }

    fn exec_pseudo(&mut self, p: &Pseudo) {
        match p {
            Pseudo::Org(e) => {
                let addr = self.eval(e);
                self.exec_org(addr);
            }
            Pseudo::Db(list) | Pseudo::Lobytes(list) => {
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
                    self.write_byte(((b as i64 + off) & 0xFF) as u8);
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
                    let idx = match_color(
                        ((color >> 16) & 0xFF) as u8,
                        ((color >> 8) & 0xFF) as u8,
                        (color & 0xFF) as u8,
                    );
                    self.write_byte(idx);
                }
            }
            Pseudo::Enum(v, inc) => {
                self.enum_active = true;
                self.enum_value = self.eval(v);
                self.enum_inc = match inc {
                    Some(e) => self.eval(e),
                    None => 1,
                };
            }
            Pseudo::Endenum => {
                self.enum_active = false;
                self.enum_value = 0;
                self.enum_inc = 0;
            }
            Pseudo::Rsset(e) => {
                self.rsset = self.eval(e);
            }
            Pseudo::Rs(label, size) => {
                let size = self.eval(size);
                if self.enum_active {
                    let value = self.enum_value;
                    self.add_symbol(label, value, SymType::Enum);
                    self.enum_value += size * self.enum_inc;
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
                self.segment_prg = true;
                self.prg_index = self.eval(e).max(0) as usize % MAX_BANKS;
            }
            Pseudo::Chr(e) => {
                self.segment_prg = false;
                self.chr_index = self.eval(e).max(0) as usize % MAX_BANKS;
            }
            Pseudo::InesTrn => {
                self.nes = true;
                self.ines.trn = 1;
            }
            Pseudo::If(e) => {
                let cond = self.eval(e);
                self.if_stack.push(cond != 0);
            }
            Pseudo::Ifdef(name) => {
                let defined = self.find_symbol(name).is_some();
                self.if_stack.push(defined);
            }
            Pseudo::Ifndef(name) => {
                let defined = self.find_symbol(name).is_some();
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
                if let Some(rest) = name.strip_prefix("PRG") {
                    self.segment_prg = true;
                    self.prg_index = rest.parse::<usize>().unwrap_or(0) % MAX_BANKS;
                } else if let Some(rest) = name.strip_prefix("CHR") {
                    self.segment_prg = false;
                    self.chr_index = rest.parse::<usize>().unwrap_or(0) % MAX_BANKS;
                }
            }
            Pseudo::Incbin(file, offset, limit) => {
                let off = offset.as_ref().map_or(0, |e| self.eval(e)).max(0) as usize;
                let lim = limit.as_ref().map(|e| self.eval(e).max(0) as usize);
                match self.read_media_file(file) {
                    Some(bytes) => {
                        let out = nessemble_media::incbin_slice(&bytes, off, lim);
                        self.write_all(&out);
                    }
                    None => self.hard_error(t!("could-not-read", file = file)),
                }
            }
            Pseudo::Incpng(file, offset, limit) => {
                let off = offset.as_ref().map_or(0, |e| self.eval(e)) as i32;
                let lim = limit.as_ref().map(|e| self.eval(e) as i32);
                match self.decode_media_png(file) {
                    Some(png) => {
                        let out = nessemble_media::png_to_tiles(&png, off, lim);
                        self.write_all(&out);
                    }
                    None => self.hard_error(t!("could-not-load-png")),
                }
            }
            Pseudo::Incpal(file) => match self.decode_media_png(file) {
                Some(png) => {
                    let out = nessemble_media::png_to_palette(&png);
                    self.write_all(&out);
                }
                None => self.hard_error(t!("could-not-load-png")),
            },
            Pseudo::Incrle(file) => match self.read_media_file(file) {
                Some(bytes) => {
                    let out = nessemble_media::rle_encode(&bytes);
                    self.write_all(&out);
                }
                None => self.hard_error(t!("could-not-read", file = file)),
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
            F::Mir => self.ines.mir = value,
            F::Bat => self.ines.bat = value,
            F::FourScreen => self.ines.fsc = value,
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
    fn exec_custom(&mut self, name: &str, args: &[CustomArg]) {
        let mut ints = Vec::new();
        let mut texts = Vec::new();
        for arg in args {
            match arg {
                CustomArg::Int(e) => ints.push(self.eval(e)),
                CustomArg::Str(s) => texts.push(s.clone()),
            }
        }
        match (self.custom)(name, &ints, &texts, self.cur_dir()) {
            Ok(bytes) => self.write_all(&bytes),
            Err(msg) => self.hard_error(msg),
        }
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
    /// contains the directive (see [`Self::cur_dir`]).
    fn read_media_file(&self, name: &str) -> Option<Vec<u8>> {
        std::fs::read(self.cur_dir().join(name)).ok()
    }

    /// Read and decode a media PNG (open failure and decode failure are
    /// indistinguishable, matching the reference's `stbi_load`).
    fn decode_media_png(&self, name: &str) -> Option<nessemble_media::Png> {
        let bytes = self.read_media_file(name)?;
        nessemble_media::decode_png(&bytes).ok()
    }

    fn exec_incwav(&mut self, file: &str, amplitude: i32) {
        let Some(bytes) = self.read_media_file(file) else {
            self.hard_error(t!("could-not-open", file = file));
            return;
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
                    self.write_byte(op);
                    self.write_byte((value & 0xFF) as u8);
                }
            }
            Operand::Indirect(e) => {
                let value = self.eval(&e);
                if let Some(op) = self.resolve_opcode(&mnem, AddressingMode::Indirect) {
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
                    self.write_byte(op);
                    self.write_byte((value & 0xFF) as u8);
                }
            }
            Operand::Absolute(e) => {
                let value = self.eval(&e);
                if let Some(op) = self.get_opcode(&mnem, AddressingMode::Absolute) {
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
            Expr::Binary(a, op, b) => {
                let lhs = self.eval(a);
                let rhs = self.eval(b);
                Self::apply(lhs, *op, rhs)
            }
        }
    }

    fn eval_symbol(&mut self, name: &str) -> i64 {
        if let Some(id) = self.find_symbol(name) {
            if self.pass == 2 && self.symbols[id].kind == SymType::Undefined {
                let msg = t!("symbol-not-defined", name = self.symbols[id].name);
                self.error(msg);
            }
            self.symbols[id].value
        } else {
            // Reference behavior: unknown symbols are registered as
            // undefined (value 1) during pass 1.
            self.add_symbol(name, 1, SymType::Undefined);
            1
        }
    }

    fn eval_local(&mut self, direction: i32) -> i64 {
        if self.pass != 2 {
            return 1;
        }
        match self.get_symbol_local(direction) {
            Some(id) => self.symbols[id].value,
            None => 1,
        }
    }

    fn eval_bank(&mut self, name: &str) -> i64 {
        match self.find_symbol(name) {
            Some(id) => self.symbols[id].bank as i64,
            None => 1,
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

/// CRC-32 (poly 0xEDB88320), matching the reference `crc_32`.
fn crc_32(data: &[u8]) -> u32 {
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

/// The 64-entry NES palette (RGB) used by `.color`, from the reference tables.
const COLORS_FULL: [u32; 64] = [
    0x7C7C7C, 0x0000FC, 0x0000BC, 0x4428BC, 0x940084, 0xA80020, 0xA81000, 0x881400, 0x503000,
    0x007800, 0x006800, 0x005800, 0x004058, 0x000000, 0x000000, 0x000000, 0xBCBCBC, 0x0078F8,
    0x0058F8, 0x6844FC, 0xD800CC, 0xE40058, 0xF83800, 0xE45C10, 0xAC7C00, 0x00B800, 0x00A800,
    0x00A844, 0x008888, 0x000000, 0x000000, 0x000000, 0xF8F8F8, 0x3CBCFC, 0x6888FC, 0x9878F8,
    0xF878F8, 0xF85898, 0xF87858, 0xFCA044, 0xF8B800, 0xB8F818, 0x58D854, 0x58F898, 0x00E8D8,
    0x787878, 0x000000, 0x000000, 0xFCFCFC, 0xA4E4FC, 0xB8B8F8, 0xD8B8F8, 0xF8B8F8, 0xF8A4C0,
    0xF0D0B0, 0xFCE0A8, 0xF8D878, 0xD8F878, 0xB8F8B8, 0xB8F8D8, 0x00FCFC, 0xF8D8F8, 0x000000,
    0x000000,
];

/// Find the nearest NES palette index for an RGB triple, matching the
/// reference `match_color` (Euclidean distance, truncated to int, first match;
/// index 0x0D is remapped to 0x0F).
fn match_color(r1: u8, g1: u8, b1: u8) -> u8 {
    let mut diff: i32 = 0xFFFFFF;
    let mut color: usize = 0;
    for (i, rgb) in COLORS_FULL.iter().enumerate() {
        let r2 = ((rgb >> 16) & 0xFF) as i32;
        let g2 = ((rgb >> 8) & 0xFF) as i32;
        let b2 = (rgb & 0xFF) as i32;
        let dr = (r2 - r1 as i32) as f64;
        let dg = (g2 - g1 as i32) as f64;
        let db = (b2 - b1 as i32) as f64;
        let next_diff = (dr * dr + dg * dg + db * db).sqrt() as i32;
        if next_diff < diff {
            diff = next_diff;
            color = i;
        }
    }
    if color == 0x0D {
        0x0F
    } else {
        color as u8
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
