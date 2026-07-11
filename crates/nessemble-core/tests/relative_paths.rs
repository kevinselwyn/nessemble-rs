//! Filename-based directives (`.include`, `.inestrn`, and the `.inc*` media
//! importers) resolve relative to the directory of the file that *contains*
//! them, not the top-level project directory. These tests build a small
//! on-disk tree with directives nested in a subdirectory and assert that the
//! paths resolve from the including file's location.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use nessemble_core::{assemble_file, Options};

/// A throwaway directory tree, removed on drop.
struct TempTree {
    root: PathBuf,
}

impl TempTree {
    fn new() -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = format!(
            "nessemble-relpath-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        let root = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&root).expect("create temp root");
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

fn assemble(path: &Path) -> Vec<u8> {
    assemble_file(path, &Options::default())
        .unwrap_or_else(|e| panic!("assembly failed: {e:?}"))
        .rom
}

/// A `.include` inside an included file resolves relative to *that* file, so a
/// sibling include in a subdirectory is found there (not in the project root).
#[test]
fn nested_include_is_relative_to_including_file() {
    let tree = TempTree::new();
    tree.write("main.asm", b".db 1\n.include \"sub/a.asm\"\n.db 3\n");
    // `a.asm` lives in `sub/`; its own `.include "b.asm"` must resolve to
    // `sub/b.asm`, not `b.asm` at the root.
    tree.write("sub/a.asm", b".db 2\n.include \"b.asm\"\n");
    tree.write("sub/b.asm", b".db 42\n");
    // A decoy at the root that would be picked up under the old root-relative
    // behavior — its presence must not change the result.
    tree.write("b.asm", b".db 99\n");

    let rom = assemble(&tree.path("main.asm"));
    assert_eq!(rom, vec![1, 2, 42, 3]);
}

/// A `.incbin` inside an included file resolves its asset relative to the
/// included file's directory.
#[test]
fn media_include_is_relative_to_including_file() {
    let tree = TempTree::new();
    tree.write("main.asm", b".include \"sub/a.asm\"\n");
    tree.write("sub/a.asm", b".incbin \"data.bin\"\n");
    tree.write("sub/data.bin", &[0xAA, 0xBB, 0xCC]);
    // Decoy at the root that the old behavior would have read instead.
    tree.write("data.bin", &[0x11, 0x22, 0x33]);

    let rom = assemble(&tree.path("main.asm"));
    assert_eq!(rom, vec![0xAA, 0xBB, 0xCC]);
}

/// The top-level file's own directives still resolve relative to its directory.
#[test]
fn top_level_include_is_relative_to_top_file() {
    let tree = TempTree::new();
    tree.write("prog/main.asm", b".db 7\n.incbin \"payload.bin\"\n");
    tree.write("prog/payload.bin", &[0xDE, 0xAD]);

    let rom = assemble(&tree.path("prog/main.asm"));
    assert_eq!(rom, vec![7, 0xDE, 0xAD]);
}
