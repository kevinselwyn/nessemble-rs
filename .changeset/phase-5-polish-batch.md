---
nessemble: patch
---
Internal: a batch of low-risk readability cleanups — borrow (rather than clone)
the stride list in the formatter's data-consolidation pass; collapse the
`.nessemblerc` scalar-field overlay into a small local macro; replace the obscure
`&args[args.len().min(1)..]` argv slicing in `xtask` with `args.get(1..)`; and
flatten the single-variant `AssembleError` enum into a `AssembleError(Diag)`
newtype. No change to output, formatting, or diagnostics — Phase 5 of
`plans/006-idiomatic-rust-refactor.md`.
