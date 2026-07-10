//! Hermetic parity tests against the committed golden ROMs for the in-scope
//! (Phase 2) subset of the reference corpus. No oracle binary or network is
//! required — the `.rom` files are the goldens the reference v1.1.1 binary
//! produces (verified separately by `xtask verify-goldens`).

use std::path::{Path, PathBuf};

use nessemble_core::{assemble, AssembleError, Options};

fn corpus_dir() -> PathBuf {
    // crates/nessemble-core -> repo root -> tests/corpus
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/corpus")
        .canonicalize()
        .expect("corpus dir exists")
}

/// Assemble `<dir>/<name>.asm` and compare to the golden `<dir>/<name>.rom`.
fn check_ok(group: &str, name: &str, undocumented: bool) {
    let dir = corpus_dir().join(group).join(name);
    let src = std::fs::read_to_string(dir.join(format!("{name}.asm"))).unwrap();
    let golden = std::fs::read(dir.join(format!("{name}.rom"))).unwrap();
    let opts = Options {
        undocumented,
        ..Options::default()
    };
    match assemble(&src, &opts) {
        Ok(a) => assert_eq!(a.rom, golden, "ROM mismatch for {group}/{name}"),
        Err(e) => panic!("{group}/{name} failed to assemble: {e:?}"),
    }
}

/// Assemble an error case and compare the formatted diagnostic to the golden.
fn check_err(name: &str) {
    let dir = corpus_dir().join("errors").join(name);
    let src = std::fs::read_to_string(dir.join(format!("{name}.asm"))).unwrap();
    let golden = std::fs::read_to_string(dir.join(format!("{name}.rom"))).unwrap();
    match assemble(&src, &Options::default()) {
        Ok(_) => panic!("{name} unexpectedly assembled"),
        Err(AssembleError::Diagnostic(d)) => {
            let formatted = format!("Error in `{name}.asm` on line {}: {}\n", d.line, d.message);
            assert_eq!(formatted, golden, "diagnostic mismatch for errors/{name}");
        }
    }
}

#[test]
fn all_opcodes_match() {
    let dir = corpus_dir().join("opcodes");
    let mut count = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if !path.is_dir() {
            continue;
        }
        let name = path.file_name().unwrap().to_str().unwrap().to_string();
        if !path.join(format!("{name}.asm")).is_file() {
            continue;
        }
        // The `undocumented` case is assembled with -u, like the reference test.
        check_ok("opcodes", &name, name == "undocumented");
        count += 1;
    }
    assert!(count > 40, "expected many opcode cases, found {count}");
}

#[test]
fn simple_examples_match() {
    // Non-iNES examples using only Phase 2 features.
    for name in [
        "ascii",
        "bases",
        "comments",
        "db",
        "dot-notation",
        "dw",
        "fill",
        "hibytes",
        "highlow",
        "instructions",
        "labels",
        "labels-local",
        "lobytes",
        "math",
        "org",
    ] {
        check_ok("examples", name, false);
    }
}

#[test]
fn ines_banking_and_directive_examples_match() {
    // Full iNES output, PRG/CHR banking, and the Phase 3 directives.
    for name in [
        "ines",
        "bank",
        "mmc1",
        "mmc1chrram",
        "segment",
        "checksum",
        "color",
        "enum",
        "random",
        "rs",
    ] {
        check_ok("examples", name, false);
    }
    for name in ["square1", "triad"] {
        check_ok("nerdy-nights", name, false);
    }
}

#[test]
fn error_cases_match() {
    for name in [
        "undefined-symbol",
        "opcode",
        "mode",
        "branch-minus",
        "branch-plus",
    ] {
        check_err(name);
    }
}
