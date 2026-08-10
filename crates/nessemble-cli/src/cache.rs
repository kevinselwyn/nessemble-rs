//! The persistent cache of what a custom pseudo-op emitted.
//!
//! A script that crunches a PNG into CHR data should cost that crunch once per
//! change to the PNG, not once per build. An entry stores the bytes a
//! `custom()` invocation returned, alongside everything that could change them:
//! the [`Key`] (the script's identity and the arguments it was called with) and a
//! [`Stamp`] for every file the script actually read, recorded by the script host
//! rather than declared in the source.
//!
//! # Freshness
//!
//! A stamp is `(path, size, mtime)` — deliberately **not** a content hash. A
//! changed file changes its mtime, so the normal case is caught; a `git checkout`
//! or `touch` rewrites mtimes without changing content, which costs a needless
//! re-run but never correctness. The gap is an edit that preserves a file's byte
//! size *and* lands inside the same mtime tick as the recorded one, which
//! nanosecond timestamps make very narrow. `--no-cache` and `nessemble cache
//! clear` are the escape hatch when it matters.
//!
//! # Why a CRC names the files
//!
//! An entry's filename is [`nessemble_core::crc_32`] of the serialized key, which
//! is a checksum, not a digest — and is not trusted as one. The entry stores the
//! **whole** key and [`Cache::get`] compares it field for field, so a collision
//! produces a mismatch (handled as a miss and overwritten) rather than another
//! invocation's bytes. That is what lets the cache exist with no hashing
//! dependency at all.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::home;

/// On-disk layout version. An entry written by a different version is a miss,
/// never a misread. Bumped when [`Key`]'s shape changes (most recently, adding
/// `root` — `plans/013-structured-data-parsing.md` §11.1); `serde`'s
/// missing-field error already makes a differently-shaped old entry
/// undeserializable, so the bump is a documentation signal more than a
/// mechanism.
const FORMAT: u32 = 2;

/// Total size the cache is trimmed back to on write, oldest entries first.
const MAX_BYTES: u64 = 256 * 1024 * 1024;

/// A file's identity for freshness purposes: where it is, how big it is, and when
/// it last changed. See the module docs on why this is not a content hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    path: PathBuf,
    size: u64,
    mtime_secs: i64,
    mtime_nanos: u32,
}

impl Stamp {
    /// Stamp `path` as it is now, or `None` if it cannot be read.
    pub fn of(path: &Path) -> Option<Stamp> {
        let meta = std::fs::metadata(path).ok()?;
        let mtime = meta.modified().ok()?;
        let (secs, nanos) = match mtime.duration_since(UNIX_EPOCH) {
            Ok(d) => (d.as_secs() as i64, d.subsec_nanos()),
            // Predates the epoch: keep it representable rather than bailing.
            Err(e) => (
                -(e.duration().as_secs() as i64),
                e.duration().subsec_nanos(),
            ),
        };
        Some(Stamp {
            path: path.to_path_buf(),
            size: meta.len(),
            mtime_secs: secs,
            mtime_nanos: nanos,
        })
    }

    /// Whether the file still matches this stamp.
    fn is_fresh(&self) -> bool {
        Stamp::of(&self.path).as_ref() == Some(self)
    }
}

/// Everything that can change what an invocation emits.
///
/// The script is identified by a [`Stamp`], so editing a script invalidates every
/// entry that ran it, and re-pointing a `--pseudo` mapping at a different file
/// changes the key outright. `base_dir` is here because the script's own relative
/// reads resolve against it, `root` because a `@/`-prefixed one instead resolves
/// against the project root (`plans/013-structured-data-parsing.md` §11.1) —
/// without it, two builds sharing a `base_dir` but disagreeing on the root (say,
/// a `.nessemblerc` added between them) could serve one build's `@/` bytes to
/// the other — and the `nessemble` version is here because the host helpers
/// (`nes_shade`, `find_cell`, …) define the output — a release must not serve
/// bytes computed by a different implementation of them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Key {
    format: u32,
    nessemble: String,
    directive: String,
    ints: Vec<i64>,
    texts: Vec<String>,
    base_dir: PathBuf,
    root: Option<PathBuf>,
    script: Stamp,
}

impl Key {
    /// The key for one invocation, or `None` if the script cannot be stamped.
    pub fn new(
        directive: &str,
        ints: &[i64],
        texts: &[String],
        base_dir: &Path,
        root: Option<&Path>,
        script: &Path,
    ) -> Option<Key> {
        Some(Key {
            format: FORMAT,
            nessemble: env!("CARGO_PKG_VERSION").to_string(),
            directive: directive.to_string(),
            ints: ints.to_vec(),
            texts: texts.to_vec(),
            base_dir: base_dir.to_path_buf(),
            root: root.map(Path::to_path_buf),
            script: Stamp::of(script)?,
        })
    }

    /// The entry's base filename: a CRC of the serialized key, in hex.
    fn digest(&self) -> String {
        let serialized = serde_json::to_vec(self).unwrap_or_default();
        format!("{:08x}", nessemble_core::crc_32(&serialized))
    }
}

/// A stored invocation: the key it answers, and the files it depended on.
#[derive(Debug, Serialize, Deserialize)]
struct Entry {
    key: Key,
    inputs: Vec<Stamp>,
    /// Length of the sibling `.bin`, so a truncated pair is a miss.
    len: usize,
}

/// The cache directory, under `~/.nessemble` beside `scripts/` and `locales/`.
pub struct Cache {
    root: PathBuf,
}

impl Cache {
    /// Open the cache, or `None` when there is no home directory to put it in.
    pub fn open() -> Option<Cache> {
        Some(Cache {
            root: home::config_dir()?.join("cache").join("pseudo"),
        })
    }

    /// Open a cache rooted at an explicit directory (for tests).
    #[cfg(test)]
    fn at(root: PathBuf) -> Cache {
        Cache { root }
    }

    /// The bytes stored for `key`, if an entry exists whose key matches exactly
    /// and whose every recorded input is still fresh.
    pub fn get(&self, key: &Key) -> Option<Vec<u8>> {
        let (meta_path, bin_path) = self.paths(key);
        let meta = std::fs::read(&meta_path).ok()?;
        let entry: Entry = serde_json::from_slice(&meta).ok()?;
        // The CRC only *names* the file; this is what makes a hit a hit.
        if entry.key != *key || !entry.inputs.iter().all(Stamp::is_fresh) {
            return None;
        }
        let bytes = std::fs::read(&bin_path).ok()?;
        if bytes.len() != entry.len {
            return None;
        }
        // Touch the metadata so eviction is least-recently-*used*. Rewriting the
        // small JSON is the portable way to move an mtime; the bytes are left
        // alone, since they are the part that can be large.
        write_atomic(&meta_path, &meta);
        Some(bytes)
    }

    /// Store `bytes` for `key`, remembering `inputs` as what it depended on.
    ///
    /// Failure is silent by design: a cache that cannot be written is a
    /// performance problem, not a build problem.
    pub fn put(&self, key: &Key, inputs: &[PathBuf], bytes: &[u8]) {
        let stamps: Vec<Stamp> = inputs.iter().filter_map(|p| Stamp::of(p)).collect();
        // An input that vanished between the read and the stamp would store a
        // dependency we can never confirm, so store nothing at all.
        if stamps.len() != inputs.len() {
            return;
        }
        let entry = Entry {
            key: key.clone(),
            inputs: stamps,
            len: bytes.len(),
        };
        let Ok(meta) = serde_json::to_vec(&entry) else {
            return;
        };
        let (meta_path, bin_path) = self.paths(key);
        if let Some(parent) = meta_path.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                return;
            }
        }
        // Bytes first: a reader that finds metadata always finds the bytes it
        // describes, and an orphaned `.bin` is harmless.
        if write_atomic(&bin_path, bytes) {
            write_atomic(&meta_path, &meta);
        }
        self.evict();
    }

    /// The `(metadata, bytes)` paths for `key`, bucketed by the first two hex
    /// digits of its digest so no single directory grows unbounded.
    fn paths(&self, key: &Key) -> (PathBuf, PathBuf) {
        let digest = key.digest();
        let dir = self.root.join(&digest[..2]);
        (
            dir.join(format!("{digest}.json")),
            dir.join(format!("{digest}.bin")),
        )
    }

    /// Every `(metadata path, bytes path, mtime, total size)` pair in the cache.
    fn entries(&self) -> Vec<(PathBuf, PathBuf, SystemTime, u64)> {
        let mut out = Vec::new();
        let Ok(buckets) = std::fs::read_dir(&self.root) else {
            return out;
        };
        for bucket in buckets.flatten() {
            let Ok(files) = std::fs::read_dir(bucket.path()) else {
                continue;
            };
            for file in files.flatten() {
                let meta_path = file.path();
                if meta_path.extension().is_some_and(|e| e == "json") {
                    let bin_path = meta_path.with_extension("bin");
                    let meta_len = file.metadata().map(|m| m.len()).unwrap_or_default();
                    let bin_len = std::fs::metadata(&bin_path)
                        .map(|m| m.len())
                        .unwrap_or_default();
                    let mtime = file
                        .metadata()
                        .and_then(|m| m.modified())
                        .unwrap_or(UNIX_EPOCH);
                    out.push((meta_path, bin_path, mtime, meta_len + bin_len));
                }
            }
        }
        out
    }

    /// Trim the cache back under [`MAX_BYTES`], dropping least-recently-used
    /// entries first. Run after a write, which is the only time it can grow.
    fn evict(&self) {
        let mut entries = self.entries();
        let mut total: u64 = entries.iter().map(|(_, _, _, size)| size).sum();
        if total <= MAX_BYTES {
            return;
        }
        entries.sort_by_key(|(_, _, mtime, _)| *mtime);
        for (meta_path, bin_path, _, size) in entries {
            if total <= MAX_BYTES {
                break;
            }
            let _ = std::fs::remove_file(&meta_path);
            let _ = std::fs::remove_file(&bin_path);
            total = total.saturating_sub(size);
        }
    }

    /// Entry count and total size on disk, for `nessemble cache info`.
    pub fn stats(&self) -> (usize, u64) {
        let entries = self.entries();
        let total = entries.iter().map(|(_, _, _, size)| size).sum();
        (entries.len(), total)
    }

    /// The cache directory, for reporting.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Delete every entry, returning how many were removed and how much they
    /// occupied.
    pub fn clear(&self) -> (usize, u64) {
        let (count, bytes) = self.stats();
        let _ = std::fs::remove_dir_all(&self.root);
        (count, bytes)
    }
}

/// Write `bytes` to `path` via a temporary file and a rename, so a concurrent
/// reader never sees a half-written file. Returns whether it succeeded.
///
/// The tmp name is unique per **call**, not just per process: prewarming
/// (`plans/013-structured-data-parsing.md` §7) calls `get`/`put` for
/// different keys concurrently from multiple threads in the same process, and
/// two entries that happen to share a `path` — a `get`'s touch racing a
/// `put`, or two `put`s, for the same [`Key`] — must not collide on the same
/// tmp file, which a process-id-only name allows.
fn write_atomic(path: &Path, bytes: &[u8]) -> bool {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let tmp = path.with_extension(format!(
        "tmp{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    if std::fs::write(&tmp, bytes).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return false;
    }
    if std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cache in a throwaway directory, removed on drop.
    struct TempCache {
        cache: Cache,
        root: PathBuf,
    }

    impl TempCache {
        fn new(tag: &str) -> TempCache {
            use std::sync::atomic::{AtomicU32, Ordering};
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let root = std::env::temp_dir().join(format!(
                "nessemble-cache-{tag}-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&root).expect("create root");
            TempCache {
                cache: Cache::at(root.join("pseudo")),
                root,
            }
        }

        /// A script file in the tree, stamped as it is now.
        fn script(&self, name: &str, body: &str) -> PathBuf {
            let path = self.root.join(name);
            std::fs::write(&path, body).expect("write script");
            path
        }

        fn key(&self, script: &Path) -> Key {
            Key::new(
                "tilemap",
                &[1],
                &["a".to_string()],
                &self.root,
                None,
                script,
            )
            .expect("key")
        }
    }

    impl Drop for TempCache {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn a_stored_entry_comes_back() {
        let t = TempCache::new("roundtrip");
        let script = t.script("s.rhai", "fn custom(i, t) { [1] }");
        let key = t.key(&script);

        assert_eq!(t.cache.get(&key), None, "cold cache misses");
        t.cache.put(&key, &[], &[0xAA, 0xBB]);
        assert_eq!(t.cache.get(&key), Some(vec![0xAA, 0xBB]));
    }

    #[test]
    fn a_changed_input_misses() {
        let t = TempCache::new("input");
        let script = t.script("s.rhai", "fn custom(i, t) { [1] }");
        let asset = t.root.join("map.png");
        std::fs::write(&asset, b"before").expect("write asset");
        let key = t.key(&script);
        t.cache.put(&key, std::slice::from_ref(&asset), &[0x01]);
        assert_eq!(t.cache.get(&key), Some(vec![0x01]));

        // A different size is a different stamp, whatever the clock did.
        std::fs::write(&asset, b"after (longer)").expect("rewrite asset");
        assert_eq!(t.cache.get(&key), None);
    }

    #[test]
    fn a_deleted_input_misses() {
        let t = TempCache::new("deleted");
        let script = t.script("s.rhai", "fn custom(i, t) { [1] }");
        let asset = t.root.join("map.png");
        std::fs::write(&asset, b"x").expect("write asset");
        let key = t.key(&script);
        t.cache.put(&key, std::slice::from_ref(&asset), &[0x01]);

        std::fs::remove_file(&asset).expect("remove asset");
        assert_eq!(t.cache.get(&key), None);
    }

    #[test]
    fn an_edited_script_misses() {
        // The script is a dependency like any other: its stamp is in the key.
        let t = TempCache::new("script");
        let script = t.script("s.rhai", "fn custom(i, t) { [1] }");
        let key = t.key(&script);
        t.cache.put(&key, &[], &[0x01]);
        assert_eq!(t.cache.get(&key), Some(vec![0x01]));

        std::fs::write(&script, "fn custom(i, t) { [2] }  // edited, and longer")
            .expect("edit script");
        let new_key = t.key(&script);
        assert_ne!(new_key, key, "the script's stamp is part of the key");
        assert_eq!(t.cache.get(&new_key), None);
    }

    #[test]
    fn a_different_script_path_misses() {
        // Re-pointing a `--pseudo` mapping at another file changes the key.
        let t = TempCache::new("repoint");
        let one = t.script("one.rhai", "fn custom(i, t) { [1] }");
        let two = t.script("two.rhai", "fn custom(i, t) { [1] }");
        t.cache.put(&t.key(&one), &[], &[0x01]);

        assert_eq!(t.cache.get(&t.key(&two)), None);
    }

    #[test]
    fn arguments_and_base_dir_are_part_of_the_key() {
        let t = TempCache::new("args");
        let script = t.script("s.rhai", "fn custom(i, t) { [1] }");
        let base = &t.root;
        let key = Key::new("foo", &[1], &["a".to_string()], base, None, &script).unwrap();
        t.cache.put(&key, &[], &[0x01]);

        let other_int = Key::new("foo", &[2], &["a".to_string()], base, None, &script).unwrap();
        let other_text = Key::new("foo", &[1], &["b".to_string()], base, None, &script).unwrap();
        let other_name = Key::new("bar", &[1], &["a".to_string()], base, None, &script).unwrap();
        let other_dir = Key::new(
            "foo",
            &[1],
            &["a".to_string()],
            Path::new("/elsewhere"),
            None,
            &script,
        )
        .unwrap();
        for key in [other_int, other_text, other_name, other_dir] {
            assert_eq!(t.cache.get(&key), None);
        }
    }

    #[test]
    fn the_project_root_is_part_of_the_key() {
        // Two builds with the same base_dir but different project roots must not
        // share a cache entry: a script that resolves a `@/`-prefixed path itself
        // could read a different file under each root
        // (`plans/013-structured-data-parsing.md` §11.1).
        let t = TempCache::new("root");
        let script = t.script("s.rhai", "fn custom(i, t) { [1] }");
        let no_root = Key::new("foo", &[1], &["a".to_string()], &t.root, None, &script).unwrap();
        t.cache.put(&no_root, &[], &[0x01]);

        let with_root = Key::new(
            "foo",
            &[1],
            &["a".to_string()],
            &t.root,
            Some(Path::new("/proj")),
            &script,
        )
        .unwrap();
        assert_eq!(t.cache.get(&with_root), None);

        let other_root = Key::new(
            "foo",
            &[1],
            &["a".to_string()],
            &t.root,
            Some(Path::new("/elsewhere")),
            &script,
        )
        .unwrap();
        t.cache.put(&with_root, &[], &[0x02]);
        assert_eq!(t.cache.get(&other_root), None);
        assert_eq!(t.cache.get(&with_root), Some(vec![0x02]));
    }

    #[test]
    fn an_entry_whose_stored_key_disagrees_misses() {
        // A CRC collision would land two keys on one filename. The stored key is
        // compared in full, so the worst case is a miss, never another
        // invocation's bytes.
        let t = TempCache::new("collision");
        let script = t.script("s.rhai", "fn custom(i, t) { [1] }");
        let key = t.key(&script);
        t.cache.put(&key, &[], &[0x01]);

        let (meta_path, _) = t.cache.paths(&key);
        let mut entry: Entry = serde_json::from_slice(&std::fs::read(&meta_path).unwrap()).unwrap();
        entry.key.directive = "somethingelse".to_string();
        std::fs::write(&meta_path, serde_json::to_vec(&entry).unwrap()).unwrap();

        assert_eq!(t.cache.get(&key), None);
    }

    #[test]
    fn a_truncated_entry_misses_instead_of_panicking() {
        let t = TempCache::new("truncated");
        let script = t.script("s.rhai", "fn custom(i, t) { [1] }");
        let key = t.key(&script);
        t.cache.put(&key, &[], &[0x01, 0x02, 0x03]);

        let (_, bin_path) = t.cache.paths(&key);
        std::fs::write(&bin_path, [0x01]).expect("truncate bytes");
        assert_eq!(t.cache.get(&key), None);
    }

    #[test]
    fn corrupt_metadata_misses_instead_of_panicking() {
        let t = TempCache::new("corrupt");
        let script = t.script("s.rhai", "fn custom(i, t) { [1] }");
        let key = t.key(&script);
        t.cache.put(&key, &[], &[0x01]);

        let (meta_path, _) = t.cache.paths(&key);
        std::fs::write(&meta_path, b"not json at all").expect("corrupt");
        assert_eq!(t.cache.get(&key), None);
    }

    #[test]
    fn stats_and_clear_report_what_is_there() {
        let t = TempCache::new("stats");
        let script = t.script("s.rhai", "fn custom(i, t) { [1] }");
        assert_eq!(t.cache.stats().0, 0);

        t.cache.put(&t.key(&script), &[], &[0u8; 32]);
        let (count, bytes) = t.cache.stats();
        assert_eq!(count, 1);
        assert!(bytes >= 32, "bytes: {bytes}");

        let (cleared, freed) = t.cache.clear();
        assert_eq!((cleared, freed), (count, bytes));
        assert_eq!(t.cache.stats(), (0, 0));
        assert_eq!(t.cache.get(&t.key(&script)), None);
    }

    #[test]
    fn an_input_that_vanished_is_not_stored() {
        let t = TempCache::new("vanished");
        let script = t.script("s.rhai", "fn custom(i, t) { [1] }");
        let key = t.key(&script);

        t.cache.put(&key, &[t.root.join("never-existed")], &[0x01]);
        assert_eq!(
            t.cache.get(&key),
            None,
            "an unconfirmable dependency stores nothing"
        );
    }

    #[test]
    fn concurrent_puts_to_distinct_keys_do_not_corrupt_each_other() {
        // Prewarming (`plans/013-structured-data-parsing.md` §7) calls `put`
        // for many different keys concurrently from multiple threads in one
        // process — `write_atomic`'s tmp file must be unique per *call*, not
        // just per process, or two threads racing to write different entries
        // can clobber each other's temp file.
        let t = TempCache::new("concurrent-distinct");
        let script = t.script("s.rhai", "fn custom(i, t) { [1] }");
        let keys: Vec<Key> = (0..64u8)
            .map(|i| Key::new("foo", &[i64::from(i)], &[], &t.root, None, &script).unwrap())
            .collect();

        std::thread::scope(|scope| {
            for (i, key) in keys.iter().enumerate() {
                let cache = &t.cache;
                scope.spawn(move || cache.put(key, &[], &[i as u8]));
            }
        });

        for (i, key) in keys.iter().enumerate() {
            assert_eq!(
                t.cache.get(key),
                Some(vec![i as u8]),
                "entry {i} corrupted by a concurrent write to another key"
            );
        }
    }

    #[test]
    fn concurrent_puts_to_the_same_key_leave_a_consistent_entry() {
        // Two call sites can share identical arguments (`plans/011-pseudo-op-caching.md`),
        // so two prewarm tasks can legitimately race to write the *same* cache
        // key. The result must always be one complete, self-consistent entry
        // — never bytes from one write paired with another write's metadata.
        let t = TempCache::new("concurrent-same");
        let script = t.script("s.rhai", "fn custom(i, t) { [1] }");
        let key = t.key(&script);

        std::thread::scope(|scope| {
            for _ in 0..32 {
                let cache = &t.cache;
                let key = &key;
                scope.spawn(move || cache.put(key, &[], &[0xAB, 0xCD, 0xEF]));
            }
        });

        assert_eq!(t.cache.get(&key), Some(vec![0xAB, 0xCD, 0xEF]));
    }
}
