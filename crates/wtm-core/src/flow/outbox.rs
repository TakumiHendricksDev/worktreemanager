//! The outbox: every write a flow makes outside this machine.
//!
//! # Why writes are queued rather than made
//!
//! Pushing a branch, opening a pull request, publishing a review and moving an issue each change
//! something other people see, and the app can die between deciding to do one and learning whether
//! it happened. So each is an [`Operation`] recorded first and executed second, keyed by
//! [`OperationKey`] so the same write is never queued twice: approving the same text again finds the
//! operation already queued, or already done.
//!
//! # The remote is read before every send
//!
//! Before an operation is sent, the executor reads what the remote holds and looks for the
//! operation's own result there (for a review, by its [marker](super::publish::Marker)). A result
//! already there is adopted and the operation is done without sending anything; that is why a
//! pending or failed operation may move straight to `Done`. Only when the read shows the write did
//! not happen is it marked `Executing` — durably, before the request leaves — and sent. A read that
//! fails before then leaves it `Failed`: nothing was sent, and it can be tried again.
//!
//! # Why a crash is not a retry
//!
//! An operation found `Executing` at launch may or may not have reached the remote. Sending it again
//! could post a second review or open a second pull request. So `Executing` cannot move to
//! `Executing`: recovery must first read the remote and settle the operation as `Done` (the result
//! exists, adopt it) or `Failed` (it does not), and only a failed operation is sent again, when you
//! say so.
//!
//! # A compound action is a sequence
//!
//! One approval can cover several operations — a review's comments and then its submission — run in
//! [`Operation::position`] order, each waiting for the one before. A partial success is then exactly
//! visible: these are done, this one failed or stopped, these were never tried.

use serde::{Deserialize, Serialize};

use super::UnixMs;
use super::approval::{Approval, ApprovalId};
use super::publish::{PendingReviewPayload, ReviewPayload, SubmitReviewPayload, hash_of, target};
use super::state::FlowId;

id! {
    /// One operation, as the store numbers it.
    OperationId
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    PushBranch,
    CreateDraftPullRequest,
    UpdatePullRequest,
    MarkReady,
    /// A review's inline comments, on a pending review only you can see.
    PendingReview,
    /// A pending review submitted with its summary and state, which is when the author sees it.
    SubmitReview,
    PostComment,
    MoveIssue,
    Merge,
}

impl OperationKind {
    /// The kind as the store and a marker spell it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PushBranch => "push_branch",
            Self::CreateDraftPullRequest => "create_draft_pull_request",
            Self::UpdatePullRequest => "update_pull_request",
            Self::MarkReady => "mark_ready",
            Self::PendingReview => "pending_review",
            Self::SubmitReview => "submit_review",
            Self::PostComment => "post_comment",
            Self::MoveIssue => "move_issue",
            Self::Merge => "merge",
        }
    }
}

/// What makes two operations the same write.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationKey {
    pub flow: FlowId,
    pub kind: OperationKind,
    /// What it writes to, such as `owner/name#40` or a branch.
    pub target: String,
    /// The hash of exactly what it sends.
    pub payload_hash: String,
}

/// Exactly what an operation sends, apart from its marker. Its hash is the key's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OperationPayload {
    PendingReview(PendingReviewPayload),
    SubmitReview(SubmitReviewPayload),
}

impl OperationPayload {
    #[must_use]
    pub fn kind(&self) -> OperationKind {
        match self {
            Self::PendingReview(_) => OperationKind::PendingReview,
            Self::SubmitReview(_) => OperationKind::SubmitReview,
        }
    }

    #[must_use]
    pub fn target(&self) -> String {
        match self {
            Self::PendingReview(p) => target(&p.repo, p.number),
            Self::SubmitReview(p) => target(&p.repo, p.number),
        }
    }

    #[must_use]
    pub fn hash(&self) -> String {
        hash_of(self)
    }
}

/// An operation to queue: its key, made from its payload, and its place in its approval's sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewOperation {
    pub key: OperationKey,
    pub position: u32,
    pub payload: OperationPayload,
}

impl NewOperation {
    #[must_use]
    pub fn new(flow: FlowId, position: u32, payload: OperationPayload) -> Self {
        Self {
            key: OperationKey {
                flow,
                kind: payload.kind(),
                target: payload.target(),
                payload_hash: payload.hash(),
            },
            position,
            payload,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    Pending,
    Executing,
    Done,
    /// Did not happen, and may be sent again.
    Failed,
    /// Stopped because the remote was not as expected. You decide what happens next.
    NeedsAttention,
    /// You stopped it, or a later publish replaced it. Approving it again queues it again.
    Cancelled,
}

impl OperationState {
    /// Whether an operation in this state may move to `next`.
    #[must_use]
    pub fn can_move_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (
                Self::Pending | Self::Failed,
                Self::Executing
                    | Self::Done
                    | Self::Failed
                    | Self::NeedsAttention
                    | Self::Cancelled
            ) | (
                Self::Executing,
                Self::Done | Self::Failed | Self::NeedsAttention
            ) | (Self::NeedsAttention, Self::Pending | Self::Cancelled)
                | (Self::Cancelled, Self::Pending)
        )
    }

    pub fn move_to(self, next: Self) -> Result<Self, OutboxError> {
        if self.can_move_to(next) {
            Ok(next)
        } else {
            Err(OutboxError::NoSuchStep {
                from: self,
                to: next,
            })
        }
    }

    /// Nothing more will happen to it unless you act.
    #[must_use]
    pub fn is_settled(self) -> bool {
        matches!(self, Self::Done | Self::Cancelled)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OutboxError {
    #[error("an operation cannot move from {from:?} to {to:?}")]
    NoSuchStep {
        from: OperationState,
        to: OperationState,
    },
}

/// What an operation left on the remote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OperationResult {
    #[serde(rename_all = "camelCase")]
    Review {
        review: u64,
        node_id: String,
        url: String,
        /// Found on the remote by its marker rather than sent this time: it had happened already.
        adopted: bool,
    },
}

impl OperationResult {
    #[must_use]
    pub fn review(&self) -> (u64, &str) {
        match self {
            Self::Review {
                review, node_id, ..
            } => (*review, node_id),
        }
    }
}

/// Why an operation stopped and waits for you.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Attention {
    /// The pull request moved past the head the approval covered. Nothing more is sent without a
    /// new approval.
    HeadMoved { approved: String, current: String },
    /// A pending review of yours is on the pull request, and the forge allows one per person.
    #[serde(rename_all = "camelCase")]
    PendingReview {
        review: u64,
        node_id: String,
        url: String,
        /// How many comments it holds.
        comments: u32,
        /// It carries a marker: an earlier publish from wtm started it.
        started_by_wtm: bool,
    },
    /// The forge refused the write as it stands, so sending it again would be refused again.
    Refused { message: String },
}

/// How you settled what stopped an operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Resolution {
    /// Put this operation's comments on your pending review rather than starting one.
    #[serde(rename_all = "camelCase")]
    AddToPendingReview { review: u64, node_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Operation {
    pub id: OperationId,
    pub key: OperationKey,
    /// The approval it runs under now: the latest that covered exactly this payload.
    pub approval: ApprovalId,
    /// Its place in that approval's sequence.
    pub position: u32,
    pub payload: OperationPayload,
    pub state: OperationState,
    /// How many times it was sent.
    pub attempts: u32,
    pub last_error: Option<String>,
    pub result: Option<OperationResult>,
    pub attention: Option<Attention>,
    pub resolution: Option<Resolution>,
    pub created_at: UnixMs,
    pub updated_at: UnixMs,
}

/// A change to an operation's record, with the state it moves to. Checked against
/// [`OperationState::can_move_to`] by the store before anything is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationChange {
    pub state: OperationState,
    pub error: super::run::Field<String>,
    pub result: super::run::Field<OperationResult>,
    pub attention: super::run::Field<Attention>,
    pub resolution: super::run::Field<Resolution>,
    /// The operation is being sent: count the attempt.
    pub attempt: bool,
}

impl OperationChange {
    /// To `state`, leaving everything else as it is.
    #[must_use]
    pub fn to(state: OperationState) -> Self {
        Self {
            state,
            error: super::run::Field::Keep,
            result: super::run::Field::Keep,
            attention: super::run::Field::Keep,
            resolution: super::run::Field::Keep,
            attempt: false,
        }
    }

    /// Sent now: executing, one more attempt, the last error and what stopped it cleared.
    #[must_use]
    pub fn sending() -> Self {
        Self {
            error: super::run::Field::Clear,
            attention: super::run::Field::Clear,
            attempt: true,
            ..Self::to(OperationState::Executing)
        }
    }

    /// Done, with what it left.
    #[must_use]
    pub fn done(result: OperationResult) -> Self {
        Self {
            result: super::run::Field::Set(result),
            error: super::run::Field::Clear,
            attention: super::run::Field::Clear,
            ..Self::to(OperationState::Done)
        }
    }

    #[must_use]
    pub fn failed(error: impl Into<String>) -> Self {
        Self {
            error: super::run::Field::Set(error.into()),
            ..Self::to(OperationState::Failed)
        }
    }

    #[must_use]
    pub fn stopped(attention: Attention) -> Self {
        Self {
            attention: super::run::Field::Set(attention),
            ..Self::to(OperationState::NeedsAttention)
        }
    }
}

/// What this process is doing with a publication right now. Not stored: a crash forgets it, which
/// is the point — an operation left `Executing` by a process that is gone is one to check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Activity {
    /// Sending its operations.
    Posting,
    /// Reading the remote to settle an operation a crash left mid-write.
    Checking,
}

/// A publication's state as the window shows it, from its operations and what this process is
/// doing with them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicationState {
    /// Being sent now.
    Posting,
    /// An operation was mid-write when wtm stopped, and the remote is read, or about to be, to
    /// learn whether it happened.
    Checking,
    /// Every operation is done.
    Published,
    /// The pull request moved past the approved head. A new approval is needed.
    Stale,
    /// Stopped on something only you can decide: a pending review in the way, or a refusal.
    NeedsYou,
    /// An operation failed and can be sent again.
    Failed,
    /// Queued and not running: the next operation waits for Retry, as after a crash.
    Waiting,
    /// You stopped it, or a later publish replaced it.
    Cancelled,
}

/// One approval of a review, with exactly what it covered and the operations that post it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Publication {
    pub approval: Approval,
    pub review: ReviewPayload,
    /// In order.
    pub operations: Vec<Operation>,
}

impl Publication {
    /// The state to show, given what this process is doing with it.
    #[must_use]
    pub fn state(&self, activity: Option<Activity>) -> PublicationState {
        let ops = &self.operations;
        let any = |state: OperationState| ops.iter().any(|op| op.state == state);
        if !ops.is_empty() && ops.iter().all(|op| op.state == OperationState::Done) {
            return PublicationState::Published;
        }
        if any(OperationState::Executing) {
            return match activity {
                Some(Activity::Posting) => PublicationState::Posting,
                _ => PublicationState::Checking,
            };
        }
        if activity == Some(Activity::Posting) {
            return PublicationState::Posting;
        }
        let stopped = ops
            .iter()
            .find(|op| op.state == OperationState::NeedsAttention);
        if let Some(op) = stopped {
            return match op.attention {
                Some(Attention::HeadMoved { .. }) => PublicationState::Stale,
                _ => PublicationState::NeedsYou,
            };
        }
        if any(OperationState::Failed) {
            PublicationState::Failed
        } else if any(OperationState::Cancelled) {
            PublicationState::Cancelled
        } else {
            PublicationState::Waiting
        }
    }

    /// The operation to run next: the first that is not done.
    #[must_use]
    pub fn next(&self) -> Option<&Operation> {
        self.operations
            .iter()
            .find(|op| op.state != OperationState::Done)
    }

    #[must_use]
    pub fn done(&self) -> usize {
        self.operations
            .iter()
            .filter(|op| op.state == OperationState::Done)
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::approval::Binding;
    use crate::flow::forge::RepoSlug;
    use crate::flow::publish::ReviewEvent;

    fn through(states: &[OperationState]) -> Result<OperationState, OutboxError> {
        states[1..]
            .iter()
            .try_fold(states[0], |state, next| state.move_to(*next))
    }

    #[test]
    fn an_operation_runs_from_pending_through_executing_to_done() {
        assert_eq!(
            through(&[
                OperationState::Pending,
                OperationState::Executing,
                OperationState::Done
            ]),
            Ok(OperationState::Done)
        );
    }

    #[test]
    fn a_failed_operation_can_be_retried() {
        assert_eq!(
            OperationState::Failed.move_to(OperationState::Executing),
            Ok(OperationState::Executing)
        );
    }

    #[test]
    fn a_result_found_on_the_remote_before_sending_settles_the_operation_without_sending_it() {
        assert!(OperationState::Pending.can_move_to(OperationState::Done));
        assert!(OperationState::Failed.can_move_to(OperationState::Done));
        assert!(OperationState::Pending.can_move_to(OperationState::NeedsAttention));
    }

    #[test]
    fn a_read_that_fails_before_anything_is_sent_leaves_the_operation_failed_and_unsent() {
        // Failing from pending is "nothing was sent, and it can be tried again", which is what
        // failed already means; it never passes through executing.
        assert!(OperationState::Pending.can_move_to(OperationState::Failed));
        assert!(OperationState::Failed.can_move_to(OperationState::Failed));
    }

    #[test]
    fn an_operation_left_executing_by_a_crash_is_settled_before_it_can_run_again() {
        assert_eq!(
            OperationState::Executing.move_to(OperationState::Executing),
            Err(OutboxError::NoSuchStep {
                from: OperationState::Executing,
                to: OperationState::Executing,
            })
        );
        assert!(OperationState::Executing.can_move_to(OperationState::Done));
        assert!(OperationState::Executing.can_move_to(OperationState::Failed));
        assert!(!OperationState::Executing.can_move_to(OperationState::Cancelled));
    }

    #[test]
    fn a_done_operation_never_runs_again() {
        for next in [
            OperationState::Pending,
            OperationState::Executing,
            OperationState::Failed,
            OperationState::NeedsAttention,
            OperationState::Cancelled,
        ] {
            assert!(!OperationState::Done.can_move_to(next), "{next:?}");
        }
    }

    #[test]
    fn an_operation_that_needs_attention_waits_for_you_before_it_runs_again() {
        assert!(!OperationState::NeedsAttention.can_move_to(OperationState::Executing));
        assert_eq!(
            through(&[
                OperationState::NeedsAttention,
                OperationState::Pending,
                OperationState::Executing
            ]),
            Ok(OperationState::Executing)
        );
    }

    #[test]
    fn a_cancelled_operation_runs_again_only_once_it_is_approved_again() {
        assert!(!OperationState::Cancelled.can_move_to(OperationState::Executing));
        assert_eq!(
            through(&[
                OperationState::Cancelled,
                OperationState::Pending,
                OperationState::Executing
            ]),
            Ok(OperationState::Executing)
        );
    }

    fn publication(states: &[OperationState]) -> Publication {
        let review = ReviewPayload {
            repo: RepoSlug::new("github.com", "acme", "webapp"),
            number: 4031,
            commit_id: "7c9e1d".to_owned(),
            event: ReviewEvent::Comment,
            comments: Vec::new(),
            folded: Vec::new(),
            summary: "s".to_owned(),
        };
        let operations = review
            .operations(FlowId(1))
            .into_iter()
            .zip(states)
            .enumerate()
            .map(|(index, (new, state))| Operation {
                id: OperationId(i64::try_from(index).unwrap()),
                key: new.key,
                approval: ApprovalId(1),
                position: new.position,
                payload: new.payload,
                state: *state,
                attempts: 0,
                last_error: None,
                result: None,
                attention: (*state == OperationState::NeedsAttention).then(|| Attention::Refused {
                    message: "no".to_owned(),
                }),
                resolution: None,
                created_at: 0,
                updated_at: 0,
            })
            .collect();
        Publication {
            approval: Approval {
                id: ApprovalId(1),
                flow: FlowId(1),
                intent: crate::flow::approval::ApprovalIntent::Post,
                binding: Binding::default(),
                granted_at: 0,
            },
            review,
            operations,
        }
    }

    #[test]
    fn a_publication_reads_as_what_its_operations_and_this_process_say() {
        use OperationState::{Cancelled, Done, Executing, Failed, NeedsAttention, Pending};
        assert_eq!(
            publication(&[Done, Done]).state(None),
            PublicationState::Published
        );
        assert_eq!(
            publication(&[Done, Executing]).state(Some(Activity::Posting)),
            PublicationState::Posting
        );
        assert_eq!(
            publication(&[Executing, Pending]).state(None),
            PublicationState::Checking,
            "left mid-write by a process that is gone"
        );
        assert_eq!(
            publication(&[Done, Failed]).state(None),
            PublicationState::Failed
        );
        assert_eq!(
            publication(&[Done, Pending]).state(None),
            PublicationState::Waiting
        );
        assert_eq!(
            publication(&[NeedsAttention, Pending]).state(None),
            PublicationState::NeedsYou
        );
        assert_eq!(
            publication(&[Done, Cancelled]).state(None),
            PublicationState::Cancelled
        );
        let next = publication(&[Done, Failed]);
        assert_eq!(next.next().map(|op| op.position), Some(1));
        assert_eq!(next.done(), 1);
    }
}
