//! Durable storage for work items and flows.
//!
//! # The store enforces uniqueness, not the caller
//!
//! "Is there already a work item for this issue?" followed by "then make one" is a race between
//! two clicks, or a click and a drag. So the rules that make starting work idempotent are the
//! store's: one work item per issue, one per pull request, and one open flow of each kind per work
//! item. Breaking one is a typed error naming the record that already exists, which is what lets a
//! repeated Start focus the existing flow instead of reporting a failure.
//!
//! # Every flow change goes through the rules
//!
//! [`FlowStore::record`] applies the event through
//! [`FlowState::apply`](crate::flow::FlowState::apply) and writes the event and the new state
//! together, or neither. A store cannot be asked to set a phase directly.
//!
//! # Time comes from the caller
//!
//! Every write takes the instant it happened at, from the [`Clock`](super::Clock) port, so a store
//! never reads the clock itself and a test can pin every timestamp.

use serde::Serialize;

use crate::flow::{
    Answer, InboxItem, InboxItemId, InboxKind, InboxTarget, NewInboxItem, NewQuestion, Question,
    QuestionError, QuestionId,
};
use crate::flow::{
    ApprovalId, Approved, NewApproval, NewOperation, Operation, OperationChange, OperationId,
    OutboxError, Publication, RouteSend,
};
use crate::flow::{CheckRun, FindingId, FindingRecord, NewCheckRun, Reconciled};
use crate::flow::{Fix, FixChange, FixId, Lease, LeaseId, LeaseRelease, NewFix, NewLease};
use crate::flow::{
    Flow, FlowEvent, FlowId, FlowKind, NewWorkItem, RecordedEvent, RecordedLink, TransitionError,
    UnixMs, WorkItem, WorkItemId, WorkKey, WorkLink,
};
use crate::flow::{
    Invocation, InvocationChange, InvocationId, NewInvocation, NewReviewRun, NewStep, ReviewRun,
    Snapshot, SnapshotId, Step, StepChange, StepId,
};
use crate::flow::{MirroredPr, NewOutcome, NewReviewedHead, Outcome, ReviewClaim, ReviewedHead};
use crate::model::ProjectId;

#[derive(Debug, thiserror::Error, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FlowStoreError {
    /// The issue or pull request already has a work item.
    #[error("work item {existing} already exists for that issue or pull request")]
    WorkItemExists { existing: WorkItemId },

    /// Another work item's current pull request is this one.
    #[error("that pull request already belongs to work item {owner}")]
    PullRequestTaken { owner: WorkItemId },

    /// A flow of that kind is still open for the work item.
    #[error("flow {existing} of that kind is still open for this work item")]
    FlowAlreadyOpen { existing: FlowId },

    #[error("no work item {0}")]
    UnknownWorkItem(WorkItemId),

    #[error("no flow {0}")]
    UnknownFlow(FlowId),

    #[error("no finding {0}")]
    UnknownFinding(crate::flow::FindingId),

    #[error("no question {0}")]
    UnknownQuestion(QuestionId),

    /// The question refused the answer: it was answered already, or the answer picks nothing.
    /// Nothing was written. A field rather than a newtype, so its own `kind` tag does not collide
    /// with this enum's on the wire.
    #[error("{reason}")]
    Unanswerable {
        #[from]
        reason: QuestionError,
    },

    /// The flow rules refused the event. Nothing was written.
    #[error(transparent)]
    Refused(#[from] TransitionError),

    #[error("no approval {0}")]
    UnknownApproval(ApprovalId),

    #[error("no operation {0}")]
    UnknownOperation(OperationId),

    /// The outbox's rules refused the change. Nothing was written. A field, for the reason
    /// [`Self::Unanswerable`] gives.
    #[error("{reason}")]
    Outbox {
        #[from]
        reason: OutboxError,
    },

    /// The worktree is leased, by a writer or, for a writer, by a review: the lease in the way.
    /// Nothing was written.
    #[error("the worktree is held by {} of run {}", .lease.holder, .lease.flow)]
    Leased { lease: Box<Lease> },

    #[error("no lease {0}")]
    UnknownLease(LeaseId),

    #[error("no fix {0}")]
    UnknownFix(FixId),

    /// A fix of the run is still being written.
    #[error("fix {existing} of this run is still being written")]
    FixWriting { existing: FixId },

    /// An approval would take over an operation that is being sent, or was being sent when wtm
    /// stopped: whether it reached the remote is not known yet. Nothing was written.
    #[error("operation {operation} is still being sent; wait for it to finish or be checked")]
    StillSending { operation: OperationId },

    /// The store itself failed. Carries the driver's message, which holds no secrets: nothing in
    /// this store is one.
    #[error("flow store: {0}")]
    Storage(String),
}

/// Work items and their flows.
pub trait FlowStore: Send + Sync {
    /// Create a work item. Refused with [`FlowStoreError::WorkItemExists`] when its issue or pull
    /// request already has one.
    fn create_work_item(&self, item: &NewWorkItem, at: UnixMs) -> Result<WorkItem, FlowStoreError>;

    fn work_item(&self, id: WorkItemId) -> Result<Option<WorkItem>, FlowStoreError>;

    /// The work item for an issue, or the one whose current pull request this is.
    fn find_work_item(&self, key: &WorkKey) -> Result<Option<WorkItem>, FlowStoreError>;

    /// A project's work items, oldest first, which is the order they were added to its queue.
    fn work_items(&self, project: &ProjectId) -> Result<Vec<WorkItem>, FlowStoreError>;

    /// Record something the work item used. Linking a pull request also makes it the item's
    /// current one, which is refused with [`FlowStoreError::PullRequestTaken`] when another item's
    /// current pull request is the same.
    fn link(&self, item: WorkItemId, link: &WorkLink, at: UnixMs) -> Result<(), FlowStoreError>;

    /// Everything the work item has used, oldest first.
    fn links(&self, item: WorkItemId) -> Result<Vec<RecordedLink>, FlowStoreError>;

    /// Start a flow, recording its `Started` event. Refused with
    /// [`FlowStoreError::FlowAlreadyOpen`] while a flow of the same kind is open for the item.
    fn start_flow(
        &self,
        item: WorkItemId,
        kind: FlowKind,
        at: UnixMs,
    ) -> Result<Flow, FlowStoreError>;

    /// Apply `event` to the flow and append it, returning the flow as it now is.
    fn record(&self, flow: FlowId, event: &FlowEvent, at: UnixMs) -> Result<Flow, FlowStoreError>;

    fn flow(&self, id: FlowId) -> Result<Option<Flow>, FlowStoreError>;

    /// The work item's flows that have not ended.
    fn open_flows(&self, item: WorkItemId) -> Result<Vec<Flow>, FlowStoreError>;

    /// The flow's history, in order.
    fn events(&self, flow: FlowId) -> Result<Vec<RecordedEvent>, FlowStoreError>;
}

/// A review run's working records: what it is of, its snapshot, and its steps and their attempts.
///
/// A port of its own beside [`FlowStore`] rather than more methods on it, because the two change
/// for different reasons: a flow's lifecycle is the same for every kind of work, and these records
/// are what one kind of flow, a review, keeps while it runs. The same database implements both.
///
/// Each write is its own transaction. The engine orders them so that a crash between two leaves a
/// record recovery can read — a step marked running whose invocation never got a session is one
/// recovery calls interrupted — rather than relying on one transaction across a session's life.
pub trait RunStore: Send + Sync {
    /// The work item a worktree has been reviewed as before, or that it was created for, found by
    /// the worktree links its items recorded.
    fn work_item_for_worktree(
        &self,
        project: &ProjectId,
        worktree_path: &str,
    ) -> Result<Option<WorkItemId>, FlowStoreError>;

    fn create_review(&self, review: &NewReviewRun, at: UnixMs)
    -> Result<ReviewRun, FlowStoreError>;

    fn review(&self, flow: FlowId) -> Result<Option<ReviewRun>, FlowStoreError>;

    /// A worktree's review runs, newest first.
    fn reviews_of(
        &self,
        project: &ProjectId,
        worktree: &str,
    ) -> Result<Vec<ReviewRun>, FlowStoreError>;

    /// Every review run whose flow has not ended, oldest first: what recovery at launch looks at.
    fn open_reviews(&self) -> Result<Vec<ReviewRun>, FlowStoreError>;

    /// Record the run's snapshot, and make it the one the run reads.
    fn record_snapshot(
        &self,
        flow: FlowId,
        snapshot: &Snapshot,
        at: UnixMs,
    ) -> Result<SnapshotId, FlowStoreError>;

    fn snapshot(&self, id: SnapshotId) -> Result<Option<Snapshot>, FlowStoreError>;

    /// Add steps to a run. Refused for a role the run already has a step for: a role's result,
    /// its retries and its fallback are all found by its id within the run.
    fn add_steps(
        &self,
        flow: FlowId,
        steps: &[NewStep],
        at: UnixMs,
    ) -> Result<Vec<Step>, FlowStoreError>;

    /// A run's steps, in stage order and then the order they were added.
    fn steps(&self, flow: FlowId) -> Result<Vec<Step>, FlowStoreError>;

    fn update_step(
        &self,
        step: StepId,
        change: &StepChange,
        at: UnixMs,
    ) -> Result<Step, FlowStoreError>;

    fn start_invocation(
        &self,
        invocation: &NewInvocation,
        at: UnixMs,
    ) -> Result<Invocation, FlowStoreError>;

    fn update_invocation(
        &self,
        invocation: InvocationId,
        change: &InvocationChange,
        at: UnixMs,
    ) -> Result<Invocation, FlowStoreError>;

    /// A run's invocations, oldest first.
    fn invocations(&self, flow: FlowId) -> Result<Vec<Invocation>, FlowStoreError>;

    /// A run's findings that are still reported, dismissed ones included, each with its evidence,
    /// in the order they were saved.
    fn findings(&self, flow: FlowId) -> Result<Vec<FindingRecord>, FlowStoreError>;

    /// One finding, superseded or not: a rule saved from it still names it.
    fn finding(&self, id: FindingId) -> Result<Option<FindingRecord>, FlowStoreError>;

    /// Save a consolidation, as [`reconcile`](crate::flow::reconcile) decided it, in one
    /// transaction: the superseded findings are marked, and the new ones written with their
    /// evidence. Returns the run's findings afterwards.
    fn save_consolidation(
        &self,
        flow: FlowId,
        change: &Reconciled,
        at: UnixMs,
    ) -> Result<Vec<FindingRecord>, FlowStoreError>;

    /// Dismiss a finding with a reason, or take a dismissal back with `None`.
    fn dismiss_finding(
        &self,
        id: FindingId,
        reason: Option<&str>,
        at: UnixMs,
    ) -> Result<FindingRecord, FlowStoreError>;

    /// Record a check the app ran, or skipped, for a role. Never changed afterwards.
    fn record_check(&self, run: &NewCheckRun, at: UnixMs) -> Result<CheckRun, FlowStoreError>;

    /// A run's check records, oldest first.
    fn checks(&self, flow: FlowId) -> Result<Vec<CheckRun>, FlowStoreError>;
}

/// What wtm keeps about a teammate's pull request beyond its review runs: what the forge last
/// said, your "I'm reviewing this" flag, the heads a review of it left behind, and the outcomes you
/// chose. See [`crate::flow::pull_review`] for why each is kept.
///
/// A port of its own for [`RunStore`]'s reason: a review run is the same for your work and a
/// teammate's, and these records exist only for the second.
pub trait PullReviewStore: Send + Sync {
    /// Keep what the forge just said about a work item's pull request, replacing the last reading.
    fn mirror_pr(&self, mirror: &MirroredPr) -> Result<(), FlowStoreError>;

    /// The last reading, if the pull request has been read since it was added.
    fn mirrored_pr(&self, item: WorkItemId) -> Result<Option<MirroredPr>, FlowStoreError>;

    /// Set "I'm reviewing this" (`Some(since)`) or clear it (`None`). Returns the flag as it is now.
    fn set_claim(
        &self,
        item: WorkItemId,
        since: Option<UnixMs>,
    ) -> Result<Option<ReviewClaim>, FlowStoreError>;

    fn claim(&self, item: WorkItemId) -> Result<Option<ReviewClaim>, FlowStoreError>;

    /// Record a head a review moved on from. Never changed afterwards.
    fn record_reviewed_head(
        &self,
        head: &NewReviewedHead,
        at: UnixMs,
    ) -> Result<ReviewedHead, FlowStoreError>;

    /// The heads a review moved on from, oldest first.
    fn reviewed_heads(&self, flow: FlowId) -> Result<Vec<ReviewedHead>, FlowStoreError>;

    /// Record an outcome. Never changed afterwards: a later decision is a later row.
    fn record_outcome(&self, outcome: &NewOutcome, at: UnixMs) -> Result<Outcome, FlowStoreError>;

    /// A review's outcomes, oldest first. The last is the current one.
    fn outcomes(&self, flow: FlowId) -> Result<Vec<Outcome>, FlowStoreError>;
}

/// What runs need you for, and the questions they ask: the inbox.
///
/// Its own port because it is the one record every kind of flow writes to and every part of the
/// window reads from — Home, the Work view, the dock badge — where [`RunStore`] is what one kind of
/// flow keeps while it runs. The same database implements it.
///
/// # Once per problem, kept until closed
///
/// An item is filed once per `(target, kind)` while it is open, which is what keeps a cause reported
/// twice from notifying twice. Closing is the only way an item leaves the list, and there are two:
/// resolved, when the thing it was about is settled, and dismissed, when a person put it away.
pub trait InboxStore: Send + Sync {
    /// File an item, unless an open one already has its target and kind. `None` when it was already
    /// there, which is what keeps a notification from firing twice.
    fn file_inbox(
        &self,
        item: &NewInboxItem,
        at: UnixMs,
    ) -> Result<Option<InboxItem>, FlowStoreError>;

    /// Resolve a flow's open items of `kind`, about `target` when one is given. Returns how many.
    fn resolve_inbox(
        &self,
        flow: FlowId,
        kind: InboxKind,
        target: Option<&InboxTarget>,
        at: UnixMs,
    ) -> Result<usize, FlowStoreError>;

    /// Resolve one item. The item as it now is, or `None` when it was not open.
    fn resolve_inbox_item(
        &self,
        item: InboxItemId,
        at: UnixMs,
    ) -> Result<Option<InboxItem>, FlowStoreError>;

    /// Dismiss one item, as a person does. The item as it now is, or `None` when it was not open.
    fn dismiss_inbox_item(
        &self,
        item: InboxItemId,
        at: UnixMs,
    ) -> Result<Option<InboxItem>, FlowStoreError>;

    /// One item, open or not.
    fn inbox_item(&self, item: InboxItemId) -> Result<Option<InboxItem>, FlowStoreError>;

    /// Open items, newest first: one run's, one project's, or everyone's.
    fn inbox(
        &self,
        flow: Option<FlowId>,
        project: Option<&ProjectId>,
    ) -> Result<Vec<InboxItem>, FlowStoreError>;

    /// Record a question and file its item, in one transaction, so a question never exists without
    /// the item that tells you about it. The item is `None` only if one was somehow already open.
    fn ask(
        &self,
        question: &NewQuestion,
        title: &str,
        at: UnixMs,
    ) -> Result<(Question, Option<InboxItem>), FlowStoreError>;

    fn question(&self, id: QuestionId) -> Result<Option<Question>, FlowStoreError>;

    /// A flow's questions, oldest first.
    fn questions(&self, flow: FlowId) -> Result<Vec<Question>, FlowStoreError>;

    /// Answer a question and resolve its item, in one transaction. The first answer is the one that
    /// counts: a second is refused with [`QuestionError::AlreadyAnswered`] and changes nothing, so
    /// answering in two places cannot answer twice.
    fn answer_question(&self, id: QuestionId, answer: &Answer) -> Result<Question, FlowStoreError>;
}

/// Approvals and the outbox: what you allowed to be sent, and each write's progress.
///
/// A port of its own, because every kind of flow writes through it — a review and a fix's routes
/// now; a draft pull request and an issue move later — and because its rules are the outbox's: one
/// operation per key, an approval never changed once given, and an operation's state moving only as
/// [`OperationState::can_move_to`](crate::flow::OperationState::can_move_to) allows. The same
/// database implements it.
pub trait OutboxStore: Send + Sync {
    /// Record `approval` and queue `operations` under it, in one transaction.
    ///
    /// An operation whose key is already queued is not queued twice. It moves to this approval: one
    /// that stopped, failed or was cancelled goes back to pending, and one that is done stays done,
    /// because the write it stands for happened. One that is executing refuses the whole approval
    /// with [`FlowStoreError::StillSending`], since whether it happened is not known yet.
    fn approve(
        &self,
        approval: &NewApproval,
        operations: &[NewOperation],
        at: UnixMs,
    ) -> Result<Approved, FlowStoreError>;

    /// One approval, with what it was of and its operations in order.
    fn approved(&self, approval: ApprovalId) -> Result<Option<Approved>, FlowStoreError>;

    /// A flow's approvals, oldest first. An operation belongs to the latest approval that covered
    /// it, so an earlier approval of the same payload has none left.
    fn approvals(&self, flow: FlowId) -> Result<Vec<Approved>, FlowStoreError>;

    /// One approval of something posted as a review: a review, or a fix's suggestions.
    fn publication(&self, approval: ApprovalId) -> Result<Option<Publication>, FlowStoreError> {
        Ok(self
            .approved(approval)?
            .and_then(Approved::into_publication))
    }

    /// A flow's publications, oldest first.
    fn publications(&self, flow: FlowId) -> Result<Vec<Publication>, FlowStoreError> {
        Ok(self
            .approvals(flow)?
            .into_iter()
            .filter_map(Approved::into_publication)
            .collect())
    }

    /// One approval of a push route.
    fn route(&self, approval: ApprovalId) -> Result<Option<RouteSend>, FlowStoreError> {
        Ok(self.approved(approval)?.and_then(Approved::into_route))
    }

    /// A flow's push routes, oldest first.
    fn routes(&self, flow: FlowId) -> Result<Vec<RouteSend>, FlowStoreError> {
        Ok(self
            .approvals(flow)?
            .into_iter()
            .filter_map(Approved::into_route)
            .collect())
    }

    fn operation(&self, id: OperationId) -> Result<Option<Operation>, FlowStoreError>;

    /// Change an operation, checked against the outbox's rules. What stopped it is cleared whenever
    /// it moves anywhere but `NeedsAttention`. Returns it as it now is.
    fn change_operation(
        &self,
        id: OperationId,
        change: &OperationChange,
        at: UnixMs,
    ) -> Result<Operation, FlowStoreError>;

    /// Every operation left executing: at launch, the ones a crash cut off mid-write.
    fn executing_operations(&self) -> Result<Vec<Operation>, FlowStoreError>;
}

/// Who holds a worktree: the writer lease, and the reviews reading it.
///
/// The rule — one writer, never beside a review — is checked and the lease granted in one
/// transaction, so it holds between two clicks as well as between two threads.
pub trait LeaseStore: Send + Sync {
    /// Take a lease.
    ///
    /// # Errors
    ///
    /// [`FlowStoreError::Leased`], naming the lease in the way, when
    /// [`lease_conflict`](crate::flow::lease_conflict) says one is.
    fn acquire_lease(&self, lease: &NewLease, at: UnixMs) -> Result<Lease, FlowStoreError>;

    /// Let a lease go, once. `None` when it was released already: the first release's reason is
    /// the one kept.
    fn release_lease(
        &self,
        id: LeaseId,
        why: LeaseRelease,
        at: UnixMs,
    ) -> Result<Option<Lease>, FlowStoreError>;

    /// Every open lease, on one worktree or on all of them, oldest first.
    fn open_leases(&self, worktree: Option<&str>) -> Result<Vec<Lease>, FlowStoreError>;
}

/// Local fixes: what a writer was asked to fix, what held it, and what it left.
pub trait FixStore: Send + Sync {
    /// Record a fix being started.
    ///
    /// # Errors
    ///
    /// [`FlowStoreError::FixWriting`] when another fix of the run is still being written.
    fn start_fix(&self, fix: &NewFix, at: UnixMs) -> Result<Fix, FlowStoreError>;

    /// Change a fix. A fix that is over keeps its state; its head, diff and allowed files may still
    /// change, as when you let an outlier in and the rest is committed.
    fn change_fix(&self, id: FixId, change: &FixChange, at: UnixMs) -> Result<Fix, FlowStoreError>;

    fn fix(&self, id: FixId) -> Result<Option<Fix>, FlowStoreError>;

    /// A run's fixes, oldest first.
    fn fixes(&self, flow: FlowId) -> Result<Vec<Fix>, FlowStoreError>;
}
