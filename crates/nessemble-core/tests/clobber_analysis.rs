//! Whole-program clobber analysis
//! (`plans/023-whole-program-clobber-analysis.md` §6): each case is a small
//! in-memory program, assembled with `build_program` and linted with
//! `lint_programs`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use nessemble_core::tooling::{
    lint, lint_programs, missing_clobbers_in, Finding, LintOptions, ProgramFile, RuleId,
    RuleSeverity, SeverityMap, Slot,
};
use nessemble_core::{
    build_program, lenient_custom_resolver, CustomOutput, CustomResolver, Options,
};

const DIR: &str = "/nessemble-virtual";

fn severities() -> SeverityMap {
    let mut s = SeverityMap::default();
    s.set(RuleId::RequireBlockComment, RuleSeverity::Off);
    s
}

/// Assemble `files` (the first is the entry) and lint every one of them in
/// program mode, returning `file:line rule: message` rows.
fn lint_with(
    files: &[(&str, &str)],
    severities: &SeverityMap,
    custom: CustomResolver,
) -> Vec<String> {
    let ignore = |name: &str| name.starts_with("ignored_");
    let opts = LintOptions {
        severities: severities.clone(),
        window: 3,
        ignore: &ignore,
    };
    let (rows, _) = run(files, &opts, custom);
    rows
}

fn run(
    files: &[(&str, &str)],
    opts: &LintOptions,
    custom: CustomResolver,
) -> (Vec<String>, nessemble_core::tooling::ProgramLint) {
    let dir = PathBuf::from(DIR);
    let owned: Vec<(PathBuf, String)> = files
        .iter()
        .map(|(n, t)| (dir.join(n), (*t).to_string()))
        .collect();
    let overlay = |p: &Path| {
        owned
            .iter()
            .find(|(path, _)| path == p)
            .map(|(_, t)| t.clone())
    };
    let build = build_program(
        &dir.join(files[0].0),
        files[0].1,
        &Options::default(),
        &overlay,
        custom,
    );
    assert!(build.errors.is_empty(), "{:?}", build.errors);
    let program = build.program.expect("a program");
    let paths: Vec<PathBuf> = files.iter().map(|(n, _)| dir.join(n)).collect();
    let pfiles: Vec<ProgramFile> = files
        .iter()
        .zip(&paths)
        .map(|((_, text), path)| ProgramFile {
            path,
            source: text,
            options: opts,
            report: true,
        })
        .collect();
    let result = lint_programs(std::slice::from_ref(&program), &pfiles);
    let mut rows = Vec::new();
    for ((name, _), findings) in files.iter().zip(&result.findings) {
        for f in findings {
            rows.push(format!("{name}:{} {}: {}", f.line, f.rule.id(), f.message));
        }
    }
    (rows, result)
}

fn lint_project(files: &[(&str, &str)]) -> Vec<String> {
    lint_with(
        files,
        &severities(),
        lenient_custom_resolver(HashSet::new()),
    )
}

fn assert_rows(rows: &[String], expected: &[&str]) {
    assert_eq!(
        rows.iter().map(String::as_str).collect::<Vec<_>>(),
        expected,
        "\n{}",
        rows.join("\n")
    );
}

/// File-mode lint of one buffer, for pinning today's behaviour.
fn file_mode(source: &str) -> Vec<Finding> {
    let ignore = |_: &str| false;
    lint(
        source,
        &LintOptions {
            severities: severities(),
            window: 3,
            ignore: &ignore,
        },
    )
}

const UTIL_HELPER_AX: &str = "\
; @nessemble-clobbers A, X
helper:
    lda #1
    ldx #2
    rts
";

// ─── Blind spot 1: cross-file callees ────────────────────────────────────────

#[test]
fn a_cross_file_contract_is_trusted() {
    let main = "\
; @nessemble-clobbers A
caller:
    jsr helper
    rts

.include \"util.asm\"
";
    let rows = lint_project(&[("main.asm", main), ("util.asm", UTIL_HELPER_AX)]);
    assert_rows(
        &rows,
        &[
            "main.asm:2 undeclared-clobber: routine `caller` writes X but does not declare it \
           clobbered (X: `JSR helper` on line 3, whose contract clobbers X)",
        ],
    );
    // File mode cannot see the callee: today's behaviour, pinned.
    assert!(file_mode(main)
        .iter()
        .all(|f| f.rule != RuleId::UndeclaredClobber));
}

#[test]
fn a_verified_cross_file_call_can_be_over_declared() {
    let main = "\
; @nessemble-clobbers A, X, Y
caller:
    jsr helper
    rts

.include \"util.asm\"
";
    let rows = lint_project(&[("main.asm", main), ("util.asm", UTIL_HELPER_AX)]);
    assert_rows(
        &rows,
        &["main.asm:2 overdeclared-clobber: routine `caller` declares Y clobbered but never writes it"],
    );
}

// ─── Blind spot 2: fall-through and always-taken branches ────────────────────

const UTIL_UNGATED: &str = "\
; @nessemble-clobbers X, Y
ungated:
    ldx #0
    ldy #0
    rts
";

#[test]
fn falling_through_an_include_into_a_contract() {
    let truthful = "\
; @nessemble-clobbers A, X, Y
gated:
    lda #1
.include \"util.asm\"
";
    assert_rows(
        &lint_project(&[("main.asm", truthful), ("util.asm", UTIL_UNGATED)]),
        &[],
    );
    let short = truthful.replace("A, X, Y", "A, X");
    assert_rows(
        &lint_project(&[("main.asm", &short), ("util.asm", UTIL_UNGATED)]),
        &["main.asm:2 undeclared-clobber: routine `gated` writes Y but does not declare it \
           clobbered (Y: falls through into `ungated` on line 2 of `/nessemble-virtual/util.asm`, whose contract \
           clobbers Y)"],
    );
}

#[test]
fn an_always_taken_branch_prunes_the_fall_through() {
    let main = "\
; @nessemble-clobbers X, Y
gated:
    ldx #0
    beq ungated
    .db $02

.include \"util.asm\"
";
    assert_rows(
        &lint_project(&[("main.asm", main), ("util.asm", UTIL_UNGATED)]),
        &[],
    );
}

// ─── Blind spot 3: save/restore ──────────────────────────────────────────────

const UTIL_EATS_A: &str = "\
; @nessemble-clobbers A
util_eats_a:
    lda #0
    rts
";

#[test]
fn push_and_pull_preserve_a() {
    let main = "\
; @nessemble-clobbers none
keeps_a:
    pha
    jsr util_eats_a
    pla
    rts

.include \"util.asm\"
";
    assert_rows(
        &lint_project(&[("main.asm", main), ("util.asm", UTIL_EATS_A)]),
        &[],
    );
}

#[test]
fn txa_pha_pla_tax_preserves_x_and_clobbers_a() {
    let main = "\
; @nessemble-clobbers none
keeps_x:
    txa
    pha
    jsr util_eats_x
    pla
    tax
    rts

.include \"util.asm\"
";
    let util = "\
; @nessemble-clobbers X
util_eats_x:
    ldx #0
    rts
";
    assert_rows(
        &lint_project(&[("main.asm", main), ("util.asm", util)]),
        &[
            "main.asm:2 undeclared-clobber: routine `keeps_x` writes A but does not declare it \
           clobbered (A: `TXA` on line 3)",
        ],
    );
    let tya = main
        .replace("txa", "tya")
        .replace("tax", "tay")
        .replace("util_eats_x", "util_eats_y");
    let util_y = util
        .replace("X\n", "Y\n")
        .replace("ldx", "ldy")
        .replace("_x", "_y");
    assert_rows(
        &lint_project(&[("main.asm", &tya), ("util.asm", &util_y)]),
        &[
            "main.asm:2 undeclared-clobber: routine `keeps_x` writes A but does not declare it \
           clobbered (A: `TYA` on line 3)",
        ],
    );
}

#[test]
fn a_save_on_one_path_but_not_the_other_reports() {
    let main = "\
; @nessemble-clobbers none
sometimes:
    beq :+
    txa
    pha
    ldx #9
    pla
    tax
    rts
:
    ldx #1
    rts
";
    // The `TXA PHA … PLA TAX` arm preserves X; the other arm does not.
    assert_rows(
        &lint_project(&[("main.asm", main)]),
        &[
            "main.asm:2 undeclared-clobber: routine `sometimes` writes A, X but does not declare \
           them clobbered (A: `TXA` on line 4; X: `LDX` on line 11)",
        ],
    );
}

// ─── Blind spot 4: unreachable fragments ─────────────────────────────────────

#[test]
fn an_unreachable_fragment_is_not_counted() {
    let main = "\
; @nessemble-clobbers A
routine:
    jsr far_routine
    rts
    ldx #0
    rts

.include \"util.asm\"
";
    let util = "\
; @nessemble-clobbers A
far_routine:
    lda #1
    rts
";
    assert_rows(
        &lint_project(&[("main.asm", main), ("util.asm", util)]),
        &[],
    );
}

// ─── Required cases ──────────────────────────────────────────────────────────

#[test]
fn a_contracted_callee_s_extra_write_is_reported_on_the_callee() {
    let main = "\
; @nessemble-clobbers A
caller:
    jsr helper
    rts

; @nessemble-clobbers A
helper:
    ldx #1
    lda #1
    rts
";
    assert_rows(
        &lint_project(&[("main.asm", main)]),
        &[
            "main.asm:7 undeclared-clobber: routine `helper` writes X but does not declare it \
           clobbered (X: `LDX` on line 8)",
        ],
    );
}

#[test]
fn an_uncontracted_callee_is_walked() {
    let main = "\
; @nessemble-clobbers none
caller:
    jsr helper
    rts

.include \"util.asm\"
";
    let util = "\
helper:
    nop
    inx
    rts
";
    assert_rows(
        &lint_project(&[("main.asm", main), ("util.asm", util)]),
        &["main.asm:2 undeclared-clobber: routine `caller` writes X but does not declare it \
           clobbered (X: `INX` on line 3 of `/nessemble-virtual/util.asm`, reached through `JSR helper` on line 3)"],
    );
}

#[test]
fn a_branch_where_only_one_arm_writes_x_reports_x() {
    let main = "\
; @nessemble-clobbers none
routine:
    bne :+
    ldx #1
:
    rts
";
    assert_rows(
        &lint_project(&[("main.asm", main)]),
        &[
            "main.asm:2 undeclared-clobber: routine `routine` writes X but does not declare it \
           clobbered (X: `LDX` on line 4)",
        ],
    );
}

#[test]
fn an_indirect_jmp_is_unverifiable_but_definite_writes_still_report() {
    let main = "\
vec = $0200
; @nessemble-clobbers A, Y
routine:
    ldx #1
    jmp [vec]
";
    assert_rows(
        &lint_project(&[("main.asm", main)]),
        &[
            "main.asm:3 undeclared-clobber: routine `routine` writes X but does not declare it \
             clobbered (X: `LDX` on line 4)",
            "main.asm:3 unverifiable-clobbers: routine `routine` cannot be fully verified: indirect \
             `JMP [vec]` on line 5 ends a path",
        ],
    );
}

#[test]
fn unreachable_code_after_rts_is_not_counted() {
    let main = "\
; @nessemble-clobbers none
routine:
    rts
    ldx #1
    rts
";
    assert_rows(&lint_project(&[("main.asm", main)]), &[]);
}

#[test]
fn a_for_unrolled_body_and_an_if_inside_it_are_counted() {
    let main = "\
; @nessemble-clobbers none
routine:
.for 0, 2
    .if \\i == 2
    ldy #1
    .endif
    inx
.endfor
    rts
";
    let rows = lint_project(&[("main.asm", main)]);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(
        rows[0].contains("writes X, Y but does not declare them"),
        "{rows:?}"
    );
}

#[test]
fn an_if_guarded_write_counts_only_when_assembled() {
    let guarded = |flag: u8| {
        format!(
            "FLAG = {flag}\n; @nessemble-clobbers none\nroutine:\n    .if FLAG\n    ldx #1\n    .endif\n    rts\n"
        )
    };
    assert_rows(&lint_project(&[("main.asm", &guarded(0))]), &[]);
    let rows = lint_project(&[("main.asm", &guarded(1))]);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(rows[0].contains("writes X"), "{rows:?}");
}

// ─── Further cases ───────────────────────────────────────────────────────────

#[test]
fn a_split_hand_encoded_branch_is_verified() {
    let main = "\
; @nessemble-clobbers A
routine:
    lda #1
    .db $B0
br_off:
    .db done-(br_off+1)
    ldx #2
done:
    rts
";
    let rows = lint_project(&[("main.asm", main)]);
    assert_rows(
        &rows,
        &[
            "main.asm:2 undeclared-clobber: routine `routine` writes X but does not declare it \
           clobbered (X: `LDX` on line 7)",
        ],
    );
}

#[test]
fn the_bit_skip_decodes_as_the_cpu_would() {
    let main = "\
; @nessemble-clobbers A
routine:
    lda #1
    .db $2C
    ldx #2
    rts
";
    // Entering at `r`, `LDX #2` is BIT's operand: X is never written.
    assert_rows(&lint_project(&[("main.asm", main)]), &[]);
}

#[test]
fn reaching_an_unknown_opcode_is_unverifiable() {
    let main = "\
; @nessemble-clobbers none
routine:
    nop
    .db $02
";
    assert_rows(
        &lint_project(&[("main.asm", main)]),
        &["main.asm:2 unverifiable-clobbers: routine `routine` cannot be fully verified: the path \
           reaches data on line 4 that is not a known instruction (`$02`)"],
    );
}

#[test]
fn inline_arguments_after_jsr_return_into_data() {
    let main = "\
; @nessemble-clobbers none
routine:
    jsr takes_args
    .dw $1234
    rts

takes_args:
    rts
";
    assert_rows(
        &lint_project(&[("main.asm", main)]),
        &[
            "main.asm:2 unverifiable-clobbers: routine `routine` cannot be fully verified: `JSR \
           takes_args` on line 3 returns into data on line 4",
        ],
    );
}

#[test]
fn rts_dispatch_is_a_computed_jump() {
    let main = "\
; @nessemble-clobbers A
dispatch:
    lda #0
    pha
    pha
    rts
";
    assert_rows(
        &lint_project(&[("main.asm", main)]),
        &[
            "main.asm:2 unverifiable-clobbers: routine `dispatch` cannot be fully verified: `RTS` \
           on line 6 returns through values the routine pushed (a computed jump)",
        ],
    );
}

#[test]
fn a_callee_that_pulls_its_return_address() {
    let main = "\
; @nessemble-clobbers none
routine:
    jsr trampoline
    rts

trampoline:
    pla
    pla
    rts
";
    let rows = lint_project(&[("main.asm", main)]);
    assert!(
        rows.iter().any(|r| r.contains(
            "`JSR trampoline` on line 3 enters `trampoline`, which pulls its own return address"
        )),
        "{rows:?}"
    );
}

#[test]
fn mutual_recursion_reaches_a_fixpoint() {
    let main = "\
; @nessemble-clobbers A
routine:
    jsr ping
    rts

ping:
    inx
    bne pong
    rts

pong:
    iny
    jsr ping
    rts
";
    let rows = lint_project(&[("main.asm", main)]);
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(
        rows[0].contains("writes X, Y but does not declare them"),
        "{rows:?}"
    );
    assert!(
        rows[1].contains("declares A clobbered but never writes it"),
        "{rows:?}"
    );
}

#[test]
fn falling_into_a_contract_with_a_save_pending_walks_into_it() {
    let main = "\
; @nessemble-clobbers none
outer:
    pha
; @nessemble-clobbers A
inner:
    lda #1
    pla
    rts
";
    // `outer` saves A, falls into `inner` (which writes A), and restores A.
    // `inner` on its own really does pull a value it never pushed.
    assert_rows(
        &lint_project(&[("main.asm", main)]),
        &[
            "main.asm:5 unverifiable-clobbers: routine `inner` cannot be fully verified: `PLA` on \
           line 7 pulls the routine's own return address",
        ],
    );
}

#[test]
fn stacked_labels_do_not_trust_each_other() {
    let main = "\
; @nessemble-clobbers A
first:
; @nessemble-clobbers none
second:
    lda #1
    rts
";
    assert_rows(
        &lint_project(&[("main.asm", main)]),
        &[
            "main.asm:4 undeclared-clobber: routine `second` writes A but does not declare it \
           clobbered (A: `LDA` on line 5)",
        ],
    );
}

#[test]
fn local_targets_are_followed_and_never_stop_the_walk() {
    let main = "\
; @nessemble-clobbers none
routine:
    bne @skip
    nop
@skip:
    ldx #1
    rts
";
    let rows = lint_project(&[("main.asm", main)]);
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert!(rows[0].contains("X: `LDX` on line 6"), "{rows:?}");
}

#[test]
fn assertions_are_invisible_and_a_table_is_data() {
    let main = "\
; @nessemble-clobbers none
routine:
    .assert 1 == 1
    nop
    .table 2
    [0] .db $02
    [1] .db $03
    .endtable
";
    assert_rows(
        &lint_project(&[("main.asm", main)]),
        &["main.asm:2 unverifiable-clobbers: routine `routine` cannot be fully verified: the path \
           reaches data on line 6 that is not a known instruction (`$02`)"],
    );
}

#[test]
fn a_macro_write_names_the_call_site() {
    let main = "\
.macrodef set_bank
    lda #\\1
    sta $8000
.endm
; @nessemble-clobbers none
routine:
    .macro set_bank, 2
    rts
";
    assert_rows(
        &lint_project(&[("main.asm", main)]),
        &[
            "main.asm:6 undeclared-clobber: routine `routine` writes A but does not declare it \
           clobbered (A: `LDA` in `.macro set_bank` on line 7)",
        ],
    );
}

#[test]
fn emit_source_instructions_are_seen() {
    let main = "\
; @nessemble-clobbers none
routine:
    .gen
    rts
";
    let custom: CustomResolver =
        Box::new(|_, _, _, _, _| Ok(CustomOutput::Source("    ldy #1\n".into())));
    assert_rows(
        &lint_with(&[("main.asm", main)], &severities(), custom),
        &[
            "main.asm:2 undeclared-clobber: routine `routine` writes Y but does not declare it \
           clobbered (Y: `LDY` in `.gen` on line 3)",
        ],
    );
}

#[test]
fn require_routine_doc_sees_cross_file_calls_and_tail_jumps_but_not_loops() {
    let main = "\
; doc
start:
    jsr helper
    jmp tail

; doc
loop_top:
    nop
    jmp loop_top

.include \"util.asm\"
";
    let util = "\
; doc
helper:
    rts

; doc
tail:
    rts
";
    let mut s = severities();
    s.set(RuleId::RequireRoutineDoc, RuleSeverity::Warn);
    let rows = lint_with(
        &[("main.asm", main), ("util.asm", util)],
        &s,
        lenient_custom_resolver(HashSet::new()),
    );
    assert_rows(
        &rows,
        &[
            "util.asm:2 require-routine-doc: called routine `helper` has no signature annotations",
            "util.asm:6 require-routine-doc: called routine `tail` has no signature annotations",
        ],
    );
}

#[test]
fn ignored_names_are_trusted_as_callees_with_no_findings_of_their_own() {
    let main = "\
; @nessemble-clobbers none
caller:
    jsr ignored_helper
    rts

; @nessemble-clobbers none
ignored_helper:
    ldx #1
    rts
";
    // `ignored_helper` lies (it writes X) but gets no finding; `caller` trusts
    // its contract, so it gets none either.
    assert_rows(&lint_project(&[("main.asm", main)]), &[]);
}

#[test]
fn missing_clobbers_in_reads_the_program_analysis() {
    let main = "\
; @nessemble-clobbers A
caller:
    jsr helper
    rts

.include \"util.asm\"
";
    let ignore = |_: &str| false;
    let opts = LintOptions {
        severities: severities(),
        window: 3,
        ignore: &ignore,
    };
    let (_, result) = run(
        &[("main.asm", main), ("util.asm", UTIL_HELPER_AX)],
        &opts,
        lenient_custom_resolver(HashSet::new()),
    );
    assert_eq!(
        missing_clobbers_in(&result, &PathBuf::from(DIR).join("main.asm"), "caller"),
        Some(vec![Slot::X])
    );
}

// ─── unused-lint-ignore (Phase 3) ────────────────────────────────────────────

/// File-mode lint rows, with `require-block-comment` on (a text rule).
fn file_rows(source: &str) -> Vec<String> {
    let ignore = |_: &str| false;
    lint(
        source,
        &LintOptions {
            severities: SeverityMap::default(),
            window: 1,
            ignore: &ignore,
        },
    )
    .iter()
    .map(|f| format!("{} {}: {}", f.line, f.rule.id(), f.message))
    .collect()
}

#[test]
fn an_unneeded_next_line_suppression_is_reported() {
    let src = "\
; documented
; @nessemble-lint-ignore-next-line require-block-comment
documented:
    rts
";
    assert_rows(
        &file_rows(src),
        &[
            "2 unused-lint-ignore: `@nessemble-lint-ignore-next-line` suppresses \
           `require-block-comment`, which reports nothing on line 3",
        ],
    );
}

#[test]
fn an_unneeded_region_is_reported_with_its_lines() {
    let src = "\
; @nessemble-lint-ignore start require-block-comment
; a comment
documented:
    rts
; @nessemble-lint-ignore end

; @nessemble-lint-ignore start require-block-comment
; again
also:
    rts
";
    assert_rows(
        &file_rows(src),
        &[
            "1 unused-lint-ignore: `@nessemble-lint-ignore start` suppresses \
             `require-block-comment`, which reports nothing on lines 1\u{2013}5",
            "7 unused-lint-ignore: `@nessemble-lint-ignore start` suppresses \
             `require-block-comment`, which reports nothing on lines 7 to the end of the file",
        ],
    );
}

#[test]
fn a_needed_suppression_is_not_reported() {
    let src =
        "\n\n\n\n; @nessemble-lint-ignore-next-line require-block-comment\n\nbare:\n    rts\n";
    assert_rows(&file_rows(src), &[]);
}

#[test]
fn each_named_rule_is_judged_on_its_own() {
    let main = "\
; @nessemble-lint-ignore-next-line undeclared-clobber, overdeclared-clobber
; @nessemble-clobbers none
routine:
    ldx #1
    rts
";
    assert_rows(
        &lint_project(&[("main.asm", main)]),
        &[
            "main.asm:1 unused-lint-ignore: `@nessemble-lint-ignore-next-line` suppresses \
           `overdeclared-clobber`, which reports nothing on line 3",
        ],
    );
}

#[test]
fn a_rule_configured_off_is_not_judged() {
    let main = "\
; @nessemble-lint-ignore-next-line overdeclared-clobber
; @nessemble-clobbers none
routine:
    rts
";
    let mut s = severities();
    s.set(RuleId::OverdeclaredClobber, RuleSeverity::Off);
    assert_rows(
        &lint_with(
            &[("main.asm", main)],
            &s,
            lenient_custom_resolver(HashSet::new()),
        ),
        &[],
    );
    // With it on, the suppression is unused.
    assert_eq!(lint_project(&[("main.asm", main)]).len(), 1);
}

#[test]
fn file_mode_does_not_judge_program_rules_or_bare_suppressions() {
    let src = "\
; @nessemble-lint-ignore-next-line undeclared-clobber
; @nessemble-clobbers none
routine:
    rts

; @nessemble-lint-ignore-next-line
; doc
other:
    rts
";
    let rows: Vec<String> = file_rows(src)
        .into_iter()
        .filter(|r| r.contains("unused-lint-ignore"))
        .collect();
    assert_rows(&rows, &[]);
}

#[test]
fn a_bare_suppression_is_judged_in_program_mode() {
    let main = "\
; @nessemble-lint-ignore-next-line
; @nessemble-clobbers none
routine:
    rts
";
    assert_rows(
        &lint_project(&[("main.asm", main)]),
        &[
            "main.asm:1 unused-lint-ignore: `@nessemble-lint-ignore-next-line` suppresses nothing \
           on line 3",
        ],
    );
}

#[test]
fn an_assembled_out_label_is_not_judged() {
    let main = "\
    .if 0
; @nessemble-lint-ignore-next-line undeclared-clobber
; @nessemble-clobbers none
routine:
    rts
    .endif
";
    assert_rows(&lint_project(&[("main.asm", main)]), &[]);
}

#[test]
fn a_region_naming_unused_lint_ignore_that_silences_one_counts_as_used() {
    let main = "\
; @nessemble-lint-ignore start unused-lint-ignore
; @nessemble-lint-ignore-next-line undeclared-clobber
; @nessemble-clobbers none
routine:
    rts
; @nessemble-lint-ignore end

; @nessemble-lint-ignore start unused-lint-ignore
; @nessemble-clobbers none
other:
    rts
; @nessemble-lint-ignore end
";
    assert_rows(
        &lint_project(&[("main.asm", main)]),
        &[
            "main.asm:8 unused-lint-ignore: `@nessemble-lint-ignore start` suppresses \
           `unused-lint-ignore`, which reports nothing on lines 8\u{2013}12",
        ],
    );
}

#[test]
fn a_suppression_on_a_routine_with_no_clobber_list_is_flagged() {
    let main = "\
; @nessemble-lint-ignore-next-line undeclared-clobber
; @nessemble-param A the value
routine:
    ldx #1
    rts
";
    assert_rows(
        &lint_project(&[("main.asm", main)]),
        &[
            "main.asm:1 unused-lint-ignore: `@nessemble-lint-ignore-next-line` suppresses \
           `undeclared-clobber`, which reports nothing on line 3",
        ],
    );
}

#[test]
fn a_still_needed_program_suppression_is_not_flagged() {
    let main = "\
; @nessemble-lint-ignore-next-line undeclared-clobber
; @nessemble-clobbers none
routine:
    ldx #1
    rts
";
    assert_rows(&lint_project(&[("main.asm", main)]), &[]);
}
