---
nessemble: patch
---
Internal: two idiomatic-Rust follow-ups from the round-2 review. Hoist the
`--pseudo` mapping parser into `nessemble_core::parse_pseudo_mapping`, so the CLI
reader and the language server's project scan share one implementation instead of
two that had begun to drift; and rewrite the `xtask` doc-pipeline markdown
scanners (`rewrite_chapter_links`, `strip_md_links`) from manual byte-index loops
to `find`/slice/`strip_prefix`. No change to assembled output, custom pseudo-op
resolution, or the generated docs.
