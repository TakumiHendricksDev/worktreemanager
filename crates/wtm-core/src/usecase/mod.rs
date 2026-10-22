//! Use-cases: the application's actual behaviour.
//!
//! Each holds `Arc<dyn Port>` handles and nothing concrete, so `src-tauri` is the
//! only place that decides what a `Git` or a `PtyHost` really is.
//!
//! - [`slug`] — the one piece of naming logic that must be bit-compatible with an
//!   existing shell function.
//! - [`create`] — the ten-stage create pipeline, built around the rule that stages 1–6
//!   mutate nothing, with two inputs: the New Worktree form, and a pull request to review.
//! - [`remove`] — teardown, then `git worktree remove`, then optionally the branch.
//! - [`snapshot`] — what a review read: a tree, a patch, and a detached checkout of it.
//! - [`tracker_sync`] — a project's queue from its tracker into the mirror, and never further.

pub mod create;
pub mod remove;
pub mod slug;
pub mod snapshot;
pub mod tracker_sync;

pub use create::{
    AdvanceOutcome, AdvanceRequest, CreatePipeline, CreateRequest, ReuseReason, ReviewCheckout,
    ReviewFetch, ReviewOutcome, ReviewPlan, ReviewPreview, ReviewRequest, ReviewSetup,
    ReviewTarget, SetupRequest,
};
pub use remove::{RemoveOutcome, RemovePipeline, RemoveRequest, TeardownStep};
pub use slug::slugify;
pub use snapshot::{SnapshotCheckout, SnapshotError, Snapshots};
pub use tracker_sync::{SyncError, SyncReason, SyncReport, SyncRequest, read_issue, sync_queue};
