//! Internationalization for `nessemble-rs`.
//!
//! Implemented in Phase 7 using Project Fluent. `en-US` ships first, with the
//! layout designed so additional locales are easy to add. This placeholder
//! reserves the workspace seam.

/// The default (and initially only) shipped locale.
pub const DEFAULT_LOCALE: &str = "en-US";
