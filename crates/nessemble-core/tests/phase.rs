//! Tests for `.phase` / `.dephase`: labels defined inside a phase block take
//! the bank's run-time (post-swap) address, while ROM layout keeps flowing from
//! `.org`. Branch math and ROM placement are unaffected because the phase delta
//! cancels in any address difference and never touches the physical offset.

use nessemble_core::{assemble, Options};

/// Value of the named symbol in the assembled program.
fn sym(src: &str, name: &str) -> i64 {
    let a = assemble(src, &Options::default()).expect("assembles cleanly");
    a.symbols
        .iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("symbol `{name}` not found"))
        .value
}

/// `.phase` shifts a label to the run address; a `.dw` of it emits that address.
#[test]
fn phase_shifts_label_and_reference() {
    let src = "\
.phase $9000
here:
.dw here
";
    let a = assemble(src, &Options::default()).expect("assembles cleanly");
    // The label reads as the run address, not its physical offset (0).
    assert_eq!(a.rom, vec![0x00, 0x90], "`.dw here` should emit $9000");
    assert_eq!(sym(src, "here"), 0x9000);
}

/// After `.dephase`, later labels revert to their physical load address.
#[test]
fn dephase_reverts_to_load_address() {
    let src = "\
.phase $9000
one:
.dephase
two:
";
    assert_eq!(sym(src, "one"), 0x9000);
    assert_eq!(sym(src, "two"), 0x0000);
}

/// The delta is fixed at `.phase` time, so run addresses keep tracking correctly
/// as the location counter advances — including across a later `.org`.
#[test]
fn phase_delta_survives_org() {
    // Raw `.org` must land in $C000-$FFFF; load base $C000 mapped to run $8000.
    let src = "\
.org $C000
.phase $8000
one:
.org $D000
two:
";
    assert_eq!(sym(src, "one"), 0x8000);
    // run($D000) = $8000 + ($D000 - $C000) = $9000
    assert_eq!(sym(src, "two"), 0x9000);
}

/// A bank switch clears any active phase so it never leaks into the next bank.
#[test]
fn phase_resets_on_bank_switch() {
    let src = "\
.phase $9000
one:
.prg 1
two:
";
    assert_eq!(sym(src, "one"), 0x9000);
    assert_eq!(sym(src, "two"), 0x0000);
}

/// A relative branch inside a phase block resolves identically to the unphased
/// program: the phase delta cancels in `target - pc`, and ROM bytes are unchanged.
#[test]
fn branch_unaffected_by_phase() {
    let phased = "\
.org $C000
.phase $8000
loop:
    nop
    bne loop
";
    let plain = "\
.org $C000
loop:
    nop
    bne loop
";
    let a = assemble(phased, &Options::default()).expect("phased assembles");
    let b = assemble(plain, &Options::default()).expect("plain assembles");
    // NOP ($EA), BNE ($D0), relative -3 ($FD).
    assert_eq!(a.rom, vec![0xEA, 0xD0, 0xFD]);
    assert_eq!(a.rom, b.rom, "phase must not change emitted branch bytes");
}
