//! `.for START, END` … `.endfor` unrolled loops (`plans/016-unrolled-loops.md`).
//!
//! The directive runs in the assembler: each pass walks the body once per
//! counter value, `\i` / `\j` / `\k` read the loop stack, and a name directly
//! followed by a counter is pasted (`row\i:` defines `row0`, `row1`, ...).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use nessemble_core::{
    assemble, assemble_file, assemble_with, diagnose_source_as, Assembly, CustomOutput,
    CustomResolver, Options,
};

fn raw() -> Options {
    Options {
        nes: false,
        ..Options::default()
    }
}

/// Assemble `src` as a raw binary and return its bytes.
fn bytes(src: &str) -> Vec<u8> {
    assemble(src, &raw())
        .unwrap_or_else(|e| panic!("assembles: {e}"))
        .rom
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

#[test]
fn plan_example_matches_hand_unrolled() {
    let looped = bytes(
        "src_label = $10\n.org $C000\n.for 0, 2\n  LDA src_label+\\i\n  STA $2007\n.endfor\n",
    );
    let by_hand = bytes(
        "src_label = $10\n.org $C000\n  LDA src_label+0\n  STA $2007\n  LDA src_label+1\n  STA $2007\n  LDA src_label+2\n  STA $2007\n",
    );
    assert_eq!(looped, by_hand);
    assert_eq!(looped.len(), 18);
}

#[test]
fn bounds_are_inclusive_and_count_down() {
    assert_eq!(bytes(".for 0, 2\n.db \\i\n.endfor\n"), vec![0, 1, 2]);
    assert_eq!(bytes(".for 3, 0\n.db \\i\n.endfor\n"), vec![3, 2, 1, 0]);
}

#[test]
fn single_iteration_loop() {
    assert_eq!(bytes(".for 3, 3\n.db \\i\n.endfor\n"), vec![3]);
}

#[test]
fn directives_are_case_insensitive_and_may_be_indented() {
    assert_eq!(bytes("  .FOR 1, 2\n    .db \\i\n  .EndFor\n"), vec![1, 2]);
}

#[test]
fn nesting_uses_j_and_k_for_outer_counters() {
    let out = bytes(".for 0, 1\n.for 0, 2\n.db \\i+\\j*10\n.endfor\n.endfor\n");
    assert_eq!(out, vec![0, 1, 2, 10, 11, 12]);
    let out = bytes(
        ".for 0, 1\n.for 0, 1\n.for 0, 1\n.db \\i+(\\j*2)+(\\k*4)\n.endfor\n.endfor\n.endfor\n",
    );
    assert_eq!(out, (0..8).collect::<Vec<u8>>());
}

#[test]
fn bounds_are_expressions_over_constants() {
    let src = "LEN = 4\nFIRST = 2\n.for FIRST, FIRST+LEN-1\n.db \\i\n.endfor\n";
    assert_eq!(bytes(src), vec![2, 3, 4, 5]);
}

#[test]
fn an_inner_bound_may_use_the_outer_counter() {
    // Triangular: 1 + 2 + 3 iterations.
    let out = bytes(".for 0, 2\n.for 0, \\i\n.db \\i\n.endfor\n.endfor\n");
    assert_eq!(out, vec![0, 0, 1, 0, 1, 2]);
}

#[test]
fn counter_works_in_immediates_and_conditions() {
    let out = bytes(
        "\
.for 0, 3
  LDA #\\i*4
  .if \\i == 0
    NOP
  .endif
  .if \\i % 2
    .db $AA
  .else
    .db $BB
  .endif
.endfor
",
    );
    assert_eq!(
        out,
        vec![
            0xA9, 0, 0xEA, 0xBB, //
            0xA9, 4, 0xAA, //
            0xA9, 8, 0xBB, //
            0xA9, 12, 0xAA,
        ]
    );
}

#[test]
fn a_counter_may_follow_a_mnemonic_as_an_address() {
    // `LDA \i` is a mnemonic then an operand, not a pasted name `LDA\i`.
    assert_eq!(
        bytes(".for 1, 2\n  LDA \\i\n.endfor\n"),
        vec![0xAD, 1, 0, 0xAD, 2, 0]
    );
}

#[test]
fn anonymous_labels_and_branches_work_in_a_body() {
    let src = "\
.org $C000
.for 0, 2
  BEQ :+
  NOP
:
  NOP
.endfor
";
    // Each iteration: BEQ +1, NOP, NOP.
    assert_eq!(
        bytes(src),
        vec![0xF0, 0x01, 0xEA, 0xEA, 0xF0, 0x01, 0xEA, 0xEA, 0xF0, 0x01, 0xEA, 0xEA]
    );
}

#[test]
fn pasted_labels_jump_table_matches_hand_written() {
    let looped = bytes(
        "\
.org $C000
jump_table:
.for 0, 2
  .dw row\\i
.endfor
.for 0, 2
row\\i:
  LDA #\\i
  RTS
.endfor
  JSR row1
",
    );
    let by_hand = bytes(
        "\
.org $C000
jump_table:
  .dw row0
  .dw row1
  .dw row2
row0:
  LDA #0
  RTS
row1:
  LDA #1
  RTS
row2:
  LDA #2
  RTS
  JSR row1
",
    );
    assert_eq!(looped, by_hand);
}

#[test]
fn pasted_names_are_usable_outside_the_loop_and_flagged_from_macro() {
    let a = assembly(
        "\
.org $C000
.for 0, 2
row\\i:
  NOP
.endfor
  JMP row2
",
    );
    assert_eq!(sym(&a, "row0"), 0);
    assert_eq!(sym(&a, "row2"), 2);
    assert_eq!(a.rom[3..], [0x4C, 2, 0]);
    for name in ["row0", "row1", "row2"] {
        let s = a.symbols.iter().find(|s| s.name == name).expect("symbol");
        assert!(s.from_macro, "{name} should be flagged from_macro");
        assert!(s.label);
    }
}

#[test]
fn pasted_constants_define_one_constant_per_iteration() {
    let a = assembly(".for 1, 3\nSLOT\\i = \\i*10\n.endfor\n.db SLOT1, SLOT2, SLOT3\n");
    assert_eq!(a.rom, vec![10, 20, 30]);
    assert!(!a.symbols.iter().find(|s| s.name == "SLOT2").unwrap().label);
}

#[test]
fn constants_are_reassigned_each_iteration() {
    // Plain (unpasted) constants behave exactly as if the body were written out
    // N times — including the assembler's existing rule that a constant
    // redefined on consecutive lines reads its *final* value in pass 2.
    let looped = bytes(".for 0, 2\nCUR = \\i+5\n.db CUR\n.endfor\n");
    let by_hand = bytes("CUR = 5\n.db CUR\nCUR = 6\n.db CUR\nCUR = 7\n.db CUR\n");
    assert_eq!(looped, by_hand);
}

#[test]
fn counter_pastes_inside_a_macro_body_invoked_in_a_loop() {
    let src = "\
.org $C000
.macrodef ENTRY
entry\\i:
  LDA #\\1
.endm
.for 0, 1
  .macro ENTRY, \\i+7
.endfor
  JMP entry1
";
    let a = assembly(src);
    assert_eq!(a.rom, vec![0xA9, 7, 0xA9, 8, 0x4C, 2, 0]);
    assert_eq!(sym(&a, "entry0"), 0);
    assert_eq!(sym(&a, "entry1"), 2);
}

#[test]
fn counter_as_a_macro_argument() {
    let src = "\
.macrodef PUT
  LDA #\\1
  STA $2007
.endm
.for 4, 5
  .macro PUT, \\i
.endfor
";
    assert_eq!(
        bytes(src),
        vec![0xA9, 4, 0x8D, 7, 0x20, 0xA9, 5, 0x8D, 7, 0x20]
    );
}

/// A throwaway directory for an `.include` test.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "nessemble-for-{}-{}",
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
fn an_include_inside_a_body_runs_every_iteration() {
    let dir = TempDir::new();
    dir.write("part.asm", "  .db \\i, $FF\n");
    let main = dir.write("main.asm", ".for 1, 3\n.include \"part.asm\"\n.endfor\n");
    let rom = assemble_file(Path::new(&main), &raw())
        .expect("assembles")
        .rom;
    assert_eq!(rom, vec![1, 0xFF, 2, 0xFF, 3, 0xFF]);
}

/// `.emit` returns the integer arguments it is given as bytes.
fn emit_resolver() -> CustomResolver {
    Box::new(|name, ints, _texts, _base, _root| {
        if name == "emit" {
            Ok(CustomOutput::Bytes(ints.iter().map(|&i| i as u8).collect()))
        } else if name == "gen" {
            Ok(CustomOutput::Source(format!(
                ".for 0, 1\n.db {}+\\i\n.endfor\n",
                ints[0]
            )))
        } else {
            Err(format!("unknown `.{name}`"))
        }
    })
}

#[test]
fn a_custom_pseudo_op_sees_a_fresh_argument_each_iteration() {
    let rom = assemble_with(".for 5, 7\n.emit \\i*2\n.endfor\n", &raw(), emit_resolver())
        .expect("assembles")
        .rom;
    assert_eq!(rom, vec![10, 12, 14]);
}

#[test]
fn emitted_source_may_contain_a_loop() {
    let rom = assemble_with(".gen 10\n.gen 20\n", &raw(), emit_resolver())
        .expect("assembles")
        .rom;
    assert_eq!(rom, vec![10, 11, 20, 21]);
}

#[test]
fn a_loop_in_a_false_if_emits_nothing_and_pairs_its_endfor() {
    let src = "\
.db 1
.if 0
.for 0, 2
  .db $EE
  .for 0, 1
    .db $DD
  .endfor
.endfor
.endif
.db 2
";
    assert_eq!(bytes(src), vec![1, 2]);
    // A bound that would be an error is not even evaluated when suppressed.
    assert_eq!(
        bytes(".if 0\n.for 0, $FFFF\n.endfor\n.endif\n.db 9\n"),
        vec![9]
    );
}

// -- diagnostics ------------------------------------------------------------

#[test]
fn error_for_without_endfor() {
    let (line, msg) = error(".db 1\n.for 0, 2\n.db 2\n");
    assert_eq!(
        (line, msg.as_str()),
        (2, "`.for` has no matching `.endfor`")
    );
}

#[test]
fn error_endfor_without_for() {
    let (line, msg) = error(".db 1\n.endfor\n");
    assert_eq!((line, msg.as_str()), (2, "`.endfor` has no open `.for`"));
}

#[test]
fn error_wrong_number_of_bounds() {
    let (line, msg) = error(".db 1\n.for 3\n.endfor\n");
    assert_eq!(line, 2);
    assert!(
        msg.contains("two bounds") && msg.contains("1 were given"),
        "{msg}"
    );
    let (_, msg) = error(".for 1, 2, 3\n.endfor\n");
    assert!(msg.contains("3 were given"), "{msg}");
    let (_, msg) = error(".for\n.endfor\n");
    assert!(msg.contains("0 were given"), "{msg}");
}

#[test]
fn error_forward_referenced_bound() {
    let (line, msg) = error(".db 1\n.for 0, LATER\n.db 2\n.endfor\nLATER = 3\n");
    assert_eq!(line, 2);
    assert!(
        msg.contains("not known on the first pass") && msg.contains("LATER"),
        "{msg}"
    );
    // A label defined further down is a forward reference too.
    let (line, _) = error(".for 0, end_label\n NOP\n.endfor\nend_label:\n");
    assert_eq!(line, 1);
}

#[test]
fn error_iteration_limits() {
    let (line, msg) = error(".db 1\n.for 0, 4096\n.endfor\n");
    assert_eq!(line, 2);
    assert!(msg.contains("4097") && msg.contains("4096"), "{msg}");
    // Exactly the limit is fine.
    assert_eq!(bytes(".for 1, 4096\n.endfor\n.db 1\n"), vec![1]);
    // 256 x 257 = 65792 > 65536; each loop alone is within its limit.
    let (line, msg) = error(".for 1, 256\n.for 1, 257\n.endfor\n.endfor\n");
    assert_eq!(line, 2);
    assert!(msg.contains("65792") && msg.contains("65536"), "{msg}");
    // 256 x 256 is exactly the nest limit.
    assert_eq!(
        bytes(".for 1, 256\n.for 1, 256\n.endfor\n.endfor\n.db 1\n"),
        vec![1]
    );
}

#[test]
fn error_counter_outside_a_loop() {
    let (line, msg) = error(".db 1\n.db \\i\n");
    assert_eq!(line, 2);
    assert!(
        msg.contains("`\\i` can only be used inside a `.for` loop"),
        "{msg}"
    );
}

#[test]
fn error_counter_with_too_few_enclosing_loops() {
    let (line, msg) = error(".for 0, 1\n.db 5\n.db \\j\n.endfor\n");
    assert_eq!(line, 3);
    assert!(
        msg.contains("`\\j` needs 2 enclosing") && msg.contains("only 1 are open"),
        "{msg}"
    );
    let (_, msg) = error(".for 0, 1\n.for 0, 1\n.db \\k\n.endfor\n.endfor\n");
    assert!(
        msg.contains("`\\k` needs 3 enclosing") && msg.contains("only 2"),
        "{msg}"
    );
}

#[test]
fn error_plain_label_in_a_body() {
    let (line, msg) = error(".for 0, 2\n  NOP\nspot:\n  NOP\n.endfor\n");
    assert_eq!(line, 3);
    assert!(
        msg.contains("`spot`") && msg.contains("once per iteration") && msg.contains("spot\\i:"),
        "{msg}"
    );
    // A macro's `label\@` is a plain name too.
    let src = ".macrodef MAC\nhere\\@:\n  NOP\n.endm\n.for 0, 1\n.macro MAC\n.endfor\n";
    let (_, msg) = error(src);
    assert!(msg.contains("once per iteration"), "{msg}");
}

#[test]
fn error_name_pasting_two_counters() {
    let (line, msg) = error(".for 0, 1\n.for 0, 1\ncell\\j\\i:\n.endfor\n.endfor\n");
    assert_eq!(line, 3);
    assert!(msg.contains("more than one loop counter"), "{msg}");
    let (line, _) = error(".for 0, 1\n.for 0, 1\n.dw cell\\j\\i\n.endfor\n.endfor\n");
    assert_eq!(line, 3);
}

#[test]
fn error_pasting_a_negative_counter() {
    let (line, msg) = error(".for 1, 0-1\nrow\\i:\n NOP\n.endfor\n");
    assert_eq!(line, 2);
    assert!(
        msg.contains("negative counter value -1")
            && msg.contains("`row`")
            && msg.contains("(in `.for` iteration `\\i` = -1)"),
        "{msg}"
    );
}

#[test]
fn error_pasted_name_already_defined() {
    let (line, msg) = error("row1:\n NOP\n.for 0, 2\nrow\\i:\n NOP\n.endfor\n");
    assert_eq!(line, 4);
    assert!(
        msg.contains("Pasted name `row1` is already defined"),
        "{msg}"
    );
    assert!(msg.contains("(in `.for` iteration `\\i` = 1)"), "{msg}");
    // Two loops pasting the same names collide as well.
    let (line, msg) =
        error(".for 0, 1\nrow\\i:\n NOP\n.endfor\n.for 1, 2\nrow\\i:\n NOP\n.endfor\n");
    assert_eq!(line, 6);
    assert!(msg.contains("`row1`"), "{msg}");
}

#[test]
fn a_reference_before_the_pasted_definition_is_not_a_collision() {
    // `row1` was merely referenced (forward) before the loop defined it.
    assert_eq!(
        bytes(".org $C000\n.dw row1\n.for 0, 1\nrow\\i:\n .db 0\n.endfor\n"),
        vec![3, 0, 0, 0]
    );
}

#[test]
fn error_macrodef_inside_a_body() {
    let (line, msg) = error(".for 0, 1\n.macrodef MAC\n NOP\n.endm\n.endfor\n");
    assert_eq!(line, 2);
    assert!(msg.contains(".macrodef") && msg.contains(".for"), "{msg}");
}

#[test]
fn error_conditional_straddling_the_endfor() {
    // Opened in the body, closed outside.
    let (line, msg) = error(".for 0, 1\n.if 1\n.db 1\n.endfor\n.endif\n");
    assert_eq!(line, 4);
    assert!(msg.contains("must close in the same body"), "{msg}");
    // Opened outside, closed in the body.
    let (line, msg) = error(".if 1\n.for 0, 1\n.db 1\n.endif\n.endfor\n");
    assert_eq!(line, 5);
    assert!(msg.contains("must close in the same body"), "{msg}");
}

#[test]
fn a_per_iteration_error_is_reported_once_naming_the_first_iteration() {
    // Every iteration branches out of range; the error is one diagnostic, on
    // the branch line, suffixed with the first failing iteration's counter.
    let src = ".org $C000\n.for 5, 7\n  BEQ far\n.endfor\n.fill 300\nfar:\n";
    let diags = diagnose_source_as(Path::new("t.asm"), src, &raw());
    let msgs: Vec<_> = diags
        .errors
        .iter()
        .map(|d| (d.line, d.message.clone()))
        .collect();
    assert_eq!(
        msgs,
        vec![(
            3,
            "Branch address out of range (in `.for` iteration `\\i` = 5)".to_string()
        )]
    );
}

#[test]
fn nested_per_iteration_errors_name_every_counter() {
    let src = ".org $C000\n.for 0, 1\n.for 3, 4\n  BEQ far\n.endfor\n.endfor\n.fill 300\nfar:\n";
    let diags = diagnose_source_as(Path::new("t.asm"), src, &raw());
    assert_eq!(diags.errors.len(), 1, "{:?}", diags.errors);
    assert!(
        diags.errors[0]
            .message
            .ends_with("(in `.for` iteration `\\i` = 3, `\\j` = 0)"),
        "{}",
        diags.errors[0].message
    );
}

#[test]
fn diagnostics_collect_structure_errors_without_running_the_loop() {
    let src = ".for 0, 2\nspot:\n.endfor\n.endfor\n";
    let diags = diagnose_source_as(Path::new("t.asm"), src, &raw());
    let lines: Vec<u32> = diags.errors.iter().map(|d| d.line).collect();
    assert_eq!(lines, vec![2, 4]);
}

#[test]
fn source_map_attributes_body_lines_to_their_real_line() {
    let src = ".org $C000\n.for 0, 1\n  .db \\i, $FF\n.endfor\n";
    let options = Options {
        source_map: true,
        ..raw()
    };
    let a = assemble(src, &options).expect("assembles");
    let map = a.source_map.expect("map");
    assert!(map.spans.iter().all(|s| s.line == 3), "{:?}", map.spans);
    assert_eq!(map.spans.iter().map(|s| s.len).sum::<usize>(), 4);
}

#[test]
fn bounds_are_fixed_on_pass_one_even_if_a_constant_is_reassigned_later() {
    // Pass 2 would read NN = 4 and run 5 iterations, overwriting `end`'s byte.
    let src = "NN = 2\n.for 0, NN\n  .db $AA\n.endfor\nNN = 4\nend:\n  .db end\n";
    assert_eq!(bytes(src), vec![0xAA, 0xAA, 0xAA, 0x03]);
}

#[test]
fn nested_bounds_are_fixed_on_pass_one_after_a_reassigned_constant() {
    let src =
        "NN = 1\n.for 0, NN\n.for 0, NN\n  .db $AA\n.endfor\n.endfor\nNN = 3\nend:\n  .db end\n";
    assert_eq!(bytes(src), vec![0xAA, 0xAA, 0xAA, 0xAA, 0x04]);
}
