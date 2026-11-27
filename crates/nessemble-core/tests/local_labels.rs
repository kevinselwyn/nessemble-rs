//! Scoped `@local` labels (`plans/021-local-labels.md`).
//!
//! An `@name` label is local to the nearest global label above it in the same
//! file, referenced as `@name` inside that scope and as `parent@name` from
//! anywhere. Includes start with no scope, and a macro expansion's (or emitted
//! source's) own `@` definitions are private to it.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use nessemble_core::{
    assemble, assemble_file, assemble_with, render_list_file, Assembly, CustomOutput,
    CustomResolver, Diag, Options,
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

fn bytes(src: &str) -> Vec<u8> {
    assembly(src).rom
}

fn diag(src: &str) -> Diag {
    assemble(src, &raw()).expect_err("should not assemble").0
}

/// The (line, message) assembling `src` fails with.
fn error(src: &str) -> (u32, String) {
    let d = diag(src);
    (d.line, d.message)
}

fn sym(a: &Assembly, name: &str) -> i64 {
    a.symbols
        .iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("symbol `{name}` not found in {:?}", names(a)))
        .value
}

fn names(a: &Assembly) -> Vec<String> {
    a.symbols.iter().map(|s| s.name.clone()).collect()
}

/// A throwaway directory for multi-file tests.
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "nessemble-locals-{}-{}",
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

fn source_resolver(source: &str) -> CustomResolver {
    let source = source.to_string();
    Box::new(move |name, _ints, _texts, _base, _root| {
        if name == "gen" {
            Ok(CustomOutput::Source(source.clone()))
        } else {
            Err(format!("unknown custom pseudo-instruction `.{name}`"))
        }
    })
}

// -- bytes ------------------------------------------------------------------

#[test]
fn each_routine_has_its_own_locals() {
    let a = assembly("p1:\n@loc:\n    BNE @loc\np2:\n@loc:\n    BNE @loc\n");
    assert_eq!(a.rom, vec![0xD0, 0xFE, 0xD0, 0xFE]);
    assert_eq!(sym(&a, "p1@loc"), 0);
    assert_eq!(sym(&a, "p2@loc"), 2);
    assert!(a.warnings.is_empty(), "{:?}", a.warnings);
}

#[test]
fn backward_reference() {
    assert_eq!(
        bytes("p1:\n@top:\n    NOP\n    BNE @top\n"),
        vec![0xEA, 0xD0, 0xFD]
    );
}

#[test]
fn forward_reference() {
    assert_eq!(
        bytes("p1:\n    BEQ @done\n    NOP\n@done:\n    RTS\n"),
        vec![0xF0, 0x01, 0xEA, 0x60]
    );
}

#[test]
fn qualified_reference_in_the_same_file() {
    assert_eq!(
        bytes("p1:\n@a:\n    RTS\np2:\n    JMP p1@a\n"),
        vec![0x60, 0x4C, 0x00, 0x00]
    );
}

#[test]
fn qualified_reference_from_another_file() {
    let dir = TempDir::new();
    dir.write("lib.asm", "draw_sprite:\n@clip:\n    RTS\n");
    let main = dir.write(
        "main.asm",
        "    JMP draw_sprite@clip\n.include \"lib.asm\"\n    JMP draw_sprite@clip\n",
    );
    let a = assemble_file(&main, &raw()).expect("assembles");
    assert_eq!(a.rom, vec![0x4C, 0x03, 0x00, 0x60, 0x4C, 0x03, 0x00]);
}

#[test]
fn an_include_starts_without_a_scope_and_the_includer_resumes_its_own() {
    let dir = TempDir::new();
    dir.write("inc.asm", "tbl:\n@loc:\n.db 1\n");
    let main = dir.write(
        "main.asm",
        "p1:\n    BNE @loc\n.include \"inc.asm\"\n@loc:\n    RTS\n",
    );
    let a = assemble_file(&main, &raw()).expect("assembles");
    assert_eq!(a.rom, vec![0xD0, 0x01, 0x01, 0x60]);
    assert_eq!(sym(&a, "tbl@loc"), 2);
    assert_eq!(sym(&a, "p1@loc"), 3);
}

#[test]
fn an_include_that_starts_with_a_local_has_no_scope() {
    let dir = TempDir::new();
    dir.write("inc.asm", "@loc:\n    RTS\n");
    let main = dir.write("main.asm", "p1:\n.include \"inc.asm\"\n");
    let e = assemble_file(&main, &raw()).expect_err("errors").0;
    assert_eq!(
        (e.file.as_str(), e.line, e.message.as_str()),
        ("inc.asm", 1, "Local label `@loc` has no enclosing label")
    );
}

const WAIT: &str = ".macrodef wait\n@spin:\n    BIT $2002\n    BPL @spin\n.endm\n";

#[test]
fn a_macro_expanded_twice_in_one_scope_has_private_locals() {
    let a = assembly(&format!("{WAIT}p1:\n    .macro wait\n    .macro wait\n"));
    assert_eq!(
        a.rom,
        vec![0x2C, 0x02, 0x20, 0x10, 0xFB, 0x2C, 0x02, 0x20, 0x10, 0xFB]
    );
    // Private locals are macro-defined: listed only with `--mlist`.
    let list = render_list_file(&a.symbols, false);
    assert!(!list.contains("@spin"), "{list}");
    let mlist = render_list_file(&a.symbols, true);
    assert!(mlist.contains("00/0000 = p1@spin\\@1"), "{mlist}");
    assert!(mlist.contains("00/0005 = p1@spin\\@2"), "{mlist}");
}

#[test]
fn a_macro_body_reference_resolves_in_the_callers_scope() {
    assert_eq!(
        bytes(
            ".macrodef skip\n    BEQ @out\n.endm\np1:\n    .macro skip\n    NOP\n@out:\n    RTS\n"
        ),
        vec![0xF0, 0x01, 0xEA, 0x60]
    );
}

#[test]
fn a_local_passed_as_a_macro_argument_names_the_callers_label() {
    assert_eq!(
        bytes(".macrodef jmp_to\n    .dw \\1\n.endm\np1:\n@x:\n    .macro jmp_to, @x\n"),
        vec![0x00, 0x00]
    );
    // A nested macro receiving the outer expansion's private local.
    assert_eq!(
        bytes(concat!(
            ".macrodef inner\n    .dw \\1\n.endm\n",
            ".macrodef outer\n    NOP\n@here:\n    .macro inner, @here\n.endm\n",
            "p1:\n    .macro outer\n    .macro outer\n",
        )),
        vec![0xEA, 0x01, 0x00, 0xEA, 0x04, 0x00]
    );
}

#[test]
fn pasted_locals_in_a_for_body() {
    let a = assembly("p1:\n.for 0, 1\n@x\\i:\n    NOP\n.endfor\n    JMP @x1\n");
    assert_eq!(a.rom, vec![0xEA, 0xEA, 0x4C, 0x01, 0x00]);
    assert_eq!(sym(&a, "p1@x0"), 0);
    assert_eq!(sym(&a, "p1@x1"), 1);
}

#[test]
fn a_pasted_global_gives_each_iteration_its_own_scope() {
    let a = assembly(".for 0, 1\nrow\\i:\n@x:\n    BNE @x\n.endfor\n");
    assert_eq!(a.rom, vec![0xD0, 0xFE, 0xD0, 0xFE]);
    assert_eq!(sym(&a, "row0@x"), 0);
    assert_eq!(sym(&a, "row1@x"), 2);
}

#[test]
fn locals_in_a_phase_block_get_the_phased_address() {
    let a = assembly(
        ".phase $0300\nram_fn:\n@loop:\n    DEX\n    BNE @loop\n    JMP @loop\n.dephase\n",
    );
    assert_eq!(a.rom, vec![0xCA, 0xD0, 0xFD, 0x4C, 0x00, 0x03]);
    assert_eq!(sym(&a, "ram_fn@loop"), 0x0300);
}

#[test]
fn an_anonymous_label_counts_across_a_local() {
    assert_eq!(
        bytes("p1:\n    BEQ :+\n@x:\n    NOP\n:\n    RTS\n"),
        vec![0xF0, 0x01, 0xEA, 0x60]
    );
}

#[test]
fn ifdef_and_bank_resolve_locals() {
    assert_eq!(
        bytes("p1:\n@x:\n.ifdef @x\n.db 1\n.else\n.db 2\n.endif\n.ifndef @y\n.db 3\n.endif\n"),
        vec![1, 3]
    );
    assert_eq!(bytes("p1:\n@x:\n.db BANK(p1@x)\n"), vec![0]);
}

#[test]
fn emitted_source_locals_are_private_to_each_invocation() {
    let a = assemble_with(
        "p1:\n.gen\n.gen\n",
        &raw(),
        source_resolver("@l:\n    DEX\n    BNE @l\n"),
    )
    .expect("assembles");
    assert_eq!(a.rom, vec![0xCA, 0xD0, 0xFD, 0xCA, 0xD0, 0xFD]);
}

#[test]
fn a_label_in_a_false_if_opens_no_scope() {
    assert_eq!(
        bytes("p1:\n.if 0\np2:\n.endif\n@x:\n    .db 1\n    JMP p1@x\n"),
        vec![1, 0x4C, 0x00, 0x00]
    );
}

#[test]
fn a_unique_macro_label_opens_no_scope_but_a_plain_one_does() {
    assert_eq!(
        bytes(".macrodef wait\nwt\\@:\n    BNE @done\n.endm\np1:\n    .macro wait\n@done:\n    JMP p1@done\n"),
        vec![0xD0, 0x00, 0x4C, 0x02, 0x00]
    );
    assert_eq!(
        bytes(
            ".macrodef open\ninner:\n    NOP\n.endm\np1:\n    .macro open\n@x:\n    JMP inner@x\n"
        ),
        vec![0xEA, 0x4C, 0x01, 0x00]
    );
}

#[test]
fn a_rs_reservation_opens_no_scope() {
    assert_eq!(
        bytes("p1:\n.rsset $10\nvar .rs 1\n@x:\n    JMP p1@x\n"),
        vec![0x4C, 0x00, 0x00]
    );
}

#[test]
fn table_entries_may_use_locals() {
    assert_eq!(
        bytes("tbl:\n.table 2\n[0] .db 1\n[1] .db 2\n.endtable\n@after:\n.dw tbl@after\n"),
        vec![1, 2, 2, 0]
    );
}

// -- errors -----------------------------------------------------------------

#[test]
fn a_local_before_any_label_has_no_scope() {
    assert_eq!(
        error("@x:\n    RTS\n"),
        (1, "Local label `@x` has no enclosing label".to_string())
    );
    assert_eq!(
        error("    JMP @x\np1:\n"),
        (1, "Local label `@x` has no enclosing label".to_string())
    );
}

#[test]
fn a_local_defined_twice_in_a_scope_is_an_error() {
    assert_eq!(
        error("p1:\n@x:\n    NOP\n@x:\n    RTS\n"),
        (4, "`@x` is already defined in `p1` (line 2)".to_string())
    );
}

#[test]
fn a_local_defined_twice_across_files_names_the_other_file() {
    let dir = TempDir::new();
    dir.write("inc.asm", "p1:\n@x:\n    NOP\n");
    let main = dir.write("main.asm", ".include \"inc.asm\"\np1:\n@x:\n    RTS\n");
    let e = assemble_file(&main, &raw()).expect_err("errors").0;
    assert_eq!(
        (e.line, e.message.as_str()),
        (3, "`@x` is already defined in `p1` (`inc.asm` line 2)")
    );
}

#[test]
fn a_file_included_twice_is_not_a_local_duplicate() {
    let dir = TempDir::new();
    dir.write("inc.asm", "tbl:\n@loc:\n.db 1\n");
    let main = dir.write("main.asm", ".include \"inc.asm\"\n.include \"inc.asm\"\n");
    let a = assemble_file(&main, &raw()).expect("assembles");
    assert_eq!(a.rom, vec![1, 1]);
}

#[test]
fn an_undefined_local_suggests_another_scopes() {
    assert_eq!(
        error("p1:\n@x:\n    RTS\np2:\n    JMP @x\n"),
        (
            5,
            "`@x` is not defined in `p2`; did you mean `p1@x`?".to_string()
        )
    );
    assert_eq!(
        error("p1:\n    RTS\np2:\n    JMP @x\n"),
        (4, "`@x` is not defined in `p2`".to_string())
    );
}

#[test]
fn an_undefined_qualified_reference() {
    assert_eq!(
        error("p1:\n    RTS\n    JMP p1@x\n"),
        (3, "`p1` has no local label `@x`".to_string())
    );
    assert_eq!(
        error("p1:\n    JMP nope@x\n"),
        (2, "Symbol `nope` was not defined".to_string())
    );
}

#[test]
fn a_qualified_name_cannot_be_defined() {
    let want =
        "`p1@a` is a qualified name and can only be referenced; define it as `@a:` under `p1`";
    assert_eq!(error("p1:\np1@a = 3\n"), (2, want.to_string()));
    assert_eq!(error("p1:\np1@a:\n"), (2, want.to_string()));
    assert_eq!(error("p1:\np1@a .rs 1\n"), (2, want.to_string()));
}

#[test]
fn a_local_name_can_only_be_a_label() {
    let want = "`@a` is a local name, so it can only be a label; give the constant a global name";
    assert_eq!(error("p1:\n@a = 3\n"), (2, want.to_string()));
    assert_eq!(error("p1:\n@a\n"), (2, want.to_string()));
    assert_eq!(error("p1:\n@a .rs 1\n"), (2, want.to_string()));
}

#[test]
fn a_plain_local_in_a_for_body_is_rejected() {
    let (line, message) = error("p1:\n.for 0, 1\n@x:\n.endfor\n");
    assert_eq!(line, 3);
    assert!(
        message.starts_with("Label `@x` is defined inside a `.for` body"),
        "{message}"
    );
}

#[test]
fn a_macro_body_defining_a_local_twice_reports_the_body_line() {
    let d = diag(".macrodef twice\n@x:\n    NOP\n@x:\n.endm\np1:\n    .macro twice\n");
    assert_eq!(
        (d.line, d.message.as_str()),
        (4, "`@x` is already defined in macro `twice` (line 2)")
    );
    assert_eq!(
        d.notes,
        vec!["in expansion of macro `twice` at stdin line 7"]
    );
}

#[test]
fn a_macro_body_reference_out_of_scope_reports_the_body_line() {
    let d = diag(".macrodef go\n    BNE @out\n.endm\np1:\n@out:\np2:\n    .macro go\n");
    assert_eq!(
        (d.line, d.message.as_str()),
        (2, "`@out` is not defined in `p2`; did you mean `p1@out`?")
    );
    assert_eq!(d.notes, vec!["in expansion of macro `go` at stdin line 7"]);
}

#[test]
fn a_private_local_macro_inside_a_for_body_is_label_in_for() {
    let (line, message) = error(&format!("{WAIT}p1:\n.for 0, 1\n    .macro wait\n.endfor\n"));
    assert_eq!(line, 2);
    assert!(
        message.starts_with("Label `@spin` is defined inside a `.for` body"),
        "{message}"
    );
}

// -- unaffected -------------------------------------------------------------

#[test]
fn unaffected_anonymous_labels_at_every_depth() {
    let src = ":\n:\n:\n:\n:\n:\n:\n    BNE :-------\n    BEQ :+++++++\n:\n:\n:\n:\n:\n:\n:\n";
    assert_eq!(bytes(src), vec![0xD0, 0xFF, 0xF0, 0x00]);
}

#[test]
fn unaffected_unique_macro_labels() {
    assert_eq!(
        bytes(".macrodef mm\nlb\\@:\n    BNE lb\\@\n.endm\n    .macro mm\n    .macro mm\n"),
        vec![0xD0, 0xFE, 0xD0, 0xFE]
    );
}

#[test]
fn unaffected_jump_table_from_plan_016() {
    assert_eq!(
        bytes(".for 0, 2\n    .dw handler\\i\n.endfor\n.for 0, 2\nhandler\\i:\n    RTS\n.endfor\n"),
        vec![0x06, 0x00, 0x07, 0x00, 0x08, 0x00, 0x60, 0x60, 0x60]
    );
}

#[test]
fn unaffected_labels_in_phase() {
    assert_eq!(
        bytes(".phase $0300\nfn:\n    JMP fn\n.dephase\n"),
        vec![0x4C, 0x00, 0x03]
    );
}

#[test]
fn unaffected_at_in_strings_chars_and_comments() {
    assert_eq!(
        bytes("; @nessemble-clobbers A\np1:\n.db '@' ; @x\n.ascii \"@x\"\n"),
        vec![0x40, 0x40, 0x78]
    );
}

#[test]
fn unaffected_project_root_include() {
    let dir = TempDir::new();
    dir.write(".nessemblerc", "{}\n");
    std::fs::create_dir_all(dir.0.join("lib")).unwrap();
    dir.write("lib/x.asm", ".db 7\n");
    let main = dir.write("main.asm", ".include \"@/lib/x.asm\"\n");
    assert_eq!(
        assemble_file(&main, &raw()).expect("assembles").rom,
        vec![7]
    );
}

// -- coverage ---------------------------------------------------------------

#[test]
fn a_source_map_with_locals_matches_the_same_program_with_globals() {
    let opts = Options {
        nes: false,
        source_map: true,
        ..Options::default()
    };
    let map = |src: &str| {
        assemble(src, &opts)
            .expect("assembles")
            .source_map
            .expect("map")
    };
    let local = map("p1:\n@loop:\n    DEX\n    BNE @loop\np2:\n@loop:\n    JMP @loop\n");
    let global = map("p1:\np1_loop:\n    DEX\n    BNE p1_loop\np2:\np2_loop:\n    JMP p2_loop\n");
    assert_eq!(local, global);
}
