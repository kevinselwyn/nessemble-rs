//! Resolving a directive's filename argument to a path on disk.
//!
//! Every filename argument resolves against **the directory of the file that
//! contains it** — `.include` in a subdirectory finds its neighbours, not the
//! top-level file's. That rule is the default and is unchanged. What this module
//! adds is an escape from it: an argument beginning with [`PROJECT_ROOT_PREFIX`]
//! (`@/`) resolves from the **project root** instead, so a path stops depending
//! on how deeply the file spelling it happens to be nested.
//!
//! ```text
//! src/gfx/meta/big.asm:  .incbin "../../../assets/logo.chr"   ; depth leaks in
//! src/gfx/meta/big.asm:  .incbin "@/assets/logo.chr"          ; moves with the file
//! ```
//!
//! [`resolve_path_arg`] is the single entry point: it handles both spellings, so
//! a caller has one code path rather than a branch of its own. See
//! `plans/012-project-root-paths.md`.

use std::path::{Component, Path, PathBuf};

/// The prefix that resolves a filename argument from the project root:
/// `.incbin "@/assets/logo.chr"`. See [`resolve_path_arg`].
///
/// Like [`crate::FILE_URL_PREFIX`], this is a *leading* marker rather than a
/// scheme: only the first two characters count, so an ordinary file whose name
/// begins with `@` is still addressable (`@weird.chr` is untouched, and `./@`
/// names a directory literally called `@`).
pub const PROJECT_ROOT_PREFIX: &str = "@/";

/// The filenames whose presence marks a directory as a project root, searched
/// for by walking up from the entry file ([`find_project_root`]).
///
/// This is the union of the config files `nessemble-rc` discovers — it looks for
/// `.nessemblerc`/`.nessemblerc.json` and `.nessembleignore` independently — so a
/// project that already carries any of them gets `@/` with no new file and no
/// new flag.
pub const PROJECT_MARKERS: &[&str] = &[".nessemblerc", ".nessemblerc.json", ".nessembleignore"];

/// Why a `@/` argument could not be resolved.
///
/// Both cases are hard errors rather than a silent fallback to file-relative
/// resolution: a `@/` path that quietly resolved somewhere else would reintroduce
/// exactly the "reads the wrong file, says nothing" failure the prefix exists to
/// remove.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathArgError {
    /// `@/` was used but no project root could be determined — there is no entry
    /// file to walk up from and no explicit root was configured. In practice only
    /// the wasm build, which has no filesystem at all, reaches this.
    NoProjectRoot,
    /// The path climbs above the project root (`"@/../secret.bin"`). `@/` means
    /// "from the root"; a spelling that leaves it defeats the purpose and is
    /// almost always a typo.
    EscapesProjectRoot,
}

impl PathArgError {
    /// The localized diagnostic for this failure, naming the offending argument.
    #[must_use]
    pub fn message(self, arg: &str) -> String {
        match self {
            PathArgError::NoProjectRoot => {
                nessemble_i18n::t!("project-root-unresolved", file = arg)
            }
            PathArgError::EscapesProjectRoot => {
                nessemble_i18n::t!("project-root-escape", file = arg)
            }
        }
    }
}

/// The directory `arg` names, resolved for a directive in a file whose directory
/// is `base`, within a project rooted at `root`.
///
/// - `"@/assets/logo.chr"` → `<root>/assets/logo.chr`
/// - `"assets/logo.chr"` → `<base>/assets/logo.chr`
/// - `"/abs/logo.chr"` → itself, as before (`Path::join` replaces on an absolute)
///
/// `root` is `None` only where no root exists at all (see
/// [`PathArgError::NoProjectRoot`]); a `@/` argument is then an error, while
/// every other argument resolves as it always has. Callers strip a `file://`
/// declaration first ([`crate::strip_file_url`]) — the two markers stack in that
/// order, so `"file://@/lib/defs.asm"` is a declared, root-relative path.
pub fn resolve_path_arg(
    root: Option<&Path>,
    base: &Path,
    arg: &str,
) -> Result<PathBuf, PathArgError> {
    let Some(rel) = arg.strip_prefix(PROJECT_ROOT_PREFIX) else {
        return Ok(base.join(arg));
    };
    let root = root.ok_or(PathArgError::NoProjectRoot)?;
    let rel = normalize_under_root(rel).ok_or(PathArgError::EscapesProjectRoot)?;
    Ok(root.join(rel))
}

/// The project root for an assembly whose entry file lives in `base`, following
/// the ladder in `plans/012-project-root-paths.md` §4: an explicitly configured
/// root, else the nearest [`PROJECT_MARKERS`] directory at or above `base`, else
/// `base` itself — which is the only sensible reading when a lone `.asm` file
/// *is* the project.
#[must_use]
pub fn project_root(explicit: Option<&Path>, base: &Path) -> PathBuf {
    if let Some(root) = explicit {
        return root.to_path_buf();
    }
    // Canonicalize so the walk reaches the filesystem root even from a relative
    // or bare-filename base, whose `parent()` chain otherwise ends at `""` after
    // one step. `nessemble-rc`'s discovery canonicalizes for the same reason.
    let start = base.canonicalize();
    let start = start.as_deref().unwrap_or(base);
    find_project_root(start).unwrap_or_else(|| start.to_path_buf())
}

/// The nearest directory at or above `start` containing a [`PROJECT_MARKERS`]
/// file, or `None` if there is none all the way up.
#[must_use]
pub fn find_project_root(start: &Path) -> Option<PathBuf> {
    let mut dir = Some(start);
    while let Some(d) = dir {
        if PROJECT_MARKERS.iter().any(|name| d.join(name).is_file()) {
            return Some(d.to_path_buf());
        }
        dir = d.parent();
    }
    None
}

/// Lexically normalize the part of a `@/` argument after the prefix, returning
/// `None` if it climbs above the root.
///
/// Lexical, not [`Path::canonicalize`]: the target may not exist yet (a
/// directive can name a file a build step will produce), and following symlinks
/// would make "inside the root" depend on filesystem state rather than on what
/// the source says.
///
/// Iterating [`Component`]s rather than splitting on `/` keeps this correct per
/// platform: on Windows a backslash is a separator, so `"..\\..\\secret"` is a
/// real traversal and is caught, while on Unix the same string is an ordinary
/// (if odd) filename and is left alone.
fn normalize_under_root(rel: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    let mut depth = 0usize;
    for component in Path::new(rel).components() {
        match component {
            // `@//x` and `@/./x` are just `@/x`; neither is a separate spelling.
            Component::CurDir | Component::RootDir => {}
            Component::ParentDir => {
                // Popping past what we have pushed would leave the root.
                depth = depth.checked_sub(1)?;
                out.pop();
            }
            // A volume prefix (`C:`) names a different root by definition.
            Component::Prefix(_) => return None,
            Component::Normal(part) => {
                depth += 1;
                out.push(part);
            }
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A throwaway directory tree, removed on drop.
    struct TempTree {
        root: PathBuf,
    }

    impl TempTree {
        fn new() -> Self {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let unique = format!(
                "nessemble-paths-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            );
            let root = std::env::temp_dir().join(unique);
            std::fs::create_dir_all(&root).expect("create temp root");
            // The temp dir itself may be a symlink (macOS `/tmp`), which would
            // make canonicalized roots compare unequal to the paths we built.
            let root = root.canonicalize().expect("canonicalize temp root");
            TempTree { root }
        }

        fn dir(&self, rel: &str) -> PathBuf {
            let path = self.root.join(rel);
            std::fs::create_dir_all(&path).expect("create dir");
            path
        }

        fn write(&self, rel: &str) -> PathBuf {
            let path = self.root.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("create parent dir");
            }
            std::fs::write(&path, b"").expect("write file");
            path
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    fn resolved(arg: &str) -> PathBuf {
        resolve_path_arg(Some(Path::new("/proj")), Path::new("/proj/src/gfx"), arg)
            .expect("resolves")
    }

    // -- the prefix rule ----------------------------------------------------

    #[test]
    fn a_root_relative_argument_resolves_from_the_root() {
        assert_eq!(
            resolved("@/assets/logo.chr"),
            Path::new("/proj/assets/logo.chr")
        );
        assert_eq!(resolved("@/logo.chr"), Path::new("/proj/logo.chr"));
    }

    #[test]
    fn an_ordinary_argument_still_resolves_from_the_containing_file() {
        assert_eq!(
            resolved("assets/logo.chr"),
            Path::new("/proj/src/gfx/assets/logo.chr")
        );
        assert_eq!(
            resolved("../logo.chr"),
            Path::new("/proj/src/gfx/../logo.chr"),
        );
    }

    #[test]
    fn an_absolute_argument_is_used_as_written() {
        assert_eq!(
            resolved("/elsewhere/logo.chr"),
            Path::new("/elsewhere/logo.chr")
        );
    }

    #[test]
    fn only_a_leading_at_slash_counts() {
        // A file whose *name* starts with `@` is untouched, and `./@` is the
        // escape hatch for a directory literally called `@`.
        assert_eq!(
            resolved("@weird/logo.chr"),
            Path::new("/proj/src/gfx/@weird/logo.chr")
        );
        assert_eq!(
            resolved("./@/logo.chr"),
            Path::new("/proj/src/gfx/./@/logo.chr")
        );
        // Not a substring match: `@/` in the middle is an ordinary path.
        assert_eq!(
            resolved("dir/@/logo.chr"),
            Path::new("/proj/src/gfx/dir/@/logo.chr")
        );
    }

    #[test]
    fn the_prefix_composes_with_a_file_url_declaration() {
        // `file://` is stripped first, so what reaches us already begins `@/`.
        let (stripped, declared) = crate::strip_file_url("file://@/lib/defs.asm");
        assert!(declared);
        assert_eq!(resolved(stripped), Path::new("/proj/lib/defs.asm"));
    }

    // -- normalization and the escape check ---------------------------------

    #[test]
    fn redundant_separators_and_dots_collapse() {
        assert_eq!(
            resolved("@//assets/logo.chr"),
            Path::new("/proj/assets/logo.chr")
        );
        assert_eq!(
            resolved("@/./assets/logo.chr"),
            Path::new("/proj/assets/logo.chr")
        );
        assert_eq!(
            resolved("@/assets/../art/logo.chr"),
            Path::new("/proj/art/logo.chr")
        );
    }

    #[test]
    fn the_bare_prefix_names_the_root_itself() {
        assert_eq!(resolved("@/"), Path::new("/proj"));
    }

    #[test]
    fn a_path_that_climbs_above_the_root_is_rejected() {
        for arg in ["@/../secret.bin", "@/..", "@/assets/../../secret.bin"] {
            assert_eq!(
                resolve_path_arg(Some(Path::new("/proj")), Path::new("/proj/src"), arg),
                Err(PathArgError::EscapesProjectRoot),
                "argument: {arg}"
            );
        }
    }

    #[test]
    fn climbing_is_measured_against_the_root_not_the_filesystem() {
        // `@/a/../b` dips and returns; only a net climb past the root is an error.
        assert_eq!(resolved("@/a/../b"), Path::new("/proj/b"));
    }

    #[test]
    fn without_a_root_only_the_prefixed_form_fails() {
        let base = Path::new("/proj/src");
        assert_eq!(
            resolve_path_arg(None, base, "@/assets/logo.chr"),
            Err(PathArgError::NoProjectRoot)
        );
        // Everything else is unaffected by the absence of a root.
        assert_eq!(
            resolve_path_arg(None, base, "assets/logo.chr"),
            Ok(PathBuf::from("/proj/src/assets/logo.chr"))
        );
    }

    #[test]
    fn each_failure_names_the_argument_it_came_from() {
        let arg = "@/../secret.bin";
        assert!(PathArgError::EscapesProjectRoot.message(arg).contains(arg));
        assert!(PathArgError::NoProjectRoot.message(arg).contains(arg));
    }

    // -- finding the root ---------------------------------------------------

    #[test]
    fn every_marker_anchors_a_root() {
        for marker in PROJECT_MARKERS {
            let tree = TempTree::new();
            tree.write(marker);
            let deep = tree.dir("src/gfx/meta");
            assert_eq!(
                find_project_root(&deep).as_deref(),
                Some(tree.root.as_path()),
                "marker: {marker}"
            );
        }
    }

    #[test]
    fn the_nearest_marker_wins() {
        let tree = TempTree::new();
        tree.write(".nessemblerc");
        tree.write("src/.nessemblerc");
        let deep = tree.dir("src/gfx");
        assert_eq!(
            find_project_root(&deep).as_deref(),
            Some(tree.root.join("src").as_path())
        );
    }

    #[test]
    fn a_marker_directory_is_not_a_marker() {
        let tree = TempTree::new();
        tree.dir(".nessemblerc");
        let deep = tree.dir("src");
        // Nothing between `src` and the filesystem root carries a marker *file*,
        // so discovery must not stop at the directory of the same name.
        assert_ne!(
            find_project_root(&deep).as_deref(),
            Some(tree.root.as_path())
        );
    }

    // -- the ladder ---------------------------------------------------------

    #[test]
    fn an_explicit_root_beats_a_marker() {
        let tree = TempTree::new();
        tree.write(".nessemblerc");
        let base = tree.dir("src");
        let explicit = tree.dir("elsewhere");
        assert_eq!(project_root(Some(&explicit), &base), explicit);
    }

    #[test]
    fn a_marker_beats_the_entry_directory() {
        let tree = TempTree::new();
        tree.write(".nessemblerc");
        let base = tree.dir("src/gfx");
        assert_eq!(project_root(None, &base), tree.root);
    }

    #[test]
    fn without_a_marker_the_entry_directory_is_the_root() {
        let tree = TempTree::new();
        let base = tree.dir("src");
        // No marker anywhere up to the filesystem root (the temp tree is fresh),
        // so a lone file's own directory is the project.
        assert_eq!(project_root(None, &base), base);
    }
}
