//! The git adapter.
//!
//! Two halves, split so each can be tested the way it deserves:
//!
//! - [`porcelain`] — pure parsers for git's machine-readable output. No I/O, so the
//!   grammar is tested against captured bytes, including the awkward real cases
//!   (detached worktrees, paths with spaces, a directory name that disagrees with
//!   its branch).
//! - [`cli`] — [`GitCli`], which builds argv and delegates every spawn to a
//!   `CommandRunner`. Its logic is tested against a fake runner; its behaviour
//!   against a real `git` binary lives in `tests/real_git.rs`.
//!
//! # Why the CLI rather than a library
//!
//! The porcelain interface *is* git's compatibility contract. Shelling out means the
//! user's git config, credential helpers, hooks and commit signing all behave
//! exactly as they do in their terminal — which matters here, because this app's job
//! is to do what the user would otherwise have typed. `git2`'s worktree support is
//! also incomplete.

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod cli;
pub mod porcelain;

pub use cli::{GitCli, track_flag};
pub use porcelain::{parse_ahead_behind, parse_branch_lines, parse_status, parse_worktree_list};
