---
nessemble: patch
---
Internal: make the i18n locale catalog process-global (a `OnceLock<RwLock<…>>`
over the concurrent Fluent bundle) instead of thread-local, so a locale
registered or selected on one thread is honored on all of them — the language
server analyzes on worker threads. Message output is unchanged (parity holds);
`t!` takes only a read lock, off the assembly hot path — Phase 7 of
`plans/006-idiomatic-rust-refactor.md`.
