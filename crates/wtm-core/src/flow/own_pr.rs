//! The issue's own pull request: drafted from its task branch at each hand-over, marked ready and
//! moved to review on one approval, followed, and merged only from the Merge sheet.
//!
//! # The draft is automatic, and narrow
//!
//! Pushing the task branch and creating or updating its draft pull request is the plan's one
//! automatic write to a forge (§2.2 step 12, §4.2): it happens at the first hand-over and after each
//! revision, inside a task you started and a plan you approved, and a draft asks nobody for
//! anything. So the app grants that approval itself, and it covers exactly one thing: this commit,
//! pushed to `refs/heads/<task branch>` on the project's remote, and a draft pull request from that
//! branch into the base it was started from ([`DraftPlan`]). The refspec names the commit and the
//! full ref, never a pattern, and [`DraftPlan::new`] refuses a task branch that is an integration
//! branch. It is a fast-forward of where the remote branch is, never a force.
//!
//! # Ready and Needs Review are one approval of yours, bound to the head
//!
//! Marking the pull request ready asks people to review it, and moving the issue to the mapped
//! "needs review" status tells the team it is; both are yours to say. One approval covers the two
//! writes, in that order ([`ReadyRequest`]), and pins the head you saw: the executor reads the pull
//! request again first, and a head that moved stops it. Each write is its own operation, so a
//! partial success is exactly that: the pull request ready and the issue not moved is Retry of the
//! move alone, never a second pull request.
//!
//! # Merged only from the sheet
//!
//! A merge is an approval of exactly one head, by one method, and only the Merge sheet asks for it.
//! The forge is told the head too (`gh pr merge --match-head-commit`), so a push that lands between
//! your click and the merge is refused by GitHub itself rather than merged unseen. No agent tool
//! reaches any of this.
//!
//! # What the remote holds that wtm did not put there
//!
//! After a writer's round, `git ls-remote` reads the task branch and the integration branches, and
//! [`unexplained`] compares them with what the outbox recorded: the task branch at a commit this
//! worktree does not have, or gone after wtm pushed it, and an integration branch at one of this
//! run's own commits, which nothing but a merge from the sheet should ever put there.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::UnixMs;
use super::approval::{Binding, Pinned};
use super::forge::{ChecksSummary, RepoSlug};
use super::moves::MoveIssuePayload;
use super::outbox::{NewOperation, OperationPayload};
use super::publish::{RemoteComment, RemoteReview, RemoteReviewState, hash_of, target};
use super::route::{Destination, Expect, IntegrationBranches, PushPayload, Refspec, RouteError};
use super::state::FlowId;

/// The draft of an issue's pull request at one hand-over: its task branch's commit, pushed to the
/// project's remote, and the draft that shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftPlan {
    pub key: String,
    /// The project's remote and `refs/heads/<branch>` on it.
    pub destination: Destination,
    pub branch: String,
    /// The branch the pull request merges into: the one the task was started from.
    pub base: String,
    /// The task branch's `HEAD` at the hand-over: what is pushed.
    pub commit: String,
    /// Where the remote branch is before the push: nowhere for a branch never pushed, otherwise where
    /// wtm, or you from this worktree, last left it.
    pub expect: Expect,
    /// The hash of `git diff --binary <base> <commit>`: what the draft shows.
    pub diff_hash: String,
    pub title: String,
    /// Without its marker, which the executor signs it with.
    pub body: String,
    /// The pull request already open from the branch, which the draft updates instead.
    pub pull_request: Option<u64>,
}

/// The pieces a [`DraftPlan`] is made of, as the app reads them at a hand-over.
#[derive(Debug, Clone)]
pub struct DraftFacts<'a> {
    pub key: &'a str,
    pub destination: Destination,
    pub branch: &'a str,
    pub base: &'a str,
    pub commit: &'a str,
    pub expect: Expect,
    pub diff_hash: &'a str,
    pub title: &'a str,
    pub body: &'a str,
    pub pull_request: Option<u64>,
}

impl DraftPlan {
    /// The draft `facts` describe, if the app may push it.
    ///
    /// # Errors
    ///
    /// When the task branch is an integration branch, or is not a branch a push may name, or the
    /// commit is not a full commit id. Nothing is drafted then.
    pub fn new(
        facts: DraftFacts<'_>,
        integration: &IntegrationBranches,
    ) -> Result<Self, RouteError> {
        if integration.covers(facts.branch) {
            return Err(RouteError(format!(
                "`{}` is an integration branch, and wtm never pushes a task's work there: the \
                 issue's worktree should be on a branch of its own",
                facts.branch
            )));
        }
        let refspec = Refspec::new(facts.commit, facts.branch, integration)?;
        if facts.destination.reference != refspec.dst {
            return Err(RouteError(format!(
                "the draft would push to `{}`, which is not the task branch `{}`",
                facts.destination.reference, refspec.dst
            )));
        }
        Ok(Self {
            key: facts.key.to_owned(),
            destination: facts.destination,
            branch: facts.branch.to_owned(),
            base: facts.base.to_owned(),
            commit: facts.commit.to_owned(),
            expect: facts.expect,
            diff_hash: facts.diff_hash.to_owned(),
            title: facts.title.to_owned(),
            body: facts.body.to_owned(),
            pull_request: facts.pull_request,
        })
    }

    #[must_use]
    pub fn hash(&self) -> String {
        hash_of(self)
    }

    /// What the app's approval of the draft pins: the remote, the ref, the diff and the commit.
    #[must_use]
    pub fn binding(&self) -> Binding {
        Binding::default()
            .with(Pinned::Remote, self.destination.url.clone())
            .with(Pinned::Ref, self.destination.reference.clone())
            .with(Pinned::Diff, self.diff_hash.clone())
            .with(Pinned::Head, self.commit.clone())
    }

    /// The push, then the draft: created when the branch has none, updated when it has one.
    #[must_use]
    pub fn operations(&self, flow: FlowId) -> Vec<NewOperation> {
        let draft = self.hash();
        let push = OperationPayload::PushBranch(PushPayload {
            route: draft.clone(),
            destination: self.destination.clone(),
            commit: self.commit.clone(),
            expect: self.expect.clone(),
        });
        let pull = match self.pull_request {
            None => OperationPayload::CreateDraftPullRequest(DraftPullRequestPayload {
                draft,
                repo: self.destination.repo.clone(),
                base: self.base.clone(),
                head: self.branch.clone(),
                title: self.title.clone(),
                body: self.body.clone(),
            }),
            Some(number) => OperationPayload::UpdatePullRequest(UpdatePullRequestPayload {
                draft,
                repo: self.destination.repo.clone(),
                number,
                title: self.title.clone(),
                body: self.body.clone(),
            }),
        };
        vec![
            NewOperation::new(flow, 0, push),
            NewOperation::new(flow, 1, pull),
        ]
    }
}

/// Opening the issue's pull request as a draft.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftPullRequestPayload {
    /// [`DraftPlan::hash`], so two hand-overs never share a write.
    pub draft: String,
    pub repo: RepoSlug,
    pub base: String,
    /// The task branch, in `repo` itself.
    pub head: String,
    pub title: String,
    pub body: String,
}

impl DraftPullRequestPayload {
    #[must_use]
    pub fn target(&self) -> String {
        format!("{} {} <- {}", self.repo.full_name(), self.base, self.head)
    }
}

/// The issue's pull request's title and description, as the latest hand-over says them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdatePullRequestPayload {
    pub draft: String,
    pub repo: RepoSlug,
    pub number: u64,
    pub title: String,
    pub body: String,
}

/// Marking the pull request ready for review, at exactly this head.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkReadyPayload {
    pub repo: RepoSlug,
    pub number: u64,
    pub head: String,
}

/// How a pull request is merged. The sheet's choice; squash unless you pick another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MergeMethod {
    #[default]
    Squash,
    Merge,
    Rebase,
}

impl MergeMethod {
    /// The flag `gh pr merge` takes for it.
    #[must_use]
    pub fn flag(self) -> &'static str {
        match self {
            Self::Squash => "--squash",
            Self::Merge => "--merge",
            Self::Rebase => "--rebase",
        }
    }
}

/// Merging the pull request, at exactly this head, by this method.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergePayload {
    pub repo: RepoSlug,
    pub number: u64,
    pub head: String,
    pub method: MergeMethod,
}

/// "Mark ready + Needs Review" as its approval keeps it: the pull request at the head you saw, and
/// the issue's move to the mapped status, when it is not there already.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadyRequest {
    pub key: String,
    pub ready: MarkReadyPayload,
    pub to_review: MoveIssuePayload,
}

impl ReadyRequest {
    /// The one pin a publish needs: the head. The move is checked against the tracker itself.
    #[must_use]
    pub fn binding(&self) -> Binding {
        Binding::default().with(Pinned::Head, self.ready.head.clone())
    }

    /// The pull request ready, then the issue moved, in that order.
    #[must_use]
    pub fn operations(&self, flow: FlowId) -> Vec<NewOperation> {
        vec![
            NewOperation::new(flow, 0, OperationPayload::MarkReady(self.ready.clone())),
            NewOperation::new(flow, 1, OperationPayload::MoveIssue(self.to_review.clone())),
        ]
    }
}

/// A merge as its approval keeps it: exactly what the sheet showed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeRequest {
    pub key: String,
    pub merge: MergePayload,
    /// The task branch, and the branch it merges into.
    pub branch: String,
    pub base: String,
    /// The forge's checks at that head, as the sheet showed them.
    pub checks: ChecksSummary,
}

impl MergeRequest {
    #[must_use]
    pub fn binding(&self) -> Binding {
        Binding::default().with(Pinned::Head, self.merge.head.clone())
    }

    #[must_use]
    pub fn operations(&self, flow: FlowId) -> Vec<NewOperation> {
        vec![NewOperation::new(
            flow,
            0,
            OperationPayload::Merge(self.merge.clone()),
        )]
    }
}

/// Where a write to a pull request is aimed, as an operation's target.
#[must_use]
pub fn pull_target(repo: &RepoSlug, number: u64) -> String {
    target(repo, number)
}

/// A review someone left on the issue's pull request, kept as wtm first read it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrFeedback {
    pub flow: FlowId,
    /// The forge's id for the review.
    pub review: u64,
    pub author: Option<String>,
    pub state: RemoteReviewState,
    pub body: String,
    pub url: String,
    /// The head it was left on.
    pub commit_id: Option<String>,
    /// Its inline comments.
    pub comments: Vec<FeedbackComment>,
    pub seen_at: UnixMs,
    /// The round of the revision that took it in.
    pub revised_in: Option<u32>,
}

/// One inline comment of a review.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FeedbackComment {
    pub path: String,
    pub body: String,
}

impl FeedbackComment {
    #[must_use]
    pub fn of(comment: &RemoteComment) -> Self {
        Self {
            path: comment.path.clone(),
            body: comment.body.clone(),
        }
    }
}

/// Whether a review on your own pull request is feedback to act on: someone else's, asking for
/// changes, or commenting with something to say. An approval is news the tracking shows, not work.
#[must_use]
pub fn is_feedback(review: &RemoteReview, me: Option<&str>, comments: usize) -> bool {
    let theirs = match (&review.author, me) {
        (Some(author), Some(me)) => !author.eq_ignore_ascii_case(me),
        _ => true,
    };
    theirs
        && match review.state {
            RemoteReviewState::ChangesRequested => true,
            RemoteReviewState::Commented => !review.body.trim().is_empty() || comments > 0,
            _ => false,
        }
}

/// The note a revision started from feedback carries: each review not yet taken into a revision,
/// in the reviewer's words, with its inline comments where they sit.
#[must_use]
pub fn feedback_note(feedback: &[PrFeedback]) -> String {
    use std::fmt::Write as _;
    let mut out = String::from("The pull request's reviewers asked for this:\n");
    for review in feedback.iter().filter(|f| f.revised_in.is_none()) {
        let who = review.author.as_deref().unwrap_or("someone");
        let asked = match review.state {
            RemoteReviewState::ChangesRequested => "asked for changes",
            _ => "commented",
        };
        let _ = writeln!(out, "\n{who} {asked} ({}):", review.url);
        if !review.body.trim().is_empty() {
            let _ = writeln!(out, "{}", review.body.trim());
        }
        for comment in &review.comments {
            let _ = writeln!(out, "- `{}`: {}", comment.path, comment.body.trim());
        }
    }
    out
}

/// Something on the remote that wtm did not put there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Unexplained {
    /// The task branch is at a commit this worktree does not have, or was deleted after wtm pushed
    /// it. Pushing now could overwrite someone's work, so the next draft stops instead.
    #[serde(rename_all = "camelCase")]
    TaskBranch {
        branch: String,
        pushed: Option<String>,
        now: Option<String>,
    },
    /// An integration branch is at one of this run's own commits, which only a merge from the sheet
    /// should put there.
    #[serde(rename_all = "camelCase")]
    Integration { branch: String, commit: String },
}

/// The task branch on the remote, as `git ls-remote` read it after a writer's round.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRemote<'a> {
    pub branch: &'a str,
    /// What the outbox last pushed there.
    pub pushed: Option<&'a str>,
    /// What the remote holds now.
    pub now: Option<&'a str>,
    /// Whether `now` is a commit this worktree's history holds: pushed from here, by wtm or by you.
    pub now_is_ours: bool,
}

/// Compare what `git ls-remote` read with what the outbox recorded. `ours` is this run's commits;
/// `integration` each integration branch with where it is now.
#[must_use]
pub fn unexplained(
    task: &TaskRemote<'_>,
    ours: &BTreeSet<String>,
    integration: &[(String, Option<String>)],
) -> Vec<Unexplained> {
    let mut found = Vec::new();
    let moved = match (task.pushed, task.now) {
        // Gone after wtm pushed it.
        (Some(_), None) => true,
        // Somewhere this worktree has never been: someone else pushed to it.
        (_, Some(now)) => Some(now) != task.pushed && !task.now_is_ours,
        (None, None) => false,
    };
    if moved {
        found.push(Unexplained::TaskBranch {
            branch: task.branch.to_owned(),
            pushed: task.pushed.map(str::to_owned),
            now: task.now.map(str::to_owned),
        });
    }
    for (branch, now) in integration {
        if let Some(now) = now
            && ours.contains(now)
        {
            found.push(Unexplained::Integration {
                branch: branch.clone(),
                commit: now.clone(),
            });
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::TrackerKind;
    use crate::flow::publish::RemoteReview;

    const COMMIT: &str = "3f2a9c0b1d2e3f4a5b6c7d8e9f0a1b2c3d4e5f6a";

    fn destination(branch: &str) -> Destination {
        Destination {
            remote: Some("origin".to_owned()),
            url: "git@github.com:acme/units.git".to_owned(),
            repo: RepoSlug::new("github.com", "acme", "units"),
            reference: format!("refs/heads/{branch}"),
        }
    }

    fn facts(branch: &'static str, pull_request: Option<u64>) -> DraftFacts<'static> {
        DraftFacts {
            key: "ACME-9",
            destination: destination(branch),
            branch,
            base: "develop",
            commit: COMMIT,
            expect: Expect::Absent,
            diff_hash: "sha256:abc",
            title: "ACME-9: Export the unit list",
            body: "What was done",
            pull_request,
        }
    }

    fn integration() -> IntegrationBranches {
        IntegrationBranches::new(&["develop".to_owned(), "release/*".to_owned()], "develop")
    }

    #[test]
    fn a_draft_pushes_exactly_its_commit_to_the_task_branch_then_opens_the_draft() {
        let plan = DraftPlan::new(facts("feature/ACME-9", None), &integration()).unwrap();
        let ops = plan.operations(FlowId(4));
        let OperationPayload::PushBranch(push) = &ops[0].payload else {
            panic!("the push comes first: {ops:?}");
        };
        assert_eq!(
            push.refspec(),
            format!("{COMMIT}:refs/heads/feature/ACME-9"),
            "a full commit to a full ref, never a pattern and never forced"
        );
        assert!(!push.refspec().starts_with('+'));
        assert!(matches!(
            &ops[1].payload,
            OperationPayload::CreateDraftPullRequest(p)
                if p.base == "develop" && p.head == "feature/ACME-9"
        ));
        assert_eq!(
            plan.binding().0.get(&Pinned::Head).map(String::as_str),
            Some(COMMIT)
        );
    }

    #[test]
    fn a_task_branch_with_a_pull_request_updates_it_rather_than_opening_another() {
        let plan = DraftPlan::new(facts("feature/ACME-9", Some(40)), &integration()).unwrap();
        assert!(matches!(
            &plan.operations(FlowId(4))[1].payload,
            OperationPayload::UpdatePullRequest(p) if p.number == 40
        ));
    }

    #[test]
    fn an_integration_branch_is_never_drafted_from_whatever_the_worktree_is_on() {
        for branch in ["develop", "main", "master", "release/2.4"] {
            let mut asked = facts("x", None);
            asked.branch = branch;
            asked.destination = destination(branch);
            assert!(
                DraftPlan::new(asked, &integration()).is_err(),
                "{branch} was drafted"
            );
        }
    }

    #[test]
    fn ready_and_needs_review_are_one_approval_of_two_writes_bound_to_the_head() {
        let request = ReadyRequest {
            key: "ACME-9".to_owned(),
            ready: MarkReadyPayload {
                repo: RepoSlug::new("github.com", "acme", "units"),
                number: 40,
                head: COMMIT.to_owned(),
            },
            to_review: MoveIssuePayload {
                tracker: TrackerKind::Jira,
                key: "ACME-9".to_owned(),
                from: "In Progress".to_owned(),
                to: "Needs Reviewed".to_owned(),
            },
        };
        let ops = request.operations(FlowId(4));
        assert!(matches!(ops[0].payload, OperationPayload::MarkReady(_)));
        assert!(matches!(ops[1].payload, OperationPayload::MoveIssue(_)));
        assert_eq!(
            request.binding(),
            Binding::default().with(Pinned::Head, COMMIT)
        );
    }

    #[test]
    fn feedback_is_someone_elses_review_asking_for_changes_or_saying_something() {
        let review = |author: &str, state, body: &str| RemoteReview {
            id: 1,
            node_id: "PRR_1".to_owned(),
            author: Some(author.to_owned()),
            state,
            body: body.to_owned(),
            commit_id: None,
            url: "https://github.com/acme/units/pull/40#pullrequestreview-1".to_owned(),
        };
        let me = Some("takumi");
        assert!(is_feedback(
            &review("sam", RemoteReviewState::ChangesRequested, ""),
            me,
            0
        ));
        assert!(is_feedback(
            &review("sam", RemoteReviewState::Commented, ""),
            me,
            2
        ));
        assert!(!is_feedback(
            &review("sam", RemoteReviewState::Commented, " "),
            me,
            0
        ));
        assert!(!is_feedback(
            &review("sam", RemoteReviewState::Approved, "LGTM"),
            me,
            0
        ));
        assert!(
            !is_feedback(
                &review("Takumi", RemoteReviewState::ChangesRequested, "x"),
                me,
                0
            ),
            "your own review is not feedback to you"
        );
    }

    #[test]
    fn a_revision_from_feedback_carries_only_what_no_revision_took_in_yet() {
        let feedback = |review, body: &str, revised_in| PrFeedback {
            flow: FlowId(4),
            review,
            author: Some("sam".to_owned()),
            state: RemoteReviewState::ChangesRequested,
            body: body.to_owned(),
            url: format!("https://github.com/acme/units/pull/40#pullrequestreview-{review}"),
            commit_id: None,
            comments: vec![FeedbackComment {
                path: "src/export.rs".to_owned(),
                body: format!("comment on {review}"),
            }],
            seen_at: 0,
            revised_in,
        };
        let note = feedback_note(&[
            feedback(1, "Sort them by floor", Some(5)),
            feedback(2, "Name the file after the building", None),
        ]);
        assert!(note.contains("sam asked for changes"));
        assert!(note.contains("Name the file after the building"));
        assert!(note.contains("- `src/export.rs`: comment on 2"));
        assert!(!note.contains("Sort them by floor"), "{note}");
    }

    #[test]
    fn the_remote_task_branch_somewhere_this_worktree_never_was_is_unexplained() {
        let ours: BTreeSet<String> = [COMMIT.to_owned()].into();
        let task = |pushed, now, now_is_ours| TaskRemote {
            branch: "feature/ACME-9",
            pushed,
            now,
            now_is_ours,
        };
        // Where wtm pushed it, or a commit of ours you pushed by hand: nothing to say.
        assert!(unexplained(&task(Some("a"), Some("a"), true), &ours, &[]).is_empty());
        assert!(unexplained(&task(Some("a"), Some("b"), true), &ours, &[]).is_empty());
        assert!(unexplained(&task(None, None, false), &ours, &[]).is_empty());
        // Someone else's commit on it, or the branch gone after the push.
        assert_eq!(
            unexplained(&task(Some("a"), Some("z"), false), &ours, &[]).len(),
            1
        );
        assert_eq!(
            unexplained(&task(Some("a"), None, false), &ours, &[]).len(),
            1
        );
        assert_eq!(
            unexplained(&task(None, Some("z"), false), &ours, &[]).len(),
            1
        );
    }

    #[test]
    fn an_integration_branch_at_one_of_the_runs_own_commits_is_unexplained() {
        let ours: BTreeSet<String> = [COMMIT.to_owned()].into();
        let quiet = TaskRemote {
            branch: "feature/ACME-9",
            pushed: None,
            now: None,
            now_is_ours: false,
        };
        let found = unexplained(
            &quiet,
            &ours,
            &[
                ("develop".to_owned(), Some("0000".to_owned())),
                ("main".to_owned(), Some(COMMIT.to_owned())),
            ],
        );
        assert_eq!(
            found,
            vec![Unexplained::Integration {
                branch: "main".to_owned(),
                commit: COMMIT.to_owned(),
            }]
        );
    }
}
