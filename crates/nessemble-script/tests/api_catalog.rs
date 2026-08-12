//! Drift gate between the engine's registrations and the host API catalog
//! (`nessemble-script-api`).
//!
//! The catalog is what the docs table of contents, `nessemble reference
//! script`, and the language server all read, so a function registered without
//! an entry is a function nobody can discover, and an entry that outlives its
//! function is documentation that lies. Both directions are checked here, in
//! the crate that owns [`engine`](../src/lib.rs).
//!
//! **Why a source scan and not the engine itself.** Asking Rhai — via
//! `Engine::gen_fn_signatures` — would be the better check, and needs Rhai's
//! `metadata` feature, which does not build in this workspace: rhai 1.25.1
//! fails with `no method named with_params_info found for struct
//! FuncRegistration`, out of `#[export_module]` expansions inside its own
//! packages. Scanning the registration literals catches the drift that actually
//! happens (a `register_fn` added without a doc entry) at no dependency cost.
//! See `plans/014-scripting-docs-and-tooling.md` §3.3 and §11.2.

use std::collections::BTreeSet;

use nessemble_script_api::{Origin, SCRIPT_API};

/// The engine's own source. Registrations live in `engine`/`engine_recording`,
/// which are in this file and above its test module.
const ENGINE_SOURCE: &str = include_str!("../src/lib.rs");

/// Registered names that are deliberately absent from the catalog.
///
/// `path` is not a script-facing API: it is `rhai-fs`'s path-conversion hook,
/// redefined so a script's relative paths resolve against the directive's
/// source directory instead of the process CWD. A script never calls it by
/// name.
const NOT_SCRIPT_FACING: &[&str] = &["path"];

/// Every name the engine registers, read out of its source.
///
/// Finds each `.register_…(` call, then takes the first string literal that
/// follows it — which covers `register_fn("x", …)`, the multi-line form where
/// the literal is on the next line, and `register_type_with_name::<T>("x")`
/// alike. A call whose first argument is not a literal (the wholesale
/// `register_into_engine(&mut engine)` package installs) contributes nothing,
/// which is correct: those functions are catalogued by hand as
/// [`Origin::Package`], because their names are not in this source at all.
fn registered_names() -> BTreeSet<String> {
    // Registrations all precede the test module; and a commented-out or merely
    // mentioned `register_*` must not count.
    let source = ENGINE_SOURCE.split("#[cfg(test)]").next().unwrap_or("");
    let code: String = source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");

    let bytes = code.as_bytes();
    let mut names = BTreeSet::new();
    let mut search = 0;
    while let Some(found) = code[search..].find(".register_") {
        let mut i = search + found + ".register_".len();
        search = i;
        // Skip the method name and any turbofish, up to the opening paren.
        let Some(paren) = code[i..].find('(') else {
            break;
        };
        i += paren + 1;
        // The first argument, if it is a string literal, is the registered name.
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'"' {
            continue;
        }
        i += 1;
        let start = i;
        while i < bytes.len() && bytes[i] != b'"' {
            i += 1;
        }
        names.insert(code[start..i].to_string());
    }
    names
}

#[test]
fn every_registered_function_is_catalogued() {
    let registered = registered_names();
    assert!(
        registered.len() > 20,
        "the scan found only {} registrations — it has stopped matching the source, \
         which would make this whole gate vacuous",
        registered.len()
    );

    let catalogued: BTreeSet<&str> = SCRIPT_API.iter().map(|e| e.name).collect();
    let missing: Vec<&String> = registered
        .iter()
        .filter(|name| !catalogued.contains(name.as_str()))
        .filter(|name| !NOT_SCRIPT_FACING.contains(&name.as_str()))
        .collect();

    assert!(
        missing.is_empty(),
        "registered in `engine()` but absent from the catalog: {missing:?}\n\
         Add an entry to `nessemble-script-api`, or list it in `NOT_SCRIPT_FACING` \
         if a script never calls it by name."
    );
}

#[test]
fn every_catalogued_host_entry_is_registered() {
    let registered = registered_names();
    let stale: Vec<&str> = SCRIPT_API
        .iter()
        .filter(|e| e.origin == Origin::Host)
        .map(|e| e.name)
        .filter(|name| !registered.contains(*name))
        .collect();

    assert!(
        stale.is_empty(),
        "catalogued as host-registered but not registered in `engine()`: {stale:?}\n\
         Remove the entry, or mark it `Origin::Package(..)` if a Rhai package provides it."
    );
}

#[test]
fn package_provided_entries_are_not_registered_here() {
    // The point of the `Package` origin: these names are not in this crate's
    // source, so the scan cannot see them and they are curated by hand. If one
    // ever *does* get registered directly, its origin is wrong.
    //
    // Unless the catalog *also* carries a host entry under that name, which is
    // a deliberate, documented shape rather than a mistake: `read_blob` is both
    // `file.read_blob()` from `rhai-fs` and the one-call `read_blob(path)` this
    // crate registers. The registration belongs to the host entry; the package
    // entry is a different function that happens to share a name.
    let registered = registered_names();
    let host_names: BTreeSet<&str> = SCRIPT_API
        .iter()
        .filter(|e| e.origin == Origin::Host)
        .map(|e| e.name)
        .collect();
    let misfiled: Vec<&str> = SCRIPT_API
        .iter()
        .filter(|e| matches!(e.origin, Origin::Package(_)))
        .map(|e| e.name)
        .filter(|name| registered.contains(*name) && !host_names.contains(name))
        .collect();

    assert!(
        misfiled.is_empty(),
        "catalogued as package-provided but registered directly in `engine()`: {misfiled:?}"
    );
}

#[test]
fn the_scan_finds_the_registrations_it_is_supposed_to() {
    // A spot-check of each registration shape the source uses, so a scanner
    // regression fails here — naming the shape it broke on — rather than as a
    // confusing "missing from the catalog" report above.
    let registered = registered_names();
    for (name, shape) in [
        ("to_char", "register_fn(\"name\", func)"),
        ("parse_xml", "register_fn(\n    \"name\",\n    closure)"),
        ("width", "register_get(\"name\", func)"),
        ("xml_node", "register_type_with_name::<T>(\"name\")"),
        ("read_blob", "register_fn(\"name\", { block })"),
    ] {
        assert!(
            registered.contains(name),
            "the scan missed `{name}`, registered as {shape}"
        );
    }
}
