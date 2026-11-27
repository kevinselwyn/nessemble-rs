//! Build-time assertions: `.assert EXPR[, "message"]`, `.error "message"` and
//! `.out "message"` (`plans/022-assertions.md`).
//!
//! None of the three emits bytes, moves the PC or defines a symbol. `.assert`
//! and `.error` are queued on the final pass and checked once, after it,
//! against the final symbol table; `.out` lines are collected on the final
//! pass for the CLI to print.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use nessemble_core::{
    assemble, assemble_file_with_out, assemble_with, assemble_with_out, diagnose_source_as,
    diagnose_source_with, lenient_custom_resolver, render_list_file, Assembly, CustomOutput,
    CustomResolver, Diag, Options,
};

/// Raw output: `.org $C000` is offset 0, and labels read as offsets.
fn raw() -> Options {
    Options::default()
}

fn assembly(src: &str) -> Assembly {
    assemble(src, &raw()).unwrap_or_else(|e| panic!("assembles: {e}"))
}

fn bytes(src: &str) -> Vec<u8> {
    assembly(src).rom
}

/// The diagnostic assembling `src` fails with.
fn failure(src: &str) -> Diag {
    assemble(src, &raw()).expect_err("should not assemble").0
}

/// The (line, message) assembling `src` fails with.
fn error(src: &str) -> (u32, String) {
    let d = failure(src);
    (d.line, d.message)
}

/// Every error `src` reports in collect mode.
fn diags(src: &str) -> Vec<Diag> {
    diagnose_source_as(Path::new("t.asm"), src, &raw()).errors
}

fn errors(src: &str) -> Vec<(u32, String)> {
    diags(src)
        .into_iter()
        .map(|d| (d.line, d.message))
        .collect()
}

/// The `.out` lines `src` prints.
fn out(src: &str) -> Vec<String> {
    let outcome = assemble_with_out(src, &raw(), no_custom());
    outcome.out.into_iter().map(|d| d.message).collect()
}

const NO_ERRORS: [(u32, String); 0] = [];
const NO_OUT: [String; 0] = [];

fn no_custom() -> CustomResolver {
    Box::new(|name, _, _, _, _| Err(format!("unknown `.{name}`")))
}

/// `src` with every `.assert`/`.error`/`.out` line removed.
fn without_checks(src: &str) -> String {
    let mut out = String::new();
    for l in src.lines() {
        let t = l.trim_start();
        if !(t.starts_with(".assert") || t.starts_with(".error") || t.starts_with(".out")) {
            out.push_str(l);
            out.push('\n');
        }
    }
    out
}

// -- basics -------------------------------------------------------------------

#[test]
fn a_passing_assertion_emits_nothing() {
    let src = "\
    .db 0
start:
    .db 1, 2, 3
    .assert start == 1, \"start is fixed\"
    .assert 1
end:
    .db 4
";
    assert_eq!(bytes(src), bytes(&without_checks(src)));
    assert_eq!(bytes(src), vec![0, 1, 2, 3, 4]);
}

#[test]
fn a_failing_assertion_reports_the_expression_values_and_names() {
    let src = "\
KIND_LAST = $07
ptr_table:
    .dw 1, 2, 3, 4, 5, 6, 7
kind_table:
    .assert (kind_table - ptr_table) == (2 * (KIND_LAST + 1)), \"one pointer per KIND_*\"
";
    assert_eq!(
        error(src),
        (
            5,
            "assertion failed: one pointer per KIND_* — `(kind_table - ptr_table) == (2 * (KIND_LAST + 1))` \
             is 0 ($0E == $10; kind_table = $0E, ptr_table = 0, KIND_LAST = 7)"
                .to_string()
        )
    );
}

#[test]
fn a_bare_assertion_leaves_out_the_message() {
    assert_eq!(
        error("BIG = 300\n    .assert BIG < 256\n"),
        (
            2,
            "assertion failed: `BIG < 256` is 0 ($012C < $0100; BIG = $012C)".to_string()
        )
    );
    // Literals on both sides: no comparison detail, no names.
    assert_eq!(
        error("    .assert 1 == 2\n"),
        (1, "assertion failed: `1 == 2` is 0".to_string())
    );
    // Not a comparison: names only.
    assert_eq!(
        error("FLAG = 0\n    .assert FLAG\n"),
        (2, "assertion failed: `FLAG` is 0 (FLAG = 0)".to_string())
    );
}

#[test]
fn values_print_as_decimal_below_ten_and_hex_above() {
    let msg =
        error("V1 = 9\nV2 = 10\nV3 = $12345\nV4 = 0 - 3\n    .assert (V1 + V2 + V3 + V4) == 0\n").1;
    assert!(
        msg.ends_with("; V1 = 9, V2 = $0A, V3 = $12345, V4 = -3)"),
        "{msg}"
    );
}

#[test]
fn the_name_list_is_capped_at_eight() {
    let mut src = String::new();
    for n in 0..10 {
        writeln!(src, "N{n} = {n}").unwrap();
    }
    src.push_str("    .assert (N0 + N1 + N2 + N3 + N4 + N5 + N6 + N7 + N8 + N9) == 0\n");
    let msg = error(&src).1;
    assert!(msg.ends_with("N7 = 7, …)"), "{msg}");
    assert!(!msg.contains("N8 ="), "{msg}");
}

#[test]
fn a_forward_reference_passes() {
    let src = "    .assert later == 2\n    .db 0, 0\nlater:\n    .db 1\n";
    assert_eq!(bytes(src), vec![0, 0, 1]);
}

#[test]
fn an_unresolved_symbol_is_its_own_error() {
    assert_eq!(
        error("    .assert NOPE == 1, \"never shown\"\n"),
        (1, "Symbol `NOPE` in `.assert` was not defined".to_string())
    );
    assert_eq!(
        error("    .assert BANK(nowhere) == 0\n"),
        (
            1,
            "Symbol `nowhere` in `.assert` was not defined".to_string()
        )
    );
    assert_eq!(
        error("    .assert \\i == 0\n"),
        (1, "`\\i` can only be used inside a `.for` loop".to_string())
    );
    assert_eq!(
        error("    .assert :+ == 0\n"),
        (1, "No anonymous label for `:+` in `.assert`".to_string())
    );
}

#[test]
fn division_by_zero_stays_zero() {
    assert_eq!(
        errors("ZERO = 0\n    .assert (4 / ZERO) == 0\n    .assert (4 % ZERO) == 0\n"),
        NO_ERRORS
    );
}

// -- macros, loops and conditionals ------------------------------------------

#[test]
fn a_macro_assertion_reports_at_the_body_with_the_call() {
    let src = "\
.macrodef check
    .assert \\1 < 4, \"\\1 is too big (\\# arg, call \\@)\"
.endm
    .macro check, 2
    .macro check, 1 + 5
";
    let d = failure(src);
    assert_eq!(d.line, 2);
    assert_eq!(
        d.message,
        "assertion failed: 1 + 5 is too big (1 arg, call 2) — `\\1 < 4` is 0 (6 < 4)"
    );
    assert_eq!(
        d.notes,
        vec!["in expansion of macro `check` at stdin line 5".to_string()]
    );
}

#[test]
fn a_message_only_argument_counts_toward_the_macros_arity() {
    let src = ".macrodef say\n    .out \"hello \\1 and \\2\"\n.endm\n    .macro say, a, b\n";
    assert_eq!(out(src), vec!["hello a and b".to_string()]);
}

#[test]
fn a_nested_macro_names_each_call_and_its_arguments() {
    let src = "\
.macrodef inner
    .assert \\1 == 0, \"inner got \\1\"
.endm
.macrodef outer
    .macro inner, \\1
.endm
    .macro outer, 3
";
    let d = failure(src);
    assert_eq!(d.line, 2);
    assert!(
        d.message.starts_with("assertion failed: inner got 3 — "),
        "{}",
        d.message
    );
    assert_eq!(d.notes.len(), 2, "{:?}", d.notes);
}

#[test]
fn a_macro_in_an_included_file_names_the_body_file() {
    let dir = TempDir::new();
    dir.write(
        "lib.asm",
        ".macrodef must\n    .assert \\1, \"must\"\n.endm\n",
    );
    let main = dir.write("main.asm", ".include \"lib.asm\"\n    .macro must, 0\n");
    let e = assemble_file_with_out(&main, &raw(), no_custom())
        .result
        .expect_err("fails")
        .0;
    assert_eq!((e.file.as_str(), e.line), ("lib.asm", 2));
    assert_eq!(e.notes.len(), 1);
}

#[test]
fn a_loop_assertion_is_reported_once_naming_the_first_failing_iteration() {
    let src = ".for 0, 5\n    .assert \\i < 3, \"row \\i\"\n.endfor\n";
    assert_eq!(
        errors(src),
        vec![(
            2,
            "assertion failed: row 3 — `\\i < 3` is 0 (3 < 3) (in `.for` iteration `\\i` = 3)"
                .to_string()
        )]
    );
    assert_eq!(
        errors(".for 0, 5\n    .assert \\i < 6\n.endfor\n"),
        NO_ERRORS
    );
}

#[test]
fn pasted_names_and_bank_resolve_and_are_listed() {
    let src = "\
.for 0, 1
row\\i = \\i * 10
.endfor
.org $C000
here:
    .db 0
    .for 0, 1
    .assert (row\\i == 0) & (BANK(here) == 0)
    .endfor
";
    let msg = error(src).1;
    assert!(msg.contains("(row1 = $0A, BANK(here) = 0)"), "{msg}");
}

#[test]
fn checks_in_a_false_branch_are_skipped() {
    let src = "\
.if 0
    .assert 0, \"never\"
    .assert NOPE
    .error \"never\"
    .out \"never\"
.endif
";
    assert_eq!(errors(src), NO_ERRORS);
    assert_eq!(out(src), NO_OUT);
}

#[test]
fn the_strict_test_skips_checks_three_levels_under_a_false_if() {
    // The reference quirk assembles `.db 7` here; the checks still skip.
    let src = ".if 0\n.if 1\n.if 1\n    .db 7\n    .assert 0\n    .error \"deep\"\n.endif\n.endif\n.endif\n";
    assert_eq!(bytes(src), vec![7]);
    assert_eq!(errors(src), NO_ERRORS);
}

#[test]
fn error_reports_its_message_in_a_true_branch_only() {
    assert_eq!(
        error(
            "LEVEL_TYPE = 5\n.if LEVEL_TYPE > 4\n    .error \"unsupported LEVEL_TYPE\"\n.endif\n"
        ),
        (3, "unsupported LEVEL_TYPE".to_string())
    );
    assert_eq!(
        errors("LEVEL_TYPE = 2\n.if LEVEL_TYPE > 4\n    .error \"no\"\n.endif\n"),
        NO_ERRORS
    );
    assert_eq!(error("    .error \"\"\n").1, "`.error` reached");
}

#[test]
fn error_follows_the_final_pass_branch() {
    // On pass 1 `LATER` reads as 1, so the `.if` is false there; pass 2 sees
    // 3 and takes the branch.
    let src = ".if LATER == 3\n    .error \"took it\"\n.endif\nLATER = 3\n";
    assert_eq!(error(src), (2, "took it".to_string()));
}

// -- .out ---------------------------------------------------------------------

#[test]
fn out_prints_once_per_build_and_once_per_iteration() {
    assert_eq!(
        out("    .out \"building bank 3\"\n"),
        vec!["building bank 3"]
    );
    assert_eq!(
        out(".for 1, 3\n    .out \"row \\i\"\n.endfor\n"),
        vec!["row 1", "row 2", "row 3"]
    );
    // Outside a loop `\i` is left as written.
    assert_eq!(out("    .out \"C:\\items\"\n"), vec!["C:\\items"]);
    assert_eq!(out(".if 0\n    .out \"no\"\n.endif\n"), NO_OUT);
}

#[test]
fn out_lines_survive_a_failure_later_in_the_final_pass() {
    let outcome = assemble_with_out("    .out \"before\"\n    .db NOPE\n", &raw(), no_custom());
    assert!(outcome.result.is_err());
    assert_eq!(outcome.out.len(), 1);
}

// -- once per build ------------------------------------------------------------

#[test]
fn a_failure_is_reported_once() {
    let src = "    .assert 0\n";
    assert_eq!(diags(src).len(), 1);
    assert!(assemble(src, &raw()).is_err());
}

#[test]
fn collect_mode_reports_every_failure_and_other_errors() {
    let src = "    .assert 0, \"a\"\n    .db NOPE\n    .assert 0, \"b\"\n";
    let lines: Vec<u32> = diags(src).iter().map(|d| d.line).collect();
    assert_eq!(lines, vec![2, 1, 3]);
}

#[test]
fn a_final_pass_error_wins_over_the_assertions() {
    let src = "    .assert 0\n    .db NOPE\n";
    assert_eq!(error(src), (2, "Symbol `NOPE` was not defined".to_string()));
}

#[test]
fn an_assertion_and_a_table_check_are_both_reported() {
    let src = "\
    .assert 0, \"first\"
    .table 3
    [0] .db 0
    [1] .db 1
    .endtable
";
    let found = errors(src);
    assert_eq!(found.len(), 2, "{found:?}");
    assert_eq!(found[0].0, 1);
    assert_eq!(
        found[1],
        (2, "Table has 2 entries; `.table` expects 3".to_string())
    );
}

// -- values and context ----------------------------------------------------------

#[test]
fn a_phased_label_reads_its_run_time_address() {
    let src = ".org $C000\n.phase $8000\nload_stub:\n    RTS\n.dephase\n    .assert load_stub == $8000, \"other banks call this stub by address\"\n";
    assert_eq!(errors(src), NO_ERRORS);
    assert_eq!(bytes(src), vec![0x60]);
}

#[test]
fn anonymous_labels_resolve_against_the_captured_pc() {
    let src = "\
:
    .db 0
    .assert :- == 0
    .assert :+ == 2
    .db 0
:
    .db 0
";
    assert_eq!(errors(src), NO_ERRORS);
}

#[test]
fn a_reassigned_constant_is_read_at_its_final_value() {
    let src = "VAL = 1\n    .assert VAL == 2\nVAL = 2\n";
    assert_eq!(errors(src), NO_ERRORS);
}

#[test]
fn local_labels_resolve_in_the_scope_they_were_written_in() {
    let src = "first:\n    .db 0\n@x:\n    .assert @x == 1\nsecond:\n@x:\n    .db 0\n";
    assert_eq!(errors(src), NO_ERRORS);
}

#[test]
fn unary_operators_are_quoted_as_written() {
    assert_eq!(errors("    .assert -1 == ~0\n"), NO_ERRORS);
    assert_eq!(
        error("    .assert !1 == - 2\n").1,
        "assertion failed: `!1 == - 2` is 0 (0 == -2)"
    );
}

// -- byte identity ----------------------------------------------------------------

#[test]
fn an_assertion_does_not_register_a_name_for_ifdef() {
    let src =
        "    .assert FUTURE == 1\n.ifdef FUTURE\n    .db 1\n.else\n    .db 2\n.endif\nFUTURE = 1\n";
    assert_eq!(bytes(src), bytes(&without_checks(src)));
}

#[test]
fn the_list_file_is_unchanged() {
    let src = "ONE = 1\nmain:\n    .assert main == 0\n    .assert ONE\n    .out \"x\"\n    RTS\n";
    for mlist in [false, true] {
        assert_eq!(
            render_list_file(&assembly(src).symbols, mlist),
            render_list_file(&assembly(&without_checks(src)).symbols, mlist)
        );
    }
}

#[test]
fn a_nes_program_assembles_to_the_same_bytes_without_its_checks() {
    let src = "\
    .inesprg 2
    .ineschr 1
    .prg 0
    .org $8000
reset:
    .assert reset == $8000
    .out \"bank 0\"
    LDA #1
    RTS
    .prg 1
    .org $C000
nmi:
    .assert BANK(nmi) == 1, \"nmi lives in bank 1\"
.if 0
    .error \"never\"
.endif
    RTI
";
    let nes = Options {
        nes: true,
        ..Options::default()
    };
    let with = assemble(src, &nes).expect("assembles").rom;
    let without = assemble(&without_checks(src), &nes).expect("assembles").rom;
    assert_eq!(with, without);
}

// -- parse errors -------------------------------------------------------------------

#[test]
fn malformed_directives_are_parse_errors() {
    for (src, message) in [
        ("    .assert\n", "`.assert` needs an expression to check"),
        (
            "    .assert \"only\"\n",
            "`.assert` needs an expression to check",
        ),
        (
            "    .assert 1, 2\n",
            "The `.assert` message must be a quoted string",
        ),
        (
            "    .assert 1,\n    .db 0\n",
            "The `.assert` message must be a quoted string",
        ),
        ("    .error\n", "`.error` needs a quoted message"),
        ("    .out 5\n", "`.out` needs a quoted message"),
    ] {
        assert_eq!(error(src), (1, message.to_string()), "{src:?}");
    }
    let (line, message) = error("    .assert 1, \"m\" extra\n");
    assert_eq!(line, 1);
    assert!(message.contains("extra"), "{message}");
}

// -- message strings ------------------------------------------------------------------

#[test]
fn other_strings_in_a_macro_body_are_not_substituted() {
    let src = ".macrodef text\n    .ascii \"\\2\"\n.endm\n    .macro text\n";
    assert_eq!(bytes(src), b"\\2".to_vec());
}

// -- editor layout approximation ---------------------------------------------------------

fn lenient(src: &str) -> Vec<(u32, String)> {
    let known: HashSet<String> = ["blob".to_string()].into_iter().collect();
    diagnose_source_with(
        Path::new("t.asm"),
        src,
        &raw(),
        None,
        lenient_custom_resolver(known),
    )
    .errors
    .into_iter()
    .map(|d| (d.line, d.message))
    .collect()
}

#[test]
fn a_label_after_an_unmodeled_custom_op_is_not_checked_in_the_editor() {
    let src = "\
.org $C000
    .blob
after:
    .assert after == $C010
    .org $C020
fixed:
    .assert fixed == $C021, \"checked again\"
STRIDE = 4
    .assert STRIDE == 5, \"constants are always checked\"
";
    let found = lenient(src);
    assert_eq!(found.len(), 2, "{found:?}");
    assert_eq!(found[0].0, 7);
    assert_eq!(found[1].0, 9);
}

#[test]
fn a_constant_derived_from_an_approximate_label_is_approximate() {
    let src = ".org $C000\n    .blob\nend:\nSIZE = end - $C000\n    .assert SIZE == 16\n";
    assert_eq!(lenient(src), NO_ERRORS);
}

#[test]
fn table_checks_skip_unmodeled_entries_in_the_editor() {
    let src = "\
    .table 3
    [0] .db 0, 0
    [1] .blob
    [2] .db 0, 0
    .endtable
";
    assert_eq!(lenient(src), NO_ERRORS);
    // The real build still checks the entry the script filled.
    let resolver: CustomResolver = Box::new(|_, _, _, _, _| Ok(CustomOutput::Bytes(vec![1])));
    let e = assemble_with(src, &raw(), resolver)
        .expect_err("size mismatch")
        .0;
    assert_eq!(e.line, 3);
}

// -- helpers --------------------------------------------------------------------------------

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "nessemble-assertions-{}-{}",
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
