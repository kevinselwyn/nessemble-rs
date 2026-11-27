//! Diagnostics for source the assembler used to accept silently
//! (`plans/017-assembler-diagnostics.md`).
//!
//! Three shapes are always errors — a label with content on its line, tokens
//! left over after a complete statement, and a macro called with the wrong
//! number of arguments — and two are warnings a project can configure: a
//! redefined symbol (`duplicate-symbol`, `duplicate-symbol-same-value`) and a
//! value narrowed to fit a field it does not fit (`value-out-of-range`).
//!
//! Every "must keep working" case asserts no diagnostic and the bytes (and,
//! where it matters, the list file) v2.27.0 produced for it.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use nessemble_core::{
    assemble_file, assemble_file_with, diagnose_source_as, render_list_file, AsmRule, AsmRules,
    Assembly, CustomOutput, CustomResolver, Diag, Options, RuleSeverity,
};

/// A fresh directory holding `files` (name → text); returns its path.
fn project(files: &[(&str, &str)]) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "nessemble-diag-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (name, text) in files {
        std::fs::write(dir.join(name), text).unwrap();
    }
    dir
}

fn options(rules: AsmRules) -> Options {
    Options {
        rules,
        ..Options::default()
    }
}

/// Assemble `src` as the file `t.asm` with `rules`.
fn run_with(src: &str, rules: AsmRules) -> Result<Assembly, Diag> {
    let dir = project(&[("t.asm", src)]);
    let out = assemble_file(&dir.join("t.asm"), &options(rules)).map_err(|e| e.0);
    let _ = std::fs::remove_dir_all(&dir);
    out
}

fn run(src: &str) -> Result<Assembly, Diag> {
    run_with(src, AsmRules::default())
}

/// The assembly of `src`, which must succeed with no warnings at all.
fn clean(src: &str) -> Assembly {
    let a = run(src).unwrap_or_else(|d| panic!("unexpected error: {d:?}"));
    assert!(
        a.warnings.is_empty(),
        "unexpected warnings: {:?}",
        a.warnings
    );
    a
}

fn clean_bytes(src: &str) -> Vec<u8> {
    clean(src).rom
}

/// The error `src` fails with.
fn error(src: &str) -> Diag {
    match run(src) {
        Ok(a) => panic!("expected an error, assembled {:02x?}", a.rom),
        Err(d) => d,
    }
}

/// Assert `src` fails on `line` with exactly `message` in `t.asm`, no rule id.
fn assert_error(src: &str, line: u32, message: &str) {
    let d = error(src);
    assert_eq!(
        (d.file.as_str(), d.line, d.message.as_str()),
        ("t.asm", line, message),
        "for source {src:?}"
    );
    assert_eq!(d.rule, None, "always-on errors carry no rule id");
}

/// The single warning `src` produces; asserts it assembled.
fn one_warning(src: &str) -> (Assembly, Diag) {
    let a = run(src).unwrap_or_else(|d| panic!("unexpected error: {d:?}"));
    assert_eq!(a.warnings.len(), 1, "warnings: {:?}", a.warnings);
    let w = a.warnings[0].clone();
    (a, w)
}

fn list(a: &Assembly) -> String {
    render_list_file(&a.symbols, false)
}

// ─── 1. `label: <anything>` ────────────────────────────────────────────────

#[test]
fn label_with_content_is_an_error_for_every_statement_kind() {
    for (src, rest) in [
        ("foo: .db 1\n", ".db 1"),
        ("foo: .db 5, 6\n", ".db 5, 6"),
        ("foo: .dw $BEEF\n", ".dw $BEEF"),
        ("foo: LDA #$07\n", "LDA #$07"),
        ("foo: LDA $1234\n", "LDA $1234"),
        ("foo: NOP\n", "NOP"),
        ("foo: RTS\n", "RTS"),
        ("foo:\t.db 1\n", ".db 1"),
        ("foo: .db 1 ; trailing comment\n", ".db 1"),
    ] {
        assert_error(
            src,
            1,
            &format!("a label must be on a line of its own; move `{rest}` to the next line"),
        );
    }
}

#[test]
fn indented_label_with_content_is_the_same_error() {
    assert_error(
        "    foo: NOP\n",
        1,
        "a label must be on a line of its own; move `NOP` to the next line",
    );
}

#[test]
fn anonymous_label_with_content_is_an_error() {
    // v2.27.0 dropped the NOP, so `BNE :-` branched to itself.
    assert_error(
        "    .db $AA\n: NOP\n    BNE :-\n",
        2,
        "a label must be on a line of its own; move `NOP` to the next line",
    );
}

#[test]
fn pasted_label_with_content_is_an_error() {
    assert_error(
        ".for 0, 1\nlbl\\i: .db $EE\n.endfor\n",
        2,
        "a label must be on a line of its own; move `.db $EE` to the next line",
    );
}

#[test]
fn macro_label_with_content_reports_the_body_line_and_the_call() {
    let d = error(".macrodef MAC\nlab\\@: NOP\n.endm\n.macro MAC\n");
    assert_eq!(
        (d.file.as_str(), d.line, d.message.as_str()),
        (
            "t.asm",
            2,
            "a label must be on a line of its own; move `NOP` to the next line"
        )
    );
    assert_eq!(d.notes, vec!["in expansion of macro `MAC` at t.asm line 4"]);
}

#[test]
fn label_alone_on_its_line_still_works() {
    let a = clean("foo:\n    .db 1\n");
    assert_eq!(a.rom, vec![1]);
    assert_eq!(list(&a), "[labels]\n00/0000 = foo\n");

    let a = clean("    .db 0\nfoo:   ; a comment\n    .db 1\n");
    assert_eq!(a.rom, vec![0, 1]);
    assert_eq!(list(&a), "[labels]\n00/0001 = foo\n");

    let a = clean("    .db 0\nfoo :\n    NOP\n");
    assert_eq!(a.rom, vec![0, 0xEA]);
    assert_eq!(list(&a), "[labels]\n00/0001 = foo\n");
}

#[test]
fn anonymous_labels_and_constants_still_work() {
    assert_eq!(
        clean_bytes("    .db $AA\n:\n    NOP\n    BNE :-\n"),
        vec![0xAA, 0xEA, 0xD0, 0xFD]
    );
    let a = clean("NAME = $12\n    .db NAME\n");
    assert_eq!(a.rom, vec![0x12]);
    assert_eq!(list(&a), "[constants]\n0012 = NAME\n");
    // An indented branch to an anonymous label is an instruction, not a label.
    assert_eq!(
        clean_bytes("    BEQ :+\n    NOP\n:\n    RTS\n"),
        vec![0xF0, 0x01, 0xEA, 0x60]
    );
}

// ─── 2. Tokens left over after a complete statement ────────────────────────

#[test]
fn leftover_tokens_are_an_error() {
    for (src, message) in [
        (
            ".db 1 2 3\n",
            "unexpected `2` after `.db 1`; separate values with commas",
        ),
        (
            ".dw $1234 $5678\n",
            "unexpected `$5678` after `.dw $1234`; separate values with commas",
        ),
        (
            "    .byte 1 2\n",
            "unexpected `2` after `.byte 1`; separate values with commas",
        ),
        (
            "    .word 1 2\n",
            "unexpected `2` after `.word 1`; separate values with commas",
        ),
        (
            "    .lobytes 1 2\n",
            "unexpected `2` after `.lobytes 1`; separate values with commas",
        ),
        (
            "    .hibytes 1 2\n",
            "unexpected `2` after `.hibytes 1`; separate values with commas",
        ),
        (".db $01, $02 )\n", "unexpected `)` after `.db $01, $02`"),
        (".db 0 ? 5 : 7\n", "unexpected `?` after `.db 0`"),
        ("    LDA #$01 $02\n", "unexpected `$02` after `LDA #$01`"),
        (
            "    LDA $10 garbage\n",
            "unexpected `garbage` after `LDA $10`",
        ),
        ("QQ = 1 2\n    .db QQ\n", "unexpected `2` after `QQ = 1`"),
    ] {
        assert_error(src, 1, message);
    }
}

#[test]
fn an_unindented_instruction_gets_an_indentation_hint() {
    assert_error(
        "LDA #1\n",
        1,
        "unexpected `#` after `LDA`; indent an instruction, or it is read as a constant definition",
    );
}

#[test]
fn leftover_tokens_in_a_macro_body_report_the_body_line() {
    let d = error(".macrodef MAC\n    .db \\1 2\n.endm\n    .macro MAC, 5\n");
    assert_eq!(
        (d.line, d.message.as_str()),
        (
            2,
            "unexpected `2` after `.db \\1`; separate values with commas"
        )
    );
    assert_eq!(d.notes, vec!["in expansion of macro `MAC` at t.asm line 4"]);
}

#[test]
fn trailing_comments_are_not_content() {
    let src = "\
NAME = 1 ; c
label: ; c
    .db 1, 2 ; c
    .dw $1234 ; c
    LDA #1 ; c
    LDA $10, X ; c
    LDA [$10], Y ; c
    ASL A ; c
    NOP ; c
    .org $C010 ; c
    .if 1 ; c
    .endif ; c
";
    let a = clean(src);
    assert_eq!(
        a.rom,
        vec![1, 2, 0x34, 0x12, 0xA9, 1, 0xBD, 0x10, 0x00, 0xB1, 0x10, 0x0A, 0xEA]
    );
}

#[test]
fn continuation_forms_still_work() {
    assert_eq!(
        clean_bytes("    .db $00, $01,  ; low\n        $02, $03\n"),
        vec![0, 1, 2, 3]
    );
    let defchr = clean_bytes(
        ".defchr 00000000,\n        11111111,\n        22222222,\n        33333333,\n        \
         00000000,\n        11111111,\n        22222222,\n        33333333\n",
    );
    assert_eq!(defchr.len(), 16);
}

/// A resolver for `.metasprite` that emits its integer arguments as bytes.
fn metasprite_resolver(calls: &'static AtomicU32) -> CustomResolver {
    Box::new(move |name, ints, _texts, _base, _root| {
        calls.fetch_add(1, Ordering::Relaxed);
        if name == "metasprite" {
            Ok(CustomOutput::Bytes(ints.iter().map(|&v| v as u8).collect()))
        } else {
            Err(format!("unknown `.{name}`"))
        }
    })
}

#[test]
fn a_twelve_line_custom_pseudo_call_still_receives_every_argument() {
    static CALLS: AtomicU32 = AtomicU32::new(0);
    let mut src = String::from("    .metasprite $00,\n");
    for i in 1..12 {
        let sep = if i == 11 { "" } else { "," };
        writeln!(src, "                ${i:02X}{sep}").unwrap();
    }
    let dir = project(&[("t.asm", &src)]);
    let a = assemble_file_with(
        &dir.join("t.asm"),
        &Options::default(),
        metasprite_resolver(&CALLS),
    )
    .unwrap();
    assert_eq!(a.rom, (0u8..12).collect::<Vec<_>>());
    assert!(a.warnings.is_empty(), "{:?}", a.warnings);
}

// ─── 3. Macro argument count ───────────────────────────────────────────────

#[test]
fn too_few_macro_arguments_is_an_error_at_the_call() {
    let d = error(".macrodef MAC\n    .db \\1, \\2, \\3\n.endm\n    .macro MAC, $05\n");
    assert_eq!(
        (d.file.as_str(), d.line, d.message.as_str()),
        (
            "t.asm",
            4,
            "macro `MAC` takes 3 arguments (it uses \\1-\\3), but this call passes 1"
        )
    );
    assert_eq!(d.notes, vec!["`MAC` is defined at t.asm line 1"]);
    assert_eq!(d.rule, None);
}

#[test]
fn arity_counts_the_highest_index_across_directives() {
    assert_error(
        ".macrodef MAC\n    .db \\1\n    .dw \\2\n.endm\n    .macro MAC, $05\n",
        5,
        "macro `MAC` takes 2 arguments (it uses \\1-\\2), but this call passes 1",
    );
    assert_error(
        ".macrodef MAC\n    .db \\1\n.endm\n    .macro MAC\n",
        4,
        "macro `MAC` takes 1 argument (it uses \\1), but this call passes 0",
    );
}

#[test]
fn too_many_macro_arguments_is_an_error() {
    assert_error(
        ".macrodef MAC\n    .db \\1\n.endm\n    .macro MAC, $05, $06, $07\n",
        4,
        "macro `MAC` takes 1 argument (it uses \\1), but this call passes 3",
    );
    assert_error(
        ".macrodef MAC\n    NOP\n.endm\n    .macro MAC, 1\n",
        4,
        "macro `MAC` takes no arguments, but this call passes 1",
    );
}

#[test]
fn an_index_used_only_inside_an_if_still_counts() {
    assert_error(
        ".macrodef MAC\n    .db \\1\n.if 0\n    .db \\2\n.endif\n.endm\n    .macro MAC, 1\n",
        7,
        "macro `MAC` takes 2 arguments (it uses \\1-\\2), but this call passes 1",
    );
}

#[test]
fn variadic_macros_and_non_positional_tokens_are_exempt() {
    // `\#` opts out of both checks.
    assert_eq!(
        clean_bytes(".macrodef MAC\n    .db \\#\n.endm\n    .macro MAC, 1, 2\n"),
        vec![2]
    );
    assert_eq!(
        clean_bytes(
            ".macrodef MAC\n.if \\# >= 2\n    .db \\2\n.else\n    .db \\1\n.endif\n.endm\n    \
             .macro MAC, 7\n    .macro MAC, 7, 8\n"
        ),
        vec![7, 8]
    );
    // `\@` and the loop counters are not positional parameters.
    assert_eq!(
        clean_bytes(
            ".macrodef MAC\nhere\\@:\n.for 0, 1\n    .db \\i\n.endfor\n.endm\n    .macro MAC\n"
        ),
        vec![0, 1]
    );
    // An exact match is fine.
    assert_eq!(
        clean_bytes(".macrodef MAC\n    .db \\1, \\2\n.endm\n    .macro MAC, 1, 2\n"),
        vec![1, 2]
    );
}

#[test]
fn a_nested_call_reports_its_own_site_and_the_expansion_chain() {
    let d = error(
        ".macrodef INNER\n    .db \\1, \\2\n.endm\n.macrodef OUTER\n    .macro INNER, 1\n.endm\n    .macro OUTER\n",
    );
    assert_eq!(
        (d.line, d.message.as_str()),
        (
            5,
            "macro `INNER` takes 2 arguments (it uses \\1-\\2), but this call passes 1"
        )
    );
    assert_eq!(
        d.notes,
        vec![
            "`INNER` is defined at t.asm line 1",
            "in expansion of macro `OUTER` at t.asm line 7",
        ]
    );
}

#[test]
fn a_macro_defined_in_an_include_is_located_in_that_file() {
    let dir = project(&[
        ("t.asm", ".include \"macros.asm\"\n    .macro MAC\n"),
        (
            "macros.asm",
            "; macros\n.macrodef MAC\n    .db \\1\n.endm\n",
        ),
    ]);
    let d = assemble_file(&dir.join("t.asm"), &Options::default())
        .unwrap_err()
        .0;
    assert_eq!((d.file.as_str(), d.line), ("t.asm", 2));
    assert_eq!(d.notes, vec!["`MAC` is defined at macros.asm line 2"]);
    let _ = std::fs::remove_dir_all(&dir);
}

// ─── 4. Duplicate symbol definitions ───────────────────────────────────────

#[test]
fn a_redefined_label_warns_and_last_still_wins() {
    let (a, w) = one_warning("foo:\n    .db 1\nfoo:\n    .db 2\n    .dw foo\n");
    assert_eq!(a.rom, vec![1, 2, 1, 0]);
    assert_eq!(
        (w.file.as_str(), w.line, w.message.as_str(), w.rule),
        (
            "t.asm",
            3,
            "`foo` is already defined at t.asm line 1 (as a label at $0000); this definition replaces it",
            Some(AsmRule::DuplicateSymbol)
        )
    );
}

#[test]
fn a_redefined_constant_warns_and_keeps_its_forward_leak() {
    let (a, w) = one_warning("QQ = 1\n    .db QQ\nQQ = 2\n    .db QQ\n");
    assert_eq!(a.rom, vec![2, 2]);
    assert_eq!(
        (w.line, w.message.as_str(), w.rule),
        (
            3,
            "`QQ` is already defined at t.asm line 1 (as a constant, $0001); this definition replaces it",
            Some(AsmRule::DuplicateSymbol)
        )
    );
}

#[test]
fn a_label_and_a_constant_of_the_same_name_warn() {
    let (a, w) = one_warning("QQ = $10\nQQ:\n    .db QQ\n");
    assert_eq!(a.rom, vec![0]);
    assert_eq!(
        (w.line, w.message.as_str()),
        (
            2,
            "`QQ` is already defined at t.asm line 1 (as a constant, $0010); this definition replaces it"
        )
    );
    // Same value, different kind: still the dangerous rule.
    assert_eq!(w.rule, Some(AsmRule::DuplicateSymbol));

    let (a, w) = one_warning("QQ:\n    .db 0\nQQ = 5\n    .db QQ\n");
    assert_eq!(a.rom, vec![0, 5]);
    assert_eq!(w.line, 3);
}

#[test]
fn at_prefixed_labels_are_local_and_do_not_warn() {
    // Before scoped locals (`plans/021-local-labels.md`) the second `@loc`
    // replaced the first and both branches cross-linked; each is now local to
    // its routine, and local keys never raise `duplicate-symbol`.
    let a = nessemble_core::assemble(
        "p1:\n@loc:\n    BNE @loc\np2:\n@loc:\n    BNE @loc\n",
        &nessemble_core::Options {
            nes: false,
            ..nessemble_core::Options::default()
        },
    )
    .expect("assembles");
    assert_eq!(a.rom, vec![0xD0, 0xFE, 0xD0, 0xFE]);
    assert!(a.warnings.is_empty(), "{:?}", a.warnings);
}

#[test]
fn a_redefined_macro_warns() {
    let (a, w) = one_warning(
        ".macrodef MAC\n    .db 1\n.endm\n.macrodef MAC\n    .db 2\n.endm\n    .macro MAC\n",
    );
    assert_eq!(a.rom, vec![2]);
    assert_eq!(
        (w.line, w.message.as_str(), w.rule),
        (
            4,
            "macro `MAC` is already defined at t.asm line 1; this definition replaces it",
            Some(AsmRule::DuplicateSymbol)
        )
    );
}

#[test]
fn a_same_value_redefinition_uses_its_own_rule() {
    // An includer places the label and the included file places it again at the
    // same address: harmless, and kept apart from the dangerous case.
    let dir = project(&[
        ("t.asm", "    .db 0\ntable:\n.include \"table.asm\"\n"),
        ("table.asm", "table:\n    .db 1, 2\n"),
    ]);
    let a = assemble_file(&dir.join("t.asm"), &Options::default()).unwrap();
    assert_eq!(a.rom, vec![0, 1, 2]);
    assert_eq!(a.warnings.len(), 1, "{:?}", a.warnings);
    let w = &a.warnings[0];
    assert_eq!(
        (w.file.as_str(), w.line, w.message.as_str(), w.rule),
        (
            "table.asm",
            1,
            "`table` is already defined at t.asm line 2 (as a label at $0001); this definition replaces it",
            Some(AsmRule::DuplicateSymbolSameValue)
        )
    );
    let _ = std::fs::remove_dir_all(&dir);

    let (_, w) = one_warning("A1 = 3\nA1 = 3\n");
    assert_eq!(w.rule, Some(AsmRule::DuplicateSymbolSameValue));
}

#[test]
fn per_expansion_and_anonymous_names_do_not_warn() {
    clean(".macrodef WT\nwait_\\@:\n    BNE wait_\\@\n.endm\n    .macro WT\n    .macro WT\n");
    clean(".for 0, 2\nrow\\i:\n    .db \\i\n.endfor\n");
    clean(".macrodef MM\n    .db 1\n.endm\n    .macro MM\n    .macro MM\n");
    clean(":\n    NOP\n:\n    NOP\n:\n    BNE :-\n");
    // Every label in a file is seen on both passes; that is one definition each.
    clean("aa:\n    NOP\nbb:\n    JMP aa\ncc = bb\n    .dw cc\n");
}

#[test]
fn a_redefinition_in_a_loop_body_warns_once() {
    let a = run(".for 0, 3\nQQ = \\i\n.endfor\n").unwrap();
    assert_eq!(a.warnings.len(), 1, "{:?}", a.warnings);
}

#[test]
fn duplicate_rules_are_configurable_independently() {
    let src = "foo:\n    .db 1\nfoo:\n    .db 2\nbar:\nbar:\n";

    let mut rules = AsmRules::default();
    rules.set(AsmRule::DuplicateSymbol, RuleSeverity::Error);
    rules.set(AsmRule::DuplicateSymbolSameValue, RuleSeverity::Off);
    let d = run_with(src, rules).unwrap_err();
    assert_eq!((d.line, d.rule), (3, Some(AsmRule::DuplicateSymbol)));

    let mut rules = AsmRules::default();
    rules.set(AsmRule::DuplicateSymbol, RuleSeverity::Off);
    let a = run_with(src, rules).unwrap();
    assert_eq!(a.warnings.len(), 1);
    assert_eq!(a.warnings[0].rule, Some(AsmRule::DuplicateSymbolSameValue));
    // Promoting a rule changes nothing about what assembles when it passes.
    assert_eq!(a.rom, vec![1, 2]);

    let mut rules = AsmRules::default();
    rules.set(AsmRule::DuplicateSymbol, RuleSeverity::Off);
    rules.set(AsmRule::DuplicateSymbolSameValue, RuleSeverity::Off);
    let warnings = run_with(src, rules).unwrap().warnings;
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn a_macro_redefinition_promoted_to_an_error_fails() {
    let mut rules = AsmRules::default();
    rules.set(AsmRule::DuplicateSymbol, RuleSeverity::Error);
    let d = run_with(".macrodef MM\n.endm\n.macrodef MM\n.endm\n", rules).unwrap_err();
    assert_eq!((d.line, d.rule), (3, Some(AsmRule::DuplicateSymbol)));
}

// ─── 5. Out-of-range values ────────────────────────────────────────────────

#[test]
fn out_of_range_values_warn_and_still_emit_the_narrowed_bytes() {
    for (src, bytes, message) in [
        (
            ".db 300\n",
            vec![0x2C],
            "value 300 ($012C) does not fit in a byte; emitting $2C",
        ),
        (
            ".db $100\n",
            vec![0x00],
            "value 256 ($0100) does not fit in a byte; emitting $00",
        ),
        (
            ".db 0 - 129\n",
            vec![0x7F],
            "value -129 does not fit in a byte; emitting $7F",
        ),
        (
            ".dw $12345\n",
            vec![0x45, 0x23],
            "value 74565 ($12345) does not fit in a word; emitting $2345",
        ),
        (
            ".dw 0 - 32769\n",
            vec![0xFF, 0x7F],
            "value -32769 does not fit in a word; emitting $7FFF",
        ),
        (
            ".fill 2, $1FF\n",
            vec![0xFF, 0xFF],
            "value 511 ($01FF) does not fit in a byte; emitting $FF",
        ),
        (
            ".ascii \"A\" + 200\n",
            vec![0x09],
            "value 265 ($0109) does not fit in a byte; emitting $09",
        ),
        (
            "    LDA #$1FF\n",
            vec![0xA9, 0xFF],
            "value 511 ($01FF) does not fit in a byte; emitting $FF",
        ),
        (
            "    STA <$1234\n",
            vec![0x85, 0x34],
            "value 4660 ($1234) does not fit in a zero-page operand; emitting $34",
        ),
        (
            "    LDA <$100, X\n",
            vec![0xB5, 0x00],
            "value 256 ($0100) does not fit in a zero-page operand; emitting $00",
        ),
        (
            "    LDA [$1FF], Y\n",
            vec![0xB1, 0xFF],
            "value 511 ($01FF) does not fit in a zero-page operand; emitting $FF",
        ),
        (
            "    LDA $12345\n",
            vec![0xAD, 0x45, 0x23],
            "value 74565 ($12345) does not fit in an absolute operand; emitting $2345",
        ),
        (
            "    JMP $12345\n",
            vec![0x4C, 0x45, 0x23],
            "value 74565 ($12345) does not fit in an absolute operand; emitting $2345",
        ),
        (
            "    JMP [$12345]\n",
            vec![0x6C, 0x45, 0x23],
            "value 74565 ($12345) does not fit in an absolute operand; emitting $2345",
        ),
    ] {
        let (a, w) = one_warning(src);
        assert_eq!(a.rom, bytes, "bytes for {src:?}");
        assert_eq!(
            (w.line, w.message.as_str(), w.rule),
            (1, message, Some(AsmRule::ValueOutOfRange)),
            "for {src:?}"
        );
    }
}

#[test]
fn intentional_narrowing_does_not_warn() {
    assert_eq!(
        clean_bytes(".db LOW($12345), HIGH($12345)\n"),
        vec![0x45, 0x23]
    );
    assert_eq!(
        clean_bytes(".lobytes $12345, $1FF\n.hibytes $12345\n"),
        vec![0x45, 0xFF, 0x23]
    );
    assert_eq!(clean_bytes(".db (0 - 1) & $FF\n"), vec![0xFF]);
    assert_eq!(clean_bytes(".db 0 - 4\n"), vec![0xFC]);
    assert_eq!(clean_bytes(".db 0 - 128, 255\n"), vec![0x80, 0xFF]);
    assert_eq!(clean_bytes(".dw 0 - 40\n"), vec![0xD8, 0xFF]);
    assert_eq!(
        clean_bytes(".dw 0 - 32768, 65535\n"),
        vec![0x00, 0x80, 0xFF, 0xFF]
    );
    assert_eq!(clean_bytes("    LDA #0 - 1\n"), vec![0xA9, 0xFF]);
    assert_eq!(clean_bytes("    LDA <$FF\n"), vec![0xA5, 0xFF]);
    assert_eq!(clean_bytes("    LDA $FFFF\n"), vec![0xAD, 0xFF, 0xFF]);
}

#[test]
fn values_known_only_on_the_final_pass_are_checked_then() {
    // `later` is undefined (placeholder 1) on pass 1 and $2000 on pass 2; only
    // the final value matters, and it fits.
    assert_eq!(
        clean_bytes("    LDA later\n    .dw later\nlater = $2000\n"),
        vec![0xAD, 0x00, 0x20, 0x00, 0x20]
    );
    // A forward value that does not fit warns once, from the final pass.
    let (_, w) = one_warning("    .db big\nbig = 300\n");
    assert_eq!(w.line, 1);
}

#[test]
fn phased_operands_are_checked_after_remapping() {
    // Physically at $C000, but `here` reads as $0080 — a zero-page address.
    let src = ".org $C000\n.phase $0080\nhere:\n    LDA <here\n    JMP here\n.dephase\n";
    assert_eq!(clean_bytes(src), vec![0xA5, 0x80, 0x4C, 0x80, 0x00]);
}

#[test]
fn a_false_conditional_does_not_warn() {
    assert_eq!(clean_bytes(".if 0\n.db 300\n.endif\n.db 1\n"), vec![1]);
}

#[test]
fn an_out_of_range_line_in_a_loop_or_macro_warns_once() {
    let a = run(".for 0, 3\n.db 300 + \\i\n.endfor\n").unwrap();
    assert_eq!(a.warnings.len(), 1, "{:?}", a.warnings);
    let a = run(".macrodef MM\n.db 300\n.endm\n.macro MM\n.macro MM\n").unwrap();
    assert_eq!(a.warnings.len(), 1, "{:?}", a.warnings);
    assert_eq!(a.warnings[0].line, 2);
}

#[test]
fn out_of_range_is_configurable() {
    let mut rules = AsmRules::default();
    rules.set(AsmRule::ValueOutOfRange, RuleSeverity::Off);
    let warnings = run_with(".db 300\n", rules).unwrap().warnings;
    assert!(warnings.is_empty(), "{warnings:?}");

    let mut rules = AsmRules::default();
    rules.set(AsmRule::ValueOutOfRange, RuleSeverity::Error);
    let d = run_with(".db 1\n.db 300\n", rules).unwrap_err();
    assert_eq!((d.line, d.rule), (2, Some(AsmRule::ValueOutOfRange)));
}

#[test]
fn warnings_as_errors_promotes_every_warning() {
    let mut rules = AsmRules::default();
    rules.warnings_as_errors = true;
    let d = run_with(".db 300\n", rules.clone()).unwrap_err();
    assert_eq!(d.rule, Some(AsmRule::ValueOutOfRange));
    // A rule switched off stays off.
    let mut off = rules;
    off.set(AsmRule::ValueOutOfRange, RuleSeverity::Off);
    assert!(run_with(".db 300\n", off).is_ok());
}

// ─── Cross-cutting ─────────────────────────────────────────────────────────

#[test]
fn cached_custom_output_does_not_hide_neighbouring_diagnostics() {
    static CALLS: AtomicU32 = AtomicU32::new(0);
    let src = "foo:\n    .metasprite 1, 2\n    .db 300\nfoo:\n    .metasprite 1, 2\n";
    let dir = project(&[("t.asm", src)]);
    let a = assemble_file_with(
        &dir.join("t.asm"),
        &Options::default(),
        metasprite_resolver(&CALLS),
    )
    .unwrap();
    // Each call site ran its script once (memoized across passes) ...
    assert_eq!(CALLS.load(Ordering::Relaxed), 2);
    // ... and both neighbouring diagnostics still surface, once each.
    let rules: Vec<_> = a.warnings.iter().map(|w| (w.line, w.rule)).collect();
    assert_eq!(
        rules,
        vec![
            (4, Some(AsmRule::DuplicateSymbol)),
            (3, Some(AsmRule::ValueOutOfRange)),
        ]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn tooling_diagnostics_surface_the_new_errors_and_warnings() {
    let dir = project(&[]);
    let path = dir.join("t.asm");
    let d = diagnose_source_as(&path, "foo: .db 1\n.db 1 2\n", &Options::default());
    let lines: Vec<u32> = d.errors.iter().map(|e| e.line).collect();
    assert_eq!(lines, vec![1, 2], "{:?}", d.errors);

    let d = diagnose_source_as(&path, "foo:\nfoo:\n.db 300\n", &Options::default());
    assert!(d.errors.is_empty(), "{:?}", d.errors);
    let rules: Vec<_> = d.warnings.iter().map(|w| w.rule).collect();
    assert_eq!(
        rules,
        vec![
            Some(AsmRule::DuplicateSymbolSameValue),
            Some(AsmRule::ValueOutOfRange)
        ]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rule_ids_are_stable() {
    let ids: Vec<&str> = AsmRule::ALL.iter().map(|r| r.id()).collect();
    assert_eq!(
        ids,
        vec![
            "duplicate-symbol",
            "duplicate-symbol-same-value",
            "value-out-of-range"
        ]
    );
    for rule in AsmRule::ALL {
        assert_eq!(AsmRule::from_id(rule.id()), Some(rule));
        assert_eq!(AsmRules::default().get(rule), RuleSeverity::Warn);
    }
}

#[test]
fn a_file_included_twice_is_one_definition_per_line() {
    // The same constants file reached by two spellings: each line runs twice,
    // but it is still one definition. A different value would still warn.
    let dir = project(&[
        (".nessemblerc", "{}"),
        (
            "t.asm",
            ".include \"defs.asm\"\n.prg 1\n.include \"@/defs.asm\"\n    .db PPU\n",
        ),
        ("defs.asm", "PPU = $20\n"),
    ]);
    let a = assemble_file(&dir.join("t.asm"), &Options::default()).unwrap();
    assert_eq!(a.rom[a.rom.len() - 1], 0x20);
    assert!(a.warnings.is_empty(), "{:?}", a.warnings);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_label_in_another_bank_is_a_different_value() {
    let (_, w) = one_warning(
        "foo:
.prg 1
foo:
",
    );
    assert_eq!(w.rule, Some(AsmRule::DuplicateSymbol));
}
