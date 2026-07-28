//! Template rendering for `wtm.toml`.
//!
//! Three pieces:
//!
//! - [`filters`] — the fixed filter vocabulary a config may use. Small on purpose:
//!   these templates are configuration read from a repository, so the engine is a
//!   sandbox, not a scripting language.
//! - [`context`] — nests the domain's flat dotted-key context into something jinja
//!   can walk, and reports the one thing a flat map can express that a nested one
//!   cannot (a field shadowing a reserved namespace).
//! - [`engine`] — [`Engine`], the `TemplateEngine` port, including the scope
//!   validation that turns a mistyped or out-of-order token into a load-time error
//!   rather than an empty render and a branch called `experiment/ACME-0000-`.

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod context;
pub mod engine;
pub mod filters;

pub use context::{NestedContext, RESERVED_PREFIXES, shadows_reserved_prefix};
pub use engine::Engine;
