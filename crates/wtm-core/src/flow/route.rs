//! Where a local fix of a teammate's pull request can go, said exactly, and the writes that send it.
//!
//! # Fixing it locally is not permission to push anywhere
//!
//! A fix is made in the review worktree, by a writer that cannot push, and committed by the app
//! (`fix.rs`). Nothing about that says where it may go. Each way out is a route, and each route is
//! an approval of its exact destination — the remote, the ref, and the hash of the diff — before the
//! outbox sends anything ([`RoutePlan::binding`], [`SuggestionRoute::binding`]):
//!
//! - **A follow-up pull request**, the default: the fix on a new branch of its own, pushed to the
//!   base repository when theirs is a branch there and you can push to it, otherwise to your fork,
//!   with a pull request whose base is their branch ([`follow_up`]).
//! - **Suggestion comments**: each hunk of the fix as a GitHub suggestion the author can apply,
//!   posted as a review through the same two writes a published review makes ([`suggestions`]).
//! - **A push to their branch**: only with write access, never to a fork whose author did not allow
//!   maintainer edits, and only as a fast-forward of the head the fix was made from
//!   ([`push_to_their_branch`]).
//!
//! # A refspec is spelled out, and never names an integration branch
//!
//! Every push is `<commit>:refs/heads/<branch>`: a commit id, never a local ref that could have
//! moved, and a full destination ref, never a pattern. There is no `+` and no force flag anywhere,
//! so a push that is not a fast-forward is refused by the remote rather than overwriting anyone.
//! [`Refspec::new`] refuses a destination that is an integration branch — the configured ones, the
//! pull request's own base, and `main` and `master` whatever the config says — because a fix of
//! somebody's pull request has no business landing on the branch it was going to be reviewed into.
//!
//! # The remote is read before the push
//!
//! A push to their branch was approved as a fast-forward from the head you saw. If the author has
//! pushed since, the push would either be refused or, worse, be a fast-forward from something you
//! never looked at. So the outbox reads the remote ref first ([`before_push`]): at the head you
//! approved, it pushes; at the fix, it already happened; anywhere else, it stops and asks.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use super::approval::{Binding, Pinned, review_binding};
use super::consolidate::FindingRecord;
use super::forge::{Permission, PullRequest, RepoSlug, remote_for};
use super::glob;
use super::outbox::{NewOperation, OperationPayload};
use super::publish::{
    ChangedLines, FileLines, FoldReason, FoldedComment, InlineComment, PublishError, ReviewEvent,
    ReviewPayload, hash_of,
};
use super::pull_review::{PushToBranch, push_to_branch};
use super::review::FindingId;
use super::state::FlowId;
use crate::ports::git::RemoteUrl;

/// Why a route is not open, in a sentence that says what would open it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize)]
#[error("{0}")]
pub struct RouteError(pub String);

/// The three ways a fix can leave this Mac.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteKind {
    FollowUp,
    Suggestions,
    PushToBranch,
}

// ═══════════════════════════════ refs ═══════════════════════════════

/// Branches a fix is never pushed to whatever the config says: the names a repository's default
/// branch has almost always had. The configured list adds to these, never replaces them.
pub const ALWAYS_INTEGRATION: [&str; 2] = ["main", "master"];

/// The branches no route pushes to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct IntegrationBranches(Vec<String>);

impl IntegrationBranches {
    /// `configured` (`[workflow] integration_branches`), [`ALWAYS_INTEGRATION`], and `base`, the
    /// branch the pull request merges into, which is an integration branch by being one.
    #[must_use]
    pub fn new(configured: &[String], base: &str) -> Self {
        let mut patterns: Vec<String> = configured.to_vec();
        patterns.extend(ALWAYS_INTEGRATION.iter().map(|b| (*b).to_owned()));
        if !base.is_empty() {
            patterns.push(base.to_owned());
        }
        patterns.sort();
        patterns.dedup();
        Self(patterns)
    }

    #[must_use]
    pub fn covers(&self, branch: &str) -> bool {
        self.0.iter().any(|pattern| glob::matches(pattern, branch))
    }
}

/// One push, spelled out: a commit to a branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Refspec {
    /// A full commit id.
    pub src: String,
    /// `refs/heads/<branch>`.
    pub dst: String,
}

impl Refspec {
    /// `commit` to `branch`, if a route may push there.
    ///
    /// # Errors
    ///
    /// When `commit` is not a full commit id, `branch` is not a name git would take as one plain
    /// branch, or it is an integration branch.
    pub fn new(
        commit: &str,
        branch: &str,
        integration: &IntegrationBranches,
    ) -> Result<Self, RouteError> {
        if !is_commit_id(commit) {
            return Err(RouteError(format!(
                "`{commit}` is not a full commit id, and a push names exactly the commit it sends"
            )));
        }
        if !is_branch_name(branch) {
            return Err(RouteError(format!(
                "`{branch}` is not a branch name a push may take"
            )));
        }
        if integration.covers(branch) {
            return Err(RouteError(format!(
                "`{branch}` is an integration branch, and wtm never pushes a fix there"
            )));
        }
        Ok(Self {
            src: commit.to_owned(),
            dst: format!("refs/heads/{branch}"),
        })
    }

    /// The argument `git push` is given: `<src>:<dst>`, with no `+`.
    #[must_use]
    pub fn arg(&self) -> String {
        format!("{}:{}", self.src, self.dst)
    }

    #[must_use]
    pub fn branch(&self) -> &str {
        self.dst.strip_prefix("refs/heads/").unwrap_or(&self.dst)
    }
}

/// A full SHA-1 or SHA-256 commit id, lower-case hex.
fn is_commit_id(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// What `git check-ref-format --branch` would take, minus everything a route has no use for: no
/// pattern characters, no `@{`, no leading `-`, `+` or `refs/`, no `..`, no component starting
/// with `.` or ending `.lock`. A leading `+` is git's own name for a branch, but in a refspec it
/// reads as a force, and nobody reading the approval should have to tell the two apart.
fn is_branch_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with(['-', '/', '+'])
        && !name.ends_with(['/', '.'])
        && !name.starts_with("refs/")
        && !name.contains("..")
        && !name.contains("@{")
        && !name.contains("//")
        && name != "@"
        && !name.chars().any(|c| {
            c.is_control()
                || c.is_whitespace()
                || matches!(c, '~' | '^' | ':' | '?' | '*' | '[' | '\\')
        })
        && name.split('/').all(|part| {
            // `.lock` is git's own suffix for a lock file, in any case on a case-insensitive disk.
            !part.starts_with('.') && !part.to_ascii_lowercase().ends_with(".lock")
        })
}

// ═══════════════════════════════ destinations ═══════════════════════════════

/// Exactly where a push goes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Destination {
    /// The remote's name in your checkout, or `None` when it is pushed to by URL: a fork whose
    /// author allowed maintainer edits, which you have no remote for. wtm does not add one, because
    /// that would change your config to send one fix.
    pub remote: Option<String>,
    /// The URL it goes to: the remote's, or the one derived for the fork.
    pub url: String,
    pub repo: RepoSlug,
    /// `refs/heads/<branch>`.
    pub reference: String,
}

impl Destination {
    /// What `git push` and `git ls-remote` are pointed at: the remote by name when there is one, so
    /// its own push URL and settings apply, otherwise the URL.
    #[must_use]
    pub fn remote_arg(&self) -> &str {
        self.remote.as_deref().unwrap_or(&self.url)
    }
}

/// The remote whose repository is your fork of `base`: same name, owned by `login`.
#[must_use]
pub fn your_fork(base: &RepoSlug, login: &str, remotes: &[RemoteUrl]) -> Option<RemoteUrl> {
    remotes
        .iter()
        .find(|remote| {
            RepoSlug::parse_url(&remote.url).is_some_and(|slug| {
                slug.owner.eq_ignore_ascii_case(login) && slug.name.eq_ignore_ascii_case(&base.name)
            })
        })
        .cloned()
}

/// `like`, a remote URL of some repository, pointed at `repo` instead: same scheme, host and user,
/// so it is authenticated the way your other pushes are.
#[must_use]
pub fn url_like(like: &str, repo: &RepoSlug) -> Option<String> {
    let slug = RepoSlug::parse_url(like)?;
    let from = format!("{}/{}", slug.owner, slug.name);
    let at = like.rfind(&from)?;
    Some(format!(
        "{}{}{}",
        &like[..at],
        repo.full_name(),
        &like[at + from.len()..]
    ))
}

/// What a push expects to find at its destination before it sends anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Expect {
    /// Nothing: a new branch.
    Absent,
    /// This commit: a fast-forward from it.
    At { sha: String },
}

/// What the read before a push says to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BeforePush {
    /// The ref is at the commit already: it happened.
    AlreadyThere,
    /// The ref is where the approval expected: push.
    Send,
    /// The ref is somewhere the approval did not cover. Nothing is sent without you.
    Moved {
        expected: Option<String>,
        current: Option<String>,
    },
}

/// What to do, given what the destination ref holds now (`None` for no such ref).
#[must_use]
pub fn before_push(expect: &Expect, commit: &str, now: Option<&str>) -> BeforePush {
    if now == Some(commit) {
        return BeforePush::AlreadyThere;
    }
    match (expect, now) {
        (Expect::Absent, None) => BeforePush::Send,
        (Expect::At { sha }, Some(now)) if now == sha => BeforePush::Send,
        (Expect::Absent, now) => BeforePush::Moved {
            expected: None,
            current: now.map(str::to_owned),
        },
        (Expect::At { sha }, now) => BeforePush::Moved {
            expected: Some(sha.clone()),
            current: now.map(str::to_owned),
        },
    }
}

// ═══════════════════════════════ plans ═══════════════════════════════

/// The pull request a fix is of.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteTarget {
    pub repo: RepoSlug,
    pub number: u64,
    pub url: String,
}

/// Their branch, as the forge described it when the route was planned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TheirBranch {
    /// `None` when the fork it was in has been deleted.
    pub repo: Option<RepoSlug>,
    pub branch: String,
    pub head: String,
}

/// The fix being sent: from which commit, to which, and its diff's hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FixRef {
    pub base: String,
    pub head: String,
    pub diff_hash: String,
}

/// The pull request a follow-up opens, exactly as it is sent apart from its marker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FollowUpPr {
    /// Where it is opened: the repository their branch is in.
    pub repo: RepoSlug,
    /// Their branch.
    pub base: String,
    /// The fix's branch, as the forge wants it: `branch`, or `owner:branch` from another fork.
    pub head: String,
    pub title: String,
    pub body: String,
}

/// A push route, follow-up or to their branch: everything it sends, and where.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutePlan {
    pub kind: RouteKind,
    pub pull_request: RouteTarget,
    pub their: TheirBranch,
    pub fix: FixRef,
    pub push: Destination,
    pub expect: Expect,
    /// The follow-up pull request, for that route.
    pub open: Option<FollowUpPr>,
}

/// The pins every route is checked against: exactly where (remote and ref), exactly what (the
/// diff, and the commit carrying it), and the head of their branch you saw.
#[must_use]
pub fn route_binding(
    remote: &str,
    reference: &str,
    diff_hash: &str,
    commit: &str,
    head: &str,
) -> Binding {
    Binding::default()
        .with(Pinned::Remote, remote)
        .with(Pinned::Ref, reference)
        .with(Pinned::Diff, diff_hash)
        .with(Pinned::Commit, commit)
        .with(Pinned::Head, head)
}

impl RoutePlan {
    /// The hash an approval pins: of this value whole.
    #[must_use]
    pub fn hash(&self) -> String {
        hash_of(self)
    }

    /// The refspec it pushes.
    #[must_use]
    pub fn refspec(&self) -> String {
        format!("{}:{}", self.fix.head, self.push.reference)
    }

    /// What an approval of exactly this plan covers.
    #[must_use]
    pub fn binding(&self) -> Binding {
        self.binding_now(&self.their.head, &self.fix.diff_hash)
    }

    /// What it would cover now, with their branch at `head` and the fix's diff `diff_hash`: an
    /// approval of [`Self::binding`] still holds only if nothing moved.
    #[must_use]
    pub fn binding_now(&self, head: &str, diff_hash: &str) -> Binding {
        route_binding(
            &self.push.url,
            &self.push.reference,
            diff_hash,
            &self.fix.head,
            head,
        )
        .with(Pinned::Payload, self.hash())
    }

    /// The writes that send it, in order: the push, then the pull request for a follow-up.
    #[must_use]
    pub fn operations(&self, flow: FlowId) -> Vec<NewOperation> {
        let route = self.hash();
        let mut ops = vec![NewOperation::new(
            flow,
            0,
            OperationPayload::PushBranch(PushPayload {
                route: route.clone(),
                destination: self.push.clone(),
                commit: self.fix.head.clone(),
                expect: self.expect.clone(),
            }),
        )];
        if let Some(open) = &self.open {
            ops.push(NewOperation::new(
                flow,
                1,
                OperationPayload::OpenPullRequest(OpenPullRequestPayload {
                    route,
                    pull_request: open.clone(),
                }),
            ));
        }
        ops
    }
}

/// A push, as the outbox sends it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PushPayload {
    /// [`RoutePlan::hash`] of the route it is part of, so two routes never share a write.
    pub route: String,
    pub destination: Destination,
    pub commit: String,
    pub expect: Expect,
}

impl PushPayload {
    /// The refspec it pushes, with no `+`.
    #[must_use]
    pub fn refspec(&self) -> String {
        format!("{}:{}", self.commit, self.destination.reference)
    }
}

/// Opening the follow-up pull request, as the outbox sends it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenPullRequestPayload {
    pub route: String,
    pub pull_request: FollowUpPr,
}

/// What a route is planned from: the pull request as just read, your access and sign-in, your
/// checkout's remotes, the branches nothing is pushed to, and the fix.
#[derive(Debug, Clone, Copy)]
pub struct RouteFacts<'a> {
    pub pr: &'a PullRequest,
    pub permission: Permission,
    pub login: Option<&'a str>,
    pub remotes: &'a [RemoteUrl],
    pub integration: &'a IntegrationBranches,
    pub fix: &'a FixRef,
}

impl RouteFacts<'_> {
    fn target(&self) -> RouteTarget {
        RouteTarget {
            repo: self.pr.base.repo.clone(),
            number: self.pr.number,
            url: self.pr.url.clone(),
        }
    }

    fn their(&self) -> TheirBranch {
        TheirBranch {
            repo: self.pr.head.repo.clone(),
            branch: self.pr.head.branch.clone(),
            head: self.pr.head.sha.as_str().to_owned(),
        }
    }

    fn remote_url(&self, name: &str) -> Option<String> {
        self.remotes
            .iter()
            .find(|remote| remote.name == name)
            .map(|remote| remote.url.clone())
    }
}

/// The hash a route's approval pins as the diff it covers: of `git diff --binary base head`, byte
/// for byte, so the same commits always hash the same and any other change does not.
#[must_use]
pub fn diff_hash(diff: &str) -> String {
    super::publish::sha256_of(diff.as_bytes())
}

/// The branch a follow-up pushes the fix to.
#[must_use]
pub fn follow_up_branch(number: u64, fix_head: &str) -> String {
    format!(
        "wtm/pr-{number}-fix-{}",
        fix_head.get(..7).unwrap_or(fix_head)
    )
}

/// A follow-up pull request: the fix on a branch of its own, into their branch.
///
/// The branch goes to the base repository when their branch is there and you can push to it, and
/// otherwise to your fork: their branch is in a fork, or you lack write access. The pull request is
/// opened where their branch is.
///
/// # Errors
///
/// When their fork is gone, there is nowhere you can push the branch, or the branch would be an
/// integration branch.
pub fn follow_up(facts: &RouteFacts<'_>, title: &str, body: &str) -> Result<RoutePlan, RouteError> {
    let pr = facts.pr;
    let their_repo = pr.head.repo.clone().ok_or_else(|| {
        RouteError(
            "The fork this pull request came from has been deleted, so there is no branch to \
             open a follow-up pull request into."
                .to_owned(),
        )
    })?;
    let base_remote = remote_for(&pr.base.repo, facts.remotes);
    let (remote, repo) = match base_remote {
        Some(remote) if !pr.cross_repository && facts.permission.can_push() => {
            (remote, pr.base.repo.clone())
        }
        _ => {
            let login = facts.login.ok_or_else(|| {
                RouteError(
                    "wtm does not know which account you are signed in as, so it cannot find \
                     your fork to push the follow-up branch to."
                        .to_owned(),
                )
            })?;
            let fork = your_fork(&pr.base.repo, login, facts.remotes).ok_or_else(|| {
                RouteError(format!(
                    "{} Add a remote for your fork ({login}/{}) to send a follow-up pull request.",
                    if pr.cross_repository {
                        "Their branch is in a fork, so the follow-up branch goes to yours, and \
                         this checkout has no remote for it."
                    } else {
                        "You cannot push to the base repository, and this checkout has no remote \
                         for a fork of yours."
                    },
                    pr.base.repo.name
                ))
            })?;
            let slug = RepoSlug::parse_url(&fork.url)
                .ok_or_else(|| RouteError(format!("`{}` is not a repository URL", fork.url)))?;
            (fork.name, slug)
        }
    };
    let branch = follow_up_branch(pr.number, &facts.fix.head);
    let refspec = Refspec::new(&facts.fix.head, &branch, facts.integration)?;
    let url = facts
        .remote_url(&remote)
        .ok_or_else(|| RouteError(format!("the remote `{remote}` has no URL")))?;
    let head = if repo.same_repository(&their_repo) {
        branch.clone()
    } else {
        format!("{}:{branch}", repo.owner)
    };
    let title = title.trim();
    if title.is_empty() {
        return Err(RouteError(
            "The follow-up pull request needs a title.".to_owned(),
        ));
    }
    Ok(RoutePlan {
        kind: RouteKind::FollowUp,
        pull_request: facts.target(),
        their: facts.their(),
        fix: facts.fix.clone(),
        push: Destination {
            remote: Some(remote),
            url,
            repo,
            reference: refspec.dst,
        },
        expect: Expect::Absent,
        open: Some(FollowUpPr {
            repo: their_repo,
            base: pr.head.branch.clone(),
            head,
            title: title.to_owned(),
            body: body.trim().to_owned(),
        }),
    })
}

/// The fix pushed to their branch, as a fast-forward of the head it was made from.
///
/// # Errors
///
/// When your access or their fork does not allow it, their branch has moved since the fix was made
/// from it (the push would not be a fast-forward), it is an integration branch, or there is no way
/// to reach their repository.
pub fn push_to_their_branch(facts: &RouteFacts<'_>) -> Result<RoutePlan, RouteError> {
    let pr = facts.pr;
    match push_to_branch(pr, facts.permission) {
        PushToBranch::Allowed => {}
        PushToBranch::ForkWithoutMaintainerEdits => {
            return Err(RouteError(
                "Their branch is in a fork whose author has not allowed maintainers to edit it, \
                 so nobody but them can push to it."
                    .to_owned(),
            ));
        }
        PushToBranch::HeadRepositoryGone => {
            return Err(RouteError(
                "The fork this pull request came from has been deleted.".to_owned(),
            ));
        }
        PushToBranch::NoWriteAccess => {
            return Err(RouteError(format!(
                "Your access to {} does not include pushing.",
                pr.base.repo
            )));
        }
    }
    let their_repo = pr
        .head
        .repo
        .clone()
        .ok_or_else(|| RouteError("Their branch's repository is not known.".to_owned()))?;
    let head = pr.head.sha.as_str();
    if head != facts.fix.base {
        return Err(RouteError(format!(
            "#{} has new commits since the fix was made from {}: pushing it would not be a \
             fast-forward of their branch. Re-review the changes, or send the fix as a follow-up \
             pull request.",
            pr.number,
            facts.fix.base.get(..7).unwrap_or(&facts.fix.base)
        )));
    }
    let refspec = Refspec::new(&facts.fix.head, &pr.head.branch, facts.integration)?;
    let push = if let Some(remote) = remote_for(&their_repo, facts.remotes) {
        Destination {
            url: facts
                .remote_url(&remote)
                .ok_or_else(|| RouteError(format!("the remote `{remote}` has no URL")))?,
            remote: Some(remote),
            repo: their_repo,
            reference: refspec.dst,
        }
    } else {
        let like = remote_for(&pr.base.repo, facts.remotes)
            .and_then(|remote| facts.remote_url(&remote))
            .ok_or_else(|| {
                RouteError(format!(
                    "No remote of this checkout points at {} or {}, so wtm cannot tell how you \
                     push there.",
                    their_repo, pr.base.repo
                ))
            })?;
        Destination {
            url: url_like(&like, &their_repo).ok_or_else(|| {
                RouteError(format!("`{like}` is not a repository URL wtm can read"))
            })?,
            remote: None,
            repo: their_repo,
            reference: refspec.dst,
        }
    };
    Ok(RoutePlan {
        kind: RouteKind::PushToBranch,
        pull_request: facts.target(),
        their: facts.their(),
        fix: facts.fix.clone(),
        push,
        expect: Expect::At {
            sha: head.to_owned(),
        },
        open: None,
    })
}

// ═══════════════════════════════ suggestions ═══════════════════════════════

/// The fix as suggestion comments: a review whose comments the author can apply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuggestionRoute {
    pub review: ReviewPayload,
    pub their: TheirBranch,
    pub fix: FixRef,
}

impl SuggestionRoute {
    /// The hash a preview shows and an approval is checked against: of this value whole.
    #[must_use]
    pub fn hash(&self) -> String {
        hash_of(self)
    }

    /// The remote a suggestion goes to: the pull request's repository.
    #[must_use]
    pub fn remote(&self) -> String {
        let repo = &self.review.repo;
        format!("https://{}/{}", repo.host, repo.full_name())
    }

    /// The ref it applies to: the pull request's head.
    #[must_use]
    pub fn reference(&self) -> String {
        format!("refs/pull/{}/head", self.review.number)
    }

    /// What an approval of exactly these suggestions covers.
    #[must_use]
    pub fn binding(&self) -> Binding {
        self.binding_now(&self.their.head, &self.fix.diff_hash)
    }

    /// As [`RoutePlan::binding_now`]: what the approval would need to cover with their branch at
    /// `head` and the fix's diff `diff_hash`.
    #[must_use]
    pub fn binding_now(&self, head: &str, diff_hash: &str) -> Binding {
        let mut binding = review_binding(&self.review, head);
        binding.0.insert(Pinned::Remote, self.remote());
        binding.0.insert(Pinned::Ref, self.reference());
        binding.0.insert(Pinned::Diff, diff_hash.to_owned());
        binding
    }
}

/// How a file changed in a diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffStatus {
    Modified,
    Added,
    Deleted,
    Binary,
}

/// One hunk of a diff with no context: lines `old_start..` of the old side replaced by `new`.
/// With `old` empty, `new` is inserted after line `old_start` (0 for the top of the file).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffHunk {
    pub old_start: u32,
    pub old: Vec<String>,
    pub new: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffFile {
    pub path: String,
    pub status: DiffStatus,
    pub hunks: Vec<DiffHunk>,
}

/// Read `git diff --unified=0 --no-renames` output.
///
/// Only what a suggestion needs: each file's path, whether it was added, deleted or binary, and its
/// hunks' old ranges and lines. A path git had to quote keeps its quotes stripped and its escapes as
/// written; such a path is in the diff the person reads either way.
#[must_use]
pub fn parse_unified(diff: &str) -> Vec<DiffFile> {
    let mut files: Vec<DiffFile> = Vec::new();
    let mut hunk: Option<DiffHunk> = None;
    let flush = |files: &mut Vec<DiffFile>, hunk: &mut Option<DiffHunk>| {
        if let (Some(done), Some(file)) = (hunk.take(), files.last_mut()) {
            file.hunks.push(done);
        }
    };
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            flush(&mut files, &mut hunk);
            let path = rest
                .rsplit_once(" b/")
                .map_or(rest, |(_, b)| b)
                .trim_matches('"')
                .to_owned();
            files.push(DiffFile {
                path,
                status: DiffStatus::Modified,
                hunks: Vec::new(),
            });
            continue;
        }
        let Some(file) = files.last_mut() else {
            continue;
        };
        if hunk.is_none() {
            if line.starts_with("new file mode") {
                file.status = DiffStatus::Added;
            } else if line.starts_with("deleted file mode") {
                file.status = DiffStatus::Deleted;
            } else if line.starts_with("Binary files ") || line == "GIT binary patch" {
                file.status = DiffStatus::Binary;
            } else if let Some(path) = line.strip_prefix("+++ b/") {
                path.trim_matches('"').clone_into(&mut file.path);
            }
        }
        if let Some(header) = line.strip_prefix("@@ ") {
            flush(&mut files, &mut hunk);
            let old = header
                .split_whitespace()
                .next()
                .and_then(|range| range.strip_prefix('-'))
                .unwrap_or("0");
            let start = old
                .split(',')
                .next()
                .and_then(|n| n.parse().ok())
                .unwrap_or(0);
            hunk = Some(DiffHunk {
                old_start: start,
                old: Vec::new(),
                new: Vec::new(),
            });
            continue;
        }
        if let Some(current) = hunk.as_mut() {
            if let Some(text) = line.strip_prefix('-') {
                current.old.push(text.to_owned());
            } else if let Some(text) = line.strip_prefix('+') {
                current.new.push(text.to_owned());
            }
        }
    }
    flush(&mut files, &mut hunk);
    files
}

/// What a suggestion review is built from.
#[derive(Debug, Clone, Copy)]
pub struct SuggestionContext<'a> {
    pub repo: &'a RepoSlug,
    pub number: u64,
    /// Their head, which the fix was made from: every suggestion's line is a line of it.
    pub commit_id: &'a str,
    pub findings: &'a [FindingRecord],
    /// The findings the fix was for, first the one a hunk is credited to when none is closer.
    pub picks: &'a [FindingId],
    /// The pull request's own diff at `commit_id`: where a comment can sit.
    pub lines: &'a ChangedLines,
    /// The fix, as [`parse_unified`] read it.
    pub diff: &'a [DiffFile],
}

/// The longest run of backticks in `text`.
fn longest_backtick_run(text: &str) -> usize {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        if c == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    longest
}

/// A fenced block of `kind` holding `lines`, its fence longer than any run of backticks inside.
fn fenced(kind: &str, lines: &[String]) -> String {
    let body = lines.join("\n");
    let fence = "`".repeat(longest_backtick_run(&body).max(2) + 1);
    if lines.is_empty() {
        format!("{fence}{kind}\n{fence}")
    } else {
        format!("{fence}{kind}\n{body}\n{fence}")
    }
}

/// The finding a hunk at `path` lines `start..=end` is credited to: a pick on that file whose lines
/// overlap, else any pick on that file, else the first pick.
fn credited(ctx: &SuggestionContext<'_>, path: &str, start: u32, end: u32) -> Option<FindingId> {
    let picked = |id: &FindingId| ctx.findings.iter().find(|record| record.finding.id == *id);
    let on_file: Vec<&FindingRecord> = ctx
        .picks
        .iter()
        .filter_map(picked)
        .filter(|record| {
            record
                .finding
                .location
                .as_ref()
                .is_some_and(|at| at.path == path)
        })
        .collect();
    on_file
        .iter()
        .find(|record| {
            record
                .finding
                .location
                .as_ref()
                .is_some_and(|at| at.start_line <= end && start <= at.end_line)
        })
        .or_else(|| on_file.first())
        .map(|record| record.finding.id)
        .or_else(|| ctx.picks.first().copied())
}

fn title_of(ctx: &SuggestionContext<'_>, finding: FindingId) -> String {
    ctx.findings
        .iter()
        .find(|record| record.finding.id == finding)
        .map_or_else(String::new, |record| record.finding.title.clone())
}

/// Whether lines `start..=end` of `path` all sit in one hunk of the pull request's diff. A
/// suggestion replaces exactly the lines it is on, so unlike a comment it cannot be narrowed to the
/// part that fits.
fn sits(lines: &ChangedLines, path: &str, start: u32, end: u32) -> Result<(), FoldReason> {
    match lines.files.get(path) {
        None => Err(FoldReason::NotInChange),
        Some(FileLines::NoDiff) => Err(FoldReason::NoDiff),
        Some(FileLines::Hunks(hunks)) => hunks
            .iter()
            .any(|hunk| hunk.start <= start && end <= hunk.end)
            .then_some(())
            .ok_or(FoldReason::NotChanged),
    }
}

/// The fix as a review of suggestion comments, with `summary` as its body.
///
/// Each hunk becomes a suggestion on exactly the lines of their head it replaces, when those lines
/// are on the pull request's diff. A hunk that cannot be one — a new, deleted or binary file, lines
/// the pull request did not touch — goes into the summary as a diff, under its place, so nothing in
/// the fix is left out of what the author reads. `original` reads one line of their head, which an
/// insertion needs: a suggestion replaces lines, so text added after line `n` is suggested as line
/// `n` followed by the new text.
///
/// # Errors
///
/// When there is nothing to suggest.
pub fn suggestions(
    ctx: &SuggestionContext<'_>,
    original: &dyn Fn(&str, u32) -> Option<String>,
    summary: &str,
) -> Result<ReviewPayload, PublishError> {
    let mut comments = Vec::new();
    let mut folded = Vec::new();
    for file in ctx.diff {
        let whole_file = |what: &str, finding: FindingId| FoldedComment {
            finding,
            path: Some(file.path.clone()),
            line: None,
            body: format!(
                "This fix {what} this file; apply it from the follow-up branch or by hand."
            ),
            reason: FoldReason::WholeFile,
        };
        let Some(first) = credited(ctx, &file.path, 1, u32::MAX) else {
            continue;
        };
        match file.status {
            DiffStatus::Added => {
                folded.push(whole_file("adds", first));
                continue;
            }
            DiffStatus::Deleted => {
                folded.push(whole_file("deletes", first));
                continue;
            }
            DiffStatus::Binary => {
                folded.push(whole_file("changes the binary contents of", first));
                continue;
            }
            DiffStatus::Modified => {}
        }
        for hunk in &file.hunks {
            let len = u32::try_from(hunk.old.len()).unwrap_or(u32::MAX);
            let replaced: Option<(u32, u32, Vec<String>)> = if len > 0 {
                Some((hunk.old_start, hunk.old_start + len - 1, hunk.new.clone()))
            } else if hunk.old_start == 0 {
                original(&file.path, 1).map(|line| {
                    let mut text = hunk.new.clone();
                    text.push(line);
                    (1, 1, text)
                })
            } else {
                original(&file.path, hunk.old_start).map(|line| {
                    let mut text = vec![line];
                    text.extend(hunk.new.iter().cloned());
                    (hunk.old_start, hunk.old_start, text)
                })
            };
            let (start, end) = replaced.as_ref().map_or(
                (hunk.old_start.max(1), hunk.old_start.max(1)),
                |(s, e, _)| (*s, *e),
            );
            let Some(finding) = credited(ctx, &file.path, start, end) else {
                continue;
            };
            let diff_lines: Vec<String> = hunk
                .old
                .iter()
                .map(|l| format!("-{l}"))
                .chain(hunk.new.iter().map(|l| format!("+{l}")))
                .collect();
            let placed = replaced
                .as_ref()
                .map_or(Err(FoldReason::NoDiff), |(s, e, _)| {
                    sits(ctx.lines, &file.path, *s, *e)
                });
            match (replaced, placed) {
                (Some((start, end, text)), Ok(())) => {
                    let mut body = String::new();
                    let title = title_of(ctx, finding);
                    if !title.is_empty() {
                        let _ = write!(body, "Suggested fix: {title}\n\n");
                    }
                    body.push_str(&fenced("suggestion", &text));
                    comments.push(InlineComment {
                        finding,
                        path: file.path.clone(),
                        start_line: (start < end).then_some(start),
                        line: end,
                        body,
                    });
                }
                (_, placed) => folded.push(FoldedComment {
                    finding,
                    path: Some(file.path.clone()),
                    line: Some(start),
                    body: format!(
                        "a suggestion cannot sit here, so the change is shown as a diff:\n\n{}",
                        fenced("diff", &diff_lines)
                    ),
                    reason: placed.err().unwrap_or(FoldReason::NotChanged),
                }),
            }
        }
    }
    if comments.is_empty() && folded.is_empty() {
        return Err(PublishError(
            "The fix changes nothing that could be suggested.".to_owned(),
        ));
    }
    Ok(ReviewPayload {
        repo: ctx.repo.clone(),
        number: ctx.number,
        commit_id: ctx.commit_id.to_owned(),
        event: ReviewEvent::Comment,
        comments,
        folded,
        summary: summary.trim().to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::flow::forge::{ChecksSummary, PrBase, PrHead, PrState};
    use crate::flow::outbox::OperationKind;
    use crate::flow::publish::LineRange;
    use crate::flow::review::{Finding, FindingClass, Location, Severity};
    use crate::model::CommitId;

    const BASE: &str = "7c9e1d0000000000000000000000000000000000";
    const FIX: &str = "f1a2b3c4d5e6f708090a0b0c0d0e0f1011121314";

    fn integration() -> IntegrationBranches {
        IntegrationBranches::new(&["develop".to_owned(), "release/*".to_owned()], "trunk")
    }

    #[test]
    fn a_refspec_names_a_commit_and_a_full_branch_ref_and_never_forces() {
        let spec = Refspec::new(FIX, "wtm/pr-4031-fix-f1a2b3c", &integration()).unwrap();
        assert_eq!(
            spec.arg(),
            format!("{FIX}:refs/heads/wtm/pr-4031-fix-f1a2b3c")
        );
        assert!(!spec.arg().starts_with('+'));
        assert_eq!(spec.branch(), "wtm/pr-4031-fix-f1a2b3c");
    }

    #[test]
    fn no_refspec_targets_an_integration_branch_whether_configured_built_in_or_the_base() {
        for branch in [
            "main",
            "master",
            "develop",
            "release/4.2",
            "release/4.2/hotfix",
            "trunk",
        ] {
            let refused = Refspec::new(FIX, branch, &integration());
            assert!(
                refused.is_err_and(|e| e.0.contains("integration branch")),
                "{branch}"
            );
        }
        assert!(
            Refspec::new(FIX, "main", &IntegrationBranches::new(&[], "")).is_err(),
            "main is refused with nothing configured"
        );
        assert!(Refspec::new(FIX, "feature/csv-export", &integration()).is_ok());
    }

    #[test]
    fn a_refspec_with_a_pattern_a_ref_name_a_short_id_or_a_force_is_refused() {
        for branch in [
            "",
            "+feature",
            "feature:main",
            "refs/heads/feature",
            "feat*",
            "a..b",
            "-x",
            "with space",
            "x.lock",
            "x/.hidden",
            "@",
        ] {
            assert!(
                Refspec::new(FIX, branch, &integration()).is_err(),
                "{branch:?}"
            );
        }
        for commit in ["HEAD", "7c9e1d", "refs/heads/feature", &FIX.to_uppercase()] {
            assert!(
                Refspec::new(commit, "feature", &integration()).is_err(),
                "{commit}"
            );
        }
    }

    #[test]
    fn a_push_goes_ahead_only_from_where_its_approval_expected_the_ref_to_be() {
        let at = Expect::At {
            sha: BASE.to_owned(),
        };
        assert_eq!(before_push(&at, FIX, Some(BASE)), BeforePush::Send);
        assert_eq!(before_push(&at, FIX, Some(FIX)), BeforePush::AlreadyThere);
        assert_eq!(
            before_push(&at, FIX, Some("9999999999999999999999999999999999999999")),
            BeforePush::Moved {
                expected: Some(BASE.to_owned()),
                current: Some("9999999999999999999999999999999999999999".to_owned()),
            },
            "the author pushed after you approved"
        );
        assert!(matches!(
            before_push(&at, FIX, None),
            BeforePush::Moved { current: None, .. }
        ));
        assert_eq!(before_push(&Expect::Absent, FIX, None), BeforePush::Send);
        assert!(matches!(
            before_push(&Expect::Absent, FIX, Some(BASE)),
            BeforePush::Moved { expected: None, .. }
        ));
    }

    fn remote(name: &str, url: &str) -> RemoteUrl {
        RemoteUrl {
            name: name.to_owned(),
            url: url.to_owned(),
        }
    }

    fn pr(cross_repository: bool, maintainer_can_modify: bool) -> PullRequest {
        PullRequest {
            number: 4031,
            url: "https://github.com/acme/webapp/pull/4031".to_owned(),
            title: "CSV export".to_owned(),
            author: Some("sam".to_owned()),
            state: PrState::Open,
            draft: false,
            head: PrHead {
                repo: Some(RepoSlug::new(
                    "github.com",
                    if cross_repository { "sam" } else { "acme" },
                    "webapp",
                )),
                branch: "feature/csv-export".to_owned(),
                sha: CommitId::new(BASE),
            },
            base: PrBase {
                repo: RepoSlug::new("github.com", "acme", "webapp"),
                branch: "develop".to_owned(),
                sha: None,
            },
            cross_repository,
            maintainer_can_modify,
            requested_reviewers: Vec::new(),
            checks: ChecksSummary::default(),
        }
    }

    fn fix() -> FixRef {
        FixRef {
            base: BASE.to_owned(),
            head: FIX.to_owned(),
            diff_hash: "sha256:d1ff".to_owned(),
        }
    }

    fn facts<'a>(
        pr: &'a PullRequest,
        permission: Permission,
        remotes: &'a [RemoteUrl],
        integration: &'a IntegrationBranches,
        fix: &'a FixRef,
    ) -> RouteFacts<'a> {
        RouteFacts {
            pr,
            permission,
            login: Some("takumi"),
            remotes,
            integration,
            fix,
        }
    }

    #[test]
    fn a_follow_up_of_a_branch_in_the_base_repository_goes_to_origin_and_into_their_branch() {
        let pr = pr(false, false);
        let remotes = [remote("origin", "git@github.com:acme/webapp.git")];
        let (integration, fix) = (integration(), fix());
        let plan = follow_up(
            &facts(&pr, Permission::Write, &remotes, &integration, &fix),
            "Fixes for #4031",
            "",
        )
        .unwrap();
        assert_eq!(plan.push.remote.as_deref(), Some("origin"));
        assert_eq!(plan.push.url, "git@github.com:acme/webapp.git");
        assert_eq!(plan.push.reference, "refs/heads/wtm/pr-4031-fix-f1a2b3c");
        assert_eq!(plan.expect, Expect::Absent);
        let open = plan.open.unwrap();
        assert_eq!(open.repo.full_name(), "acme/webapp");
        assert_eq!(open.base, "feature/csv-export");
        assert_eq!(open.head, "wtm/pr-4031-fix-f1a2b3c");
    }

    #[test]
    fn a_follow_up_of_a_forks_branch_goes_to_your_fork_and_is_opened_in_theirs() {
        let pr = pr(true, false);
        let remotes = [
            remote("origin", "git@github.com:acme/webapp.git"),
            remote("mine", "git@github.com:takumi/webapp.git"),
        ];
        let (integration, fix) = (integration(), fix());
        let plan = follow_up(
            &facts(&pr, Permission::Admin, &remotes, &integration, &fix),
            "Fixes",
            "",
        )
        .unwrap();
        assert_eq!(plan.push.remote.as_deref(), Some("mine"));
        assert_eq!(plan.push.repo.full_name(), "takumi/webapp");
        let open = plan.open.unwrap();
        assert_eq!(open.repo.full_name(), "sam/webapp", "where their branch is");
        assert_eq!(open.head, "takumi:wtm/pr-4031-fix-f1a2b3c");

        let without_fork = [remote("origin", "git@github.com:acme/webapp.git")];
        let refused = follow_up(
            &facts(&pr, Permission::Admin, &without_fork, &integration, &fix),
            "Fixes",
            "",
        );
        assert!(refused.is_err_and(|e| e.0.contains("remote for your fork")));
    }

    #[test]
    fn without_write_access_a_follow_up_goes_to_your_fork_even_for_a_branch_of_the_base() {
        let pr = pr(false, false);
        let remotes = [
            remote("origin", "https://github.com/acme/webapp.git"),
            remote("fork", "https://github.com/Takumi/webapp.git"),
        ];
        let (integration, fix) = (integration(), fix());
        let plan = follow_up(
            &facts(&pr, Permission::Read, &remotes, &integration, &fix),
            "Fixes",
            "",
        )
        .unwrap();
        assert_eq!(plan.push.remote.as_deref(), Some("fork"));
        assert_eq!(plan.open.unwrap().head, "Takumi:wtm/pr-4031-fix-f1a2b3c");
    }

    #[test]
    fn a_push_to_their_branch_needs_write_access_and_a_fork_that_allows_maintainer_edits() {
        let remotes = [remote("origin", "git@github.com:acme/webapp.git")];
        let (integration, fix) = (integration(), fix());
        let fork_closed = pr(true, false);
        assert!(
            push_to_their_branch(&facts(
                &fork_closed,
                Permission::Admin,
                &remotes,
                &integration,
                &fix
            ))
            .is_err_and(|e| e.0.contains("not allowed maintainers"))
        );
        let same_repo = pr(false, false);
        assert!(
            push_to_their_branch(&facts(
                &same_repo,
                Permission::Read,
                &remotes,
                &integration,
                &fix
            ))
            .is_err_and(|e| e.0.contains("does not include pushing"))
        );
        let plan = push_to_their_branch(&facts(
            &same_repo,
            Permission::Write,
            &remotes,
            &integration,
            &fix,
        ))
        .unwrap();
        assert_eq!(plan.push.remote.as_deref(), Some("origin"));
        assert_eq!(plan.push.reference, "refs/heads/feature/csv-export");
        assert_eq!(
            plan.expect,
            Expect::At {
                sha: BASE.to_owned()
            }
        );
        assert_eq!(
            plan.refspec(),
            format!("{FIX}:refs/heads/feature/csv-export")
        );
    }

    #[test]
    fn a_fork_that_allows_maintainer_edits_is_pushed_to_by_a_url_like_your_base_remote() {
        let open = pr(true, true);
        let remotes = [remote("origin", "git@github.com:acme/webapp.git")];
        let (integration, fix) = (integration(), fix());
        let plan = push_to_their_branch(&facts(
            &open,
            Permission::Write,
            &remotes,
            &integration,
            &fix,
        ))
        .unwrap();
        assert_eq!(plan.push.remote, None, "no remote is added for one push");
        assert_eq!(plan.push.url, "git@github.com:sam/webapp.git");
        assert_eq!(plan.push.remote_arg(), "git@github.com:sam/webapp.git");
    }

    #[test]
    fn a_push_to_their_branch_is_refused_once_they_have_pushed_past_the_fixs_base() {
        let mut moved = pr(false, false);
        moved.head.sha = CommitId::new("9999999999999999999999999999999999999999");
        let remotes = [remote("origin", "git@github.com:acme/webapp.git")];
        let (integration, fix) = (integration(), fix());
        assert!(
            push_to_their_branch(&facts(
                &moved,
                Permission::Write,
                &remotes,
                &integration,
                &fix
            ))
            .is_err_and(|e| e.0.contains("fast-forward"))
        );
    }

    #[test]
    fn their_branch_being_an_integration_branch_closes_the_push_route() {
        let mut onto_main = pr(false, false);
        onto_main.head.branch = "main".to_owned();
        let remotes = [remote("origin", "git@github.com:acme/webapp.git")];
        let (integration, fix) = (integration(), fix());
        assert!(
            push_to_their_branch(&facts(
                &onto_main,
                Permission::Admin,
                &remotes,
                &integration,
                &fix
            ))
            .is_err_and(|e| e.0.contains("integration branch"))
        );
    }

    #[test]
    fn a_route_approval_pins_remote_ref_diff_commit_and_their_head_and_goes_stale_on_any() {
        let pr = pr(false, false);
        let remotes = [remote("origin", "git@github.com:acme/webapp.git")];
        let (integration, fix) = (integration(), fix());
        let plan =
            push_to_their_branch(&facts(&pr, Permission::Write, &remotes, &integration, &fix))
                .unwrap();
        let binding = plan.binding();
        for pin in [
            Pinned::Remote,
            Pinned::Ref,
            Pinned::Diff,
            Pinned::Commit,
            Pinned::Head,
            Pinned::Payload,
        ] {
            assert!(binding.0.contains_key(&pin), "{pin:?}");
        }
        let approval = crate::flow::approval::Approval::grant(
            crate::flow::approval::ApprovalId(1),
            FlowId(1),
            crate::flow::approval::ApprovalIntent::Route,
            binding,
            0,
        )
        .unwrap();
        assert_eq!(
            approval.check(&plan.binding_now(BASE, "sha256:d1ff")),
            Ok(())
        );
        let moved = approval
            .check(&plan.binding_now("9999999999999999999999999999999999999999", "sha256:d1ff"))
            .unwrap_err();
        assert_eq!(
            moved.changes.iter().map(|c| c.pin).collect::<Vec<_>>(),
            [Pinned::Head]
        );
        let refixed = approval
            .check(&plan.binding_now(BASE, "sha256:0ther"))
            .unwrap_err();
        assert_eq!(
            refixed.changes.iter().map(|c| c.pin).collect::<Vec<_>>(),
            [Pinned::Diff]
        );
    }

    #[test]
    fn a_follow_up_is_a_push_then_a_pull_request_and_a_push_route_is_the_push_alone() {
        let pr = pr(false, false);
        let remotes = [remote("origin", "git@github.com:acme/webapp.git")];
        let (integration, fix) = (integration(), fix());
        let f = facts(&pr, Permission::Write, &remotes, &integration, &fix);
        let follow = follow_up(&f, "Fixes", "").unwrap();
        let kinds = |plan: &RoutePlan| {
            plan.operations(FlowId(1))
                .into_iter()
                .map(|op| op.key.kind)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            kinds(&follow),
            [OperationKind::PushBranch, OperationKind::OpenPullRequest]
        );
        assert_eq!(
            kinds(&push_to_their_branch(&f).unwrap()),
            [OperationKind::PushBranch]
        );
    }

    #[test]
    fn a_url_for_another_repository_keeps_the_scheme_host_and_user_of_yours() {
        let sam = RepoSlug::new("github.com", "sam", "webapp");
        assert_eq!(
            url_like("git@github.com:acme/webapp.git", &sam).as_deref(),
            Some("git@github.com:sam/webapp.git")
        );
        assert_eq!(
            url_like("https://github.com/acme/webapp", &sam).as_deref(),
            Some("https://github.com/sam/webapp")
        );
        assert_eq!(url_like("/srv/git/webapp", &sam), None);
    }

    const DIFF: &str = "\
diff --git a/src/export.rs b/src/export.rs
index 1111111..2222222 100644
--- a/src/export.rs
+++ b/src/export.rs
@@ -3 +3 @@ fn export() {
-    for i in 0..=n {
+    for i in 0..n {
@@ -9,0 +10,2 @@ fn export() {
+    // the last unit
+    flush();
@@ -20,2 +21,0 @@ fn other() {
-    old();
-    older();
diff --git a/docs/new.md b/docs/new.md
new file mode 100644
index 0000000..3333333
--- /dev/null
+++ b/docs/new.md
@@ -0,0 +1 @@
+A new page.
diff --git a/logo.png b/logo.png
index 4444444..5555555 100644
Binary files a/logo.png and b/logo.png differ
";

    #[test]
    fn a_diff_with_no_context_is_read_into_files_and_their_old_ranges() {
        let files = parse_unified(DIFF);
        assert_eq!(
            files
                .iter()
                .map(|f| (f.path.as_str(), f.status, f.hunks.len()))
                .collect::<Vec<_>>(),
            [
                ("src/export.rs", DiffStatus::Modified, 3),
                ("docs/new.md", DiffStatus::Added, 1),
                ("logo.png", DiffStatus::Binary, 0),
            ]
        );
        let hunks = &files[0].hunks;
        assert_eq!(hunks[0].old_start, 3);
        assert_eq!(hunks[0].old, ["    for i in 0..=n {"]);
        assert_eq!(hunks[0].new, ["    for i in 0..n {"]);
        assert_eq!(hunks[1].old_start, 9);
        assert!(hunks[1].old.is_empty());
        assert_eq!(hunks[2].old.len(), 2);
        assert!(hunks[2].new.is_empty());
    }

    fn finding(id: i64, path: &str, start: u32, end: u32) -> FindingRecord {
        FindingRecord {
            finding: Finding {
                id: FindingId(id),
                flow: FlowId(1),
                class: FindingClass::Suggestion,
                severity: Severity::Notable,
                location: Some(Location {
                    path: path.to_owned(),
                    start_line: start,
                    end_line: end,
                }),
                title: format!("Finding {id}"),
                rationale: "because".to_owned(),
                requirement: None,
                tree_sha: "t".to_owned(),
                dismissed: None,
            },
            lines: true,
            evidence: Vec::new(),
        }
    }

    #[test]
    fn each_hunk_on_the_pull_requests_diff_is_a_suggestion_of_exactly_the_lines_it_replaces() {
        let repo = RepoSlug::new("github.com", "acme", "webapp");
        let findings = [
            finding(1, "src/export.rs", 3, 3),
            finding(2, "src/export.rs", 9, 10),
        ];
        let lines = ChangedLines {
            files: BTreeMap::from([(
                "src/export.rs".to_owned(),
                FileLines::Hunks(vec![LineRange { start: 1, end: 12 }]),
            )]),
        };
        let diff = parse_unified(DIFF);
        let original = |path: &str, line: u32| {
            (path == "src/export.rs" && line == 9).then(|| "    total += 1;".to_owned())
        };
        let review = suggestions(
            &SuggestionContext {
                repo: &repo,
                number: 4031,
                commit_id: BASE,
                findings: &findings,
                picks: &[FindingId(1), FindingId(2)],
                lines: &lines,
                diff: &diff,
            },
            &original,
            " Suggested from a local fix. ",
        )
        .unwrap();

        assert_eq!(review.event, ReviewEvent::Comment);
        assert_eq!(review.commit_id, BASE);
        assert_eq!(review.summary, "Suggested from a local fix.");
        let placed: Vec<(i64, Option<u32>, u32)> = review
            .comments
            .iter()
            .map(|c| (c.finding.0, c.start_line, c.line))
            .collect();
        assert_eq!(placed, [(1, None, 3), (2, None, 9)]);
        assert_eq!(
            review.comments[0].body,
            "Suggested fix: Finding 1\n\n```suggestion\n    for i in 0..n {\n```"
        );
        assert!(
            review.comments[1].body.ends_with(
                "```suggestion\n    total += 1;\n    // the last unit\n    flush();\n```"
            ),
            "an insertion keeps the line it follows: {}",
            review.comments[1].body
        );
        // Lines 20–21 are off the pull request's diff, and the new and binary files are whole
        // files: all in the summary, none dropped.
        let folded: Vec<(Option<&str>, FoldReason)> = review
            .folded
            .iter()
            .map(|f| (f.path.as_deref(), f.reason))
            .collect();
        assert_eq!(
            folded,
            [
                (Some("src/export.rs"), FoldReason::NotChanged),
                (Some("docs/new.md"), FoldReason::WholeFile),
                (Some("logo.png"), FoldReason::WholeFile),
            ]
        );
        assert!(
            review.folded[0]
                .body
                .contains("```diff\n-    old();\n-    older();\n```")
        );
    }

    #[test]
    fn a_suggestion_holding_backticks_gets_a_longer_fence() {
        assert_eq!(
            fenced("suggestion", &["```rust".to_owned()]),
            "````suggestion\n```rust\n````"
        );
        assert_eq!(fenced("suggestion", &[]), "```suggestion\n```");
    }

    #[test]
    fn a_suggestion_approval_pins_the_review_and_the_route_and_a_new_fix_makes_it_stale() {
        let repo = RepoSlug::new("github.com", "acme", "webapp");
        let route = SuggestionRoute {
            review: ReviewPayload {
                repo,
                number: 4031,
                commit_id: BASE.to_owned(),
                event: ReviewEvent::Comment,
                comments: Vec::new(),
                folded: Vec::new(),
                summary: "s".to_owned(),
            },
            their: TheirBranch {
                repo: None,
                branch: "feature/csv-export".to_owned(),
                head: BASE.to_owned(),
            },
            fix: fix(),
        };
        let binding = route.binding();
        assert_eq!(
            binding.0.get(&Pinned::Remote).map(String::as_str),
            Some("https://github.com/acme/webapp")
        );
        assert_eq!(
            binding.0.get(&Pinned::Ref).map(String::as_str),
            Some("refs/pull/4031/head")
        );
        let approval = crate::flow::approval::Approval::grant(
            crate::flow::approval::ApprovalId(1),
            FlowId(1),
            crate::flow::approval::ApprovalIntent::Route,
            binding,
            0,
        )
        .unwrap();
        assert_eq!(
            approval.check(&route.binding_now(BASE, "sha256:d1ff")),
            Ok(())
        );
        assert!(
            approval
                .check(&route.binding_now(BASE, "sha256:new"))
                .is_err()
        );
        assert!(
            approval
                .check(&route.binding_now(FIX, "sha256:d1ff"))
                .is_err()
        );
    }
}
