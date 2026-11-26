//! `.table [COUNT | ENUM][, FIRST]` … `.endtable` index-keyed tables
//! (`plans/020-index-keyed-tables.md`).
//!
//! Each entry starts with a `[KEY]` tag that emits nothing; the assembler
//! checks that entry *n* is tagged `FIRST + n·STEP`, that every entry is the
//! same size, and (for a count or enum) that the table has exactly that many
//! entries. Bytes are never reordered, so a tagged table assembles to the same
//! bytes as its comment-indexed original.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use nessemble_core::{
    assemble, assemble_file, assemble_with, diagnose_source_as, render_list_file, Assembly,
    CustomOutput, CustomResolver, Diag, Options,
};

fn raw() -> Options {
    Options {
        nes: false,
        ..Options::default()
    }
}

fn assembly(src: &str) -> Assembly {
    assemble(src, &raw()).unwrap_or_else(|e| panic!("assembles: {e}"))
}

/// Assemble `src` as a raw binary and return its bytes.
fn bytes(src: &str) -> Vec<u8> {
    assembly(src).rom
}

/// The (line, message) assembling `src` fails with.
fn error(src: &str) -> (u32, String) {
    let e = assemble(src, &raw()).expect_err("should not assemble");
    (e.0.line, e.0.message)
}

/// Every error `src` reports in collect mode, as diagnostics.
fn diags(src: &str) -> Vec<Diag> {
    diagnose_source_as(Path::new("t.asm"), src, &raw()).errors
}

/// Every error `src` reports in collect mode, as (line, message).
fn errors(src: &str) -> Vec<(u32, String)> {
    diags(src)
        .into_iter()
        .map(|d| (d.line, d.message))
        .collect()
}

fn one(line: u32, message: &str) -> Vec<(u32, String)> {
    vec![(line, message.to_string())]
}

// -- byte-identical conversion ----------------------------------------------

/// A 40-entry handler table, once comment-indexed and once tagged.
fn opcode_sources() -> (String, String) {
    let mut consts = String::new();
    for n in 0..40 {
        writeln!(consts, "OP_{n:02} = ${n:02X}").unwrap();
    }
    consts.push_str("OP_COUNT = 40\n.org $C000\n");
    let mut handlers = String::new();
    for n in 0..40 {
        writeln!(handlers, "op_{n:02}:\n    RTS").unwrap();
    }
    let mut commented = consts.clone();
    commented.push_str("opcode_table:\n");
    let mut tagged = consts;
    tagged.push_str("opcode_table:\n    .table OP_COUNT\n");
    for n in 0..40 {
        writeln!(commented, "    .dw op_{n:02} ; ${n:02X}").unwrap();
        writeln!(tagged, "    [OP_{n:02}] .dw op_{n:02}").unwrap();
    }
    tagged.push_str("    .endtable\n");
    commented.push_str(&handlers);
    tagged.push_str(&handlers);
    (commented, tagged)
}

#[test]
fn tagged_table_is_byte_identical_to_the_commented_one() {
    let (commented, tagged) = opcode_sources();
    let a = assembly(&commented);
    let b = assembly(&tagged);
    assert_eq!(a.rom, b.rom);
    assert_eq!(a.rom.len(), 80 + 40);
    for mlist in [false, true] {
        assert_eq!(
            render_list_file(&a.symbols, mlist),
            render_list_file(&b.symbols, mlist)
        );
    }
}

#[test]
fn plan_example_assembles() {
    let src = "OP_YIELD = 0\nOP_DRAW = 1\nOP_FRAME = 2\nop_yield = $C000\nop_draw = $C010\nop_frame = $C020\n\
               opcode_table:\n    .table\n    [OP_YIELD]  .dw op_yield\n    [OP_DRAW]   .dw op_draw\n    \
               [OP_FRAME]  .dw op_frame\n    .endtable\n";
    assert_eq!(bytes(src), vec![0x00, 0xC0, 0x10, 0xC0, 0x20, 0xC0]);
}

// -- entry shapes -----------------------------------------------------------

#[test]
fn one_byte_two_byte_and_multi_line_entries() {
    assert_eq!(
        bytes(".table 3\n[0] .db 1\n[1] .db 2\n[2] .db 3\n.endtable\n"),
        vec![1, 2, 3]
    );
    assert_eq!(
        bytes(".table 2\n[0] .dw $1234\n[1] .dw $5678\n.endtable\n"),
        vec![0x34, 0x12, 0x78, 0x56]
    );
    // A 3-line record, and a bare tag line starting the next record.
    let src = ".table 2\n\
               [0] .db 1\n    .db 2\n    .lobytes $0304\n\
               [1]\n    .db 5\n    .db 6\n    .lobytes $0708\n\
               .endtable\n";
    assert_eq!(bytes(src), vec![1, 2, 4, 5, 6, 8]);
}

#[test]
fn labels_constants_and_comments_may_sit_between_entries() {
    let src = ".table\n; a comment\n[0] .db 1\nmid:\nKK = 4\n\n[1] .db 2\n.endtable\n.db mid\n";
    assert_eq!(bytes(src), vec![1, 2, 1]);
}

#[test]
fn a_tag_prefixes_an_instruction_including_an_indirect_jump() {
    let src = "ptr = $0200\n.table\n[0] JMP [ptr]\n[1] JMP $C000\n.endtable\n";
    assert_eq!(bytes(src), vec![0x6C, 0x00, 0x02, 0x4C, 0x00, 0xC0]);
    // At column 0, the statement after a tag is an instruction.
    assert_eq!(bytes(".table\n[0] NOP\n.endtable\n"), vec![0xEA]);
}

#[test]
fn a_tagged_macro_call_is_one_entry() {
    let src = ".macrodef entry\n    .db \\1, \\1+1\n.endm\n\
               .table 2\n    [0] .macro entry, 1\n    [1] .macro entry, 5\n.endtable\n";
    assert_eq!(bytes(src), vec![1, 2, 5, 6]);
    // A body that starts with a blank line.
    let src = ".macrodef entry\n\n    .db \\1\n.endm\n\
               .table 2\n    [0] .macro entry, 1\n    [1] .macro entry, 5\n.endtable\n";
    assert_eq!(bytes(src), vec![1, 5]);
}

#[test]
fn a_tag_inside_a_macro_body_reports_at_the_body_line() {
    let src = ".macrodef row\n    [\\1] .dw \\2\n.endm\n\
               .table\n.macro row, 0, $1234\n.macro row, 2, $5678\n.endtable\n";
    let d = diags(src);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(
        (d[0].line, d[0].message.as_str()),
        (2, "Entry tagged $02 is at index $01")
    );
    assert_eq!(
        d[0].notes,
        vec!["in expansion of macro `row` at t.asm line 6"]
    );
}

#[test]
fn untagged_bytes_from_a_macro_report_the_body_line() {
    let src = ".macrodef pad\n    .db 0\n.endm\n.table\n.macro pad\n[0] .db 1\n.endtable\n";
    let d = diags(src);
    assert_eq!(d.len(), 1, "{d:?}");
    assert_eq!(d[0].line, 2);
    assert!(d[0].message.starts_with("Untagged data inside `.table`"));
    assert_eq!(
        d[0].notes,
        vec!["in expansion of macro `pad` at t.asm line 5"]
    );
}

/// `.emit` returns its integer arguments as bytes; `.gen` returns source.
fn resolver(source: &'static str) -> CustomResolver {
    Box::new(move |name, ints, _texts, _base, _root| match name {
        "emit" => Ok(CustomOutput::Bytes(ints.iter().map(|&i| i as u8).collect())),
        "gen" => Ok(CustomOutput::Source(source.to_string())),
        _ => Err(format!("unknown `.{name}`")),
    })
}

#[test]
fn a_tagged_custom_pseudo_instruction_counts_its_bytes() {
    let src = ".table 2\n[0] .emit 1, 2\n[1] .gen\n.endtable\n";
    let rom = assemble_with(src, &raw(), resolver(".db 3, 4\n"))
        .expect("assembles")
        .rom;
    assert_eq!(rom, vec![1, 2, 3, 4]);
    let e = assemble_with(
        ".table 1\n[0] .emit 1\n[1] .emit 1, 2\n.endtable\n",
        &raw(),
        resolver(""),
    )
    .expect_err("size mismatch");
    assert_eq!(
        e.0.message,
        "Entry at index $01 is 2 bytes; entries in this table are 1"
    );
}

#[test]
fn emitted_source_may_not_contain_tags_or_tables() {
    for (emitted, directive) in [
        ("[0] .db 1\n", "[KEY] tag"),
        (".table\n", ".table"),
        (".endtable\n", ".endtable"),
    ] {
        let e = assemble_with(".gen\n", &raw(), resolver(emitted)).expect_err("rejected");
        assert!(
            e.0.message.contains(&format!("emitted `{directive}`")),
            "{emitted:?}: {}",
            e.0.message
        );
    }
}

// -- bases and counts -------------------------------------------------------

#[test]
fn a_first_key_other_than_zero() {
    let mut ok = String::from(".table 24, 1\n");
    let mut bad = ok.clone();
    for n in 0..24 {
        writeln!(ok, "[{}] .db {n}", n + 1).unwrap();
        writeln!(bad, "[{n}] .db {n}").unwrap();
    }
    ok.push_str(".endtable\n");
    bad.push_str(".endtable\n");
    assert_eq!(bytes(&ok).len(), 24);
    assert_eq!(
        error(&bad),
        (2, "Entry tagged $00 is at index $01".to_string())
    );
}

#[test]
fn counts_must_match_exactly() {
    for head in [".table 3", "NN = 3\n.table NN"] {
        let line = if head.contains('\n') { 2 } else { 1 };
        assert_eq!(
            bytes(&format!(
                "{head}\n[0] .db 0\n[1] .db 1\n[2] .db 2\n.endtable\n"
            ))
            .len(),
            3
        );
        assert_eq!(
            errors(&format!("{head}\n[0] .db 0\n[1] .db 1\n.endtable\n")),
            one(line, "Table has 2 entries; `.table` expects 3")
        );
        assert_eq!(
            errors(&format!(
                "{head}\n[0] .db 0\n[1] .db 1\n[2] .db 2\n[3] .db 3\n.endtable\n"
            )),
            one(line, "Table has 4 entries; `.table` expects 3")
        );
    }
}

#[test]
fn keys_may_be_expressions_and_forward_references() {
    let src = ".table 3, BASE\n[BASE] .db 0\n[BASE + 1] .db 1\n[LAST] .db 2\n.endtable\nBASE = 5\nLAST = 7\n";
    assert_eq!(bytes(src), vec![0, 1, 2]);
}

#[test]
fn a_phased_label_as_a_key_reads_its_run_address() {
    let src = ".phase $0010\nfirst:\n.dephase\n.table 2, $10\n[first] .db 1\n[first+1] .db 2\n.endtable\n";
    assert_eq!(bytes(src), vec![1, 2]);
}

#[test]
fn an_undefined_key_is_reported() {
    let (_, message) = error(".table\n[NOPE] .db 1\n.endtable\n");
    assert!(message.contains("NOPE"), "{message}");
}

// -- loops and conditionals -------------------------------------------------

#[test]
fn a_for_loop_inside_a_table() {
    let src = ".org $C000\n.table 4\n.for 0, 3\n[\\i] .dw row\\i\n.endfor\n.endtable\n\
               .for 0, 3\nrow\\i:\n.endfor\n";
    assert_eq!(bytes(src), vec![8, 0, 8, 0, 8, 0, 8, 0]);
}

#[test]
fn a_wrong_size_iteration_reports_once_with_its_suffix() {
    let src = ".table 4\n.for 0, 3\n[\\i] .db 0\n.if \\i == 2\n.db 0\n.endif\n.endfor\n.endtable\n";
    assert_eq!(
        errors(src),
        one(
            3,
            "Entry at index $02 is 2 bytes; entries in this table are 1 (in `.for` iteration `\\i` = 2)"
        )
    );
}

#[test]
fn an_if_inside_a_table_drops_an_entry() {
    let src =
        "WITH_B = 0\n.table\n[0] .db 0\n.if WITH_B\n[1] .db 1\n.endif\n[2] .db 2\n.endtable\n";
    assert_eq!(errors(src), one(7, "Entry tagged $02 is at index $01"));
    let src =
        "WITH_B = 0\n.table\n[0] .db 0\n.if WITH_B\n[1] .db 1\n.endif\n[1] .db 2\n.endtable\n";
    assert_eq!(bytes(src), vec![0, 2]);
}

#[test]
fn a_table_inside_a_false_if_is_ignored() {
    let src = ".if 0\n.table 5\n[9] .db 1, 2\n[3]\n.endtable\n.endif\n.db 7\n";
    assert_eq!(bytes(src), vec![7]);
}

// -- diagnostics ------------------------------------------------------------

#[test]
fn only_the_first_index_mismatch_is_reported() {
    // Inserting one constant shifts every entry after it.
    let src = "KA = 0\nNEW = 1\nKB = 2\nKC = 3\nKD = 4\n\
               .table\n[KA] .db 0\n[KB] .db 0\n[KC] .db 0\n[KD] .db 0\n.endtable\n";
    assert_eq!(
        errors(src),
        one(8, "Entry tagged `KB` ($02) is at index $01")
    );
}

#[test]
fn every_size_mismatch_is_reported() {
    let src = ".table\n[0] .dw 0\n[1] .db 0\n[2] .dw 0\n[3] .db 0, 0, 0\n.endtable\n";
    assert_eq!(
        errors(src),
        vec![
            (
                3,
                "Entry at index $01 is 1 bytes; entries in this table are 2".to_string()
            ),
            (
                5,
                "Entry at index $03 is 3 bytes; entries in this table are 2".to_string()
            ),
        ]
    );
}

#[test]
fn a_zero_byte_entry() {
    let src = ".table\n[0] .db 0\n[1]\nKK = 1\n[2] .db 2\n.endtable\n";
    assert_eq!(errors(src), one(3, "Entry at index $01 emits no bytes"));
}

#[test]
fn untagged_bytes_before_the_first_tag() {
    let src = ".table\n.db 9, 9\n[0] .db 0\n.endtable\n";
    assert_eq!(
        errors(src),
        one(
            2,
            "Untagged data inside `.table`; start each entry with a `[KEY]` tag"
        )
    );
}

#[test]
fn layout_directives_are_rejected_inside_a_table() {
    let src = ".org $C000\n.table\n[0] .db 0\n.org $C010\n[1] .db 1\n.endtable\n";
    assert_eq!(
        errors(src),
        one(
            4,
            "`.org` can't be used inside `.table`; it moves the location counter"
        )
    );
    // `.phase` is allowed.
    assert_eq!(
        bytes(".table\n.phase $10\n[0] .db 0\n.dephase\n.endtable\n"),
        vec![0]
    );
}

#[test]
fn pairing_errors() {
    assert_eq!(
        error(".table\n[0] .db 0\n.table\n.endtable\n"),
        (
            3,
            "`.table` inside another `.table` (opened on line 1)".to_string()
        )
    );
    assert_eq!(
        error("[0] .db 0\n.table\n"),
        (1, "A `[KEY]` tag must be inside a `.table`".to_string())
    );
    assert_eq!(
        error(".table\n[0] .db 0\n"),
        (1, "`.table` has no matching `.endtable`".to_string())
    );
    assert_eq!(
        error(".db 0\n.endtable\n"),
        (2, "`.endtable` has no open `.table`".to_string())
    );
    assert_eq!(
        error(".db 1\n[0] .db 0\n"),
        (2, "A `[KEY]` tag must be inside a `.table`".to_string())
    );
    assert_eq!(
        error(".for 0, 1\n.table\n.endfor\n.endtable\n"),
        (
            3,
            "A `.table` and a `.for` body must not overlap; close the inner one first".to_string()
        )
    );
    assert_eq!(
        error(".table\n.if 1\n[0] .db 0\n.endtable\n.endif\n"),
        (
            4,
            "A `.table` and an `.if` block must not overlap; close the inner one first".to_string()
        )
    );
}

#[test]
fn what_a_tag_cannot_prefix() {
    assert_eq!(
        error(".table\n[0] foo:\n.endtable\n"),
        (
            2,
            "A `[KEY]` tag can't prefix a label or constant definition".to_string()
        )
    );
    assert_eq!(
        error(".table\n[0] XX = 1\n.endtable\n"),
        (
            2,
            "A `[KEY]` tag can't prefix a label or constant definition".to_string()
        )
    );
    assert_eq!(
        error("[0] .table\n.endtable\n"),
        (1, "A `[KEY]` tag can't prefix `.table`".to_string())
    );
    assert_eq!(
        error(".table\n[0] [1] .db 1\n.endtable\n"),
        (2, "Only one `[KEY]` tag may start a line".to_string())
    );
    assert_eq!(
        error(".table\n[0] .macrodef m\n.endm\n.endtable\n"),
        (2, "A `[KEY]` tag can't prefix `.macrodef`".to_string())
    );
}

#[test]
fn a_tagged_statement_still_checks_its_end() {
    let (line, message) = error(".table\n[0] .db 1 junk\n.endtable\n");
    assert_eq!(line, 2);
    assert!(message.contains("`junk`"), "{message}");
    assert!(message.contains("`.db 1`"), "{message}");
}

#[test]
fn argument_errors() {
    assert_eq!(
        error(".table 1, 2, 3\n.endtable\n"),
        (
            1,
            "`.table` takes at most two arguments (`.table [COUNT | ENUM][, FIRST]`), but 3 were given"
                .to_string()
        )
    );
    assert_eq!(
        errors(".table NOPE\n[0] .db 0\n.endtable\n"),
        one(1, "`NOPE` is neither a constant nor a named enum")
    );
    assert_eq!(
        errors(".table 0\n[0] .db 0\n.endtable\n"),
        one(1, "`.table` expects at least 1 entry, not 0")
    );
    assert_eq!(
        errors(".table\nKK = 1\n.endtable\n"),
        one(3, "`.table` has no entries")
    );
}

#[test]
fn table_errors_are_reported_once_not_once_per_pass() {
    let src = ".table 3\n[0] .db 0\n[2] .dw 0\n.endtable\n";
    assert_eq!(
        errors(src),
        vec![
            (3, "Entry tagged $02 is at index $01".to_string()),
            (
                3,
                "Entry at index $01 is 2 bytes; entries in this table are 1".to_string()
            ),
            (1, "Table has 2 entries; `.table` expects 3".to_string()),
        ]
    );
}

#[test]
fn a_strict_build_reports_the_first_table_error() {
    let src = ".table 3\n[0] .db 0\n[2] .dw 0\n.endtable\n";
    assert_eq!(
        error(src),
        (3, "Entry tagged $02 is at index $01".to_string())
    );
}

#[test]
fn tables_are_listed_for_tooling() {
    let src = "KA = 0\nKB = 1\n.table\n[KA] .dw 0\n[KB] .db 0\n.endtable\n";
    let d = diagnose_source_as(Path::new("t.asm"), src, &raw());
    let rows: Vec<_> = d
        .tables
        .iter()
        .map(|t| (t.line, t.table_line, t.key, t.index, t.size, t.ok))
        .collect();
    assert_eq!(rows, vec![(4, 3, 0, 0, 2, true), (5, 3, 1, 1, 1, false)]);
}

// -- the enum form (Phase 3) ------------------------------------------------

const OP_ENUM: &str = ".enum 0, 1, OP\nOP_YIELD\nOP_DRAW\nOP_FRAME\n.endenum\n";

#[test]
fn an_enum_keyed_table() {
    let src = format!(
        "{OP_ENUM}.table OP\n[OP_YIELD] .db 0\n[OP_DRAW] .db 1\n[OP_FRAME] .db 2\n.endtable\n"
    );
    assert_eq!(bytes(&src), vec![0, 1, 2]);
}

#[test]
fn an_enum_keyed_table_missing_a_member() {
    let src = format!("{OP_ENUM}.table OP\n[OP_YIELD] .db 0\n[OP_DRAW] .db 1\n.endtable\n");
    assert_eq!(
        errors(&src),
        one(6, "Table `OP` is missing `OP_FRAME` ($02)")
    );
}

#[test]
fn an_enum_keyed_table_with_an_extra_entry() {
    let src = format!(
        "{OP_ENUM}.table OP\n[OP_YIELD] .db 0\n[OP_DRAW] .db 1\n[OP_FRAME] .db 2\n[3] .db 3\n.endtable\n"
    );
    assert_eq!(errors(&src), one(6, "Table has 4 entries; `OP` has 3"));
}

#[test]
fn an_enum_wins_over_a_same_named_constant() {
    let src = format!("OP = 10\n{OP_ENUM}.table OP\n[0] .db 0\n[1] .db 1\n[2] .db 2\n.endtable\n");
    assert_eq!(bytes(&src), vec![0, 1, 2]);
}

#[test]
fn a_non_contiguous_enum_is_rejected() {
    let src =
        ".enum 0, 1, EE\nEA\nEB = 5\n.endenum\n.table EE\n[EA] .db 0\n[EB] .db 0\n.endtable\n";
    assert_eq!(errors(src), one(5, "`EE` is not a contiguous enum"));
}

#[test]
fn a_step_two_enum_takes_pre_doubled_keys() {
    let src = ".enum 0, 2, HH\nH_A\nH_B\nH_C\n.endenum\n\
               .table HH\n[H_A] .dw 0\n[H_B] .dw 0\n[H_C] .dw 0\n.endtable\n";
    assert_eq!(bytes(src).len(), 6);
    let src = ".enum 0, 2, HH\nH_A\nH_B\nH_C\n.endenum\n\
               .table HH\n[0] .dw 0\n[1] .dw 0\n[2] .dw 0\n.endtable\n";
    assert_eq!(
        errors(src)[0],
        (8, "Entry tagged $01 is at index $02".to_string())
    );
}

#[test]
fn an_enum_table_from_a_first_member() {
    let src = ".enum 0, 1, TT\nT_NONE\nT_A\nT_B\n.endenum\n.table TT, T_A\n[T_A] .db 1\n[T_B] .db 2\n.endtable\n";
    assert_eq!(bytes(src), vec![1, 2]);
    let src = ".enum 0, 1, TT\nT_NONE\nT_A\nT_B\n.endenum\n.table TT, 7\n[T_A] .db 1\n.endtable\n";
    assert_eq!(errors(src), one(6, "`TT` has no member with value $07"));
}

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "nessemble-tables-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    fn write(&self, name: &str, text: &str) -> PathBuf {
        let p = self.0.join(name);
        std::fs::write(&p, text).expect("write");
        p
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn the_enum_may_be_in_an_earlier_or_later_included_file() {
    let dir = TempDir::new();
    dir.write("ops.asm", OP_ENUM);
    let table = ".table OP\n[OP_YIELD] .db 0\n[OP_DRAW] .db 1\n[OP_FRAME] .db 2\n.endtable\n";
    for main in [
        format!(".include \"ops.asm\"\n{table}"),
        format!("{table}.include \"ops.asm\"\n"),
    ] {
        let path = dir.write("main.asm", &main);
        let rom = assemble_file(&path, &raw()).expect("assembles").rom;
        assert_eq!(rom, vec![0, 1, 2]);
    }
}
