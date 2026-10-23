//! Reading a forge, and the narrow set of writes a published review and a fix's follow-up need.
//!
//! # Reading and writing are two capabilities
//!
//! [`Forge`] only reads: pull requests, permissions, branch protection, a diff's lines. Writing is
//! [`ReviewWriter`], a separate trait, so a caller that only needs to read is never handed the
//! ability to write, and the one caller that writes — the outbox's executor, running an operation you
//! approved — is easy to find. Each write is a single request with a payload the outbox built and
//! hashed, and the reads beside it on that trait are the ones the outbox needs to learn whether a
//! write already happened. No agent tool reaches either trait (ARCHITECTURE §6a, §6i).
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
use crate::flow::{
    BranchProtection, ChangedLines, ForgeAccount, MergeMethod, Permission, PrState, PrTarget,
    PullRequest, RemoteComment, RemoteReview, RepoSlug, ReviewEvent,
};

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

    /// The lines of `head`'s change from where it left `base` that a review comment can sit on, by
    /// the forge's own diff: what its pull request page would show at that commit.
    fn changed_lines(
        &self,
        cwd: &Path,
        repo: &RepoSlug,
        base: &str,
        head: &str,
    ) -> Result<ChangedLines, ForgeError>;
}

/// One comment as it is sent: its place and its body, marker included.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentComment {
    pub path: String,
    pub start_line: Option<u32>,
    pub line: u32,
    pub body: String,
}

/// Publishing a review, and reading back what an earlier attempt left on the pull request.
///
/// Only the outbox's executor holds one. Every write it makes was approved, hashed and recorded as
/// started before the request is sent.
pub trait ReviewWriter: Send + Sync {
    /// Every review on pull request `number` of `repo`, oldest first: everyone's submitted ones, and
    /// your own pending one, which the forge shows only to you.
    fn reviews(
        &self,
        cwd: &Path,
        repo: &RepoSlug,
        number: u64,
    ) -> Result<Vec<RemoteReview>, ForgeError>;

    /// The comments of one review.
    fn review_comments(
        &self,
        cwd: &Path,
        repo: &RepoSlug,
        number: u64,
        review: u64,
    ) -> Result<Vec<RemoteComment>, ForgeError>;

    /// Start a pending review pinned to `commit_id`, holding `comments`, with `body`.
    ///
    /// # Errors
    ///
    /// [`ForgeError::PendingReviewExists`] when you already have one on the pull request, and
    /// [`ForgeError::Rejected`] when the forge refuses a comment's place or the review itself.
    fn create_pending_review(
        &self,
        cwd: &Path,
        repo: &RepoSlug,
        number: u64,
        commit_id: &str,
        body: &str,
        comments: &[SentComment],
    ) -> Result<RemoteReview, ForgeError>;

    /// Add one comment to your pending review `review_node_id`.
    ///
    /// # Errors
    ///
    /// [`ForgeError::Rejected`] when the forge will not put it there.
    fn add_review_comment(
        &self,
        cwd: &Path,
        repo: &RepoSlug,
        review_node_id: &str,
        comment: &SentComment,
    ) -> Result<(), ForgeError>;

    /// Submit your pending review `review` with `event` and `body`.
    fn submit_review(
        &self,
        cwd: &Path,
        repo: &RepoSlug,
        number: u64,
        review: u64,
        event: ReviewEvent,
        body: &str,
    ) -> Result<RemoteReview, ForgeError>;
}

/// A pull request as a write reads it back: enough to know whether one already exists and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemotePullRequest {
    pub number: u64,
    pub url: String,
    pub state: PrState,
    /// `owner:branch`.
    pub head: String,
    /// The branch it merges into.
    pub base: String,
    pub body: String,
    pub draft: bool,
    /// The commit its head is at.
    pub head_sha: String,
}

/// Opening the follow-up pull request of a local fix, and finding one an earlier attempt opened.
///
/// Only the outbox's executor holds one, for a route you approved.
pub trait PullRequestWriter: Send + Sync {
    /// Every pull request in `repo` from `head` (`owner:branch`), open or not, newest first.
    fn pull_requests_from(
        &self,
        cwd: &Path,
        repo: &RepoSlug,
        head: &str,
    ) -> Result<Vec<RemotePullRequest>, ForgeError>;

    /// Open a pull request in `repo` from `head` (`branch`, or `owner:branch` from another fork)
    /// into `base`, letting maintainers edit it.
    ///
    /// # Errors
    ///
    /// [`ForgeError::Rejected`] when the forge refuses it as asked, such as when one from that head
    /// into that base exists already.
    fn open_pull_request(
        &self,
        cwd: &Path,
        repo: &RepoSlug,
        base: &str,
        head: &str,
        title: &str,
        body: &str,
    ) -> Result<RemotePullRequest, ForgeError>;
}

/// The issue's own pull request: opened as a draft from its task branch, kept up to date, marked
/// ready, and merged.
///
/// Only the outbox's executor holds one, for the draft the plan makes automatic and for approvals
/// you gave — Ready + Needs Review, and a merge from the Merge sheet. No agent tool reaches it, so an
/// agent can neither mark work ready nor merge it. Finding an earlier attempt's pull request is
/// [`PullRequestWriter::pull_requests_from`].
pub trait OwnPullWriter: Send + Sync {
    /// Open a draft pull request in `repo` from its branch `head` into `base`.
    ///
    /// # Errors
    ///
    /// [`ForgeError::Rejected`] when the forge refuses it as asked, such as when one from that head
    /// into that base exists already.
    fn open_draft(
        &self,
        cwd: &Path,
        repo: &RepoSlug,
        base: &str,
        head: &str,
        title: &str,
        body: &str,
    ) -> Result<RemotePullRequest, ForgeError>;

    /// Set pull request `number`'s title and description.
    fn update(
        &self,
        cwd: &Path,
        repo: &RepoSlug,
        number: u64,
        title: &str,
        body: &str,
    ) -> Result<RemotePullRequest, ForgeError>;

    /// Mark draft pull request `number` ready for review.
    fn mark_ready(&self, cwd: &Path, repo: &RepoSlug, number: u64) -> Result<(), ForgeError>;

    /// Merge pull request `number` by `method`, only if its head is still `head`: the forge refuses
    /// it otherwise ([`ForgeError::Rejected`]).
    fn merge(
        &self,
        cwd: &Path,
        repo: &RepoSlug,
        number: u64,
        head: &str,
        method: MergeMethod,
    ) -> Result<(), ForgeError>;
}
