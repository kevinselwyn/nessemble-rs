//! Program recording (`plans/023-whole-program-clobber-analysis.md` §2.2,
//! Phase 1): what pass 2 emitted, and the guarantee that recording never
//! changes the assembled bytes.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use nessemble_core::program::{EmitKind, ProgramMap, Resolution, Site, Via};
use nessemble_core::{
    assemble_file, build_program, lenient_custom_resolver, CustomOutput, CustomResolver, Options,
};

/// Assemble an in-memory project (`(name, text)` pairs; the first is the
/// entry) under a virtual directory, recording the program.
fn build_with(files: &[(&str, &str)], options: &Options, custom: CustomResolver) -> ProgramMap {
    let dir = PathBuf::from("/nessemble-virtual");
    let owned: Vec<(PathBuf, String)> = files
        .iter()
        .map(|(n, t)| (dir.join(n), (*t).to_string()))
        .collect();
    let overlay = move |p: &Path| {
        owned
            .iter()
            .find(|(path, _)| path == p)
            .map(|(_, t)| t.clone())
    };
    let build = build_program(&dir.join(files[0].0), files[0].1, options, &overlay, custom);
    assert!(build.errors.is_empty(), "{:?}", build.errors);
    build.program.expect("a program")
}

fn build(files: &[(&str, &str)]) -> ProgramMap {
    build_with(
        files,
        &Options::default(),
        lenient_custom_resolver(HashSet::default()),
    )
}

fn label<'a>(p: &'a ProgramMap, name: &str) -> &'a nessemble_core::program::LabelDef {
    p.labels
        .iter()
        .find(|l| l.name == name)
        .unwrap_or_else(|| panic!("no label {name}"))
}

/// The target of the instruction emit on 1-based `line` of file 0.
fn target_on(p: &ProgramMap, line: u32) -> nessemble_core::program::Target {
    let e = p
        .emits
        .iter()
        .find(|e| e.site == Site { file: 0, line })
        .unwrap_or_else(|| panic!("no emit on line {line}"));
    match &e.kind {
        EmitKind::Instruction { target } => target.expect("a target"),
        k @ EmitKind::Data { .. } => panic!("not an instruction: {k:?}"),
    }
}

// ─── Byte identity ───────────────────────────────────────────────────────────

fn corpus_asm_files() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus");
    let mut out = Vec::new();
    for group in std::fs::read_dir(&root).unwrap() {
        let group = group.unwrap().path();
        if group.file_name().is_some_and(|n| n == "errors") || !group.is_dir() {
            continue;
        }
        for case in std::fs::read_dir(&group).unwrap() {
            let case = case.unwrap().path();
            let name = case.file_name().unwrap().to_string_lossy().into_owned();
            let asm = case.join(format!("{name}.asm"));
            if asm.is_file() {
                out.push(asm);
            }
        }
    }
    out
}

#[test]
fn recording_never_changes_the_bytes_of_any_corpus_golden() {
    let files = corpus_asm_files();
    assert!(files.len() > 40, "found {}", files.len());
    for asm in files {
        let undocumented = asm.to_string_lossy().contains("undocumented");
        let off = Options {
            undocumented,
            ..Options::default()
        };
        let on = Options {
            record_program: true,
            ..off.clone()
        };
        let a = assemble_file(&asm, &off);
        let b = assemble_file(&asm, &on);
        match (a, b) {
            (Ok(a), Ok(b)) => assert_eq!(a.rom, b.rom, "{}", asm.display()),
            (Err(a), Err(b)) => assert_eq!(a, b, "{}", asm.display()),
            _ => panic!("recording changed the outcome of {}", asm.display()),
        }
    }
}

#[test]
fn the_recorded_image_is_the_headerless_rom() {
    for asm in corpus_asm_files() {
        let undocumented = asm.to_string_lossy().contains("undocumented");
        let options = Options {
            undocumented,
            ..Options::default()
        };
        let Ok(asm_out) = assemble_file(&asm, &options) else {
            continue;
        };
        let source = std::fs::read_to_string(&asm).unwrap();
        let none = |_: &Path| None;
        let build = build_program(
            &asm,
            &source,
            &options,
            &none,
            lenient_custom_resolver(HashSet::default()),
        );
        let Some(program) = build.program else {
            panic!("{} did not build: {:?}", asm.display(), build.errors);
        };
        // Raw output is the image itself; an iNES file ends with it.
        assert!(asm_out.rom.ends_with(&program.image), "{}", asm.display());
    }
}

// ─── Emits ───────────────────────────────────────────────────────────────────

#[test]
fn emits_carry_offset_bank_phased_address_and_site() {
    let p = build(&[(
        "main.asm",
        "    lda #1\n    .phase $9000\nrun:\n    rts\n    .dephase\n    .db 1, 2\n",
    )]);
    assert_eq!(p.emits.len(), 3);
    let lda = &p.emits[0];
    assert_eq!((lda.rom_offset, lda.len, lda.bank), (0, 2, 0));
    assert_eq!(lda.site, Site { file: 0, line: 1 });
    assert!(matches!(lda.kind, EmitKind::Instruction { target: None }));
    let rts = &p.emits[1];
    assert_eq!((rts.rom_offset, rts.cpu_addr), (2, 0x9000));
    assert_eq!(label(&p, "run").cpu_addr, 0x9000);
    assert_eq!(label(&p, "run").rom_offset, 2);
    match &p.emits[2].kind {
        EmitKind::Data { directive } => assert_eq!(directive, ".db"),
        k @ EmitKind::Instruction { .. } => panic!("{k:?}"),
    }
}

#[test]
fn suppressed_statements_and_the_trainer_make_no_emits() {
    let p = build(&[("main.asm", "    .if 0\n    lda #1\n    .endif\n    rts\n")]);
    assert_eq!(p.emits.len(), 1);
    assert_eq!(p.emits[0].site.line, 4);
}

#[test]
fn a_nested_macro_names_the_outermost_call() {
    let p = build(&[(
        "main.asm",
        ".macrodef inner\n    ldx #1\n.endm\n.macrodef outer\n    .macro inner\n.endm\n\n    .macro outer\n",
    )]);
    assert_eq!(p.emits.len(), 1);
    assert_eq!(p.emits[0].site, Site { file: 0, line: 8 });
    assert_eq!(p.emits[0].via, Some(Via::Macro("outer".into())));
}

#[test]
fn a_for_body_emits_once_per_iteration_at_the_body_line() {
    let p = build(&[("main.asm", ".for 0, 2\n    inx\n.endfor\n")]);
    assert_eq!(p.emits.len(), 3);
    assert!(p.emits.iter().all(|e| e.site.line == 2));
    assert_eq!(
        p.emits.iter().map(|e| e.rom_offset).collect::<Vec<_>>(),
        [0, 1, 2]
    );
}

#[test]
fn emit_source_names_the_pseudo_op_line() {
    let custom: CustomResolver =
        Box::new(|_, _, _, _, _| Ok(CustomOutput::Source("    ldy #2\n    rts\n".into())));
    let p = build_with(
        &[("main.asm", "    nop\n    .gen\n")],
        &Options::default(),
        custom,
    );
    assert_eq!(p.emits.len(), 3);
    for e in &p.emits[1..] {
        assert_eq!(e.site, Site { file: 0, line: 2 });
        assert_eq!(e.via, Some(Via::Pseudo("gen".into())));
        assert!(matches!(e.kind, EmitKind::Instruction { .. }));
    }
}

#[test]
fn custom_bytes_are_data_named_after_the_pseudo_op() {
    let custom: CustomResolver = Box::new(|_, _, _, _, _| Ok(CustomOutput::Bytes(vec![0xEA])));
    let p = build_with(&[("main.asm", "    .foo\n")], &Options::default(), custom);
    match &p.emits[0].kind {
        EmitKind::Data { directive } => assert_eq!(directive, ".foo"),
        k @ EmitKind::Instruction { .. } => panic!("{k:?}"),
    }
}

#[test]
fn an_unmodelled_custom_op_is_marked() {
    let known = std::iter::once("foo".to_string()).collect();
    let p = build_with(
        &[("main.asm", "    nop\n    .foo\n")],
        &Options::default(),
        lenient_custom_resolver(known),
    );
    assert_eq!(p.unmodelled.len(), 1);
    assert_eq!(p.unmodelled[0].site.line, 2);
    assert_eq!(p.unmodelled[0].rom_offset, 1);
}

// ─── Anchors ─────────────────────────────────────────────────────────────────

#[test]
fn anchors_pin_labels_offsets_anonymous_and_pasted_names() {
    let p = build(&[(
        "main.asm",
        "\
start:
    jmp start
    jmp start+2
    bne :+
    nop
:
    jsr row1
    jmp $0003
.for 0, 1
row\\i:
    rts
.endfor
",
    )]);
    let start = label(&p, "start");
    assert_eq!(target_on(&p, 2).anchor, Some((start.rom_offset, 0)));
    assert_eq!(target_on(&p, 3).anchor, Some((start.rom_offset, 2)));
    let anon = target_on(&p, 4);
    assert_eq!(anon.anchor, Some((9, 0)));
    let row1 = label(&p, "row1");
    assert_eq!(target_on(&p, 7).anchor, Some((row1.rom_offset, 0)));
    // A number names no label.
    assert_eq!(target_on(&p, 8).anchor, None);
    assert_eq!(target_on(&p, 8).addr, 3);
}

#[test]
fn a_constant_operand_has_no_anchor() {
    let p = build(&[(
        "main.asm",
        "SUB = 4\n    jsr SUB\n    nop\n    nop\n    rts\n",
    )]);
    let t = target_on(&p, 2);
    assert_eq!((t.addr, t.anchor), (4, None));
}

#[test]
fn the_same_address_in_two_banks_resolves_by_anchor_and_is_ambiguous_by_number() {
    let p = build(&[(
        "main.asm",
        "\
.prg 0
    jmp there
    jmp $0001
.prg 1
    nop
there:
    rts
",
    )]);
    let there = label(&p, "there");
    assert_eq!(there.bank, 1);
    let anchored = target_on(&p, 2);
    assert_eq!(p.resolve(&anchored, 0), Resolution::At(there.rom_offset));
    // `$0001` is assembled in bank 0 (the `jmp` operand) and bank 1 (`rts`):
    // from bank 0 the current bank wins.
    let numeric = target_on(&p, 3);
    assert_eq!(p.resolve(&numeric, 0), Resolution::At(1));
    // From a third bank, both are candidates.
    assert_eq!(p.resolve(&numeric, 2), Resolution::Ambiguous(vec![0, 1]));
}

#[test]
fn overlapping_phase_blocks_in_one_bank_are_ambiguous_unanchored() {
    let p = build(&[(
        "main.asm",
        "\
    .phase $9000
first:
    rts
    .dephase
    .phase $9000
second:
    rts
    .dephase
    jmp $9000
    jmp second
",
    )]);
    let numeric = target_on(&p, 9);
    assert_eq!(p.resolve(&numeric, 0), Resolution::Ambiguous(vec![0]));
    let anchored = target_on(&p, 10);
    assert_eq!(
        p.resolve(&anchored, 0),
        Resolution::At(label(&p, "second").rom_offset)
    );
}

#[test]
fn fall_through_follows_the_rom_or_resolves_the_next_address() {
    let p = build(&[("main.asm", "    nop\n    nop\n")]);
    assert_eq!(p.fall_through(0, 1), Resolution::At(1));
    assert_eq!(p.fall_through(1, 1), Resolution::Nothing);
}
