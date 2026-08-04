//! A filename argument beginning with `@/` resolves from the **project root**
//! instead of the directory of the file that contains it — see
//! `plans/012-project-root-paths.md`. These tests exercise the feature
//! end-to-end (through [`assemble_file`]/[`assemble_file_with`]), covering the
//! root ladder (`Options::project_root`, a `.nessemblerc` marker, and the
//! entry-file fallback), the built-in filename directives, and declared
//! (`file://`) custom-pseudo-op arguments.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};

use nessemble_core::{assemble_file, assemble_file_with, AssembleError, CustomResolver, Options};

/// A throwaway directory tree, removed on drop. Canonicalized at creation (as
/// `nessemble_core`'s own root discovery does internally) so paths built from
/// `root` compare equal to the ones the assembler resolves, even where the
/// system temp directory is itself a symlink.
struct TempTree {
    root: PathBuf,
}

impl TempTree {
    fn new() -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = format!(
            "nessemble-projroot-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let root = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&root).expect("create temp root");
        let root = root.canonicalize().expect("canonicalize temp root");
        TempTree { root }
    }

    /// Write `contents` to `rel` (creating parent directories), returning its
    /// absolute path.
    fn write(&self, rel: &str, contents: &[u8]) -> PathBuf {
        let path = self.root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent dir");
        }
        std::fs::write(&path, contents).expect("write file");
        path
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }
}

impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn asm(path: &Path) -> Vec<u8> {
    assemble_file(path, &Options::default())
        .unwrap_or_else(|e| panic!("assembly failed: {e:?}"))
        .rom
}

/// A resolver that records the `texts` it was handed on every call and emits
/// one byte, mirroring `tests/file_url.rs`'s `recording_resolver`.
fn recording_resolver() -> (CustomResolver, Rc<RefCell<Vec<Vec<String>>>>) {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let log = Rc::clone(&seen);
    let resolver: CustomResolver = Box::new(move |_name, _ints, texts, _base| {
        log.borrow_mut().push(texts.to_vec());
        Ok(vec![0x42])
    });
    (resolver, seen)
}

// -- .include / .inestrn -----------------------------------------------------

#[test]
fn include_resolves_from_the_root_however_deep_the_referencing_file_is() {
    let tree = TempTree::new();
    tree.write("main.asm", b".include \"a/b/deep.asm\"\n");
    tree.write("a/b/deep.asm", b".include \"@/lib/defs.asm\"\n");
    tree.write("lib/defs.asm", b".db $42\n");

    assert_eq!(asm(&tree.path("main.asm")), vec![0x42]);
}

#[test]
fn incbin_resolves_from_the_root_from_a_nested_include() {
    let tree = TempTree::new();
    tree.write("main.asm", b".include \"a/b/deep.asm\"\n");
    tree.write("a/b/deep.asm", b".incbin \"@/assets/logo.chr\"\n");
    tree.write("assets/logo.chr", &[0xDE, 0xAD, 0xBE, 0xEF]);

    assert_eq!(asm(&tree.path("main.asm")), vec![0xDE, 0xAD, 0xBE, 0xEF]);
}

#[test]
fn a_nessemblerc_above_the_entry_file_sets_the_root_for_a_deep_entry() {
    let tree = TempTree::new();
    tree.write(".nessemblerc", b"");
    tree.write("src/deep/main.asm", b".include \"@/lib/defs.asm\"\n");
    tree.write("lib/defs.asm", b".db $07\n");

    assert_eq!(asm(&tree.path("src/deep/main.asm")), vec![0x07]);
}

#[test]
fn without_a_marker_the_entry_files_own_directory_is_the_root() {
    let tree = TempTree::new();
    tree.write("main.asm", b".include \"@/sibling.asm\"\n");
    tree.write("sibling.asm", b".db $09\n");

    assert_eq!(asm(&tree.path("main.asm")), vec![0x09]);
}

#[test]
fn an_explicit_project_root_overrides_a_marker_that_would_otherwise_win() {
    let tree = TempTree::new();
    // A marker at the tree root would otherwise make `tree` the project root.
    tree.write(".nessemblerc", b"");
    tree.write("src/main.asm", b".include \"@/defs.asm\"\n");
    // The decoy a marker-derived root would have found instead.
    tree.write("defs.asm", b".db $99\n");
    // The file the override actually points at.
    tree.write("elsewhere/defs.asm", b".db $AA\n");

    let options = Options {
        project_root: Some(tree.path("elsewhere")),
        ..Options::default()
    };
    let rom = assemble_file(&tree.path("src/main.asm"), &options)
        .expect("assembles")
        .rom;
    assert_eq!(rom, vec![0xAA]);
}

#[test]
fn a_file_url_declaration_composes_with_root_relative_include() {
    let tree = TempTree::new();
    tree.write("main.asm", b".include \"file://@/lib/defs.asm\"\n");
    tree.write("lib/defs.asm", b".db $11\n");

    assert_eq!(asm(&tree.path("main.asm")), vec![0x11]);
}

#[test]
fn a_missing_file_url_root_relative_include_is_still_existence_checked() {
    let tree = TempTree::new();
    tree.write("main.asm", b".include \"file://@/lib/gone.asm\"\n");

    let err = assemble_file(&tree.path("main.asm"), &Options::default()).expect_err("errors");
    let AssembleError(d) = err;
    assert!(d.message.contains("lib/gone.asm"), "message: {}", d.message);
}

#[test]
fn an_at_prefixed_name_that_is_not_at_slash_is_untouched() {
    let tree = TempTree::new();
    // `@weird` and `./@x` are ordinary paths (§3): no `@/`, no root involved.
    tree.write(
        "main.asm",
        b".incbin \"@weird/x.chr\"\n.incbin \"./@x/y.chr\"\n",
    );
    tree.write("@weird/x.chr", &[0x01]);
    tree.write("@x/y.chr", &[0x02]);

    assert_eq!(asm(&tree.path("main.asm")), vec![0x01, 0x02]);
}

#[test]
fn a_path_that_climbs_above_the_root_errors_naming_the_root() {
    let tree = TempTree::new();
    tree.write("main.asm", b".incbin \"@/../outside.bin\"\n");

    let err = assemble_file(&tree.path("main.asm"), &Options::default()).expect_err("errors");
    let AssembleError(d) = err;
    assert!(
        d.message.contains("outside the project root"),
        "message: {}",
        d.message
    );
}

#[test]
fn a_project_using_no_at_slash_assembles_byte_identically() {
    // Gaining a project root (via a marker) must not perturb output for a
    // program that never spells `@/`.
    let plain = TempTree::new();
    plain.write("main.asm", b".db $01\n.incbin \"data.bin\"\n");
    plain.write("data.bin", &[0x02, 0x03]);

    let rooted = TempTree::new();
    rooted.write(".nessemblerc", b"");
    rooted.write("main.asm", b".db $01\n.incbin \"data.bin\"\n");
    rooted.write("data.bin", &[0x02, 0x03]);

    assert_eq!(asm(&plain.path("main.asm")), asm(&rooted.path("main.asm")));
    assert_eq!(asm(&plain.path("main.asm")), vec![0x01, 0x02, 0x03]);
}

// -- declared (`file://`) custom-pseudo-op arguments (§5.1) ------------------

#[test]
fn a_declared_root_relative_argument_hands_the_script_the_resolved_path() {
    let tree = TempTree::new();
    tree.write("main.asm", b".include \"a/b/deep.asm\"\n");
    tree.write("a/b/deep.asm", b".tilemap \"file://@/art/map.png\"\n");
    tree.write("art/map.png", b"not really a png");

    let (resolver, seen) = recording_resolver();
    assemble_file_with(&tree.path("main.asm"), &Options::default(), resolver).expect("assembles");

    let expected = tree.path("art/map.png").to_string_lossy().into_owned();
    assert_eq!(seen.borrow().as_slice(), [vec![expected]]);
}

#[test]
fn a_missing_declared_root_relative_argument_names_the_bare_path_and_the_script_never_runs() {
    let tree = TempTree::new();
    tree.write("main.asm", b".tilemap \"file://@/art/gone.png\"\n");

    let (resolver, seen) = recording_resolver();
    let err = assemble_file_with(&tree.path("main.asm"), &Options::default(), resolver)
        .expect_err("errors");

    assert!(
        err.0.message.contains("@/art/gone.png"),
        "message: {}",
        err.0.message
    );
    assert!(seen.borrow().is_empty(), "the script must not have run");
}

#[test]
fn an_undeclared_root_relative_argument_is_unchanged() {
    let tree = TempTree::new();
    tree.write("main.asm", b".tilemap \"@/art/map.png\"\n");

    let (resolver, seen) = recording_resolver();
    assemble_file_with(&tree.path("main.asm"), &Options::default(), resolver).expect("assembles");

    // No resolution, no existence check: the script decides what to do with it.
    assert_eq!(
        seen.borrow().as_slice(),
        [vec!["@/art/map.png".to_string()]]
    );
}

#[test]
fn two_at_slash_spellings_of_the_same_file_key_on_the_same_resolved_text() {
    // `@/art/map.png` and `@/art/../art/map.png` name the same file but are
    // different source spellings, written from different directories. Both
    // resolve through the same root, so they key on the same `texts` instead of
    // colliding or diverging by accident (§5.1) — unlike an *unresolved* plain
    // relative declaration, which (unchanged, §7) still keys on whatever it was
    // written as.
    let tree = TempTree::new();
    tree.write(
        "main.asm",
        b".include \"a/one.asm\"\n.include \"b/two.asm\"\n",
    );
    tree.write("a/one.asm", b".tilemap \"file://@/art/map.png\"\n");
    tree.write("b/two.asm", b".tilemap \"file://@/art/../art/map.png\"\n");
    tree.write("art/map.png", b"asset");

    let (resolver, seen) = recording_resolver();
    assemble_file_with(&tree.path("main.asm"), &Options::default(), resolver).expect("assembles");

    let expected = tree.path("art/map.png").to_string_lossy().into_owned();
    assert_eq!(
        seen.borrow().as_slice(),
        [vec![expected.clone()], vec![expected]]
    );
}

#[test]
fn a_root_relative_declaration_and_a_dot_dot_declaration_key_differently() {
    // Only a `@/`-prefixed declared argument is resolved into `texts` (§7); a
    // plain relative declaration is untouched, exactly as before this plan.
    let tree = TempTree::new();
    tree.write(
        "main.asm",
        b".include \"a/one.asm\"\n.include \"b/two.asm\"\n",
    );
    tree.write("a/one.asm", b".tilemap \"file://@/art/map.png\"\n");
    tree.write("b/two.asm", b".tilemap \"file://../art/map.png\"\n");
    tree.write("art/map.png", b"asset");

    let (resolver, seen) = recording_resolver();
    assemble_file_with(&tree.path("main.asm"), &Options::default(), resolver).expect("assembles");

    let resolved = tree.path("art/map.png").to_string_lossy().into_owned();
    assert_eq!(
        seen.borrow().as_slice(),
        [vec![resolved], vec!["../art/map.png".to_string()]]
    );
}
