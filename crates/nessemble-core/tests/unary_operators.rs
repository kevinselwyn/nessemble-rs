//! Unary operators `-x`, `~x` and `!x` (`plans/018-unary-operators.md`).
//!
//! They bind tighter than every binary operator, stack freely, and are valid
//! anywhere an expression is. Binary grouping (right to left, no precedence) is
//! untouched.

use std::cell::RefCell;
use std::rc::Rc;

use nessemble_core::{
    assemble, assemble_with, AsmRule, Assembly, CustomOutput, CustomResolver, Options,
};

fn raw() -> Options {
    Options {
        nes: false,
        ..Options::default()
    }
}

/// Assemble `src` as a raw binary and return its bytes.
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

/// The bytes of `.db EXPR`.
fn db(expr: &str) -> Vec<u8> {
    bytes(&format!(".db {expr}\n"))
}

fn rules(a: &Assembly) -> Vec<Option<AsmRule>> {
    a.warnings.iter().map(|w| w.rule).collect()
}

#[test]
fn brief_examples() {
    assert_eq!(db("-1"), [0xFF]);
    assert_eq!(db("~1"), [0xFE]);
    assert_eq!(bytes(".if !0\n.db 1\n.endif\n"), [0x01]);
    assert_eq!(bytes(".if !1\n.db 1\n.endif\n"), Vec::<u8>::new());
    assert_eq!(bytes("  LDA #-1\n"), [0xA9, 0xFF]);
    assert_eq!(db("-4"), [0xFC]);
    assert_eq!(bytes(".dw -40\n"), [0xD8, 0xFF]);
    assert_eq!(db("~$0F"), [0xF0]);
}

#[test]
fn binary_spellings_are_unchanged() {
    assert_eq!(db("0 - 4"), [0xFC]);
    assert_eq!(bytes(".dw 0 - 40\n"), [0xD8, 0xFF]);
    assert_eq!(bytes("  LDA #(0 - 1)\n"), [0xA9, 0xFF]);
}

#[test]
fn in_range_negatives_do_not_warn() {
    for src in [".db -4\n", ".dw -40\n", "  LDA #-1\n", ".db ~$0F\n"] {
        assert!(assembly(src).warnings.is_empty(), "{src}");
    }
}

#[test]
fn each_operator_on_each_base() {
    let cases = [
        ("$28", 0xD8, 0xD7),
        ("28h", 0xD8, 0xD7),
        ("%0101", 0xFB, 0xFA),
        ("0101b", 0xFB, 0xFA),
        ("0101", 0xBF, 0xBE),
        ("101o", 0xBF, 0xBE),
        ("65", 0xBF, 0xBE),
        ("65d", 0xBF, 0xBE),
        ("'A'", 0xBF, 0xBE),
    ];
    for (lit, neg, compl) in cases {
        assert_eq!(db(&format!("-{lit}")), [neg], "-{lit}");
        assert_eq!(db(&format!("~{lit}")), [compl], "~{lit}");
        assert_eq!(db(&format!("!{lit}")), [0x00], "!{lit}");
    }
    assert_eq!(db("!0"), [0x01]);
    assert_eq!(db("!%0"), [0x01]);
}

#[test]
fn stacked_operators() {
    let cases = [
        ("- -4", 0x04),
        ("--4", 0x04),
        ("!!5", 0x01),
        ("!!0", 0x00),
        ("~-1", 0x00),
        ("-~0", 0x01),
        ("~~7", 0x07),
        ("-!0", 0xFF),
        ("!-1", 0x00),
        ("- - -4", 0xFC),
    ];
    for (src, want) in cases {
        assert_eq!(db(src), [want], "{src}");
    }
}

#[test]
fn precedence() {
    let cases = [
        ("-2 * 3", 0xFA),
        ("2 * -3", 0xFA),
        ("-(2 * 3)", 0xFA),
        ("4 - -2", 0x06),
        ("-2 - 1", 0xFD),
        // Binary grouping still runs right to left: 20 - ((-2) - 1).
        ("20 - -2 - 1", 0x17),
        ("20 - 2 - 1", 0x13),
        ("2*3 - 1", 0x04),
        ("~0 & $0F", 0x0F),
        ("!0 + 1", 0x02),
        ("!(0 + 1)", 0x00),
        ("-LOW($1234)", 0xCC),
    ];
    for (src, want) in cases {
        assert_eq!(db(src), [want], "{src}");
    }
}

#[test]
fn symbols_and_binary_minus_spellings() {
    assert_eq!(
        bytes("val = 5\n.db val-1, val - 1, (val)-(1), val - -4, -val\n"),
        [0x04, 0x04, 0x04, 0x09, 0xFB]
    );
    let a = assembly("NEG = -40\n.dw NEG\n");
    assert_eq!(a.rom, [0xD8, 0xFF]);
    assert_eq!(sym(&a, "NEG"), -40);
}

#[test]
fn low_high_of_a_negative_matches_the_hand_written_idiom() {
    assert_eq!(
        db("LOW(-40), HIGH(-40)"),
        db("LOW($10000 - 40), HIGH($10000 - 40)")
    );
    assert_eq!(db("LOW(-40), HIGH(-40)"), [0xD8, 0xFF]);
}

#[test]
fn out_of_range_complements_warn_under_the_range_rule() {
    let a = assembly(".db ~$FF\n");
    assert_eq!(a.rom, [0x00]);
    assert_eq!(rules(&a), [Some(AsmRule::ValueOutOfRange)]);

    let a = assembly("  AND #~$80\n");
    assert_eq!(a.rom, [0x29, 0x7F]);
    assert_eq!(rules(&a), [Some(AsmRule::ValueOutOfRange)]);

    // The documented workaround.
    let a = assembly("  AND #LOW(~$80)\n");
    assert_eq!(a.rom, [0x29, 0x7F]);
    assert!(a.warnings.is_empty());
}

#[test]
fn operand_prefixes() {
    assert_eq!(bytes("off = -$10\n  LDA <-off\n"), [0xA5, 0x10]);
    assert_eq!(bytes("off = -$10\n  LDA <-off, X\n"), [0xB5, 0x10]);
    assert_eq!(bytes("  JMP [-1]\n"), [0x6C, 0xFF, 0xFF]);

    // The addressing mode is syntactic: `-1` is out of the absolute range.
    let a = assembly("  LDA -1\n");
    assert_eq!(a.rom, [0xAD, 0xFF, 0xFF]);
    assert_eq!(rules(&a), [Some(AsmRule::ValueOutOfRange)]);
}

#[test]
fn conditionals() {
    assert_eq!(
        bytes("UNSET_FLAG = 0\n.if !UNSET_FLAG\n.db 1\n.endif\n"),
        [0x01]
    );
    assert_eq!(
        bytes("AA = 1\nBB = 2\n.if !(AA == BB)\n.db 1\n.endif\n"),
        [0x01]
    );
    assert_eq!(bytes(".if -1\n.db 1\n.endif\n"), [0x01]);
}

#[test]
fn for_bounds_and_counters() {
    assert_eq!(bytes(".for -2, 0\n.db \\i\n.endfor\n"), [0xFE, 0xFF, 0x00]);
    assert_eq!(bytes(".for 0, -2\n.db \\i\n.endfor\n"), [0x00, 0xFF, 0xFE]);
    assert_eq!(bytes(".for 0, 2\n.db -\\i\n.endfor\n"), [0x00, 0xFF, 0xFE]);
    assert_eq!(bytes(".for 0, 2\n.db ~\\i\n.endfor\n"), [0xFF, 0xFE, 0xFD]);
}

#[test]
fn an_unknown_symbol_under_a_unary_bound_is_named() {
    let (line, msg) = error(".for -LAST, 0\n.db 1\n.endfor\nLAST = 2\n");
    assert_eq!(line, 1);
    assert!(
        msg.contains("not known on the first pass") && msg.contains("LAST"),
        "{msg}"
    );
}

#[test]
fn fill_enum_and_rs() {
    assert_eq!(bytes(".fill 3, -1\n"), [0xFF, 0xFF, 0xFF]);

    let a = assembly(".enum -2\nE0 .rs 1\nE1 .rs 1\n.endenum\n");
    assert_eq!(sym(&a, "E0"), -2);
    assert_eq!(sym(&a, "E1"), -1);
}

#[test]
fn macro_arguments() {
    let emit = ".macrodef EMIT\n.db \\1\n.endm\n";
    assert_eq!(bytes(&format!("{emit}.macro EMIT, -4\n")), [0xFC]);
    assert_eq!(bytes(&format!("{emit}.macro EMIT, ~$0F\n")), [0xF0]);
    // The substituted argument is parenthesised: `10 - (-2)`.
    assert_eq!(
        bytes(".macrodef SUBR\n.db 10 - \\1\n.endm\n.macro SUBR, -2\n"),
        [0x0C]
    );
}

#[test]
fn custom_pseudo_arguments() {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let log = Rc::clone(&seen);
    let resolver: CustomResolver = Box::new(move |_name, ints, texts, _base, _root| {
        log.borrow_mut().push((ints.to_vec(), texts.to_vec()));
        Ok(CustomOutput::Bytes(vec![]))
    });
    assemble_with(".probe -1, ~0, !0, \"s\"\n", &raw(), resolver).expect("assembles");
    let seen = seen.borrow();
    assert!(!seen.is_empty());
    for (ints, texts) in seen.iter() {
        assert_eq!(ints, &[-1, -1, 1]);
        assert_eq!(texts, &["s".to_string()]);
    }
}

/// Documented behaviour, not a goal: `.db 1 -2` is one expression, `1 - 2`.
/// A future lint or fix should change this test on purpose.
#[test]
fn known_gap_space_before_minus_is_still_binary() {
    assert_eq!(db("1 -2"), [0xFF]);
}

#[test]
fn diagnostics() {
    for src in [
        ".db -\n",
        "  LDA #~\n",
        ".if !\n",
        "  LDA !\n",
        ".db -\"s\"\n",
    ] {
        assert_eq!(
            error(src),
            (1, "expected expression".to_string()),
            "{src:?}"
        );
    }
    // On a continued list the failing line is the continuation line.
    assert_eq!(
        error(".db 1,\n  -\n"),
        (2, "expected expression".to_string())
    );
}

/// Anonymous references keep eating their `-` run greedily. Bytes taken from
/// 2.27.0 (spelling the last line `:- + 0 - 1`, which groups the same way).
#[test]
fn anonymous_labels_are_byte_identical() {
    let src = ".org $C000\n:\n  NOP\n:\n  NOP\n:\n  DEX\n  BNE :-\n  BNE :---\n  BCC :+\n  NOP\n:\n  NOP\n  .dw :- + -1\n";
    assert_eq!(
        bytes(src),
        [0xEA, 0xEA, 0xCA, 0xD0, 0xFD, 0xD0, 0xF9, 0x90, 0x01, 0xEA, 0xEA, 0x09, 0x00]
    );
}
