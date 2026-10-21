//! Reading a forge: pull requests, permissions and branch protection.
//!
//! # Reads only
//!
//! There is no method here that changes anything on a forge, and that is the whole of this port's
//! safety story for now. Publishing a review, pushing a fix and merging are later items, each
//! behind an approval bound to the exact head or text (ARCHITECTURE §6a), and they will arrive as
//! a separate capability rather than as more methods on this one, so a caller that only needs to
//! read cannot be handed the ability to write.
//!
//! # `cwd` is the checkout the question is asked from
//!
//! A forge CLI resolves a bare pull request number against the checkout's remotes, the way it does
//! in a terminal opened there. Passing the project's root keeps that behaviour, so "#4031" means
//! what it would mean to the person who typed it. Every call that names its repository explicitly
//! is independent of it.
//!
//! # Timeouts are the adapter's
//!
//! Every method is a network call through the user's own CLI, so each carries a deadline, like any
//! command through [`CommandRunner`](super::CommandRunner). The adapter picks them, because how
//! long a forge takes is a property of that forge.

use std::path::Path;

pub use crate::error::ForgeError;
use crate::flow::{BranchProtection, ForgeAccount, Permission, PrTarget, PullRequest, RepoSlug};

/// What the Work view reads from a forge.
pub trait Forge: Send + Sync {
    /// Whether the CLI is installed, new enough, and signed in to `host`.
    ///
    /// Asked before the first read of an action, so "install gh" or "sign in" is the answer instead
    /// of a parse error from a CLI that printed a login prompt.
    fn check(&self, cwd: &Path, host: &str) -> Result<ForgeAccount, ForgeError>;

    /// One pull request, by number or URL.
    fn pull_request(&self, cwd: &Path, target: &PrTarget) -> Result<PullRequest, ForgeError>;

    /// The pull request whose head is `branch` in `repo` itself, not in a fork: an open one if
    /// there is one, otherwise the most recent. `None` when the branch has never had one.
    fn pull_request_for_head(
        &self,
        cwd: &Path,
        repo: &RepoSlug,
        branch: &str,
    ) -> Result<Option<PullRequest>, ForgeError>;

    /// What the signed-in account may do on `repo`.
    fn permission(&self, cwd: &Path, repo: &RepoSlug) -> Result<Permission, ForgeError>;

    /// Whether `branch` of `repo` is protected.
    fn branch_protection(
        &self,
        cwd: &Path,
        repo: &RepoSlug,
        branch: &str,
    ) -> Result<BranchProtection, ForgeError>;
}
