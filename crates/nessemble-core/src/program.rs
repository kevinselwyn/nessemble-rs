//! What an assembly emitted, recorded for whole-program analysis
//! (`plans/023-whole-program-clobber-analysis.md` §2.2).
//!
//! Recording is off by default and switched on by [`crate::Options::record_program`].
//! It reads assembler state only — the emitted bytes are identical with it on
//! or off — and is collected on **pass 2**, when every address is final.
//!
//! The walk in [`crate::clobbers`] identifies a position by its **ROM offset**
//! (the header-less PRG/CHR layout, the same coordinate space as
//! [`crate::SourceSpan`] and a CDL file), never by a bare CPU address: the same
//! address can be assembled in several banks, or twice in one bank under
//! overlapping `.phase` blocks.

use std::collections::HashMap;
use std::path::PathBuf;

/// Where a recorded statement was written: **the author's line**. For a
/// `.macro` expansion this is the outermost call site; for code from a custom
/// pseudo-op's `emit_source` it is the pseudo-op's own line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Site {
    /// Index into [`ProgramMap::files`].
    pub file: u32,
    /// 1-based line.
    pub line: u32,
}

/// What produced a statement's bytes, when it was not written out in the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Via {
    /// Expanded from `.macro NAME` (the outermost expansion).
    Macro(String),
    /// Emitted by the custom pseudo-op `.NAME`'s `emit_source`.
    Pseudo(String),
}

/// A control transfer's target as written: the operand value and, when the
/// operand names a label, where that label was defined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    /// The operand value (the branch destination for a relative branch).
    pub addr: u16,
    /// `(rom_offset, delta)`: the ROM offset the operand's label was defined
    /// at, and `operand - label value`. Pins the target to one ROM location.
    pub anchor: Option<(usize, i64)>,
}

/// What kind of statement an [`Emit`] is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmitKind {
    /// An instruction. `target` is set for `JMP abs`, `JSR` and branches.
    Instruction { target: Option<Target> },
    /// Data: `.db`, `.dw`, `.incbin`, `.fill`, …, or a custom pseudo-op's
    /// bytes (named `.foo`).
    Data { directive: String },
}

/// One executed statement that wrote at least one PRG byte on pass 2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Emit {
    /// Header-less ROM offset of the first byte.
    pub rom_offset: usize,
    /// Bytes written.
    pub len: usize,
    /// The PRG bank (`.prg` index) at emission.
    pub bank: u16,
    /// The (phased) CPU address of the first byte: the address a label here
    /// would receive.
    pub cpu_addr: u16,
    /// The author's line.
    pub site: Site,
    /// What produced the bytes, if not the line itself.
    pub via: Option<Via>,
    pub kind: EmitKind,
}

/// One label definition executed on pass 2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelDef {
    /// The symbol key (`p1@loop` for a local).
    pub name: String,
    pub cpu_addr: u16,
    pub bank: u16,
    /// The load position at definition.
    pub rom_offset: usize,
    pub site: Site,
    /// The line came from a macro expansion or emitted source.
    pub in_expansion: bool,
    /// An `@name` local.
    pub local: bool,
}

/// A custom pseudo-op the editor did not run (its bytes are not known).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unmodelled {
    pub site: Site,
    pub rom_offset: usize,
    pub cpu_addr: u16,
    pub bank: u16,
}

/// Everything the analysis needs from one assembled program.
#[derive(Debug, Clone, Default)]
pub struct ProgramMap {
    pub emits: Vec<Emit>,
    pub labels: Vec<LabelDef>,
    pub unmodelled: Vec<Unmodelled>,
    /// Resolved path of each source file, indexed by [`Site::file`].
    pub files: Vec<PathBuf>,
    /// The header-less ROM image.
    pub image: Vec<u8>,
    /// Which bytes of `image` were written.
    pub written: Vec<bool>,
    /// The program was assembled with `-u`.
    pub undocumented: bool,
    /// Per ROM byte, the index of the emit that last wrote it (`u32::MAX`
    /// for none). Built by [`ProgramMap::index`].
    owner: Vec<u32>,
    /// CPU address → every `(bank, rom_offset)` holding an emitted byte there.
    by_addr: HashMap<u16, Vec<(u16, usize)>>,
}

/// How a CPU address resolves to a ROM location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Exactly one location.
    At(usize),
    /// Several banks (or overlapping blocks in one bank) assemble code there.
    Ambiguous(Vec<u16>),
    /// Nothing was assembled there.
    Nothing,
}

impl ProgramMap {
    /// Build the lookup tables. Called once the emits are final.
    pub(crate) fn index(&mut self) {
        self.owner = vec![u32::MAX; self.image.len()];
        for (i, e) in self.emits.iter().enumerate() {
            for off in e.rom_offset..(e.rom_offset + e.len).min(self.image.len()) {
                self.owner[off] = i as u32;
            }
        }
        self.by_addr.clear();
        for (off, &owner) in self.owner.iter().enumerate() {
            if owner == u32::MAX {
                continue;
            }
            let e = &self.emits[owner as usize];
            let cpu = e.cpu_addr.wrapping_add((off - e.rom_offset) as u16);
            self.by_addr.entry(cpu).or_default().push((e.bank, off));
        }
    }

    /// The emit that holds the byte at `off`, if any.
    #[must_use]
    pub fn emit_at(&self, off: usize) -> Option<&Emit> {
        let i = *self.owner.get(off)?;
        (i != u32::MAX).then(|| &self.emits[i as usize])
    }

    /// The CPU address of the byte at `off` (as the emit holding it placed it).
    #[must_use]
    pub fn cpu_at(&self, off: usize) -> Option<u16> {
        let e = self.emit_at(off)?;
        Some(e.cpu_addr.wrapping_add((off - e.rom_offset) as u16))
    }

    /// The bank of the byte at `off`.
    #[must_use]
    pub fn bank_at(&self, off: usize) -> Option<u16> {
        self.emit_at(off).map(|e| e.bank)
    }

    /// Resolve a transfer to `target` from a position in `bank` (§2.2):
    /// anchored when the operand named a label, otherwise by address.
    #[must_use]
    pub fn resolve(&self, target: &Target, bank: u16) -> Resolution {
        if let Some((label_off, delta)) = target.anchor {
            let off = label_off as i64 + delta;
            if off >= 0 {
                let off = off as usize;
                if self.cpu_at(off) == Some(target.addr) {
                    return Resolution::At(off);
                }
            }
        }
        self.resolve_addr(target.addr, bank)
    }

    /// Resolve a bare CPU address: the emitted byte there in `bank`, else the
    /// one bank that has one.
    #[must_use]
    pub fn resolve_addr(&self, addr: u16, bank: u16) -> Resolution {
        let Some(places) = self.by_addr.get(&addr) else {
            return Resolution::Nothing;
        };
        let here: Vec<usize> = places
            .iter()
            .filter(|(b, _)| *b == bank)
            .map(|&(_, o)| o)
            .collect();
        match here.len() {
            1 => return Resolution::At(here[0]),
            0 => {}
            _ => return Resolution::Ambiguous(vec![bank]),
        }
        if places.len() == 1 {
            return Resolution::At(places[0].1);
        }
        let mut banks: Vec<u16> = places.iter().map(|&(b, _)| b).collect();
        banks.sort_unstable();
        banks.dedup();
        Resolution::Ambiguous(banks)
    }

    /// Where execution continues after the `len`-byte instruction at `off`:
    /// the next ROM byte if it continues the CPU run, otherwise the address
    /// after the instruction resolved like an unanchored target.
    #[must_use]
    pub fn fall_through(&self, off: usize, len: usize) -> Resolution {
        let (Some(cpu), Some(bank)) = (self.cpu_at(off), self.bank_at(off)) else {
            return Resolution::Nothing;
        };
        let next_cpu = cpu.wrapping_add(len as u16);
        let next = off + len;
        if self.cpu_at(next) == Some(next_cpu) && self.bank_at(next) == Some(bank) {
            return Resolution::At(next);
        }
        self.resolve_addr(next_cpu, bank)
    }

    /// The label definitions at ROM offset `off`.
    pub fn labels_at(&self, off: usize) -> impl Iterator<Item = &LabelDef> {
        self.labels.iter().filter(move |l| l.rom_offset == off)
    }
}

/// The statement being recorded right now (see [`ProgramBuilder`]).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Piece {
    pub rom_offset: usize,
    pub len: usize,
    pub bank: u16,
    pub cpu_addr: u16,
}

/// The assembler's in-progress recording (pass 2 only).
#[derive(Debug, Default)]
pub(crate) struct ProgramBuilder {
    pub emits: Vec<Emit>,
    /// Per emit, the anchoring label's `(rom_offset, value)` from the operand,
    /// turned into a [`Target`] once the image is final.
    pub anchors: Vec<Option<(usize, i64)>>,
    pub labels: Vec<LabelDef>,
    pub unmodelled: Vec<Unmodelled>,
    /// Bytes written by the statement executing now.
    pub cur: Vec<Piece>,
}

impl ProgramBuilder {
    /// Note one written byte for the current statement.
    pub fn byte(&mut self, rom_offset: usize, bank: u16, cpu_addr: u16) {
        if let Some(last) = self.cur.last_mut() {
            if last.rom_offset + last.len == rom_offset && last.bank == bank {
                last.len += 1;
                return;
            }
        }
        self.cur.push(Piece {
            rom_offset,
            len: 1,
            bank,
            cpu_addr,
        });
    }

    pub fn clear(&mut self) {
        *self = ProgramBuilder::default();
    }

    /// Turn the recording into a [`ProgramMap`] over the final `image`.
    pub fn finish(
        mut self,
        image: Vec<u8>,
        written: Vec<bool>,
        files: Vec<PathBuf>,
        undocumented: bool,
    ) -> ProgramMap {
        for (emit, anchor) in self.emits.iter_mut().zip(&self.anchors) {
            let decoded = decode_target(&image, emit);
            if let EmitKind::Instruction { target } = &mut emit.kind {
                *target = decoded.map(|addr| Target {
                    addr,
                    anchor: anchor.map(|(off, value)| (off, i64::from(addr) - value)),
                });
            }
        }
        let mut map = ProgramMap {
            emits: self.emits,
            labels: self.labels,
            unmodelled: self.unmodelled,
            files,
            image,
            written,
            undocumented,
            owner: Vec::new(),
            by_addr: HashMap::new(),
        };
        map.index();
        map
    }
}

/// The target address of a `JMP abs`, `JSR` or branch emit, from its bytes.
fn decode_target(image: &[u8], emit: &Emit) -> Option<u16> {
    let op = *image.get(emit.rom_offset)?;
    let lo = image.get(emit.rom_offset + 1).copied();
    let hi = image.get(emit.rom_offset + 2).copied();
    match op {
        0x4C | 0x20 => Some(u16::from_le_bytes([lo?, hi?])),
        _ if is_branch(op) => {
            let rel = lo? as i8;
            Some(
                emit.cpu_addr
                    .wrapping_add(2)
                    .wrapping_add(rel as i16 as u16),
            )
        }
        _ => None,
    }
}

/// Whether `op` is one of the eight conditional branches.
#[must_use]
pub fn is_branch(op: u8) -> bool {
    matches!(op, 0x10 | 0x30 | 0x50 | 0x70 | 0x90 | 0xB0 | 0xD0 | 0xF0)
}
