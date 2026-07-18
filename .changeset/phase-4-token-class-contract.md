---
nessemble: patch
---
Internal: define the highlight token-class wire ids and names once, as
`TokenClass::wire_id` / `wire_name` / `ALL` in `nessemble-core::tooling`, instead
of re-deriving the same 0–6 numbering in the wasm highlighter (`tokenize` /
`token_classes`) and the language server's semantic-token mapping. The wire ids,
class names, and LSP legend are unchanged — Phase 4 of
`plans/006-idiomatic-rust-refactor.md`.
