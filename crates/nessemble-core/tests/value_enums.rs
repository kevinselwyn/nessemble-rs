//! Value enums: bare names in `.enum`, `=` counter resets, and the named-enum
//! registry with its `NAME_COUNT` (`plans/019-value-enums.md`).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use nessemble_core::{
    assemble, assemble_file, render_list_file, AsmRule, Assembly, EnumInfo, Options,
};

fn raw() -> Options {
    Options {
        nes: false,
        ..Options::default()
    }
}

fn bytes(src: &str) -> Vec<u8> {
    assembly(src).rom
}

fn assembly(src: &str) -> Assembly {
    assemble(src, &raw()).unwrap_or_else(|e| panic!("assembles: {e}"))
}

/// The error message (and line) assembling `src` fails with.
fn error(src: &str) -> (u32, String) {
    let e = assemble(src, &raw()).expect_err("should not assemble");
    (e.0.line, e.0.message)
}

fn sym(a: &Assembly, name: &str) -> i64 {
    a.symbols
        .iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("symbol `{name}` not found"))
        .value
}

fn has_sym(a: &Assembly, name: &str) -> bool {
    a.symbols.iter().any(|s| s.name == name)
}

fn listing(src: &str) -> String {
    render_list_file(&assembly(src).symbols, false)
}

fn only_enum(a: &Assembly) -> &EnumInfo {
    assert_eq!(a.enums.len(), 1, "registry: {:?}", a.enums);
    &a.enums[0]
}

fn members(e: &EnumInfo) -> Vec<(&str, i64)> {
    e.members
        .iter()
        .map(|m| (m.name.as_str(), m.value))
        .collect()
}

const OP: &str = "\
  .enum $00, 1, OP
OP_YIELD
OP_DRAW
OP_FRAME
  .endenum
";

// -- 1-5: member forms -------------------------------------------------------

#[test]
fn bare_names_take_successive_values() {
    let a = assembly(OP);
    assert_eq!(sym(&a, "OP_YIELD"), 0);
    assert_eq!(sym(&a, "OP_DRAW"), 1);
    assert_eq!(sym(&a, "OP_FRAME"), 2);
    assert_eq!(sym(&a, "OP_COUNT"), 3);
    assert_eq!(
        listing(OP),
        "[constants]\n0000 = OP_YIELD\n0001 = OP_DRAW\n0002 = OP_FRAME\n0003 = OP_COUNT\n"
    );
}

#[test]
fn bare_names_mix_with_rs() {
    let a = assembly(".enum $00\nAA\nBB .rs 2\nCC\n.endenum\n");
    assert_eq!([sym(&a, "AA"), sym(&a, "BB"), sym(&a, "CC")], [0, 1, 3]);
}

#[test]
fn bare_names_honour_the_step() {
    let a = assembly(".enum $10, 2\nAA\nBB\nCC .rs 2\nDD\n.endenum\n");
    assert_eq!(
        [sym(&a, "AA"), sym(&a, "BB"), sym(&a, "CC"), sym(&a, "DD")],
        [0x10, 0x12, 0x14, 0x18]
    );
}

#[test]
fn assignment_resets_the_counter() {
    let a = assembly(".enum 0, 1, EN\nAA\nBB\nCC = $10\nDD\n.endenum\n");
    assert_eq!(
        [sym(&a, "AA"), sym(&a, "BB"), sym(&a, "CC"), sym(&a, "DD")],
        [0, 1, 0x10, 0x11]
    );
    assert_eq!(sym(&a, "EN_COUNT"), 4);
    assert!(!only_enum(&a).contiguous);

    // A reset to exactly the next value keeps the run contiguous.
    let a = assembly(".enum 0, 1, EN\nAA\nBB\nCC = 2\nDD\n.endenum\n");
    assert!(only_enum(&a).contiguous);

    // The reset is followed by one step, not one unit.
    let a = assembly(".enum 0, 2\nCC = $10\nDD\n.endenum\n");
    assert_eq!(sym(&a, "DD"), 0x12);
}

#[test]
fn assignment_inside_an_enum_now_moves_the_counter() {
    // v2.27.0 listed `ZZ` as $0014: `=` did not move the counter.
    let a = assembly(".enum $10, 2\nFOO .rs 1\nBAR .rs 1\nQQ = 5\nZZ .rs 1\n.endenum\n");
    assert_eq!(sym(&a, "QQ"), 5);
    assert_eq!(sym(&a, "ZZ"), 7);
}

// -- 6-8: NAME_COUNT, empty enums, comments -----------------------------------

#[test]
fn count_is_usable_before_and_after_the_enum() {
    let src = format!(".db OP_COUNT\n{OP}.db OP_COUNT\n");
    assert_eq!(bytes(&src), vec![3, 3]);
    assert!(listing(&src).contains("0003 = OP_COUNT\n"));
}

#[test]
fn count_name_is_a_literal_concatenation() {
    let a = assembly(".enum 0, 1, op\nAA\n.endenum\n");
    assert_eq!(sym(&a, "op_COUNT"), 1);
}

#[test]
fn the_enum_name_is_not_a_symbol() {
    let a = assembly(OP);
    assert!(!has_sym(&a, "OP"));
    assert_eq!(
        bytes(&format!("{OP}.ifdef OP\n.db 1\n.else\n.db 2\n.endif\n")),
        vec![2]
    );
    let a = assembly(&format!("{OP}OP = 3\n.db OP\n"));
    assert_eq!(a.rom, vec![3]);
    assert!(a.warnings.is_empty(), "{:?}", a.warnings);
}

#[test]
fn empty_named_enum() {
    let a = assembly(".enum 5, 1, EN\n.endenum\n");
    assert_eq!(sym(&a, "EN_COUNT"), 0);
    let e = only_enum(&a);
    assert_eq!((e.start, e.contiguous, e.count()), (5, true, 0));
}

#[test]
fn comments_on_member_lines() {
    let a = assembly(".enum 4\nOP_A ; c\nOP_B;c\n.endenum\n");
    assert_eq!([sym(&a, "OP_A"), sym(&a, "OP_B")], [4, 5]);
}

// -- 9: registry ---------------------------------------------------------------

#[test]
fn registry_records_each_named_enum() {
    let a = assembly(".enum $20, 2, KK\nAA\nBB .rs 2\nCC\n.endenum\n");
    let e = only_enum(&a);
    assert_eq!(e.name, "KK");
    assert_eq!(members(e), [("AA", 0x20), ("BB", 0x22), ("CC", 0x26)]);
    assert_eq!(
        e.members.iter().map(|m| m.size).collect::<Vec<_>>(),
        [1, 2, 1]
    );
    assert_eq!(
        (e.start, e.step, e.count(), e.contiguous),
        (0x20, 2, 3, false)
    );
    assert_eq!((e.line, e.members[0].line), (1, 2));

    let a = assembly(OP);
    assert!(only_enum(&a).contiguous);
    // Unnamed enums are not recorded.
    assert!(assembly(".enum 0\nAA\n.endenum\n").enums.is_empty());
}

/// A throwaway directory for an `.include` test.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "nessemble-enum-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
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
fn registry_sees_an_enum_from_an_included_file() {
    let dir = TempDir::new();
    dir.write("ops.asm", OP);
    let main = dir.write("main.asm", ".include \"ops.asm\"\n.db OP_COUNT, OP_FRAME\n");
    let a = assemble_file(&main, &raw()).expect("assembles");
    assert_eq!(a.rom, vec![3, 2]);
    let e = only_enum(&a);
    assert_eq!(
        members(e),
        [("OP_YIELD", 0), ("OP_DRAW", 1), ("OP_FRAME", 2)]
    );
}

// -- 10-12: loops, macros, conditionals ---------------------------------------

#[test]
fn pasted_bare_names_in_a_for_loop() {
    let a = assembly(".enum 0, 1, SLOTS\n.for 0, 2\nSL\\i\n.endfor\n.endenum\n");
    assert_eq!([sym(&a, "SL0"), sym(&a, "SL1"), sym(&a, "SL2")], [0, 1, 2]);
    assert_eq!(sym(&a, "SLOTS_COUNT"), 3);
    assert!(only_enum(&a).contiguous);
}

#[test]
fn a_named_enum_in_a_loop_is_defined_twice_once() {
    let (line, msg) = error(".for 0, 1\n.enum 0, 1, EN\nAA\\i\n.endenum\n.endfor\n");
    assert_eq!(line, 2);
    assert_eq!(
        msg,
        "Enum `EN` is already defined on line 2 of `stdin` (in `.for` iteration `\\i` = 1)"
    );
}

#[test]
fn macro_bodies_contribute_members() {
    let src = ".macrodef TWO\nAA\nBB\n.endm\n.enum 0, 1, EN\n.macro TWO\nCC\n.endenum\n";
    let a = assembly(src);
    assert_eq!([sym(&a, "AA"), sym(&a, "BB"), sym(&a, "CC")], [0, 1, 2]);
    assert_eq!(sym(&a, "EN_COUNT"), 3);
}

#[test]
fn a_duplicate_member_from_a_macro_reports_the_call() {
    let src = ".macrodef ONE\nAA\n.endm\n.enum 0, 1, EN\nAA\n.macro ONE\n.endenum\n";
    // Like every diagnostic on an expanded line, it points into the macro
    // body, with a note naming the `.macro` call.
    let d = assemble(src, &raw()).expect_err("should not assemble").0;
    assert_eq!(
        d.message,
        "Enum member `AA` is defined twice in the same `.enum`"
    );
    assert_eq!(d.line, 2);
    assert_eq!(d.notes, ["in expansion of macro `ONE` at stdin line 6"]);
}

#[test]
fn a_suppressed_member_still_advances_the_counter() {
    let a = assembly(".enum 0, 1, EN\nAA\n.if 0\nBB\n.endif\nCC\n.endenum\n");
    assert!(!has_sym(&a, "BB"));
    assert_eq!(sym(&a, "CC"), 2);
    assert_eq!(sym(&a, "EN_COUNT"), 2);
    let e = only_enum(&a);
    assert_eq!(members(e), [("AA", 0), ("CC", 2)]);
    assert!(!e.contiguous);
}

#[test]
fn a_suppressed_named_enum_records_nothing() {
    let a = assembly(".if 0\n.enum 0, 1, EN\nAA\n.endenum\n.endif\n");
    assert!(a.enums.is_empty());
    assert!(!has_sym(&a, "EN_COUNT"));
}

// -- 13: unchanged behaviour --------------------------------------------------

#[test]
fn a_bare_name_outside_an_enum_is_still_one() {
    assert_eq!(listing("FLAG\n"), "[constants]\n0001 = FLAG\n");
}

#[test]
fn indented_instructions_inside_an_enum_still_assemble() {
    assert_eq!(bytes(".enum 0\n  NOP\n.endenum\n"), vec![0xEA]);
}

#[test]
fn a_stray_endenum_is_accepted() {
    assert_eq!(bytes(".endenum\n.db 1\n"), vec![1]);
}

#[test]
fn unnamed_nesting_keeps_its_flat_model() {
    let a = assembly(".enum 0\nAA .rs 1\n.enum 9\nDD .rs 1\n.endenum\nEE:\n.endenum\n");
    assert_eq!(sym(&a, "AA"), 0);
    assert_eq!(sym(&a, "DD"), 9);
    assert_eq!(sym(&a, "EE"), 0);
}

#[test]
fn a_duplicate_rs_member_in_an_unnamed_enum_is_not_an_error() {
    let a = assembly(".enum 0\nAA .rs 1\nAA .rs 1\n.endenum\n");
    assert_eq!(sym(&a, "AA"), 1);
    assert_eq!(a.warnings.len(), 1);
    assert_eq!(a.warnings[0].rule, Some(AsmRule::DuplicateSymbol));
}

// -- 14: diagnostics ----------------------------------------------------------

#[test]
fn e1_a_member_defined_twice() {
    assert_eq!(
        error(".enum 0, 1, EN\nAA\nBB\nAA .rs 1\n.endenum\n"),
        (
            4,
            "Enum member `AA` is defined twice in the same `.enum`".to_string()
        )
    );
    // In an unnamed enum, only a bare definition makes it an error.
    assert_eq!(error(".enum 0\nAA .rs 1\nAA\n.endenum\n").0, 3);
}

#[test]
fn e2_a_member_that_cannot_be_a_name() {
    assert_eq!(
        error(".enum 0\n12\n.endenum\n"),
        (
            2,
            "Expected an `.enum` member name (two or more letters, digits or `_`, not starting with a digit)"
                .to_string()
        )
    );
}

#[test]
fn e3_count_colliding_with_a_symbol_warns() {
    let a = assembly("EN_COUNT = 9\n.enum 0, 1, EN\nAA\n.endenum\n");
    assert_eq!(sym(&a, "EN_COUNT"), 1);
    assert_eq!(a.warnings.len(), 1);
    let w = &a.warnings[0];
    assert_eq!(w.rule, Some(AsmRule::DuplicateSymbol));
    assert_eq!(w.line, 2);
    assert_eq!(
        w.message,
        "`EN_COUNT` is already defined at stdin line 1 (as a constant, $0009); this definition replaces it"
    );
}

#[test]
fn e4_an_enum_name_defined_twice() {
    assert_eq!(
        error(".enum 0, 1, EN\n.endenum\n.enum 5, 1, EN\n.endenum\n"),
        (
            3,
            "Enum `EN` is already defined on line 1 of `stdin`".to_string()
        )
    );
}

#[test]
fn e5_an_enum_name_that_is_not_a_name() {
    let want = "The `.enum` name must be a name, as in `.enum $00, 1, OP`".to_string();
    assert_eq!(error("\n.enum 0, 1, 5\n.endenum\n"), (2, want.clone()));
    assert_eq!(error(".enum 0, 1, \"OP\"\n.endenum\n"), (1, want));
}

#[test]
fn e7_nesting_with_a_named_enum() {
    let want = |line: u32| {
        format!(
            "An `.enum` cannot be opened inside another `.enum` when either is named; close the `.enum` on line {line} with `.endenum` first"
        )
    };
    assert_eq!(
        error(".enum 0, 1, EN\nAA\n.enum 0\n.endenum\n"),
        (3, want(1))
    );
    assert_eq!(error(".enum 0\n.enum 0, 1, EN\n.endenum\n"), (2, want(1)));
}

#[test]
fn e8_an_unterminated_named_enum() {
    assert_eq!(
        error(".db 0\n.enum 0, 1, EN\nAA\n"),
        (2, "`.enum EN` has no matching `.endenum`".to_string())
    );
    // An unterminated unnamed enum stays silent.
    assert_eq!(bytes(".enum 0\nAA\n"), Vec::<u8>::new());
}

#[test]
fn e9_an_indented_member() {
    assert_eq!(
        error(".enum 0, 1, EN\n  OP_DRAW\n.endenum\n"),
        (
            2,
            "Unknown opcode `OP_DRAW` (an `.enum` member must start in the first column)"
                .to_string()
        )
    );
    // Outside an enum, the ordinary message.
    assert_eq!(
        error("  OP_DRAW\n").1,
        "Unknown opcode `OP_DRAW`".to_string()
    );
}

#[test]
fn a_fourth_argument_is_rejected() {
    assert_eq!(error(".enum 0, 1, OP, X\n.endenum\n").0, 1);
}

// -- 15: `.rs` regressions ----------------------------------------------------

#[test]
fn rs_members_are_byte_and_listing_identical() {
    let src = ".enum $10, 2\nFOO .rs 1\nBAR .rs 1\n.endenum\n.db FOO, BAR\n";
    assert_eq!(bytes(src), vec![0x10, 0x12]);
    assert_eq!(listing(src), "[constants]\n0010 = FOO\n0012 = BAR\n");
    let a = assembly(".enum $00\nFOO .rs 2\nBAR .rs 1\n.endenum\n");
    assert_eq!([sym(&a, "FOO"), sym(&a, "BAR")], [0, 2]);
}

#[test]
fn enum_lines_record_no_source_map_spans() {
    let opts = Options {
        source_map: true,
        ..raw()
    };
    let a = assemble(&format!("{OP}.db OP_COUNT\n"), &opts).expect("assembles");
    let map = a.source_map.expect("map");
    let lines: Vec<u32> = map.spans.iter().map(|s| s.line).collect();
    assert_eq!(lines, [6]);
}
