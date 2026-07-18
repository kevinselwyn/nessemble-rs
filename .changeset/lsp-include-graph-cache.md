---
nessemble: patch
---
Internal: make the language server's per-keystroke project analysis cheaper. The
include graph now extracts each disk file's `.include` lines through an
`(mtime, len)`-keyed cache, so rebuilding it on an edit re-reads only files that
actually changed on disk (unchanged files are stat'd, not re-read and
re-scanned), and the open-buffer overlay borrows document text instead of cloning
every buffer on every change. Behavior is unchanged — an external edit to an
include line is still reflected, because the cache is keyed on the file's
signature.
