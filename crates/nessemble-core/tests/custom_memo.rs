//! A custom pseudo-op's resolver runs **once per invocation site**, not once per
//! assembler pass.
//!
//! Both passes visit every directive, so a script used to execute twice per
//! assembly. Worse than the cost: a script whose output is not deterministic
//! could return a different *number of bytes* on each pass, which sizes the ROM
//! from pass 1 and then cannot write that many bytes on pass 2. These tests pin
//! the memoization down from the outside, through `assemble_with`.

use std::cell::RefCell;
use std::rc::Rc;

use nessemble_core::{assemble_with, CustomResolver, Options};

/// A resolver that records every call and returns bytes from `outputs` in order,
/// repeating the last entry once exhausted. Returning *different* bytes per call
/// is what makes a repeat resolution visible in the output.
fn counting_resolver(outputs: Vec<Vec<u8>>) -> (CustomResolver, Rc<RefCell<Vec<String>>>) {
    let calls = Rc::new(RefCell::new(Vec::new()));
    let log = Rc::clone(&calls);
    let resolver: CustomResolver = Box::new(move |name, ints, texts, _base, _root| {
        let mut log = log.borrow_mut();
        let nth = log.len();
        log.push(format!(".{name} {ints:?} {texts:?}"));
        let last = outputs.len().saturating_sub(1);
        Ok(outputs[nth.min(last)].clone())
    });
    (resolver, calls)
}

/// Assemble `source` as a raw (headerless) binary with the given resolver.
fn assemble_raw(source: &str, resolver: CustomResolver) -> Vec<u8> {
    let options = Options {
        nes: false,
        ..Options::default()
    };
    assemble_with(source, &options, resolver)
        .expect("assembles")
        .rom
}

#[test]
fn one_directive_resolves_once_across_both_passes() {
    let (resolver, calls) = counting_resolver(vec![vec![0x11], vec![0x22]]);
    let rom = assemble_raw(".org $C000\n.foo 1, 2\n", resolver);

    assert_eq!(calls.borrow().len(), 1, "calls: {:?}", calls.borrow());
    assert_eq!(calls.borrow()[0], ".foo [1, 2] []");
    // The single resolution's bytes are what got emitted.
    assert_eq!(rom, vec![0x11]);
}

#[test]
fn a_non_deterministic_resolver_no_longer_skews_between_passes() {
    // Three bytes on the first call, one on the second: pass 1 would size the
    // ROM for three and pass 2 emit one (or vice versa). With one resolution
    // there is nothing to disagree about.
    let (resolver, calls) = counting_resolver(vec![vec![0xAA, 0xBB, 0xCC], vec![0xFF]]);
    let rom = assemble_raw(".org $C000\n.noise 3\n", resolver);

    assert_eq!(calls.borrow().len(), 1, "calls: {:?}", calls.borrow());
    assert_eq!(rom, vec![0xAA, 0xBB, 0xCC]);
}

#[test]
fn separate_call_sites_resolve_separately() {
    // Two identical-looking directives are two invocations: a script that
    // deliberately varies its output (the extending docs' `.noise`) must still
    // vary between call sites, so the site is part of the memo key.
    let (resolver, calls) = counting_resolver(vec![vec![0x01], vec![0x02]]);
    let rom = assemble_raw(".org $C000\n.foo 7\n.foo 7\n", resolver);

    assert_eq!(calls.borrow().len(), 2, "calls: {:?}", calls.borrow());
    assert_eq!(rom, vec![0x01, 0x02]);
}

#[test]
fn distinct_arguments_resolve_separately() {
    let (resolver, calls) = counting_resolver(vec![vec![0x01], vec![0x02]]);
    let rom = assemble_raw(".org $C000\n.foo 1\n.foo 2\n", resolver);

    assert_eq!(calls.borrow().len(), 2, "calls: {:?}", calls.borrow());
    assert_eq!(calls.borrow()[0], ".foo [1] []");
    assert_eq!(calls.borrow()[1], ".foo [2] []");
    assert_eq!(rom, vec![0x01, 0x02]);
}

#[test]
fn string_arguments_are_part_of_the_key() {
    let (resolver, calls) = counting_resolver(vec![vec![0x01], vec![0x02]]);
    let rom = assemble_raw(".org $C000\n.foo \"a\"\n.foo \"b\"\n", resolver);

    assert_eq!(calls.borrow().len(), 2, "calls: {:?}", calls.borrow());
    assert_eq!(calls.borrow()[0], ".foo [] [\"a\"]");
    assert_eq!(calls.borrow()[1], ".foo [] [\"b\"]");
    assert_eq!(rom, vec![0x01, 0x02]);
}

#[test]
fn a_forward_referenced_argument_resolves_on_each_pass() {
    // `later` is undefined on pass 1 (evaluating to 1) and real on pass 2, so the
    // two passes are genuinely different invocations and both must run. The two
    // padding bytes put `later` at 3, since a label whose value happened to be 1
    // would match the pass-1 placeholder and legitimately memoize.
    let (resolver, calls) = counting_resolver(vec![vec![0x01], vec![0x02]]);
    let rom = assemble_raw(".org $C000\n.foo later\n.db $00, $00\nlater:\n", resolver);

    let log = calls.borrow();
    assert_eq!(log.len(), 2, "calls: {log:?}");
    assert_eq!(
        log[0], ".foo [1] []",
        "pass 1 saw the undefined placeholder"
    );
    assert_eq!(log[1], ".foo [3] []", "pass 2 saw the resolved address");
    // Pass 2's resolution is the one that reaches the ROM.
    assert_eq!(rom, vec![0x02, 0x00, 0x00]);
}

#[test]
fn a_resolver_error_is_reported_once_and_asked_once() {
    let calls = Rc::new(RefCell::new(0usize));
    let counter = Rc::clone(&calls);
    let resolver: CustomResolver = Box::new(move |_name, _ints, _texts, _base, _root| {
        *counter.borrow_mut() += 1;
        Err("no such easing type".to_string())
    });

    let options = Options {
        nes: false,
        ..Options::default()
    };
    let err = assemble_with(".org $C000\n.foo 1\n", &options, resolver).expect_err("errors");

    assert_eq!(err.0.message, "no such easing type");
    assert_eq!(*calls.borrow(), 1, "the failing resolver ran once");
}
