//! CDL-based **runtime coverage**: classify a byte-exact [`SourceMap`] against a
//! Code/Data Logger (CDL) capture an emulator wrote after running the ROM.
//!
//! This is the analysis half of the coverage feature (see
//! `plans/007-cdl-based-coverage.md`). Phase 0 taught the assembler to emit a
//! [`SourceMap`] — which source line wrote each ROM byte. Here we take that map
//! plus a [`CdlSource`] (the emulator's per-byte access flags) and produce a
//! per-file, per-line [`CoverageReport`] of what the running game actually
//! touched.
//!
//! Only the PRG section is classified; CHR bytes are ignored (a source line that
//! emits only CHR data is omitted from the report), matching the feature's
//! PRG-only scope.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;

use crate::tooling::{Directive, DirectiveArgs, DirectiveName, RegionBound};
use crate::{SourceMap, SourceSpan};

/// FCEUX PRG CDL flag bits (`xPdcAADC`), per `docs` / the FCEUX format spec.
mod fceux {
    /// Accessed as executable code.
    pub const CODE: u8 = 0x01;
    /// Accessed as data (read).
    pub const DATA: u8 = 0x02;
    /// Indirectly accessed as code (e.g. `JMP ($nnnn)` destination).
    pub const INDIRECT_CODE: u8 = 0x10;
    /// Indirectly accessed as data (e.g. `LDA ($nn),Y` destination).
    pub const INDIRECT_DATA: u8 = 0x20;
    /// Logged as PCM audio data.
    pub const PCM: u8 = 0x40;

    /// Bits that mean "code" when set.
    pub const CODE_MASK: u8 = CODE | INDIRECT_CODE;
    /// Bits that mean "data" when set.
    pub const DATA_MASK: u8 = DATA | INDIRECT_DATA | PCM;
}

/// Mesen (Mesen2) PRG CDL flag bits. Mesen2 uses one unified `CdlFlags` set for
/// every console (`Core/Debugger/DebugTypes.h`): `Code`, `Data`, `JumpTarget`,
/// `SubEntryPoint` — and, unlike FCEUX, **no** indirect-access or PCM bits, and
/// bits 2–3 mean jump/subroutine targets rather than FCEUX's bank window. A flat
/// Mesen mask is therefore the same size and layout as an FCEUX one but is
/// **bit-incompatible above bit 1**, which is why the emulator must be stated.
mod mesen {
    /// Executed as code.
    pub const CODE: u8 = 0x01;
    /// Read as data.
    pub const DATA: u8 = 0x02;
    /// Target of a jump/branch (an executed address → code).
    pub const JUMP_TARGET: u8 = 0x04;
    /// Target of a `JSR` (a subroutine entry point → code).
    pub const SUB_ENTRY_POINT: u8 = 0x08;

    /// Bits that mean "code" when set.
    pub const CODE_MASK: u8 = CODE | JUMP_TARGET | SUB_ENTRY_POINT;
    /// Bits that mean "data" when set.
    pub const DATA_MASK: u8 = DATA;
}

/// How a source line's bytes were touched at runtime, per the CDL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CdlClass {
    /// At least one byte executed as code; none read as data.
    Code,
    /// At least one byte read as data; none executed as code.
    Data,
    /// Both code and data flags appear across the line's bytes.
    Mixed,
    /// No CDL flag set for any byte — present in source, never touched.
    Unaccessed,
}

impl CdlClass {
    /// Combine accumulated code/data flags into a class.
    #[must_use]
    fn from_flags(code: bool, data: bool) -> CdlClass {
        match (code, data) {
            (true, true) => CdlClass::Mixed,
            (true, false) => CdlClass::Code,
            (false, true) => CdlClass::Data,
            (false, false) => CdlClass::Unaccessed,
        }
    }

    /// The lowercase name used in the JSON report.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            CdlClass::Code => "code",
            CdlClass::Data => "data",
            CdlClass::Mixed => "mixed",
            CdlClass::Unaccessed => "unaccessed",
        }
    }

    /// Whether a line of this class was touched at runtime (anything but
    /// [`Unaccessed`](CdlClass::Unaccessed)). This is the boolean LCOV records.
    #[must_use]
    pub fn is_covered(self) -> bool {
        !matches!(self, CdlClass::Unaccessed)
    }
}

/// Error constructing a [`FlatMaskCdl`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CdlError {
    /// The CDL file is smaller than the ROM's PRG section, so it cannot cover
    /// every PRG byte. `len` is the file size; `prg_len` is what was required.
    TooSmall {
        /// Size of the CDL file, in bytes.
        len: usize,
        /// PRG bytes the assembled ROM has (the minimum the file must cover).
        prg_len: usize,
    },
}

impl std::fmt::Display for CdlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CdlError::TooSmall { len, prg_len } => write!(
                f,
                "CDL file is {len} bytes but the ROM's PRG section is {prg_len} bytes"
            ),
        }
    }
}

impl std::error::Error for CdlError {}

/// A source of CDL access flags: given a PRG ROM byte offset, report whether the
/// byte was accessed as code and/or data. One implementor per emulator format
/// (v1: [`FlatMaskCdl`] for FCEUX and Mesen; `BizHawk`'s container is a later
/// phase).
pub trait CdlSource {
    /// `(code, data)` flags for the byte at PRG offset `prg_offset`. Offsets at
    /// or beyond [`prg_len`](CdlSource::prg_len) report `(false, false)`.
    fn prg_class(&self, prg_offset: usize) -> (bool, bool);

    /// Number of PRG ROM bytes this CDL covers — the PRG/CHR boundary in the
    /// ROM's byte-offset space (the same space a [`SourceSpan`] uses).
    fn prg_len(&self) -> usize;
}

/// A flat ROM-mask CDL (FCEUX / Mesen): one flag byte per ROM byte, PRG section
/// first. Constructed with the emulator's code/data masks and the assembled PRG
/// size (which fixes the PRG/CHR boundary, since a flat mask carries no header).
#[derive(Debug, Clone)]
pub struct FlatMaskCdl {
    bytes: Vec<u8>,
    code_mask: u8,
    data_mask: u8,
    prg_len: usize,
}

impl FlatMaskCdl {
    /// Build an **FCEUX** flat-mask reader over `bytes`, treating the first
    /// `prg_len` bytes as the PRG section.
    ///
    /// # Errors
    /// Returns [`CdlError::TooSmall`] if `bytes` is shorter than `prg_len`.
    pub fn fceux(bytes: Vec<u8>, prg_len: usize) -> Result<FlatMaskCdl, CdlError> {
        Self::with_masks(bytes, prg_len, fceux::CODE_MASK, fceux::DATA_MASK)
    }

    /// Build a **Mesen** (Mesen2) flat-mask reader over `bytes`. Same container
    /// as FCEUX but with Mesen's code/data masks (see [`mesen`]).
    ///
    /// # Errors
    /// Returns [`CdlError::TooSmall`] if `bytes` is shorter than `prg_len`.
    pub fn mesen(bytes: Vec<u8>, prg_len: usize) -> Result<FlatMaskCdl, CdlError> {
        Self::with_masks(bytes, prg_len, mesen::CODE_MASK, mesen::DATA_MASK)
    }

    /// Build a flat-mask reader with explicit code/data masks. Mesen reuses this
    /// with its own masks (Phase 2); FCEUX callers use [`fceux`](Self::fceux).
    ///
    /// # Errors
    /// Returns [`CdlError::TooSmall`] if `bytes` is shorter than `prg_len`.
    pub fn with_masks(
        bytes: Vec<u8>,
        prg_len: usize,
        code_mask: u8,
        data_mask: u8,
    ) -> Result<FlatMaskCdl, CdlError> {
        if bytes.len() < prg_len {
            return Err(CdlError::TooSmall {
                len: bytes.len(),
                prg_len,
            });
        }
        Ok(FlatMaskCdl {
            bytes,
            code_mask,
            data_mask,
            prg_len,
        })
    }
}

impl CdlSource for FlatMaskCdl {
    fn prg_class(&self, prg_offset: usize) -> (bool, bool) {
        match self.bytes.get(prg_offset) {
            Some(&b) if prg_offset < self.prg_len => {
                (b & self.code_mask != 0, b & self.data_mask != 0)
            }
            _ => (false, false),
        }
    }

    fn prg_len(&self) -> usize {
        self.prg_len
    }
}

/// OR the CDL flags across a span's PRG bytes. Bytes at or beyond the PRG/CHR
/// boundary are skipped (spans do not straddle it in practice; CHR is ignored).
fn span_flags(cdl: &dyn CdlSource, span: &SourceSpan) -> (bool, bool) {
    let mut code = false;
    let mut data = false;
    let prg_len = cdl.prg_len();
    for i in 0..span.len {
        let off = span.rom_offset + i;
        if off >= prg_len {
            break;
        }
        let (c, d) = cdl.prg_class(off);
        code |= c;
        data |= d;
    }
    (code, data)
}

/// Classify a single span against the CDL. A span with no PRG bytes (entirely in
/// the CHR region, or an empty span) classifies as
/// [`Unaccessed`](CdlClass::Unaccessed).
#[must_use]
pub fn classify_span(cdl: &dyn CdlSource, span: &SourceSpan) -> CdlClass {
    let (code, data) = span_flags(cdl, span);
    CdlClass::from_flags(code, data)
}

/// One classified source line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineCoverage {
    /// 1-based source line.
    pub line: u32,
    /// The line's runtime classification.
    pub class: CdlClass,
}

/// Per-file coverage: every classified (PRG-emitting) line in the file, plus a
/// count of lines in each class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileCoverage {
    /// Source file display name (as it appears in the [`SourceMap`]).
    pub path: String,
    /// Classified lines, ascending by line number.
    pub lines: Vec<LineCoverage>,
    /// Number of [`Code`](CdlClass::Code) lines.
    pub code: u32,
    /// Number of [`Data`](CdlClass::Data) lines.
    pub data: u32,
    /// Number of [`Mixed`](CdlClass::Mixed) lines.
    pub mixed: u32,
    /// Number of [`Unaccessed`](CdlClass::Unaccessed) lines.
    pub unaccessed: u32,
    /// Number of emitting lines excluded by a coverage directive. These lines
    /// are in **neither** the numerator nor the denominator — they are absent
    /// from `lines` and from every class count.
    pub ignored: u32,
}

impl FileCoverage {
    /// Build a file's coverage from `(line, executed)` rows — the shape script
    /// coverage produces, where a line is simply run or not run (there is no
    /// data/mixed for code that executes inside the assembler). Each executed
    /// line becomes [`Code`](CdlClass::Code), each un-executed coverable line
    /// [`Unaccessed`](CdlClass::Unaccessed), so the same JSON/LCOV emitters
    /// apply. `rows` should already be in ascending line order.
    #[must_use]
    pub fn from_line_hits(
        path: String,
        rows: impl IntoIterator<Item = (u32, bool)>,
    ) -> FileCoverage {
        FileCoverage::from_line_hits_with_ignores(path, rows, &CoverageIgnores::default())
    }

    /// [`from_line_hits`](Self::from_line_hits), dropping any line excluded for
    /// this file by `ignores` (counted in [`ignored`](Self::ignored) instead).
    #[must_use]
    pub fn from_line_hits_with_ignores(
        path: String,
        rows: impl IntoIterator<Item = (u32, bool)>,
        ignores: &CoverageIgnores,
    ) -> FileCoverage {
        let mut file = FileCoverage {
            path,
            lines: Vec::new(),
            code: 0,
            data: 0,
            mixed: 0,
            unaccessed: 0,
            ignored: 0,
        };
        for (line, executed) in rows {
            if ignores.contains(&file.path, line) {
                file.ignored += 1;
                continue;
            }
            let class = if executed {
                file.code += 1;
                CdlClass::Code
            } else {
                file.unaccessed += 1;
                CdlClass::Unaccessed
            };
            file.lines.push(LineCoverage { line, class });
        }
        file
    }
}

/// A full coverage report over the assembled program: one [`FileCoverage`] per
/// source file that emitted PRG bytes, sorted by path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CoverageReport {
    /// Per-file coverage, sorted by path.
    pub files: Vec<FileCoverage>,
    /// Files dropped entirely because every emitting line in them was excluded
    /// (an `@nessemble-coverage-ignore start` with no `end`, typically).
    pub ignored_files: u32,
}

/// Source lines excluded from a coverage report by the
/// `@nessemble-coverage-ignore…` comment directives.
///
/// Ranges are inclusive and keyed by the same file identity the [`SourceMap`]
/// uses (canonical absolute paths), so exclusion happens before any display-path
/// rewriting. A single ignored line is a one-line range and an unclosed region
/// is a range ending at [`u32::MAX`], which is why the caller never has to know
/// how long a file is.
///
/// Building this from source text lives in the caller: `nessemble-core` does no
/// file I/O (the wasm build depends on that), so the CLI scans each file with
/// `tooling::scan_directives` and fills this in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CoverageIgnores {
    ranges: BTreeMap<String, Vec<(u32, u32)>>,
}

impl CoverageIgnores {
    /// Exclude a single 1-based line of `file`.
    pub fn ignore_line(&mut self, file: &str, line: u32) {
        self.ignore_range(file, line, line);
    }

    /// Exclude the inclusive line range `start..=end` of `file`. Pass
    /// [`u32::MAX`] as `end` for a region that runs to the end of the file.
    pub fn ignore_range(&mut self, file: &str, start: u32, end: u32) {
        self.ranges
            .entry(file.to_string())
            .or_default()
            .push((start, end));
    }

    /// Whether `line` of `file` is excluded.
    #[must_use]
    pub fn contains(&self, file: &str, line: u32) -> bool {
        self.ranges
            .get(file)
            .is_some_and(|rs| rs.iter().any(|&(lo, hi)| line >= lo && line <= hi))
    }

    /// Whether nothing at all is excluded (the common case, and the fast path).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ranges.is_empty()
    }
}

/// Turn one file's [`Directive`]s into exclusion ranges.
///
/// - `@nessemble-coverage-ignore-next-line` excludes the next **significant**
///   line, per `next_significant` (so an explanatory comment may sit between the
///   directive and its subject).
/// - `@nessemble-coverage-ignore start` opens a region; the next `end` closes
///   it. An unclosed region runs to end of file — the whole-file opt-out. A
///   `start` inside an open region and an `end` with no open region are inert
///   (the linter reports both).
/// - A directive in a trailing comment is inert.
///
/// Language-agnostic: `directives` come from `tooling::scan_directives` for
/// assembly or `tooling::scan_line_comment_directives` for scripts.
pub fn resolve_ignores(
    file: &str,
    directives: &[Directive],
    next_significant: &dyn Fn(u32) -> Option<u32>,
    ignores: &mut CoverageIgnores,
) {
    let mut region_start: Option<u32> = None;
    for d in directives.iter().filter(|d| d.own_line) {
        match (d.name, &d.args) {
            (DirectiveName::CoverageIgnoreNextLine, _) => {
                if let Some(target) = next_significant(d.line) {
                    ignores.ignore_line(file, target);
                }
            }
            (DirectiveName::CoverageIgnore, DirectiveArgs::Region(RegionBound::Start)) => {
                region_start.get_or_insert(d.line);
            }
            (DirectiveName::CoverageIgnore, DirectiveArgs::Region(RegionBound::End)) => {
                if let Some(start) = region_start.take() {
                    ignores.ignore_range(file, start, d.line);
                }
            }
            _ => {}
        }
    }
    // An unclosed region runs to the end of the file.
    if let Some(start) = region_start {
        ignores.ignore_range(file, start, u32::MAX);
    }
}

/// Build a coverage report by classifying every PRG-emitting source line in
/// `source_map` against `cdl`.
///
/// A line's class ORs the CDL flags across *all* the bytes it emitted (a line
/// may contribute more than one span). Lines that emit only CHR bytes are
/// omitted. Files are sorted by path and lines within a file by line number.
#[must_use]
pub fn build_report(source_map: &SourceMap, cdl: &dyn CdlSource) -> CoverageReport {
    build_report_with_ignores(source_map, cdl, &CoverageIgnores::default())
}

/// [`build_report`], honoring the `@nessemble-coverage-ignore…` exclusions in
/// `ignores`.
///
/// An excluded line is dropped from `lines` and from every class count — it is
/// in neither the numerator nor the denominator — and tallied in
/// [`FileCoverage::ignored`]. A file whose every emitting line is excluded is
/// dropped from the report entirely (rather than reported as an empty 0/0 file)
/// and counted in [`CoverageReport::ignored_files`], which is what makes an
/// unclosed region at the top of a file read as "this file is not measured".
#[must_use]
pub fn build_report_with_ignores(
    source_map: &SourceMap,
    cdl: &dyn CdlSource,
    ignores: &CoverageIgnores,
) -> CoverageReport {
    let prg_len = cdl.prg_len();

    // file -> (line -> accumulated (code, data) flags)
    let mut acc: BTreeMap<Arc<str>, BTreeMap<u32, (bool, bool)>> = BTreeMap::new();
    for span in &source_map.spans {
        if span.rom_offset >= prg_len {
            continue; // CHR-only line: ignored
        }
        let (c, d) = span_flags(cdl, span);
        let entry = acc
            .entry(span.file.clone())
            .or_default()
            .entry(span.line)
            .or_default();
        entry.0 |= c;
        entry.1 |= d;
    }

    let mut files = Vec::with_capacity(acc.len());
    let mut ignored_files = 0u32;
    for (path, lines) in acc {
        let mut file = FileCoverage {
            path: path.to_string(),
            lines: Vec::with_capacity(lines.len()),
            code: 0,
            data: 0,
            mixed: 0,
            unaccessed: 0,
            ignored: 0,
        };
        for (line, (code, data)) in lines {
            if ignores.contains(&file.path, line) {
                file.ignored += 1;
                continue;
            }
            let class = CdlClass::from_flags(code, data);
            match class {
                CdlClass::Code => file.code += 1,
                CdlClass::Data => file.data += 1,
                CdlClass::Mixed => file.mixed += 1,
                CdlClass::Unaccessed => file.unaccessed += 1,
            }
            file.lines.push(LineCoverage { line, class });
        }
        // Nothing left to report: the file opted out entirely.
        if file.lines.is_empty() && file.ignored > 0 {
            ignored_files += 1;
            continue;
        }
        files.push(file);
    }

    CoverageReport {
        files,
        ignored_files,
    }
}

/// Aggregate line counts across every file in a [`CoverageReport`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Totals {
    /// Total [`Code`](CdlClass::Code) lines.
    pub code: u32,
    /// Total [`Data`](CdlClass::Data) lines.
    pub data: u32,
    /// Total [`Mixed`](CdlClass::Mixed) lines.
    pub mixed: u32,
    /// Total [`Unaccessed`](CdlClass::Unaccessed) lines.
    pub unaccessed: u32,
    /// Lines excluded by a coverage directive **in reported files** (not part of
    /// [`covered`](Totals::covered) or [`total`](Totals::total)). Lines in a
    /// fully-excluded file are not counted here — that file is counted once in
    /// [`ignored_files`](Totals::ignored_files) instead.
    pub ignored: u32,
    /// Files dropped because every emitting line in them was excluded.
    pub ignored_files: u32,
}

impl Totals {
    /// Lines touched at runtime (code + data + mixed).
    #[must_use]
    pub fn covered(self) -> u32 {
        self.code + self.data + self.mixed
    }

    /// All classified (PRG-emitting) lines.
    #[must_use]
    pub fn total(self) -> u32 {
        self.covered() + self.unaccessed
    }
}

impl CoverageReport {
    /// Sum per-class line counts across all files.
    #[must_use]
    pub fn totals(&self) -> Totals {
        let mut t = Totals::default();
        for f in &self.files {
            t.code += f.code;
            t.data += f.data;
            t.mixed += f.mixed;
            t.unaccessed += f.unaccessed;
            t.ignored += f.ignored;
        }
        t.ignored_files = self.ignored_files;
        t
    }

    /// Render the report as [LCOV](https://github.com/linux-test-project/lcov):
    /// per file an `SF` record, one `DA:line,hits` per classified line (`hits` is
    /// `1` when the line was touched at runtime, else `0`), then `LF`/`LH` line
    /// totals and `end_of_record`. LCOV is line-boolean, so the code/data/mixed
    /// distinction collapses to hit/not-hit; the JSON form keeps the full class.
    #[must_use]
    pub fn to_lcov(&self) -> String {
        let mut out = String::new();
        for file in &self.files {
            let _ = writeln!(out, "SF:{}", file.path);
            let mut hit = 0u32;
            for line in &file.lines {
                let covered = u8::from(line.class.is_covered());
                hit += u32::from(line.class.is_covered());
                let _ = writeln!(out, "DA:{},{covered}", line.line);
            }
            let _ = writeln!(out, "LF:{}", file.lines.len());
            let _ = writeln!(out, "LH:{hit}");
            out.push_str("end_of_record\n");
        }
        out
    }

    /// Render the report as JSON: a `files` array (each with `path`, per-class
    /// counts, and a `lines` array of `{ "line", "class" }`) plus a `totals`
    /// object. The `class` is the full four-way [`CdlClass`] name, so this form
    /// preserves the code/data/mixed distinction the CDL affords.
    #[must_use]
    pub fn to_json(&self) -> String {
        let mut out = String::from("{\n  \"files\": [");
        for (fi, file) in self.files.iter().enumerate() {
            out.push_str(if fi == 0 { "\n" } else { ",\n" });
            let _ = writeln!(
                out,
                "    {{\n      \"path\": \"{}\",",
                json_escape(&file.path)
            );
            let _ = writeln!(
                out,
                "      \"code\": {}, \"data\": {}, \"mixed\": {}, \"unaccessed\": {}, \"ignored\": {},",
                file.code, file.data, file.mixed, file.unaccessed, file.ignored
            );
            out.push_str("      \"lines\": [");
            for (li, line) in file.lines.iter().enumerate() {
                out.push_str(if li == 0 { "\n" } else { ",\n" });
                let _ = write!(
                    out,
                    "        {{ \"line\": {}, \"class\": \"{}\" }}",
                    line.line,
                    line.class.as_str()
                );
            }
            if file.lines.is_empty() {
                out.push_str("]\n    }");
            } else {
                out.push_str("\n      ]\n    }");
            }
        }
        out.push_str(if self.files.is_empty() { "]" } else { "\n  ]" });

        let t = self.totals();
        let _ = writeln!(
            out,
            ",\n  \"totals\": {{ \"code\": {}, \"data\": {}, \"mixed\": {}, \"unaccessed\": {}, \"covered\": {}, \"total\": {}, \"ignored\": {}, \"ignoredFiles\": {} }}\n}}",
            t.code, t.data, t.mixed, t.unaccessed, t.covered(), t.total(), t.ignored, t.ignored_files
        );
        out
    }
}

/// Escape a string for embedding in a JSON string literal.
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(file: &str, line: u32, off: usize, len: usize) -> SourceSpan {
        SourceSpan {
            file: Arc::from(file),
            line,
            rom_offset: off,
            len,
        }
    }

    #[test]
    fn fceux_rejects_a_too_small_file() {
        let err = FlatMaskCdl::fceux(vec![0u8; 4], 8).unwrap_err();
        assert_eq!(err, CdlError::TooSmall { len: 4, prg_len: 8 });
    }

    #[test]
    fn prg_class_decodes_fceux_flag_bits() {
        // 0 code, 1 data, 2 indirect-code, 3 indirect-data, 4 PCM,
        // 5 bank bits only (ignored), 6 untouched, 7 code+data.
        let bytes = vec![0x01, 0x02, 0x10, 0x20, 0x40, 0x0C, 0x00, 0x03];
        let cdl = FlatMaskCdl::fceux(bytes.clone(), bytes.len()).unwrap();
        assert_eq!(cdl.prg_class(0), (true, false));
        assert_eq!(cdl.prg_class(1), (false, true));
        assert_eq!(cdl.prg_class(2), (true, false)); // indirect code
        assert_eq!(cdl.prg_class(3), (false, true)); // indirect data
        assert_eq!(cdl.prg_class(4), (false, true)); // PCM counts as data
        assert_eq!(cdl.prg_class(5), (false, false)); // bank bits ignored
        assert_eq!(cdl.prg_class(6), (false, false));
        assert_eq!(cdl.prg_class(7), (true, true));
    }

    #[test]
    fn prg_len_bounds_the_prg_section_below_the_file_size() {
        // A file larger than prg_len (PRG+CHR): bytes past prg_len are CHR and
        // never read as PRG, even though they are set in the file.
        let cdl = FlatMaskCdl::fceux(vec![0x01, 0x01, 0x01, 0x01], 2).unwrap();
        assert_eq!(cdl.prg_len(), 2);
        assert_eq!(cdl.prg_class(1), (true, false));
        assert_eq!(cdl.prg_class(2), (false, false)); // CHR region
        assert_eq!(cdl.prg_class(99), (false, false)); // past the file
    }

    #[test]
    fn classify_span_covers_the_four_classes() {
        let bytes = vec![0x01, 0x02, 0x03, 0x00];
        let cdl = FlatMaskCdl::fceux(bytes.clone(), bytes.len()).unwrap();
        assert_eq!(classify_span(&cdl, &span("f", 1, 0, 1)), CdlClass::Code);
        assert_eq!(classify_span(&cdl, &span("f", 1, 1, 1)), CdlClass::Data);
        assert_eq!(classify_span(&cdl, &span("f", 1, 2, 1)), CdlClass::Mixed);
        assert_eq!(
            classify_span(&cdl, &span("f", 1, 3, 1)),
            CdlClass::Unaccessed
        );
    }

    #[test]
    fn classify_span_ors_flags_across_its_bytes() {
        // A code byte and a data byte in one span → Mixed.
        let cdl = FlatMaskCdl::fceux(vec![0x01, 0x02], 2).unwrap();
        assert_eq!(classify_span(&cdl, &span("f", 1, 0, 2)), CdlClass::Mixed);
    }

    #[test]
    fn classify_span_entirely_in_chr_is_unaccessed() {
        let cdl = FlatMaskCdl::fceux(vec![0x01, 0x01, 0x01, 0x01], 2).unwrap();
        assert_eq!(
            classify_span(&cdl, &span("f", 1, 2, 2)),
            CdlClass::Unaccessed
        );
    }

    #[test]
    fn build_report_aggregates_lines_and_counts() {
        let bytes = vec![0x01, 0x02, 0x03, 0x00];
        let cdl = FlatMaskCdl::fceux(bytes.clone(), bytes.len()).unwrap();
        let map = SourceMap {
            spans: vec![
                span("a.asm", 3, 0, 1), // code
                span("a.asm", 4, 1, 1), // data
                span("a.asm", 5, 2, 1), // mixed
                span("a.asm", 6, 3, 1), // unaccessed
            ],
        };
        let report = build_report(&map, &cdl);
        assert_eq!(report.files.len(), 1);
        let f = &report.files[0];
        assert_eq!(f.path, "a.asm");
        assert_eq!((f.code, f.data, f.mixed, f.unaccessed), (1, 1, 1, 1));
        assert_eq!(
            f.lines,
            vec![
                LineCoverage {
                    line: 3,
                    class: CdlClass::Code
                },
                LineCoverage {
                    line: 4,
                    class: CdlClass::Data
                },
                LineCoverage {
                    line: 5,
                    class: CdlClass::Mixed
                },
                LineCoverage {
                    line: 6,
                    class: CdlClass::Unaccessed
                },
            ]
        );
    }

    #[test]
    fn build_report_ors_multiple_spans_on_one_line() {
        // Two spans on line 10: one code byte, one data byte → the line is Mixed.
        let cdl = FlatMaskCdl::fceux(vec![0x01, 0x02], 2).unwrap();
        let map = SourceMap {
            spans: vec![span("a.asm", 10, 0, 1), span("a.asm", 10, 1, 1)],
        };
        let report = build_report(&map, &cdl);
        assert_eq!(
            report.files[0].lines,
            vec![LineCoverage {
                line: 10,
                class: CdlClass::Mixed
            }]
        );
        assert_eq!(report.files[0].mixed, 1);
    }

    #[test]
    fn build_report_sorts_files_and_omits_chr_only_lines() {
        // prg_len = 2; the span at offset 2 is CHR and is dropped entirely.
        let cdl = FlatMaskCdl::fceux(vec![0x01, 0x02, 0x01, 0x01], 2).unwrap();
        let map = SourceMap {
            spans: vec![
                span("z.asm", 1, 0, 1),
                span("a.asm", 1, 1, 1),
                span("chr.asm", 1, 2, 2), // CHR region → omitted
            ],
        };
        let report = build_report(&map, &cdl);
        let paths: Vec<_> = report.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["a.asm", "z.asm"]);
    }

    #[test]
    fn mesen_masks_differ_from_fceux() {
        // Mesen: code = Code|JumpTarget|SubEntryPoint (0x0D), data = Data (0x02);
        // no PCM/indirect bits. So 0x04 and 0x08 are code (they are not in FCEUX),
        // and FCEUX's PCM bit 0x40 is *not* data under Mesen.
        let bytes = vec![0x01, 0x04, 0x08, 0x02, 0x40];
        let cdl = FlatMaskCdl::mesen(bytes.clone(), bytes.len()).unwrap();
        assert_eq!(cdl.prg_class(0), (true, false)); // Code
        assert_eq!(cdl.prg_class(1), (true, false)); // JumpTarget → code
        assert_eq!(cdl.prg_class(2), (true, false)); // SubEntryPoint → code
        assert_eq!(cdl.prg_class(3), (false, true)); // Data
        assert_eq!(cdl.prg_class(4), (false, false)); // 0x40 means nothing to Mesen

        // The same byte 0x04 is a (ignored) bank bit to FCEUX — not code.
        let fceux = FlatMaskCdl::fceux(vec![0x04], 1).unwrap();
        assert_eq!(fceux.prg_class(0), (false, false));
    }

    /// A small two-line report: line 3 code (hit), line 4 unaccessed (miss).
    fn small_report() -> CoverageReport {
        let cdl = FlatMaskCdl::fceux(vec![0x01, 0x00], 2).unwrap();
        let map = SourceMap {
            spans: vec![span("a.asm", 3, 0, 1), span("a.asm", 4, 1, 1)],
        };
        build_report(&map, &cdl)
    }

    #[test]
    fn to_lcov_emits_da_lf_lh_records() {
        assert_eq!(
            small_report().to_lcov(),
            "SF:a.asm\nDA:3,1\nDA:4,0\nLF:2\nLH:1\nend_of_record\n"
        );
    }

    #[test]
    fn to_json_is_valid_and_carries_class_and_totals() {
        let json = small_report().to_json();
        let v: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(v["files"][0]["path"], "a.asm");
        assert_eq!(v["files"][0]["lines"][0]["line"], 3);
        assert_eq!(v["files"][0]["lines"][0]["class"], "code");
        assert_eq!(v["files"][0]["lines"][1]["class"], "unaccessed");
        assert_eq!(v["totals"]["covered"], 1);
        assert_eq!(v["totals"]["total"], 2);
    }

    #[test]
    fn to_json_escapes_paths_and_handles_empty() {
        // A path with a quote and a backslash must still parse; an empty report
        // and a file with no lines must both be valid JSON.
        let report = CoverageReport {
            files: vec![FileCoverage {
                path: r#"a"b\c.asm"#.to_string(),
                lines: Vec::new(),
                code: 0,
                data: 0,
                mixed: 0,
                unaccessed: 0,
                ignored: 0,
            }],
            ignored_files: 0,
        };
        let v: serde_json::Value = serde_json::from_str(&report.to_json()).expect("valid JSON");
        assert_eq!(v["files"][0]["path"], r#"a"b\c.asm"#);

        let empty: serde_json::Value =
            serde_json::from_str(&CoverageReport::default().to_json()).expect("valid JSON");
        assert_eq!(empty["files"].as_array().unwrap().len(), 0);
        assert_eq!(empty["totals"]["total"], 0);
    }

    #[test]
    fn from_line_hits_maps_executed_to_code() {
        let f =
            FileCoverage::from_line_hits("s.rhai".to_string(), [(2, true), (4, false), (6, true)]);
        assert_eq!((f.code, f.unaccessed), (2, 1));
        assert_eq!(f.data, 0);
        assert_eq!(f.mixed, 0);
        assert_eq!(
            f.lines,
            vec![
                LineCoverage {
                    line: 2,
                    class: CdlClass::Code
                },
                LineCoverage {
                    line: 4,
                    class: CdlClass::Unaccessed
                },
                LineCoverage {
                    line: 6,
                    class: CdlClass::Code
                },
            ]
        );
    }

    #[test]
    fn totals_sum_across_files() {
        let report = CoverageReport {
            files: vec![
                FileCoverage {
                    path: "a".into(),
                    lines: Vec::new(),
                    code: 2,
                    data: 1,
                    mixed: 0,
                    unaccessed: 3,
                    ignored: 2,
                },
                FileCoverage {
                    path: "b".into(),
                    lines: Vec::new(),
                    code: 1,
                    data: 0,
                    mixed: 1,
                    unaccessed: 0,
                    ignored: 0,
                },
            ],
            ignored_files: 1,
        };
        let t = report.totals();
        assert_eq!((t.code, t.data, t.mixed, t.unaccessed), (3, 1, 1, 3));
        assert_eq!(t.covered(), 5);
        assert_eq!(t.total(), 8);
        // Exclusions are tallied separately and never enter covered/total.
        assert_eq!((t.ignored, t.ignored_files), (2, 1));
    }

    // ── Coverage ignore directives ──────────────────────────────────────────

    /// Resolve `source`'s directives for `file`, using the assembler's notion of
    /// a significant line.
    fn ignores_for(file: &str, source: &str) -> CoverageIgnores {
        let significant = crate::tooling::significant_lines(source);
        let next = |line: u32| {
            significant
                .iter()
                .enumerate()
                .skip(line as usize)
                .find(|(_, &s)| s)
                .map(|(i, _)| (i + 1) as u32)
        };
        let mut ignores = CoverageIgnores::default();
        resolve_ignores(
            file,
            &crate::tooling::scan_directives(source),
            &next,
            &mut ignores,
        );
        ignores
    }

    #[test]
    fn next_line_directive_skips_blank_and_comment_lines() {
        let src =
            "; @nessemble-coverage-ignore-next-line\n; why it is dead\n\n    lda #$00\n    rts\n";
        let ig = ignores_for("a.asm", src);
        assert!(ig.contains("a.asm", 4));
        assert!(!ig.contains("a.asm", 5));
        // A directive at end of file has nothing to exclude.
        assert!(
            ignores_for("a.asm", "    rts\n; @nessemble-coverage-ignore-next-line\n").is_empty()
        );
    }

    #[test]
    fn a_closed_region_excludes_its_span_inclusive() {
        let src = "    nop\n; @nessemble-coverage-ignore start\n    lda #$00\n    rts\n; @nessemble-coverage-ignore end\n    nop\n";
        let ig = ignores_for("a.asm", src);
        assert!(!ig.contains("a.asm", 1));
        for line in 2..=5 {
            assert!(ig.contains("a.asm", line), "line {line}");
        }
        assert!(!ig.contains("a.asm", 6));
    }

    #[test]
    fn an_unclosed_region_runs_to_end_of_file() {
        let ig = ignores_for("a.asm", "; @nessemble-coverage-ignore start\n    nop\n");
        assert!(ig.contains("a.asm", 1));
        assert!(ig.contains("a.asm", 9_999));
        assert!(ig.contains("a.asm", u32::MAX));
    }

    #[test]
    fn unbalanced_region_bounds_are_inert() {
        // An `end` with no `start` excludes nothing…
        assert!(ignores_for("a.asm", "; @nessemble-coverage-ignore end\n    nop\n").is_empty());
        // …and a nested `start` does not restart the region: it stays open from
        // the first one, so the first `end` closes the whole span.
        let src = "; @nessemble-coverage-ignore start\n    nop\n; @nessemble-coverage-ignore start\n    nop\n; @nessemble-coverage-ignore end\n    nop\n";
        let ig = ignores_for("a.asm", src);
        assert!(ig.contains("a.asm", 1) && ig.contains("a.asm", 5));
        assert!(!ig.contains("a.asm", 6));
    }

    #[test]
    fn a_trailing_comment_directive_excludes_nothing() {
        let src = "    lda #$00 ; @nessemble-coverage-ignore-next-line\n    rts\n";
        assert!(ignores_for("a.asm", src).is_empty());
    }

    #[test]
    fn ignores_are_per_file() {
        let ig = ignores_for("a.asm", "; @nessemble-coverage-ignore start\n    nop\n");
        assert!(ig.contains("a.asm", 2));
        assert!(!ig.contains("b.asm", 2), "an include must not inherit it");
    }

    #[test]
    fn build_report_drops_ignored_lines_from_both_sides_of_the_ratio() {
        // Two lines: one covered, one not. Ignoring the uncovered one must lift
        // the ratio to 1/1, not 1/2 — it leaves the denominator too.
        let map = SourceMap {
            spans: vec![span("a.asm", 1, 0, 1), span("a.asm", 2, 1, 1)],
        };
        let cdl = FlatMaskCdl::fceux(vec![0x01, 0x00], 2).unwrap();

        let plain = build_report(&map, &cdl);
        assert_eq!((plain.totals().covered(), plain.totals().total()), (1, 2));

        let mut ignores = CoverageIgnores::default();
        ignores.ignore_line("a.asm", 2);
        let report = build_report_with_ignores(&map, &cdl, &ignores);
        let t = report.totals();
        assert_eq!((t.covered(), t.total(), t.ignored), (1, 1, 1));
        assert_eq!(report.files[0].lines.len(), 1);
        assert_eq!(report.ignored_files, 0);
    }

    #[test]
    fn a_fully_ignored_file_is_dropped_from_the_report() {
        let map = SourceMap {
            spans: vec![span("a.asm", 1, 0, 1), span("b.asm", 1, 1, 1)],
        };
        let cdl = FlatMaskCdl::fceux(vec![0x01, 0x01], 2).unwrap();
        let mut ignores = CoverageIgnores::default();
        ignores.ignore_range("b.asm", 1, u32::MAX);

        let report = build_report_with_ignores(&map, &cdl, &ignores);
        assert_eq!(report.files.len(), 1);
        assert_eq!(report.files[0].path, "a.asm");
        assert_eq!(report.ignored_files, 1);
        // No `SF:` block for a dropped file.
        assert!(!report.to_lcov().contains("b.asm"));
    }

    #[test]
    fn json_reports_the_ignored_counts() {
        let map = SourceMap {
            spans: vec![span("a.asm", 1, 0, 1), span("a.asm", 2, 1, 1)],
        };
        let cdl = FlatMaskCdl::fceux(vec![0x01, 0x00], 2).unwrap();
        let mut ignores = CoverageIgnores::default();
        ignores.ignore_line("a.asm", 2);
        let report = build_report_with_ignores(&map, &cdl, &ignores);

        let v: serde_json::Value = serde_json::from_str(&report.to_json()).expect("valid JSON");
        assert_eq!(v["files"][0]["ignored"], 1);
        assert_eq!(v["totals"]["ignored"], 1);
        assert_eq!(v["totals"]["ignoredFiles"], 0);
        assert_eq!(v["totals"]["total"], 1);
    }

    #[test]
    fn script_line_hits_honor_ignores() {
        let mut ignores = CoverageIgnores::default();
        ignores.ignore_line("s.rhai", 4);
        let f = FileCoverage::from_line_hits_with_ignores(
            "s.rhai".to_string(),
            [(2, true), (4, false), (6, true)],
            &ignores,
        );
        assert_eq!((f.code, f.unaccessed, f.ignored), (2, 0, 1));
        assert_eq!(f.lines.len(), 2);
    }

    #[test]
    fn script_directives_use_line_comments() {
        let src = "// @nessemble-coverage-ignore start\nlet x = 1;\n// @nessemble-coverage-ignore end\nlet y = 2;\n";
        let directives = crate::tooling::scan_line_comment_directives(src, "//");
        let significant: Vec<bool> = src
            .lines()
            .map(|l| {
                let t = l.trim_start();
                !t.is_empty() && !t.starts_with("//")
            })
            .collect();
        let next = |line: u32| {
            significant
                .iter()
                .enumerate()
                .skip(line as usize)
                .find(|(_, &s)| s)
                .map(|(i, _)| (i + 1) as u32)
        };
        let mut ignores = CoverageIgnores::default();
        resolve_ignores("s.rhai", &directives, &next, &mut ignores);
        assert!(ignores.contains("s.rhai", 2));
        assert!(!ignores.contains("s.rhai", 4));
    }
}
