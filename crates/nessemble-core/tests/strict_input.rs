//! Input the assembler used to accept silently and now reports:
//!
//! - a statement after a label on the same line (`name: LDA #1`), which was
//!   dropped — an error;
//! - tokens left over after a complete statement (`JMP p1.loc`, `.db 1 2`),
//!   which were ignored — an error;
//! - a macro call passing fewer arguments than its body uses (the missing ones
//!   became `0`), or more than it uses — an error;
//! - a symbol redefined so that the last definition silently wins — the
//!   `duplicate-symbol` warning;
//! - a value that does not fit the field it is emitted into, which was
//!   truncated — the `value-out-of-range` warning.
//!
//! None of these changes the bytes of a program that assembles without them.

use std::path::Path;

use nessemble_core::{assemble, diagnose_source_as, Assembly, Options};

fn raw() -> Options {
    Options {
        nes: false,
        ..Options::default()
    }
}

fn assembly(src: &str) -> Assembly {
    assemble(src, &raw()).unwrap_or_else(|e| panic!("assembles: {e}"))
}

fn bytes(src: &str) -> Vec<u8> {
    assembly(src).rom
}

/// The error (line, message) assembling `src` fails with.
fn error(src: &str) -> (u32, String) {
    let e = assemble(src, &raw()).expect_err("should not assemble");
    (e.0.line, e.0.message)
}

/// The warnings assembling `src` produces, as (line, message).
fn warnings(src: &str) -> Vec<(u32, String)> {
    assembly(src)
        .warnings
        .into_iter()
        .map(|d| (d.line, d.message))
        .collect()
}

// -- leftover tokens --------------------------------------------------------

#[test]
fn dotted_operand_is_an_error_not_a_silent_jmp() {
    // `JMP p1.loc` used to assemble as `JMP p1`.
    assert_eq!(
        error("p1:\n  NOP\n  JMP p1.loc\n"),
        (
            3,
            "Unexpected `.loc` after the end of the statement".to_string()
        )
    );
}

#[test]
fn data_list_without_a_comma_is_an_error() {
    assert_eq!(
        error(".db 1 2\n"),
        (
            1,
            "Unexpected `2` after the end of the statement".to_string()
        )
    );
    assert_eq!(
        error(".db $10 $20\n"),
        (
            1,
            "Unexpected `$20` after the end of the statement".to_string()
        )
    );
}

#[test]
fn two_instructions_on_one_line_is_an_error() {
    assert_eq!(
        error("  LDA #1 2\n"),
        (
            1,
            "Unexpected `2` after the end of the statement".to_string()
        )
    );
    assert_eq!(
        error("  LDA #1 X\n"),
        (
            1,
            "Unexpected `X` after the end of the statement".to_string()
        )
    );
}

#[test]
fn leftover_tokens_report_the_line_they_are_on() {
    // A continued data list ends on its last line; the junk is reported there.
    assert_eq!(
        error("  NOP\n.db 1,\n  2 3\n"),
        (
            3,
            "Unexpected `3` after the end of the statement".to_string()
        )
    );
}

#[test]
fn complete_statements_still_assemble() {
    assert_eq!(
        bytes(
            "  LDA #1 ; a comment\n.db 1, 2,\n  3\n:\n  NOP\n  BNE :-\n  BEQ :+\n:\n  JMP [$1234]\n"
        ),
        vec![0xa9, 0x01, 1, 2, 3, 0xea, 0xd0, 0xfd, 0xf0, 0x00, 0x6c, 0x34, 0x12]
    );
}

#[test]
fn diagnostics_report_every_leftover_and_keep_going() {
    let d = diagnose_source_as(Path::new("f.asm"), "  LDA #1 2\n  NOP\n.db 1 2\n", &raw());
    let errors: Vec<(u32, &str)> = d
        .errors
        .iter()
        .map(|e| (e.line, e.message.as_str()))
        .collect();
    assert_eq!(
        errors,
        vec![
            (1, "Unexpected `2` after the end of the statement"),
            (3, "Unexpected `2` after the end of the statement"),
        ]
    );
}

// -- label and statement on one line ----------------------------------------

#[test]
fn label_followed_by_an_instruction_is_an_error() {
    // At column 0 this used to define `start` as the constant 1 and drop the
    // instruction.
    let message =
        "Label `start:` must be on a line of its own; move what follows it to the next line";
    assert_eq!(error("start: LDA #1\n"), (1, message.to_string()));
    assert_eq!(error("  NOP\n  start: NOP\n"), (2, message.to_string()));
    assert_eq!(error("start: .db 1\n"), (1, message.to_string()));
}

#[test]
fn pasted_label_followed_by_an_instruction_is_an_error() {
    assert_eq!(
        error(".for 0, 1\nrow\\i: NOP\n.endfor\n"),
        (
            2,
            "Label `row\\i:` must be on a line of its own; move what follows it to the next line"
                .to_string()
        )
    );
}

#[test]
fn label_on_its_own_line_and_anonymous_references_still_work() {
    assert_eq!(
        bytes("start:\n  LDA #1\nrow:\n:\n  NOP\n  BNE :-\n  BCC :+\n:\n  RTS\n"),
        vec![0xa9, 0x01, 0xea, 0xd0, 0xfd, 0x90, 0x00, 0x60]
    );
}

// -- macro argument counts ---------------------------------------------------

const TWO_ARGS: &str = ".macrodef pair\n  .db \\1, \\2\n.endm\n";

#[test]
fn missing_macro_argument_is_an_error_at_the_call() {
    // The missing `\2` used to become `0`.
    assert_eq!(
        error(&format!("{TWO_ARGS}  .macro pair, 1\n")),
        (
            4,
            "Macro `pair` uses `\\2`, but this call passes 1".to_string()
        )
    );
    assert_eq!(
        error(&format!("{TWO_ARGS}  .macro pair\n")),
        (
            4,
            "Macro `pair` uses `\\2`, but this call passes 0".to_string()
        )
    );
}

#[test]
fn extra_macro_argument_is_an_error_at_the_call() {
    assert_eq!(
        error(&format!("{TWO_ARGS}  .macro pair, 1, 2, 3\n")),
        (
            4,
            "Macro `pair` was passed more arguments (3) than its body uses (2)".to_string()
        )
    );
    assert_eq!(
        error(".macrodef nop\n  NOP\n.endm\n  .macro nop, 1\n"),
        (
            4,
            "Macro `nop` was passed more arguments (1) than its body uses (0)".to_string()
        )
    );
}

#[test]
fn matching_macro_calls_still_assemble() {
    assert_eq!(
        bytes(&format!(
            "{TWO_ARGS}  .macro pair, 1, 2\n.macrodef nop\n  NOP\n.endm\n  .macro nop\n"
        )),
        vec![1, 2, 0xea]
    );
}

#[test]
fn a_macro_that_reads_the_argument_count_may_take_extra_arguments() {
    // `\#` lets a body handle a variable count itself.
    assert_eq!(
        bytes(".macrodef count\n  .db \\#, \\1\n.endm\n  .macro count, 7, 8, 9\n"),
        vec![3, 7]
    );
}

// -- duplicate-symbol --------------------------------------------------------

#[test]
fn label_defined_twice_warns_and_the_last_still_wins() {
    let src = "dup:\n  NOP\ndup:\n  NOP\n  JMP dup\n";
    assert_eq!(
        warnings(src),
        vec![(
            3,
            "`dup` is already defined (line 1 of `stdin`); this definition replaces it".to_string()
        )]
    );
    assert_eq!(bytes(src), vec![0xea, 0xea, 0x4c, 0x01, 0x00]);
}

#[test]
fn label_and_constant_with_one_name_warn() {
    assert_eq!(warnings("thing = 3\nthing:\n  NOP\n").len(), 1);
    assert_eq!(warnings("thing:\n  NOP\nthing = 3\n").len(), 1);
    assert_eq!(warnings("var .rs 1\nvar .rs 1\n").len(), 1);
}

#[test]
fn reassigning_a_constant_does_not_warn() {
    // A supported idiom: `.for` bodies rely on it.
    assert!(warnings("cnt = 1\n.db cnt\ncnt = 2\n.db cnt\n").is_empty());
}

#[test]
fn restating_a_symbol_exactly_does_not_warn() {
    // A label written before an `.include` and again as the included file's
    // first line names one address; nothing is replaced.
    assert!(warnings("start:\nstart:\n  NOP\n").is_empty());
}

#[test]
fn a_forward_reference_is_not_a_definition() {
    assert!(warnings("  JMP later\nlater:\n  RTS\n").is_empty());
}

#[test]
fn a_definition_in_a_false_branch_does_not_warn() {
    assert!(warnings("dup:\n  NOP\n.if 0\ndup:\n.endif\n").is_empty());
}

// -- value-out-of-range ------------------------------------------------------

#[test]
fn byte_out_of_range_warns_and_keeps_the_low_byte() {
    let src = ".db 256, 0 - 129\n";
    assert_eq!(
        warnings(src),
        vec![
            (
                1,
                "Value $100 does not fit in a byte (-128 to $FF); only the low 8 bits are kept"
                    .to_string()
            ),
            (
                1,
                "Value -129 does not fit in a byte (-128 to $FF); only the low 8 bits are kept"
                    .to_string()
            ),
        ]
    );
    assert_eq!(bytes(src), vec![0x00, 0x7f]);
}

#[test]
fn byte_range_edges_do_not_warn() {
    assert!(warnings(".db 0 - 128, 255, 0 - 4\n  LDA #(0 - 1)\n  LDA #$FF\n").is_empty());
    assert_eq!(bytes(".db 0 - 4\n"), vec![0xfc]);
}

#[test]
fn word_out_of_range_warns() {
    assert_eq!(
        warnings(".dw $10000\n"),
        vec![(
            1,
            "Value $10000 does not fit in a word (-32768 to $FFFF); only the low 16 bits are kept"
                .to_string()
        )]
    );
    assert!(warnings(".dw 0 - 32768, $FFFF, 0 - 40\n").is_empty());
    assert_eq!(warnings("  LDA $10000\n").len(), 1);
    assert_eq!(warnings("  LDA $10000, X\n").len(), 1);
    assert_eq!(warnings("  JMP [$10000]\n").len(), 1);
}

#[test]
fn immediate_out_of_range_warns() {
    assert_eq!(warnings("  LDA #$100\n").len(), 1);
    assert_eq!(warnings("  LDA #(0 - 129)\n").len(), 1);
}

#[test]
fn zero_page_operand_must_be_0_to_255() {
    let message =
        "Value -1 does not fit in a zero-page operand (0 to $FF); only the low 8 bits are kept";
    assert_eq!(warnings("  LDA <(0 - 1)\n"), vec![(1, message.to_string())]);
    assert_eq!(warnings("  LDA <$100\n").len(), 1);
    assert_eq!(warnings("  LDA <$100, X\n").len(), 1);
    assert_eq!(warnings("  LDA [$100], Y\n").len(), 1);
    assert!(warnings("  LDA <$FF\n  LDA [$FF, X]\n").is_empty());
}

#[test]
fn fill_value_out_of_range_warns() {
    assert_eq!(warnings(".fill 2, $100\n").len(), 1);
    assert!(warnings(".fill 0, $100\n.fill 2\n").is_empty());
}

#[test]
fn byte_selectors_are_always_in_range() {
    assert!(warnings(
        ".db LOW($1234), HIGH($1234)\n.lobytes $1234, 0 - 1\n.hibytes $123456\n  LDA #LOW($ABCD)\n"
    )
    .is_empty());
}

#[test]
fn range_is_checked_against_final_values() {
    // A forward reference is checked on the final pass, once.
    assert_eq!(warnings(".db later\nlater = 300\n").len(), 1);
    assert!(warnings(".db later\nlater = 30\n").is_empty());
}

#[test]
fn a_value_in_a_false_branch_is_not_checked() {
    assert!(warnings(".if 0\n.db 999\n.endif\n").is_empty());
}

#[test]
fn a_value_in_a_loop_warns_once_per_line() {
    assert_eq!(warnings(".for 0, 3\n.db 300\n.endfor\n").len(), 1);
}
