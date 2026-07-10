//! Two-pass assembler mirroring the reference `nessemble` semantics for the
//! Phase 2 (non-iNES-output) subset: instructions, symbols, expressions,
//! `.org`, data directives, and error reporting.
//!
//! Full iNES ROM output (header, banking, CHR/trainer padding) is Phase 3;
//! here we set the iNES-related state (so address math and `.org` validation
//! match) but emit the raw written region.

use nessemble_isa::{AddressingMode, Opcode, META_UNDOCUMENTED, OPCODES};

use crate::ast::*;

const BANK_PRG: i64 = 0x4000;
const BANK_CHR: i64 = 0x2000;
const MAX_BANKS: usize = 256;

/// A diagnostic (error) with a 1-based source line and message, matching the
/// reference tool's wording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diag {
    pub line: u32,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SymType {
    Undefined,
    Label,
    Constant,
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
    // `chr`, `map`, `mir`, and `trn` are populated by the iNES directives now
    // but only consumed by the full iNES-header output path (Phase 3).
    #[allow(dead_code)]
    chr: i64,
    #[allow(dead_code)]
    map: i64,
    #[allow(dead_code)]
    mir: i64,
    prg: i64,
    #[allow(dead_code)]
    trn: i64,
}

impl Default for Ines {
    fn default() -> Self {
        // Matches the reference initializer { chr:1, map:0, mir:0, prg:1, trn:0 }.
        Ines {
            chr: 1,
            map: 0,
            mir: 0,
            prg: 1,
            trn: 0,
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
    offset_max: usize,

    symbols: Vec<Symbol>,

    pass: u8,
    errors: Vec<Diag>,
    aborted: bool,
    cur_line: u32,
}

impl Assembler {
    pub fn new(nes: bool, undocumented: bool, empty_byte: u8) -> Self {
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
            offset_max: 0,
            symbols: Vec::new(),
            pass: 1,
            errors: Vec::new(),
            aborted: false,
            cur_line: 1,
        }
    }

    /// Run both passes over the parsed program, returning ROM bytes or the
    /// diagnostic that should be reported.
    pub fn run(&mut self, lines: &[Line]) -> Result<Vec<u8>, Diag> {
        // Pass 1: build symbols, size the ROM.
        self.pass = 1;
        self.reset_location();
        self.ines = Ines::default();
        self.run_pass(lines);
        if let Some(d) = self.errors.last() {
            return Err(d.clone());
        }

        // Allocate ROM and run pass 2 to emit.
        self.rom = vec![self.empty_byte; self.offset_max];
        self.pass = 2;
        self.aborted = false;
        self.reset_location();
        self.ines = Ines::default();
        self.run_pass(lines);
        if let Some(d) = self.errors.last() {
            return Err(d.clone());
        }

        Ok(self.rom.clone())
    }

    fn reset_location(&mut self) {
        for v in self.prg_offsets.iter_mut() {
            *v = 0;
        }
        for v in self.chr_offsets.iter_mut() {
            *v = 0;
        }
        self.prg_index = 0;
        self.chr_index = 0;
        self.segment_prg = true;
    }

    fn run_pass(&mut self, lines: &[Line]) {
        for line in lines {
            if self.aborted {
                break;
            }
            self.cur_line = line.line;
            self.exec_stmt(&line.stmt);
        }
    }

    // -- error helpers ------------------------------------------------------

    fn error(&mut self, message: impl Into<String>) {
        self.errors.push(Diag {
            line: self.cur_line,
            message: message.into(),
        });
    }

    fn hard_error(&mut self, message: impl Into<String>) {
        self.error(message);
        self.aborted = true;
    }

    // -- symbol table -------------------------------------------------------

    fn find_symbol(&self, name: &str) -> Option<usize> {
        self.symbols.iter().position(|s| s.name == name)
    }

    fn add_symbol(&mut self, name: &str, value: i64, kind: SymType) {
        if self.pass != 1 {
            return;
        }
        let bank = self.prg_index;
        let existing = if name != ":" {
            self.find_symbol(name)
        } else {
            None
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
        let offset = self.rom_index();
        if offset + 1 > self.offset_max {
            self.offset_max = offset + 1;
        }
        if self.pass == 2 && offset < self.rom.len() {
            self.rom[offset] = byte;
        }
        if self.segment_prg {
            self.prg_offsets[self.prg_index] += 1;
        } else {
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
                    self.hard_error("Not enough .fill arguments");
                    return;
                }
                let count = vals[0];
                let value = if vals.len() < 2 { 0xFF } else { vals[1] };
                for _ in 0..count.max(0) {
                    self.write_byte((value & 0xFF) as u8);
                }
            }
            Pseudo::InesPrg(e) => {
                self.nes = true;
                self.ines.prg = self.eval(e);
            }
            Pseudo::InesChr(e) => {
                self.nes = true;
                self.ines.chr = self.eval(e);
            }
            Pseudo::InesMap(e) => {
                self.nes = true;
                self.ines.map = self.eval(e);
            }
            Pseudo::InesMir(e) => {
                self.nes = true;
                self.ines.mir = self.eval(e);
            }
            Pseudo::Prg(e) => {
                self.segment_prg = true;
                self.prg_index = self.eval(e).max(0) as usize % MAX_BANKS;
            }
            Pseudo::Chr(e) => {
                self.segment_prg = false;
                self.chr_index = self.eval(e).max(0) as usize % MAX_BANKS;
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
            Pseudo::Unsupported(name) => {
                self.hard_error(format!(
                    "Unsupported directive `.{name}` (not yet implemented)"
                ));
            }
        }
    }

    fn exec_org(&mut self, address: i64) {
        if self.segment_prg {
            if self.ines.prg < 2 {
                if address < 0xC000 {
                    self.hard_error(format!(
                        "Start address for PRG bank {} is 0xC000",
                        self.prg_index
                    ));
                    return;
                }
                if address >= 0xC000 + BANK_PRG {
                    self.hard_error("Address too high");
                    return;
                }
                self.prg_offsets[self.prg_index] = address - 0xC000;
            } else {
                if self.prg_index % 2 == 0 {
                    if address < 0x8000 {
                        self.hard_error(format!(
                            "Start address for PRG bank {} is 0x8000",
                            self.prg_index
                        ));
                        return;
                    }
                    if address >= 0x8000 + BANK_PRG {
                        self.hard_error("Address too high");
                        return;
                    }
                } else {
                    if address < 0xC000 {
                        self.hard_error(format!(
                            "Start address for PRG bank {} is 0xC000",
                            self.prg_index
                        ));
                        return;
                    }
                    if address >= 0xC000 + BANK_PRG {
                        self.hard_error("Address too high");
                        return;
                    }
                }
                self.prg_offsets[self.prg_index] =
                    address - 0x8000 - ((self.prg_index as i64) % 2) * BANK_PRG;
            }
        } else {
            if address >= BANK_CHR {
                self.hard_error("Address too high");
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
            self.error("Invalid addressing mode");
        } else {
            self.error(format!("Unknown opcode `{mnemonic}`"));
        }
        exists
    }

    fn register_exists(&mut self, reg: char, allowed: &str) -> bool {
        if allowed.contains(reg) {
            true
        } else {
            self.error(format!("Unknown register `{reg}`"));
            false
        }
    }

    fn opcode_byte(idx: Option<&Opcode>) -> u8 {
        match idx {
            Some(o) => o.opcode,
            None => 0xFF, // matches C's (unsigned int)(-1) low byte
        }
    }

    fn exec_instruction(&mut self, instr: &Instruction) {
        let mnem = instr.mnemonic.clone();
        match instr.operand.clone() {
            Operand::Implied => {
                let op = self.get_opcode(&mnem, AddressingMode::Implied);
                if op.is_none() && !self.mnemonic_exists(&mnem) {
                    return;
                }
                if op.is_none() {
                    // exists but wrong mode: error already recorded; do not emit.
                    return;
                }
                self.write_byte(Self::opcode_byte(op));
            }
            Operand::Accumulator(reg) => {
                let op = self.get_opcode(&mnem, AddressingMode::Accumulator);
                if !self.register_exists(reg, "A") {
                    return;
                }
                if op.is_none() && !self.mnemonic_exists(&mnem) {
                    return;
                }
                if op.is_none() {
                    return;
                }
                self.write_byte(Self::opcode_byte(op));
            }
            Operand::Immediate(e) => {
                let value = self.eval(&e);
                let op = self.get_opcode(&mnem, AddressingMode::Immediate);
                if op.is_none() && !self.mnemonic_exists(&mnem) {
                    return;
                }
                if op.is_none() {
                    return;
                }
                self.write_byte(Self::opcode_byte(op));
                self.write_byte((value & 0xFF) as u8);
            }
            Operand::Indirect(e) => {
                let value = self.eval(&e);
                let op = self.get_opcode(&mnem, AddressingMode::Indirect);
                if op.is_none() && !self.mnemonic_exists(&mnem) {
                    return;
                }
                if op.is_none() {
                    return;
                }
                self.write_byte(Self::opcode_byte(op));
                self.write_byte((value & 0xFF) as u8);
                self.write_byte(((value >> 8) & 0xFF) as u8);
            }
            Operand::IndirectIndexed(e, reg) => {
                let value = self.eval(&e);
                if !self.register_exists(reg, "XY") {
                    return;
                }
                let mode = if reg == 'X' {
                    AddressingMode::IndirectX
                } else {
                    AddressingMode::IndirectY
                };
                let op = self.get_opcode(&mnem, mode);
                if op.is_none() && !self.mnemonic_exists(&mnem) {
                    return;
                }
                if op.is_none() {
                    return;
                }
                self.write_byte(Self::opcode_byte(op));
                self.write_byte((value & 0xFF) as u8);
            }
            Operand::ZeroPage(e) => {
                let value = self.eval(&e);
                let op = self.get_opcode(&mnem, AddressingMode::ZeroPage);
                // Mirrors reference: zeropage emits without an existence check.
                self.write_byte(Self::opcode_byte(op));
                self.write_byte((value & 0xFF) as u8);
            }
            Operand::ZeroPageIndexed(e, reg) => {
                let value = self.eval(&e);
                if !self.register_exists(reg, "XY") {
                    return;
                }
                let mode = if reg == 'X' {
                    AddressingMode::ZeroPageX
                } else {
                    AddressingMode::ZeroPageY
                };
                let op = self.get_opcode(&mnem, mode);
                if op.is_none() && !self.mnemonic_exists(&mnem) {
                    return;
                }
                if op.is_none() {
                    return;
                }
                self.write_byte(Self::opcode_byte(op));
                self.write_byte((value & 0xFF) as u8);
            }
            Operand::Absolute(e) => {
                let value = self.eval(&e);
                let op = self.get_opcode(&mnem, AddressingMode::Absolute);
                if op.is_none() {
                    if self.get_opcode(&mnem, AddressingMode::Relative).is_some() {
                        self.emit_relative(&mnem, value);
                        return;
                    }
                    if !self.mnemonic_exists(&mnem) {
                        return;
                    }
                    return;
                }
                self.write_byte(Self::opcode_byte(op));
                self.write_byte((value & 0xFF) as u8);
                self.write_byte(((value >> 8) & 0xFF) as u8);
            }
            Operand::AbsoluteIndexed(e, reg) => {
                let value = self.eval(&e);
                if !self.register_exists(reg, "XY") {
                    return;
                }
                let mode = if reg == 'X' {
                    AddressingMode::AbsoluteX
                } else {
                    AddressingMode::AbsoluteY
                };
                let op = self.get_opcode(&mnem, mode);
                if op.is_none() && !self.mnemonic_exists(&mnem) {
                    return;
                }
                if op.is_none() {
                    return;
                }
                self.write_byte(Self::opcode_byte(op));
                self.write_byte((value & 0xFF) as u8);
                self.write_byte(((value >> 8) & 0xFF) as u8);
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
                self.error("Branch address out of range");
            }
        } else {
            address = target - offset - 1;
            if self.pass == 2 && address >= 0x80 {
                self.error("Branch address out of range");
            }
        }
        address &= 0xFF;
        self.write_byte(Self::opcode_byte(op));
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
        match self.find_symbol(name) {
            Some(id) => {
                if self.pass == 2 && self.symbols[id].kind == SymType::Undefined {
                    let msg = format!("Symbol `{}` was not defined", self.symbols[id].name);
                    self.error(msg);
                }
                self.symbols[id].value
            }
            None => {
                // Reference behavior: unknown symbols are registered as
                // undefined (value 1) during pass 1.
                self.add_symbol(name, 1, SymType::Undefined);
                1
            }
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
