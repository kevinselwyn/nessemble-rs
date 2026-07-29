//! Comment directives address the *tooling*, never the assembler: adding one to
//! a source file must not move a single emitted byte, or change a symbol.
//!
//! This is the guard behind the whole comment-directive family
//! (`plans/009-comment-directives.md`, `plans/010-routine-signatures.md`). The
//! lint-suppression directives are the newest members and the reason this file
//! exists, but it covers every registered spelling — a directive that changed
//! the ROM would be a bug in the lexer, and the lexer is shared.

use nessemble_core::{assemble, Options};

/// The bare program, with no directive comments at all.
const PLAIN: &str = "\
draw:
    LDA #$00
    LDX #$10
    LDY #$20
    RTS

main:
    JSR draw
    RTS
";

/// The same program, saturated with every comment directive nessemble reads.
const ANNOTATED: &str = "\
; @nessemble-lint-ignore-next-line undeclared-clobber, overdeclared-clobber
; @nessemble-param   A  the index
; @nessemble-returns C  set on error
; @nessemble-clobbers A, X, [scratch]
draw:
    LDA #$00
    LDX #$10
    LDY #$20
    RTS

; @nessemble-lint-ignore start
; @nessemble-coverage-ignore-next-line
; @nessemble-format stride=2
main:
    JSR draw
    RTS
; @nessemble-lint-ignore end
";

#[test]
fn comment_directives_do_not_change_the_assembled_bytes() {
    let plain = assemble(PLAIN, &Options::default()).expect("plain assembles");
    let annotated = assemble(ANNOTATED, &Options::default()).expect("annotated assembles");
    assert_eq!(
        plain.rom, annotated.rom,
        "comment directives must never reach the assembler"
    );
}

#[test]
fn comment_directives_do_not_change_the_symbol_table() {
    let plain = assemble(PLAIN, &Options::default()).expect("plain assembles");
    let annotated = assemble(ANNOTATED, &Options::default()).expect("annotated assembles");
    let values = |a: &nessemble_core::Assembly| -> Vec<(String, i64)> {
        let mut out: Vec<(String, i64)> = a
            .symbols
            .iter()
            .map(|s| (s.name.clone(), s.value))
            .collect();
        out.sort();
        out
    };
    assert_eq!(values(&plain), values(&annotated));
}

#[test]
fn an_unknown_directive_is_still_only_a_comment() {
    // The linter reports `@nessemble-lint-ignore-nxt-line`; the assembler must
    // not care that it is misspelled, or that it exists.
    let typo = format!("; @nessemble-lint-ignore-nxt-line\n{PLAIN}");
    let plain = assemble(PLAIN, &Options::default()).expect("plain assembles");
    let with_typo = assemble(&typo, &Options::default()).expect("typo assembles");
    assert_eq!(plain.rom, with_typo.rom);
}
