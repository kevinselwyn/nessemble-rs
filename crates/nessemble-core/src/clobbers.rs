//! Whole-program clobber analysis
//! (`plans/023-whole-program-clobber-analysis.md` §2.3–2.5).
//!
//! A routine's body is the code reachable from its label in the assembled
//! program — not the text block under it. Each routine is walked from its
//! label's ROM offset: through both arms of a branch, fall-through, `JMP` and
//! `JSR`, across files and banks. The walk trusts each callee's contract,
//! absorbs an uncontracted callee's *summary* (a monotone fixpoint over the
//! call graph), pairs push/pull and store/reload saves, and reports what it
//! cannot follow as an unverifiable [`Reason`].
//!
//! The walk is path-sensitive over [`State`]s, de-duplicated by their
//! analysis-relevant parts, so a loop terminates and a save that is restored on
//! one path but not another is reported on the path that loses it.

// Registers are indexed A, X, Y, S throughout, into several parallel arrays.
#![allow(clippy::needless_range_loop)]

use std::collections::{BTreeSet, HashMap, HashSet};

use nessemble_isa::{by_byte, RegSet};

use crate::program::{is_branch, EmitKind, ProgramMap, Resolution, Site, Target, Via};

/// The four registers the verifier tracks, as indexes.
const REGS: [RegSet; 4] = [RegSet::A, RegSet::X, RegSet::Y, RegSet::S];
const A: usize = 0;
const X: usize = 1;
const Y: usize = 2;
const S: usize = 3;

/// Give up on a routine after this many distinct states (it is then
/// unverifiable rather than slow).
const MAX_STATES: usize = 200_000;

/// A routine contract the walk trusts: a label whose signature declares a
/// clobber list.
#[derive(Debug, Clone)]
pub struct Contract {
    pub name: String,
    /// The registers it declares written: clobbers plus returns.
    pub writes: RegSet,
    /// The memory it declares clobbered, as inclusive address ranges
    /// (symbols already resolved). Releases a caller's memory save.
    pub memory: Vec<(u16, u16)>,
    /// The label's site.
    pub site: Site,
}

/// The contracts of one program, by the ROM offset of their label.
#[derive(Debug, Clone, Default)]
pub struct Contracts {
    by_offset: HashMap<usize, Vec<Contract>>,
}

impl Contracts {
    pub fn insert(&mut self, rom_offset: usize, contract: Contract) {
        self.by_offset.entry(rom_offset).or_default().push(contract);
    }

    /// The contracts at `off` (stacked labels each contribute theirs).
    #[must_use]
    pub fn at(&self, off: usize) -> &[Contract] {
        self.by_offset.get(&off).map_or(&[], Vec::as_slice)
    }
}

/// How a control transfer was written, for messages: `JSR helper` on a line.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Step {
    /// The instruction as written (`JSR helper`).
    pub text: String,
    pub site: Site,
}

/// What wrote a register.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum OriginBase {
    /// An instruction.
    Instr {
        mnemonic: &'static str,
        site: Site,
        via: Option<Via>,
        /// The data directive, when the instruction was hand-encoded.
        data: Option<String>,
    },
    /// A trusted contract, reached by `how` (`None`: falling through into it).
    Contract {
        how: Option<Step>,
        name: String,
        /// The contracted label's site.
        site: Site,
    },
}

/// Where a register's first write on some path came from.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct WriteOrigin {
    pub base: OriginBase,
    /// The outermost `JSR` into an uncontracted callee it was reached through.
    pub through: Option<Step>,
}

/// Why a path could not be followed.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ReasonKind {
    /// `JMP [ptr]`.
    IndirectJmp(Step),
    /// `RTS`/`RTI` with values the routine pushed still on the stack.
    ReturnThroughPushed(Step),
    /// `RTS`/`RTI` after a `TXS` on the path.
    ReturnAfterTxs { ret: Step, txs: Site },
    /// `JSR callee` into a routine that pulls its own return address.
    PullsReturn { call: Step, callee: String },
    /// The routine itself pulls its own return address.
    SelfPullsReturn(Step),
    /// A transfer to an address several banks assemble code at.
    Ambiguous {
        from: Step,
        addr: u16,
        banks: Vec<u16>,
    },
    /// A transfer to an address where nothing was assembled.
    Nothing { from: Step, addr: u16 },
    /// A byte that is not a known instruction.
    UnknownOpcode { site: Site, byte: u8, data: bool },
    /// A `JSR` whose return site is data (inline arguments after the call).
    ReturnsIntoData { call: Step, data: Site },
    /// The path ran past the last byte assembled.
    RunsPast { bank: u16, after: Option<Site> },
    /// A custom pseudo-op the editor did not run (LSP only): suppresses
    /// over-declaration and judging, but is not reported.
    EditorNotRun(Site),
    /// The routine has too many paths to follow.
    TooComplex,
}

/// One unverifiable reason, and the outermost `JSR` it was found through.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Reason {
    pub kind: ReasonKind,
    pub through: Option<Step>,
}

/// A register saved and restored on a path (for `overdeclared-clobber`'s
/// "it is saved on line 3 and restored on line 18").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Preserved {
    pub reg: usize,
    pub saved: Site,
    pub restored: Site,
}

/// What a walk found: a routine's effect, or an uncontracted callee's summary.
#[derive(Debug, Clone, Default)]
pub struct RoutineEffect {
    /// Registers written on some path.
    pub writes: RegSet,
    /// The first write found of each register (indexed A, X, Y, S).
    pub origins: [Option<WriteOrigin>; 4],
    /// Why some path could not be followed, first-seen order, no repeats.
    pub reasons: Vec<Reason>,
    /// A `PLA`/`PLP` at depth 0, or a `TXS`: the code pulls its own return
    /// address.
    pub pulls_return: Option<Step>,
    /// Non-indexed zero-page/absolute addresses stored to (releases a caller's
    /// memory save).
    pub stores: BTreeSet<u16>,
    /// Saves restored before they were needed.
    pub preserved: Vec<Preserved>,
}

impl RoutineEffect {
    fn commit(&mut self, reg: usize, origin: &WriteOrigin) {
        if !self.writes.contains(REGS[reg]) {
            self.writes = self.writes.union(REGS[reg]);
            self.origins[reg] = Some(origin.clone());
        }
    }

    fn reason(&mut self, reason: Reason) {
        if !self.reasons.contains(&reason) {
            self.reasons.push(reason);
        }
    }

    /// Fold `other` into `self` (a may-analysis: everything unions).
    pub fn join(&mut self, other: &RoutineEffect) {
        for reg in 0..4 {
            if let Some(o) = &other.origins[reg] {
                self.commit(reg, o);
            }
        }
        self.writes = self.writes.union(other.writes);
        for r in &other.reasons {
            self.reason(r.clone());
        }
        if self.pulls_return.is_none() {
            self.pulls_return.clone_from(&other.pulls_return);
        }
        self.stores.extend(other.stores.iter().copied());
        for p in &other.preserved {
            if !self.preserved.contains(p) {
                self.preserved.push(*p);
            }
        }
    }

    /// The parts that decide a caller's result (origins excluded, so the
    /// fixpoint compares like with like).
    fn shape(&self) -> (u8, usize, bool, usize) {
        (
            self.writes.bits(),
            self.reasons.len(),
            self.pulls_return.is_some(),
            self.stores.len(),
        )
    }

    /// Whether some path could not be followed, other than for the editor
    /// not having run a pseudo-op.
    #[must_use]
    pub fn unverifiable(&self) -> bool {
        self.reasons
            .iter()
            .any(|r| !matches!(r.kind, ReasonKind::EditorNotRun(_)))
    }

    /// Whether the editor did not run a pseudo-op this routine reaches.
    #[must_use]
    pub fn editor_not_run(&self) -> bool {
        self.reasons
            .iter()
            .any(|r| matches!(r.kind, ReasonKind::EditorNotRun(_)))
    }
}

/// What a save holds a register in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Holder {
    /// The stack (`PHA`/`PHP`).
    Stack,
    /// Memory (`STX tmp`).
    Memory(u16),
}

/// One open save on a path.
#[derive(Debug, Clone)]
struct Save {
    holder: Holder,
    /// A, X or Y; `None` for pushed flags (`PHP`).
    reg: Option<usize>,
    site: Site,
    /// The first write to `reg` made while this save was open.
    pending: Option<WriteOrigin>,
}

/// One position of a path through a routine.
#[derive(Debug, Clone)]
struct State {
    off: usize,
    saves: Vec<Save>,
    /// The previous instruction on this path (opcode, first operand byte).
    prev: Option<(u8, u8)>,
    /// After a `PLA` that restores X/Y, the matching `TAX`/`TAY` is not a write.
    transfer_restores: Option<usize>,
    /// A `TXS` on this path.
    txs: Option<Site>,
    /// How this position was reached (`None`: falling through).
    arrived: Option<Step>,
    /// The site of the previous instruction.
    last: Option<Site>,
}

type Key = (
    usize,
    Vec<(Holder, Option<usize>, bool)>,
    Option<(u8, u8)>,
    Option<usize>,
    bool,
);

impl State {
    fn key(&self) -> Key {
        (
            self.off,
            self.saves
                .iter()
                .map(|s| (s.holder, s.reg, s.pending.is_some()))
                .collect(),
            self.prev,
            self.transfer_restores,
            self.txs.is_some(),
        )
    }

    /// The innermost open save of `reg`.
    fn save_of(&mut self, reg: usize) -> Option<&mut Save> {
        self.saves.iter_mut().rev().find(|s| s.reg == Some(reg))
    }

    /// Index of the top stack save.
    fn top_stack(&self) -> Option<usize> {
        self.saves.iter().rposition(|s| s.holder == Holder::Stack)
    }

    fn depth(&self) -> usize {
        self.saves
            .iter()
            .filter(|s| s.holder == Holder::Stack)
            .count()
    }
}

/// Options for one analysis.
#[derive(Debug, Clone, Copy, Default)]
pub struct WalkOptions {
    /// Treat a register stored to memory and reloaded as preserved (Phase 4).
    pub memory_saves: bool,
}

/// Analyse the routines whose labels are at `entries`, returning each one's
/// effect. Uncontracted callees are summarised by a monotone fixpoint over the
/// call graph, so recursion is exact.
#[must_use]
pub fn analyze(
    program: &ProgramMap,
    contracts: &Contracts,
    entries: &[usize],
    options: WalkOptions,
) -> HashMap<usize, RoutineEffect> {
    let unmodelled: HashSet<usize> = program.unmodelled.iter().map(|u| u.rom_offset).collect();
    let mut summaries: HashMap<usize, RoutineEffect> = HashMap::new();
    let mut known: Vec<usize> = Vec::new();
    // Monotone by construction (each round joins into the last), over a finite
    // lattice; the cap is a backstop.
    for _ in 0..64 {
        let mut discovered = Vec::new();
        let mut changed = false;
        let walker = Walker {
            program,
            contracts,
            summaries: &summaries,
            unmodelled: &unmodelled,
            options,
        };
        let mut next = summaries.clone();
        for &callee in &known {
            let effect = walker.walk(callee, &mut discovered);
            let slot = next.entry(callee).or_default();
            let before = slot.shape();
            slot.join(&effect);
            changed |= slot.shape() != before;
        }
        for &entry in entries {
            let _ = walker.walk(entry, &mut discovered);
        }
        for d in discovered {
            if !known.contains(&d) {
                known.push(d);
                changed = true;
            }
        }
        summaries = next;
        if !changed {
            break;
        }
    }
    let walker = Walker {
        program,
        contracts,
        summaries: &summaries,
        unmodelled: &unmodelled,
        options,
    };
    let mut ignored = Vec::new();
    entries
        .iter()
        .map(|&e| (e, walker.walk(e, &mut ignored)))
        .collect()
}

/// The walker for one round of the fixpoint.
struct Walker<'a> {
    program: &'a ProgramMap,
    contracts: &'a Contracts,
    summaries: &'a HashMap<usize, RoutineEffect>,
    unmodelled: &'a HashSet<usize>,
    options: WalkOptions,
}

/// The mnemonic of an opcode, for messages.
fn mnemonic(op: u8) -> &'static str {
    by_byte(op).mnemonic
}

/// Whether a write to `reg` is held by an open save (pending) or commits.
fn write(state: &mut State, acc: &mut RoutineEffect, reg: usize, origin: &WriteOrigin) {
    if reg != S {
        if let Some(save) = state.save_of(reg) {
            if save.pending.is_none() {
                save.pending = Some(origin.clone());
            }
            return;
        }
    }
    acc.commit(reg, origin);
}

/// Commit a closed save's pending write.
fn commit_save(acc: &mut RoutineEffect, save: &Save) {
    if let (Some(reg), Some(origin)) = (save.reg, &save.pending) {
        acc.commit(reg, origin);
    }
}

/// End a path: every save still open commits.
fn end_path(state: &State, acc: &mut RoutineEffect) {
    for save in &state.saves {
        commit_save(acc, save);
    }
}

/// Release the memory save of `addr` (its pending write commits).
fn release(state: &mut State, acc: &mut RoutineEffect, addr: u16) {
    if let Some(i) = state
        .saves
        .iter()
        .position(|s| s.holder == Holder::Memory(addr))
    {
        let save = state.saves.remove(i);
        commit_save(acc, &save);
    }
}

/// Whether the branch `op` is fixed by the previous instruction: `Some(true)`
/// always taken, `Some(false)` never.
fn fixed_branch(prev: Option<(u8, u8)>, op: u8) -> Option<bool> {
    let (p, imm) = prev?;
    match (p, op) {
        (0x18, 0x90) | (0x38, 0xB0) | (0xB8, 0x50) => Some(true),
        (0x18, 0xB0) | (0x38, 0x90) | (0xB8, 0x70) => Some(false),
        (0xA9 | 0xA2 | 0xA0, _) => {
            let zero = imm == 0;
            let neg = imm & 0x80 != 0;
            match op {
                0xF0 => Some(zero),
                0xD0 => Some(!zero),
                0x30 => Some(neg),
                0x10 => Some(!neg),
                _ => None,
            }
        }
        _ => None,
    }
}

impl Walker<'_> {
    /// The name to call the code at `off` by: a global label there, else any
    /// label, else its address.
    fn name_at(&self, off: usize, addr: u16) -> String {
        let mut labels: Vec<_> = self.program.labels_at(off).collect();
        labels.sort_by_key(|l| l.local);
        labels
            .first()
            .map_or_else(|| format!("${addr:04X}"), |l| l.name.clone())
    }

    /// The control transfer at `off` as written, and its target.
    fn transfer(&self, off: usize, op: u8) -> (Step, Target) {
        let p = self.program;
        let emit = p.emit_at(off).expect("decoded from an emit");
        let site = emit.site;
        let written = (emit.rom_offset == off).then_some(emit);
        let target = written
            .and_then(|e| match &e.kind {
                EmitKind::Instruction { target } => *target,
                EmitKind::Data { .. } => None,
            })
            .unwrap_or_else(|| {
                let lo = p.image.get(off + 1).copied().unwrap_or(0);
                let hi = p.image.get(off + 2).copied().unwrap_or(0);
                let addr = if is_branch(op) {
                    let cpu = p.cpu_at(off).unwrap_or(0);
                    cpu.wrapping_add(2).wrapping_add(lo as i8 as i16 as u16)
                } else {
                    u16::from_le_bytes([lo, hi])
                };
                Target { addr, anchor: None }
            });
        let operand = written
            .and_then(|e| e.operand.clone())
            .unwrap_or_else(|| match op {
                0x6C => format!("[${:04X}]", target.addr),
                _ => format!("${:04X}", target.addr),
            });
        (
            Step {
                text: format!("{} {operand}", mnemonic(op)),
                site,
            },
            target,
        )
    }

    /// Resolve a transfer target, or the reason it cannot be followed.
    fn resolve(&self, from: &Step, target: &Target, bank: u16) -> Result<usize, ReasonKind> {
        match self.program.resolve(target, bank) {
            Resolution::At(off) => Ok(off),
            Resolution::Ambiguous(banks) => Err(ReasonKind::Ambiguous {
                from: from.clone(),
                addr: target.addr,
                banks,
            }),
            Resolution::Nothing => Err(ReasonKind::Nothing {
                from: from.clone(),
                addr: target.addr,
            }),
        }
    }

    /// Walk the routine whose label is at `entry`.
    fn walk(&self, entry: usize, discovered: &mut Vec<usize>) -> RoutineEffect {
        let mut acc = RoutineEffect::default();
        let mut seen: HashSet<Key> = HashSet::new();
        let mut work = vec![State {
            off: entry,
            saves: Vec::new(),
            prev: None,
            transfer_restores: None,
            txs: None,
            arrived: None,
            last: None,
        }];
        let mut editor_noted = false;
        while let Some(state) = work.pop() {
            if !seen.insert(state.key()) {
                continue;
            }
            if seen.len() > MAX_STATES {
                acc.reason(Reason {
                    kind: ReasonKind::TooComplex,
                    through: None,
                });
                break;
            }
            if !editor_noted && self.unmodelled.contains(&state.off) {
                editor_noted = true;
                let site = self
                    .program
                    .unmodelled
                    .iter()
                    .find(|u| u.rom_offset == state.off)
                    .map_or(Site { file: 0, line: 0 }, |u| u.site);
                acc.reason(Reason {
                    kind: ReasonKind::EditorNotRun(site),
                    through: None,
                });
            }
            self.step(entry, state, &mut acc, &mut work, discovered);
        }
        acc
    }

    /// Execute one instruction of a path, pushing its successors.
    #[allow(clippy::too_many_lines)]
    fn step(
        &self,
        entry: usize,
        mut state: State,
        acc: &mut RoutineEffect,
        work: &mut Vec<State>,
        discovered: &mut Vec<usize>,
    ) {
        let p = self.program;
        let off = state.off;
        let reason = |acc: &mut RoutineEffect, kind| {
            acc.reason(Reason {
                kind,
                through: None,
            });
        };

        // Rule 1: reaching another contracted routine's entry.
        if off != entry {
            let contracts = self.contracts.at(off);
            if !contracts.is_empty() && state.saves.is_empty() {
                for c in contracts {
                    let origin = WriteOrigin {
                        base: OriginBase::Contract {
                            how: state.arrived.clone(),
                            name: c.name.clone(),
                            site: c.site,
                        },
                        through: None,
                    };
                    for reg in 0..4 {
                        if c.writes.contains(REGS[reg]) {
                            acc.commit(reg, &origin);
                        }
                    }
                    for &(lo, hi) in &c.memory {
                        acc.stores.extend(lo..=hi);
                    }
                }
                return;
            }
        }

        // Decode.
        let Some(emit) = p.emit_at(off) else {
            let bank = state
                .last
                .and(p.bank_at(off.saturating_sub(1)))
                .unwrap_or(0);
            reason(
                acc,
                ReasonKind::RunsPast {
                    bank,
                    after: state.last,
                },
            );
            end_path(&state, acc);
            return;
        };
        let site = emit.site;
        let bank = emit.bank;
        let data = match &emit.kind {
            EmitKind::Data { directive } => Some(directive.clone()),
            EmitKind::Instruction { .. } => None,
        };
        let op = p.image[off];
        let opc = by_byte(op);
        if !opc.effects_known || (opc.is_undocumented() && !p.undocumented) {
            reason(
                acc,
                ReasonKind::UnknownOpcode {
                    site,
                    byte: op,
                    data: data.is_some(),
                },
            );
            end_path(&state, acc);
            return;
        }
        let len = usize::from(opc.length);
        let whole = (off..off + len)
            .all(|o| p.written.get(o).copied().unwrap_or(false) && p.bank_at(o) == Some(bank));
        if !whole {
            reason(
                acc,
                ReasonKind::RunsPast {
                    bank,
                    after: Some(site),
                },
            );
            end_path(&state, acc);
            return;
        }
        let operand = p.image.get(off + 1).copied().unwrap_or(0);
        let origin = WriteOrigin {
            base: OriginBase::Instr {
                mnemonic: opc.mnemonic,
                site,
                via: emit.via.clone(),
                data: data.clone(),
            },
            through: None,
        };
        let here = Step {
            text: opc.mnemonic.to_string(),
            site,
        };
        let restores = state.transfer_restores.take();
        state.last = Some(site);

        // Successor helpers.
        let fall = |state: &State, acc: &mut RoutineEffect, work: &mut Vec<State>| match p
            .fall_through(off, len)
        {
            Resolution::At(next) => {
                let mut s = state.clone();
                s.off = next;
                s.prev = Some((op, operand));
                s.arrived = None;
                work.push(s);
            }
            Resolution::Nothing => {
                acc.reason(Reason {
                    kind: ReasonKind::RunsPast {
                        bank,
                        after: Some(site),
                    },
                    through: None,
                });
                end_path(state, acc);
            }
            Resolution::Ambiguous(banks) => {
                let cpu = p.cpu_at(off).unwrap_or(0).wrapping_add(len as u16);
                acc.reason(Reason {
                    kind: ReasonKind::Ambiguous {
                        from: here.clone(),
                        addr: cpu,
                        banks,
                    },
                    through: None,
                });
                end_path(state, acc);
            }
        };

        match op {
            // RTS / RTI.
            0x60 | 0x40 => {
                if state.depth() > 0 {
                    reason(acc, ReasonKind::ReturnThroughPushed(here.clone()));
                } else if let Some(txs) = state.txs {
                    reason(
                        acc,
                        ReasonKind::ReturnAfterTxs {
                            ret: here.clone(),
                            txs,
                        },
                    );
                }
                end_path(&state, acc);
            }
            // BRK: the handler is not followed.
            0x00 => end_path(&state, acc),
            // JMP abs.
            0x4C => {
                let (step, target) = self.transfer(off, op);
                match self.resolve(&step, &target, bank) {
                    Ok(t) => {
                        state.off = t;
                        state.prev = Some((op, operand));
                        state.arrived = Some(step);
                        work.push(state);
                    }
                    Err(kind) => {
                        reason(acc, kind);
                        end_path(&state, acc);
                    }
                }
            }
            // JMP (ind).
            0x6C => {
                let (step, _) = self.transfer(off, op);
                reason(acc, ReasonKind::IndirectJmp(step));
                end_path(&state, acc);
            }
            // JSR.
            0x20 => {
                let (step, target) = self.transfer(off, op);
                let callee = match self.resolve(&step, &target, bank) {
                    Ok(t) => t,
                    Err(kind) => {
                        reason(acc, kind);
                        end_path(&state, acc);
                        return;
                    }
                };
                let contracts = self.contracts.at(callee);
                if contracts.is_empty() {
                    if !discovered.contains(&callee) {
                        discovered.push(callee);
                    }
                    let summary = self.summaries.get(&callee).cloned().unwrap_or_default();
                    for reg in 0..4 {
                        if let Some(o) = &summary.origins[reg] {
                            let mut o = o.clone();
                            o.through = Some(step.clone());
                            write(&mut state, acc, reg, &o);
                        }
                    }
                    for r in &summary.reasons {
                        let mut r = r.clone();
                        r.through = Some(step.clone());
                        acc.reason(r);
                    }
                    for &addr in &summary.stores {
                        release(&mut state, acc, addr);
                    }
                    acc.stores.extend(summary.stores.iter().copied());
                    if summary.pulls_return.is_some() {
                        let name = self.name_at(callee, target.addr);
                        reason(
                            acc,
                            ReasonKind::PullsReturn {
                                call: step,
                                callee: name,
                            },
                        );
                        end_path(&state, acc);
                        return;
                    }
                } else {
                    for c in contracts {
                        let origin = WriteOrigin {
                            base: OriginBase::Contract {
                                how: Some(step.clone()),
                                name: c.name.clone(),
                                site: c.site,
                            },
                            through: None,
                        };
                        for reg in 0..4 {
                            if c.writes.contains(REGS[reg]) {
                                write(&mut state, acc, reg, &origin);
                            }
                        }
                        for &(lo, hi) in &c.memory {
                            for addr in lo..=hi {
                                release(&mut state, acc, addr);
                            }
                            acc.stores.extend(lo..=hi);
                        }
                    }
                }
                // The return site.
                if let Resolution::At(ret) = p.fall_through(off, len) {
                    if let Some(EmitKind::Data { .. }) = p.emit_at(ret).map(|e| &e.kind) {
                        let data_site = p.emit_at(ret).map_or(site, |e| e.site);
                        reason(
                            acc,
                            ReasonKind::ReturnsIntoData {
                                call: step,
                                data: data_site,
                            },
                        );
                        end_path(&state, acc);
                        return;
                    }
                }
                fall(&state, acc, work);
            }
            // Conditional branches.
            _ if is_branch(op) => {
                let fixed = fixed_branch(state.prev, op);
                if fixed != Some(false) {
                    let (step, target) = self.transfer(off, op);
                    match self.resolve(&step, &target, bank) {
                        Ok(t) => {
                            let mut s = state.clone();
                            s.off = t;
                            s.prev = Some((op, operand));
                            s.arrived = Some(step);
                            work.push(s);
                        }
                        Err(kind) => reason(acc, kind),
                    }
                }
                if fixed != Some(true) {
                    fall(&state, acc, work);
                }
            }
            // PHA: saves X after TXA, Y after TYA, A otherwise.
            0x48 => {
                let reg = match state.prev {
                    Some((0x8A, _)) => X,
                    Some((0x98, _)) => Y,
                    _ => A,
                };
                state.saves.push(Save {
                    holder: Holder::Stack,
                    reg: Some(reg),
                    site,
                    pending: None,
                });
                fall(&state, acc, work);
            }
            // PHP: flags, depth only.
            0x08 => {
                state.saves.push(Save {
                    holder: Holder::Stack,
                    reg: None,
                    site,
                    pending: None,
                });
                fall(&state, acc, work);
            }
            // PLA.
            0x68 => {
                match state.top_stack() {
                    None => {
                        if acc.pulls_return.is_none() {
                            acc.pulls_return = Some(here.clone());
                        }
                        write(&mut state, acc, A, &origin);
                    }
                    Some(i) => {
                        let save = state.saves.remove(i);
                        match save.reg {
                            Some(A) => acc.preserved_push(A, save.site, site),
                            Some(reg) => {
                                let next = match p.fall_through(off, len) {
                                    Resolution::At(n) => p.image.get(n).copied(),
                                    _ => None,
                                };
                                let wanted = if reg == X { 0xAA } else { 0xA8 };
                                if next == Some(wanted) {
                                    acc.preserved_push(reg, save.site, site);
                                    state.transfer_restores = Some(reg);
                                } else {
                                    commit_save(acc, &save);
                                }
                                write(&mut state, acc, A, &origin);
                            }
                            None => write(&mut state, acc, A, &origin),
                        }
                    }
                }
                fall(&state, acc, work);
            }
            // PLP.
            0x28 => {
                match state.top_stack() {
                    None => {
                        if acc.pulls_return.is_none() {
                            acc.pulls_return = Some(here.clone());
                        }
                    }
                    Some(i) => {
                        let save = state.saves.remove(i);
                        commit_save(acc, &save);
                    }
                }
                fall(&state, acc, work);
            }
            // TXS: every stack save commits; the return address is unknown.
            0x9A => {
                let mut kept = Vec::new();
                for save in std::mem::take(&mut state.saves) {
                    if save.holder == Holder::Stack {
                        commit_save(acc, &save);
                    } else {
                        kept.push(save);
                    }
                }
                state.saves = kept;
                state.txs = Some(site);
                if acc.pulls_return.is_none() {
                    acc.pulls_return = Some(here.clone());
                }
                acc.commit(S, &origin);
                fall(&state, acc, work);
            }
            // TAX / TAY completing a `PLA` restore.
            0xAA | 0xA8 if restores == Some(if op == 0xAA { X } else { Y }) => {
                fall(&state, acc, work);
            }
            _ => {
                let addr = match opc.length {
                    2 => Some(u16::from(operand)),
                    3 => Some(u16::from_le_bytes([
                        operand,
                        p.image.get(off + 2).copied().unwrap_or(0),
                    ])),
                    _ => None,
                };
                let memory = self.options.memory_saves;
                match (op, addr) {
                    // STA / STX / STY, zero page or absolute.
                    (0x85 | 0x8D | 0x86 | 0x8E | 0x84 | 0x8C, Some(m)) => {
                        acc.stores.insert(m);
                        if memory {
                            release(&mut state, acc, m);
                            let reg = match op {
                                0x85 | 0x8D => A,
                                0x86 | 0x8E => X,
                                _ => Y,
                            };
                            state.saves.push(Save {
                                holder: Holder::Memory(m),
                                reg: Some(reg),
                                site,
                                pending: None,
                            });
                        }
                        fall(&state, acc, work);
                        return;
                    }
                    // LDA / LDX / LDY, zero page or absolute: a reload restores.
                    (0xA5 | 0xAD | 0xA6 | 0xAE | 0xA4 | 0xAC, Some(m)) if memory => {
                        let reg = match op {
                            0xA5 | 0xAD => A,
                            0xA6 | 0xAE => X,
                            _ => Y,
                        };
                        if let Some(i) = state
                            .saves
                            .iter()
                            .position(|s| s.holder == Holder::Memory(m) && s.reg == Some(reg))
                        {
                            let save = state.saves.remove(i);
                            acc.preserved_push(reg, save.site, site);
                            fall(&state, acc, work);
                            return;
                        }
                    }
                    // Read-modify-write on memory releases a save of it.
                    (
                        0xE6 | 0xEE | 0xC6 | 0xCE | 0x06 | 0x0E | 0x46 | 0x4E | 0x26 | 0x2E | 0x66
                        | 0x6E,
                        Some(m),
                    ) => {
                        acc.stores.insert(m);
                        release(&mut state, acc, m);
                    }
                    _ => {}
                }
                for reg in 0..4 {
                    if opc.writes.contains(REGS[reg]) {
                        write(&mut state, acc, reg, &origin);
                    }
                }
                fall(&state, acc, work);
            }
        }
    }
}

impl RoutineEffect {
    fn preserved_push(&mut self, reg: usize, saved: Site, restored: Site) {
        let p = Preserved {
            reg,
            saved,
            restored,
        };
        if !self.preserved.contains(&p) {
            self.preserved.push(p);
        }
    }
}

/// The register name for an index.
#[must_use]
pub fn reg_name(reg: usize) -> &'static str {
    ["A", "X", "Y", "S"][reg]
}

/// The register set for an index.
#[must_use]
pub fn reg_set(reg: usize) -> RegSet {
    REGS[reg]
}
