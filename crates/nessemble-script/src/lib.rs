//! Custom pseudo-op scripting host for `nessemble-rs`.
//!
//! Per the plan, a single embedded language — [Rhai](https://rhai.rs) — replaces
//! the reference project's JS/Lua/Scheme trio. The engine and host API land in
//! Phase 8; this crate reserves the workspace seam and is feature-gated
//! (`rhai`) so the default build pulls in no scripting dependencies.

/// Scripting engine identifier. Only Rhai is planned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    /// The Rhai embedded scripting engine.
    Rhai,
}
