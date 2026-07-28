//! Domain model, ports, and use-cases for wtm.
//!
//! # The rule this crate exists to enforce
//!
//! Nothing in here touches the filesystem, spawns a process, reads the clock, or
//! knows that Tauri, `git`, `just`, Jira or Docker exist. Every outside effect is
//! reached through a trait in [`ports`], and the real implementations live in the
//! adapter crates (`wtm-git`, `wtm-exec`, `wtm-config`, `wtm-render`).
//!
//! That rule is checked mechanically rather than by review:
//!
//! ```text
//! cargo check -p wtm-core --target wasm32-unknown-unknown    # just core-wasm
//! ```
//!
//! `wasm32-unknown-unknown` has no processes, no clock and no usable filesystem,
//! so if an adapter concern leaks in here, that command stops compiling. When it
//! breaks, fix the dependency — do not relax the check.

// `unwrap_used` is a warning across the workspace, which is right for library code
// and wrong for assertions: `expect("...")` in a test adds noise without adding
// information, since a panic is the failure report either way. Scoped to `cfg(test)`
// so it can never leak into a shipped path.
#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod error;
pub mod model;
pub mod ports;
pub mod usecase;

pub use error::{ConfigError, ExecError, GitError, RenderError, WtmError};
pub use model::{
    ActionSpec, BranchRef, Checkout, CommitId, CreateOutcome, CreatePlan, FieldKind, FieldSpec,
    FieldValue, FormValues, PlanPreview, PreflightItem, Project, Remedy, Worktree, WorktreeId,
};
