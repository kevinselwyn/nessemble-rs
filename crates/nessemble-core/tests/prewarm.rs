//! [`prewarm_candidates`]/[`prewarm_candidates_file`] scan a program for
//! custom-directive invocations whose arguments are knowable without running
//! either assembly pass — safe to resolve ahead of time (e.g. concurrently)
//! to warm a cache before the real, sequential assembly call reads from it
//! (`plans/013-structured-data-parsing.md` §7). These tests exercise the scan
//! directly; the actual concurrent warming is `nessemble-cli`'s concern
//! (`crates/nessemble-cli/src/custom.rs`).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use nessemble_core::{prewarm_candidates, prewarm_candidates_file, Options, PrewarmCandidate};

/// A throwaway directory tree, removed on drop.
struct TempTree {
    root: PathBuf,
}

impl TempTree {
    fn new() -> Self {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = format!(
            "nessemble-prewarm-{}-{}",
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

#[test]
fn a_directive_with_only_literal_arguments_is_a_candidate() {
    let candidates = prewarm_candidates(".tilemap 5, 8 + 2\n", &Options::default());
    assert_eq!(
        candidates,
        vec![PrewarmCandidate {
            name: "tilemap".to_string(),
            ints: vec![5, 10],
            texts: vec![],
            base_dir: std::env::current_dir().unwrap(),
            root: candidates[0].root.clone(),
        }]
    );
}

#[test]
fn an_undeclared_string_argument_needs_no_existence_check() {
    // An undeclared string could be an easing name, a label, anything — it
    // resolves to itself with no filesystem dependency at all.
    let candidates = prewarm_candidates(".ease \"easeInQuad\", 16\n", &Options::default());
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].texts, vec!["easeInQuad".to_string()]);
    assert_eq!(candidates[0].ints, vec![16]);
}

#[test]
fn a_symbol_referencing_integer_argument_is_not_a_candidate() {
    // `later`'s value depends on a pass having run — not safe to know early.
    let candidates = prewarm_candidates(".foo later\nlater:\n", &Options::default());
    assert!(candidates.is_empty(), "candidates: {candidates:?}");
}

#[test]
fn a_bank_reference_is_not_a_candidate() {
    let candidates = prewarm_candidates(".foo BANK(later)\nlater:\n", &Options::default());
    assert!(candidates.is_empty(), "candidates: {candidates:?}");
}

#[test]
fn high_and_low_of_a_literal_are_still_a_candidate() {
    let candidates = prewarm_candidates(".foo HIGH($1234), LOW($1234)\n", &Options::default());
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].ints, vec![0x12, 0x34]);
}

#[test]
fn a_declared_argument_naming_a_missing_file_is_not_a_candidate() {
    let candidates = prewarm_candidates(
        ".tilemap \"file://does-not-exist.png\"\n",
        &Options::default(),
    );
    assert!(candidates.is_empty(), "candidates: {candidates:?}");
}

#[test]
fn a_declared_argument_naming_a_present_file_is_a_candidate() {
    let tree = TempTree::new();
    tree.write("map.png", b"pretend png");
    let main = tree.write("main.asm", b".tilemap \"file://map.png\"\n");

    let candidates = prewarm_candidates_file(&main, &Options::default());
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].texts, vec!["map.png".to_string()]);
}

#[test]
fn a_root_relative_declared_argument_resolves_like_the_real_pass_would() {
    let tree = TempTree::new();
    tree.write(".nessemblerc", b"");
    tree.write("assets/map.png", b"pretend png");
    let main = tree.write("src/main.asm", b".tilemap \"file://@/assets/map.png\"\n");

    let candidates = prewarm_candidates_file(&main, &Options::default());
    assert_eq!(candidates.len(), 1);
    let resolved = tree.root.join("assets/map.png");
    assert_eq!(
        candidates[0].texts,
        vec![resolved.to_string_lossy().into_owned()]
    );
    assert_eq!(candidates[0].root, Some(tree.root.clone()));
}

#[test]
fn two_call_sites_with_identical_arguments_are_two_candidates() {
    // Prewarming doesn't need to dedupe — a caller that wants to avoid
    // redundant concurrent work can do so itself (`PrewarmCandidate` derives
    // `Hash`/`Eq` for exactly that).
    let candidates = prewarm_candidates(".foo 1\n.foo 1\n", &Options::default());
    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[0], candidates[1]);
}

#[test]
fn a_directive_inside_an_expanded_macro_is_still_scanned() {
    // Macro expansion happens at the token level during preprocessing, before
    // parsing — the same stage prewarm scanning consumes — so an invocation
    // written inside a `.macro` body is visible here exactly as it is to the
    // real assembly pass.
    let src = ".macrodef mm\n.foo 9\n.endm\n.macro mm\n";
    let candidates = prewarm_candidates(src, &Options::default());
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].ints, vec![9]);
}

#[test]
fn a_preprocessing_failure_yields_no_candidates_rather_than_an_error() {
    // The real assemble call that follows is the sole authority on reporting
    // a missing include; prewarm scanning simply has nothing to offer here.
    let candidates = prewarm_candidates(".include \"does-not-exist.asm\"\n", &Options::default());
    assert!(candidates.is_empty());
}

#[test]
fn a_parse_failure_yields_no_candidates_rather_than_an_error() {
    let candidates = prewarm_candidates(".db 1 +\n", &Options::default());
    assert!(candidates.is_empty());
}
