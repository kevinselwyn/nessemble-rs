//! A `file://` prefix declares a directive's filename argument to be an input
//! file. It is stripped before the path is used — so a script or importer sees
//! exactly what it would have seen without the declaration — and, on a custom
//! pseudo-op, a *missing* declared file is reported against the directive before
//! the script runs.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};

use nessemble_core::{assemble_file_with, strip_file_url, CustomResolver, Options};

/// A throwaway directory tree, removed on drop.
struct TempTree {
    root: PathBuf,
}

impl TempTree {
    fn new() -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = format!(
            "nessemble-fileurl-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let root = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&root).expect("create temp root");
        TempTree { root }
    }

    fn write(&self, rel: &str, contents: &[u8]) -> PathBuf {
        let path = self.root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent dir");
        }
        std::fs::write(&path, contents).expect("write file");
        path
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A resolver that records the `texts` it was handed and emits one byte.
fn recording_resolver() -> (CustomResolver, Rc<RefCell<Vec<Vec<String>>>>) {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let log = Rc::clone(&seen);
    let resolver: CustomResolver = Box::new(move |_name, _ints, texts, _base, _root| {
        log.borrow_mut().push(texts.to_vec());
        Ok(vec![0x42])
    });
    (resolver, seen)
}

fn raw_options() -> Options {
    Options {
        nes: false,
        ..Options::default()
    }
}

#[test]
fn strip_file_url_is_a_prefix_not_a_url_scheme() {
    assert_eq!(strip_file_url("file://map.png"), ("map.png", true));
    assert_eq!(strip_file_url("map.png"), ("map.png", false));
    // The third slash is just the start of an absolute path.
    assert_eq!(
        strip_file_url("file:///tmp/map.png"),
        ("/tmp/map.png", true)
    );
    // Only a *leading* prefix counts.
    assert_eq!(strip_file_url("a file://b"), ("a file://b", false));
    assert_eq!(strip_file_url("file://"), ("", true));
}

#[test]
fn a_declared_path_reaches_the_script_stripped() {
    let tree = TempTree::new();
    tree.write("map.png", b"not really a png");
    let src = tree.write("main.asm", b".org $C000\n.tilemap \"file://map.png\"\n");

    let (resolver, seen) = recording_resolver();
    let rom = assemble_file_with(&src, &raw_options(), resolver)
        .expect("assembles")
        .rom;

    assert_eq!(seen.borrow().as_slice(), [vec!["map.png".to_string()]]);
    assert_eq!(rom, vec![0x42]);
}

#[test]
fn an_undeclared_path_is_unchanged() {
    let tree = TempTree::new();
    let src = tree.write("main.asm", b".org $C000\n.tilemap \"map.png\"\n");

    let (resolver, seen) = recording_resolver();
    assemble_file_with(&src, &raw_options(), resolver).expect("assembles");

    // No existence check, no rewriting: the script decides what to do with it.
    assert_eq!(seen.borrow().as_slice(), [vec!["map.png".to_string()]]);
}

#[test]
fn a_missing_declared_file_errors_and_the_script_never_runs() {
    let tree = TempTree::new();
    let src = tree.write("main.asm", b".org $C000\n.tilemap \"file://gone.png\"\n");

    let (resolver, seen) = recording_resolver();
    let err = assemble_file_with(&src, &raw_options(), resolver).expect_err("errors");

    assert!(
        err.0.message.contains("gone.png"),
        "message: {}",
        err.0.message
    );
    // The bare path, not the declaration, is what the diagnostic names.
    assert!(
        !err.0.message.contains("file://"),
        "message: {}",
        err.0.message
    );
    assert_eq!(err.0.line, 2, "reported against the directive's own line");
    assert!(seen.borrow().is_empty(), "the script must not have run");
}

#[test]
fn a_declared_path_resolves_against_the_containing_file() {
    // The declared file sits beside the *included* file, not the top-level one,
    // so a naive resolve against the project root would miss it.
    let tree = TempTree::new();
    tree.write("sub/data.bin", b"\x01\x02");
    tree.write("sub/part.asm", b".tilemap \"file://data.bin\"\n");
    let src = tree.write("main.asm", b".org $C000\n.include \"sub/part.asm\"\n");

    let (resolver, seen) = recording_resolver();
    assemble_file_with(&src, &raw_options(), resolver).expect("assembles");

    assert_eq!(seen.borrow().as_slice(), [vec!["data.bin".to_string()]]);
}

#[test]
fn an_absolute_declared_path_is_used_as_is() {
    let tree = TempTree::new();
    let asset = tree.write("assets/logo.chr", b"\xAA");
    let source = format!(
        ".org $C000\n.tilemap \"file://{}\"\n",
        asset.to_string_lossy()
    );
    let src = tree.write("main.asm", source.as_bytes());

    let (resolver, seen) = recording_resolver();
    assemble_file_with(&src, &raw_options(), resolver).expect("assembles");

    assert_eq!(
        seen.borrow().as_slice(),
        [vec![asset.to_string_lossy().to_string()]]
    );
}

#[test]
fn incbin_accepts_a_declared_path() {
    let tree = TempTree::new();
    tree.write("logo.chr", b"\x01\x02\x03");
    let src = tree.write("main.asm", b".org $C000\n.incbin \"file://logo.chr\"\n");

    let rom = assemble_file(&src);
    assert_eq!(rom, vec![0x01, 0x02, 0x03]);
}

#[test]
fn incrle_accepts_a_declared_path() {
    let tree = TempTree::new();
    // RLE of three identical bytes, whatever the encoder makes of it: the point
    // is that the file was found at all.
    tree.write("run.bin", b"\x07\x07\x07");
    let src = tree.write("main.asm", b".org $C000\n.incrle \"file://run.bin\"\n");
    let declared = assemble_file(&src);

    let plain_src = tree.write("plain.asm", b".org $C000\n.incrle \"run.bin\"\n");
    let plain = assemble_file(&plain_src);

    assert_eq!(declared, plain, "the declaration changes nothing");
    assert!(!declared.is_empty());
}

#[test]
fn include_accepts_a_declared_path() {
    let tree = TempTree::new();
    tree.write("defs.asm", b".db $99\n");
    let src = tree.write("main.asm", b".org $C000\n.include \"file://defs.asm\"\n");

    assert_eq!(assemble_file(&src), vec![0x99]);
}

#[test]
fn a_missing_declared_importer_path_names_the_bare_path() {
    let tree = TempTree::new();
    let src = tree.write("main.asm", b".org $C000\n.incbin \"file://gone.chr\"\n");

    let err = nessemble_core::assemble_file(&src, &raw_options()).expect_err("errors");
    assert!(
        err.0.message.contains("gone.chr") && !err.0.message.contains("file://"),
        "message: {}",
        err.0.message
    );
}

/// Assemble `path` as a raw binary with no custom pseudo-ops.
fn assemble_file(path: &Path) -> Vec<u8> {
    nessemble_core::assemble_file(path, &raw_options())
        .expect("assembles")
        .rom
}

#[test]
fn diagnostics_report_a_missing_declared_file_with_no_resolver_of_their_own() {
    // The check lives ahead of the resolver, and the diagnostics path stubs only
    // the resolver — so tooling (the language server) reports a missing declared
    // file without doing any filesystem work of its own.
    let tree = TempTree::new();
    let src = tree.write("main.asm", b".org $C000\n.tilemap \"file://gone.png\"\n");
    let text = std::fs::read_to_string(&src).expect("read back");

    let known = ["tilemap".to_string()].into_iter().collect();
    let d = nessemble_core::diagnose_source_with(
        &src,
        &text,
        &raw_options(),
        None,
        nessemble_core::lenient_custom_resolver(known),
    );

    assert!(
        d.errors
            .iter()
            .any(|e| e.line == 2 && e.message.contains("gone.png")),
        "errors: {:?}",
        d.errors
    );
}
