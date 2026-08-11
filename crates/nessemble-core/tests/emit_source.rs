//! A custom directive's script may return `emit_source(text)` instead of
//! bytes: `text` is assembly source, expanded inline at the directive's own
//! call site — lexed, parsed, and executed exactly as if it had been written
//! there — rather than emitted as raw bytes (`plans/013-structured-data-parsing.md`
//! §6). These tests exercise the expansion end-to-end through
//! [`assemble_with`], using a hand-written [`CustomResolver`] (no Rhai
//! involved) so the assembler-side mechanics are isolated from the scripting
//! host's own `emit_source`/`dynamic_to_output` tests
//! (`nessemble-script/src/lib.rs`).

use std::cell::RefCell;
use std::rc::Rc;

use nessemble_core::{assemble_with, CustomOutput, CustomResolver, Options};

/// A resolver where `.gen` resolves to `CustomOutput::Source(source)` — the
/// literal text under test — and every other name is an error.
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

fn raw_options() -> Options {
    Options {
        nes: false,
        ..Options::default()
    }
}

fn assemble_raw(source: &str, resolver: CustomResolver) -> Vec<u8> {
    assemble_with(source, &raw_options(), resolver)
        .expect("assembles")
        .rom
}

#[test]
fn emitted_bytes_are_written_at_the_call_site() {
    let rom = assemble_raw(
        ".org $C000\n.db $01\n.gen\n.db $03\n",
        source_resolver(".db $02"),
    );
    assert_eq!(rom, vec![0x01, 0x02, 0x03]);
}

#[test]
fn a_label_defined_in_emitted_source_is_usable_right_after_it() {
    // `.gen` defines `target:` immediately before a NOP; the outer source's
    // `JMP target` — written *after* the directive — must see it, proving the
    // label is a real symbol, not something scoped to the expansion.
    let rom = assemble_raw(
        ".org $C000\n.gen\n    JMP target\n",
        source_resolver("target:\n    NOP"),
    );
    // `target` is defined right at the `.org` base, so its (raw-mode, offset-
    // from-`.org`) value is 0: NOP ($EA), then JMP ($4C) $00 $00.
    assert_eq!(rom, vec![0xEA, 0x4C, 0x00, 0x00]);
}

#[test]
fn a_label_defined_in_emitted_source_is_flagged_like_a_macro_label() {
    // Not visible in the `-l` list file by default (`--mlist` reveals it) —
    // it did not appear in the file's own text, the same reason a `.macro`
    // body's labels are flagged (`plans/013-structured-data-parsing.md` §13.2).
    let assembly = assemble_with(
        ".org $C000\ntarget:\n.gen\n",
        &raw_options(),
        source_resolver("inner:\n    NOP"),
    )
    .expect("assembles");
    let outer = assembly
        .symbols
        .iter()
        .find(|s| s.name == "target")
        .expect("outer label recorded");
    assert!(!outer.from_macro);
    let inner = assembly
        .symbols
        .iter()
        .find(|s| s.name == "inner")
        .expect("emitted label recorded");
    assert!(
        inner.from_macro,
        "emitted labels are flagged like macro ones"
    );
}

#[test]
fn a_nested_custom_directive_in_emitted_source_still_dispatches() {
    // `.gen` emits source that itself invokes `.inner` — an ordinary custom
    // directive, going through the same `exec_custom` dispatch as any other.
    let calls = Rc::new(RefCell::new(0usize));
    let counter = Rc::clone(&calls);
    let resolver: CustomResolver = Box::new(move |name, _ints, _texts, _base, _root| match name {
        "gen" => Ok(CustomOutput::Source(".inner\n".to_string())),
        "inner" => {
            *counter.borrow_mut() += 1;
            Ok(CustomOutput::Bytes(vec![0x99]))
        }
        other => Err(format!("unknown custom pseudo-instruction `.{other}`")),
    });
    let rom = assemble_raw(".org $C000\n.gen\n", resolver);
    assert_eq!(rom, vec![0x99]);
    // Each pass visits `.gen` once, and `.gen`'s expansion (which is itself
    // memoized alongside `.gen`) invokes `.inner` once per pass it runs.
    assert!(*calls.borrow() >= 1, "calls: {}", calls.borrow());
}

#[test]
fn emitted_source_that_fails_to_parse_names_the_directive_and_its_own_line() {
    // A dangling `+` with no right-hand operand is a parse error.
    let err = assemble_with(
        ".org $C000\n.gen\n",
        &raw_options(),
        source_resolver(".db 1 +"),
    )
    .expect_err("errors");
    // Reported on `.gen`'s own line (2), not a synthetic position inside the
    // emitted snippet — there is no file for an editor to open there.
    assert_eq!(err.0.line, 2);
    assert!(err.0.message.contains(".gen"), "message: {}", err.0.message);
}

#[test]
fn include_in_emitted_source_is_rejected_with_a_clear_error() {
    let err = assemble_with(
        ".org $C000\n.gen\n",
        &raw_options(),
        source_resolver(".include \"lib.asm\"\n"),
    )
    .expect_err("errors");
    assert!(
        err.0.message.contains(".include"),
        "message: {}",
        err.0.message
    );
}

#[test]
fn macro_in_emitted_source_is_rejected_with_a_clear_error() {
    let err = assemble_with(
        ".org $C000\n.gen\n",
        &raw_options(),
        source_resolver(".macro foo\n"),
    )
    .expect_err("errors");
    assert!(
        err.0.message.contains(".macro"),
        "message: {}",
        err.0.message
    );
}

#[test]
fn macrodef_in_emitted_source_is_rejected_with_a_clear_error() {
    let err = assemble_with(
        ".org $C000\n.gen\n",
        &raw_options(),
        source_resolver(".macrodef foo\n"),
    )
    .expect_err("errors");
    assert!(
        err.0.message.contains(".macrodef"),
        "message: {}",
        err.0.message
    );
}

#[test]
fn inestrn_in_emitted_source_is_rejected_with_a_clear_error() {
    let err = assemble_with(
        ".org $C000\n.gen\n",
        &raw_options(),
        source_resolver(".inestrn\n"),
    )
    .expect_err("errors");
    assert!(
        err.0.message.contains(".inestrn"),
        "message: {}",
        err.0.message
    );
}

#[test]
fn recursive_emit_source_past_the_depth_limit_errors_instead_of_overflowing() {
    // `.deep` always emits source that invokes `.deep` again — infinite
    // self-recursion, caught by a depth guard rather than the process stack.
    let resolver: CustomResolver = Box::new(move |name, _ints, _texts, _base, _root| {
        if name == "deep" {
            Ok(CustomOutput::Source(".deep\n".to_string()))
        } else {
            Err(format!("unknown custom pseudo-instruction `.{name}`"))
        }
    });
    let err = assemble_with(".org $C000\n.deep\n", &raw_options(), resolver).expect_err("errors");
    assert!(
        err.0.message.contains("too many levels deep"),
        "message: {}",
        err.0.message
    );
}

#[test]
fn emitted_bytes_are_source_mapped_to_the_directives_own_line() {
    let options = Options {
        nes: false,
        source_map: true,
        ..Options::default()
    };
    let assembly = assemble_with(
        ".org $C000\n.db $01\n.gen\n",
        &options,
        source_resolver(".db $02, $03"),
    )
    .expect("assembles");
    let map = assembly.source_map.expect("source map present");
    // `.gen` is line 3; both bytes it emits are attributed there, not to any
    // position inside the emitted snippet (which has no file of its own).
    let gen_span = map
        .spans
        .iter()
        .find(|s| s.rom_offset == 1)
        .expect("a span starting at the .gen directive's bytes");
    assert_eq!(gen_span.line, 3);
    assert_eq!(gen_span.len, 2);
}
