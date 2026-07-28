//! Use-cases: the application's actual behaviour.
//!
//! Each holds `Arc<dyn Port>` handles and nothing concrete, so `src-tauri` is the
//! only place that decides what a `Git` or a `PtyHost` really is.
//!
//! - [`slug`] — the one piece of naming logic that must be bit-compatible with an
//!   existing shell function.
//! - [`create`] — the ten-stage create pipeline, built around the rule that stages 1–6
//!   mutate nothing.
//! - [`remove`] — teardown, then `git worktree remove`, then optionally the branch.

pub mod create;
pub mod remove;
pub mod slug;

pub use create::{CreatePipeline, CreateRequest, SetupRequest};
pub use remove::{RemoveOutcome, RemovePipeline, RemoveRequest};
pub use slug::slugify;
