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
use crate::flow::{
    CheckRun, FindingId, FindingRecord, NewCheckRun, NewScreenshot, Reconciled, Screenshot,
};
use crate::flow::{
    CheckpointBy, Handoff, ImplementChange, ImplementCheckpoint, ImplementQuestion, Implementation,
    NewImplementation, NewRound, PrFeedback, Round,
};
use crate::flow::{
    CommandRun, CommandRunChange, CommandRunId, CommandRunRefused, NewCommandRun, ServiceRecord,
};
use crate::flow::{
    CommentSend, NewPlan, NewPlanning, PlanContext, PlanDecision, PlanId, PlanRecord, Planning,
    QuestionMessage, Speaker,
};
use crate::flow::{
    Design, DesignComment, DesignDecision, DesignRevision, DesignRevisionId, NewDesign,
    NewDesignRevision,
};
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
use crate::flow::{MoveSend, NewStartJob, OwnSend, StartJob, StartJobChange, StartJobError};
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

    /// The work item already has a start job whose run is still open.
    #[error("work item {item} is already being started, by run {flow}")]
    StartUnderway { item: WorkItemId, flow: FlowId },

    #[error("run {0} has not planned anything")]
    UnknownPlanning(FlowId),

    #[error("no plan {0}")]
    UnknownPlan(PlanId),

    #[error("run {0} has no design")]
    UnknownDesign(FlowId),

    #[error("run {0} has not started implementing")]
    UnknownImplementation(FlowId),

    /// A stopped implementation stays stopped. Nothing was written.
    #[error("run {0}'s implementation was stopped")]
    ImplementationEnded(FlowId),

    #[error("no design revision {0}")]
    UnknownDesignRevision(DesignRevisionId),

    /// A comment or decision named a revision the run does not have.
    #[error("run {flow} has no design revision {revision}")]
    NoSuchRevision { flow: FlowId, revision: u32 },

    #[error("no start job for work item {0}")]
    UnknownStartJob(WorkItemId),

    /// The start job's rules refused the change. Nothing was written. A field, for the reason
    /// [`Self::Unanswerable`] gives.
    #[error("{reason}")]
    StartJob {
        #[from]
        reason: StartJobError,
    },

    /// An approval would take over an operation that is being sent, or was being sent when wtm
    /// stopped: whether it reached the remote is not known yet. Nothing was written.
    #[error("operation {operation} is still being sent; wait for it to finish or be checked")]
    StillSending { operation: OperationId },

    /// A command of this worktree's environment is already waiting for your approval. Approve or
    /// decline it first: its inbox item is the one place it is asked, and a second would hide it.
    #[error("command run {existing} is already waiting for your approval")]
    CommandWaiting { existing: CommandRunId },

    #[error("no command run {0}")]
    UnknownCommandRun(CommandRunId),

    /// The run's rules refused the change. Nothing was written. A field, for the reason
    /// [`Self::Unanswerable`] gives.
    #[error("{reason}")]
    CommandRun {
        #[from]
        reason: CommandRunRefused,
    },

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

    /// Keep the record of a screenshot a runtime role took, or wtm took for it. Never changed
    /// afterwards.
    fn record_screenshot(
        &self,
        shot: &NewScreenshot,
        at: UnixMs,
    ) -> Result<Screenshot, FlowStoreError>;

    /// A run's screenshots, oldest first.
    fn screenshots(&self, flow: FlowId) -> Result<Vec<Screenshot>, FlowStoreError>;

    /// Keep a switch of a role's work to another role or provider, with the handover the new
    /// runner was given. Never changed afterwards.
    fn record_switch(
        &self,
        switch: &crate::flow::NewProviderSwitch,
        at: UnixMs,
    ) -> Result<crate::flow::ProviderSwitch, FlowStoreError>;

    /// A run's switches, oldest first.
    fn switches(&self, flow: FlowId) -> Result<Vec<crate::flow::ProviderSwitch>, FlowStoreError>;

    /// Keep the process behind an invocation's session: its pid and start time.
    fn keep_process(
        &self,
        invocation: InvocationId,
        process: &crate::flow::ProcessIdentity,
    ) -> Result<(), FlowStoreError>;

    /// Every invocation that started no later than `before` and still has a process kept: what a launch
    /// looks for, since a session this wtm opened is not the last one's.
    fn kept_processes(
        &self,
        before: UnixMs,
    ) -> Result<Vec<(Invocation, crate::flow::ProcessIdentity)>, FlowStoreError>;

    /// The launch has dealt with this invocation's process: forget it.
    fn forget_process(&self, invocation: InvocationId) -> Result<(), FlowStoreError>;

    /// Keep a note on a run. It is not an event, and moves nothing.
    fn note(
        &self,
        note: &crate::flow::NewNote,
        at: UnixMs,
    ) -> Result<crate::flow::FlowNote, FlowStoreError>;

    /// A run's notes, oldest first.
    fn notes(&self, flow: FlowId) -> Result<Vec<crate::flow::FlowNote>, FlowStoreError>;
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

    /// One approval of an issue's move.
    fn issue_move(&self, approval: ApprovalId) -> Result<Option<MoveSend>, FlowStoreError> {
        Ok(self.approved(approval)?.and_then(Approved::into_move))
    }

    /// A flow's issue moves, oldest first.
    fn issue_moves(&self, flow: FlowId) -> Result<Vec<MoveSend>, FlowStoreError> {
        Ok(self
            .approvals(flow)?
            .into_iter()
            .filter_map(Approved::into_move)
            .collect())
    }

    /// One approval of a write to the issue's own pull request.
    fn own_send(&self, approval: ApprovalId) -> Result<Option<OwnSend>, FlowStoreError> {
        Ok(self.approved(approval)?.and_then(Approved::into_own))
    }

    /// A flow's writes to its own pull request, oldest first.
    fn own_sends(&self, flow: FlowId) -> Result<Vec<OwnSend>, FlowStoreError> {
        Ok(self
            .approvals(flow)?
            .into_iter()
            .filter_map(Approved::into_own)
            .collect())
    }

    /// One approval of a comment on an issue.
    fn issue_comment(&self, approval: ApprovalId) -> Result<Option<CommentSend>, FlowStoreError> {
        Ok(self.approved(approval)?.and_then(Approved::into_comment))
    }

    /// A flow's comments, oldest first.
    fn issue_comments(&self, flow: FlowId) -> Result<Vec<CommentSend>, FlowStoreError> {
        Ok(self
            .approvals(flow)?
            .into_iter()
            .filter_map(Approved::into_comment)
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

/// Worktree environments: the services wtm started, the flow each environment's approvals belong
/// to, the commands run against it, and the idle stop you opted into.
pub trait EnvStore: Send + Sync {
    /// Record a service as it is now, replacing what was recorded for that service of that worktree.
    fn save_service(&self, record: &ServiceRecord) -> Result<(), FlowStoreError>;

    /// The services recorded for one worktree, or for every worktree, by worktree then service.
    fn services(&self, worktree: Option<&str>) -> Result<Vec<ServiceRecord>, FlowStoreError>;

    /// The environment flow recorded for `worktree`, if it is still open.
    fn environment_flow(&self, worktree: &str) -> Result<Option<FlowId>, FlowStoreError>;

    /// The project and worktree an environment flow belongs to.
    fn environment_place(
        &self,
        flow: FlowId,
    ) -> Result<Option<(ProjectId, String)>, FlowStoreError>;

    /// Record that `flow` is `worktree`'s environment, replacing an earlier flow for that path.
    fn bind_environment_flow(
        &self,
        flow: FlowId,
        project: &ProjectId,
        worktree: &str,
    ) -> Result<(), FlowStoreError>;

    /// Record a command run.
    ///
    /// # Errors
    ///
    /// [`FlowStoreError::CommandWaiting`] when one of the flow's runs is waiting for approval
    /// already and this one would wait too.
    fn record_command_run(
        &self,
        run: &NewCommandRun,
        at: UnixMs,
    ) -> Result<CommandRun, FlowStoreError>;

    /// Change a run, checked against [`CommandRunState::can_move`](crate::flow::CommandRunState::can_move).
    fn change_command_run(
        &self,
        id: CommandRunId,
        change: &CommandRunChange,
        at: UnixMs,
    ) -> Result<CommandRun, FlowStoreError>;

    fn command_run(&self, id: CommandRunId) -> Result<Option<CommandRun>, FlowStoreError>;

    /// A flow's runs, newest first, at most `limit`.
    fn command_runs(&self, flow: FlowId, limit: usize) -> Result<Vec<CommandRun>, FlowStoreError>;

    /// Every run, of any flow, still recorded as running: at launch, the ones a quit or a crash cut
    /// off, since a run's process does not outlive the wtm that ran it.
    fn running_command_runs(&self) -> Result<Vec<CommandRun>, FlowStoreError>;

    /// How many minutes in the background before a project's services are stopped, if you opted in.
    fn idle_stop(&self, project: &ProjectId) -> Result<Option<u32>, FlowStoreError>;

    /// Opt a project into idle stop, or out of it with `None`.
    fn set_idle_stop(
        &self,
        project: &ProjectId,
        minutes: Option<u32>,
    ) -> Result<(), FlowStoreError>;
}

/// Start Work's jobs: the worktree an Implement run is being given, one job per work item.
///
/// A port of its own for [`RunStore`]'s reason: a review run's records and an issue's start are
/// kept by different kinds of flow. Keyed by the work item rather than numbered, because "is this
/// issue being started already?" is the question every Start asks first, and the answer has to be
/// the store's (see `flow::start`).
pub trait StartStore: Send + Sync {
    /// Record the job that prepares `job.flow`.
    ///
    /// # Errors
    ///
    /// [`FlowStoreError::StartUnderway`] when the work item has a job whose run is still open. A
    /// job whose run has ended is replaced: starting the issue again is a new run, and the old
    /// one's worktree is still in the work item's links.
    fn record_start_job(&self, job: &NewStartJob, at: UnixMs) -> Result<StartJob, FlowStoreError>;

    fn start_job(&self, item: WorkItemId) -> Result<Option<StartJob>, FlowStoreError>;

    /// The job that prepares `flow`, when it is an Implement run's.
    fn start_job_of(&self, flow: FlowId) -> Result<Option<StartJob>, FlowStoreError>;

    /// Change a job, checked against [`StartJobState::can_move_to`](crate::flow::StartJobState)
    /// when its state moves. Returns it as it now is.
    fn change_start_job(
        &self,
        item: WorkItemId,
        change: &StartJobChange,
        at: UnixMs,
    ) -> Result<StartJob, FlowStoreError>;

    /// A project's jobs, or every project's, oldest first.
    fn start_jobs(&self, project: Option<&ProjectId>) -> Result<Vec<StartJob>, FlowStoreError>;
}

/// An Implement run's planning: the record of it, the plans its planner handed in, what you decided
/// about each, and the discussions of its questions.
///
/// A plan is append-only, and so is a decision: "approved" and "changes requested" are things that
/// happened to a revision, and a later decision is a later row, never an edit of an earlier one.
pub trait PlanStore: Send + Sync {
    /// Record `planning`, or return the record the run already has: planning starts again after a
    /// resume, an answer or a second click, and it is one record.
    fn begin_planning(
        &self,
        planning: &NewPlanning,
        at: UnixMs,
    ) -> Result<Planning, FlowStoreError>;

    fn planning(&self, flow: FlowId) -> Result<Option<Planning>, FlowStoreError>;

    /// Every run's planning record, oldest first: what recovery at launch looks through.
    fn plannings(&self) -> Result<Vec<Planning>, FlowStoreError>;

    /// Keep what the planner was given, for the context drawer.
    fn set_plan_context(
        &self,
        flow: FlowId,
        context: &PlanContext,
        at: UnixMs,
    ) -> Result<Planning, FlowStoreError>;

    /// Record a plan as its run's next revision: 1 for the first.
    fn record_plan(&self, plan: &NewPlan, at: UnixMs) -> Result<PlanRecord, FlowStoreError>;

    /// A run's plans, oldest revision first.
    fn plans(&self, flow: FlowId) -> Result<Vec<PlanRecord>, FlowStoreError>;

    /// Approve plan `plan`: `approval` and the decision that refers to it, in one transaction, so
    /// neither exists without the other.
    fn approve_plan(
        &self,
        approval: &NewApproval,
        plan: PlanId,
        skip_design: bool,
        at: UnixMs,
    ) -> Result<(Approved, PlanDecision), FlowStoreError>;

    /// Ask for changes to plan `plan`.
    fn request_plan_changes(
        &self,
        plan: PlanId,
        note: &str,
        at: UnixMs,
    ) -> Result<PlanDecision, FlowStoreError>;

    /// A run's decisions, oldest first.
    fn plan_decisions(&self, flow: FlowId) -> Result<Vec<PlanDecision>, FlowStoreError>;

    /// Add a message to a question's discussion.
    fn discuss(
        &self,
        question: QuestionId,
        from: Speaker,
        text: &str,
        at: UnixMs,
    ) -> Result<QuestionMessage, FlowStoreError>;

    /// A question's discussion, oldest first.
    fn discussion(&self, question: QuestionId) -> Result<Vec<QuestionMessage>, FlowStoreError>;
}

/// An Implement run's design: the record of it, the revisions its designer published, your comments
/// on each, and what you decided about them.
///
/// Like a plan's, every row here is append-only except the design record's prototype, which is set
/// once the designer has prepared one: a revision is what an approval pins, a comment is what was
/// said about a revision, and a decision is something that happened to one.
pub trait DesignStore: Send + Sync {
    /// Record `design`, or return the record the run already has: a resume or a second request is
    /// the same design, by the same role, with the same conversation.
    fn begin_design(&self, design: &NewDesign, at: UnixMs) -> Result<Design, FlowStoreError>;

    fn design(&self, flow: FlowId) -> Result<Option<Design>, FlowStoreError>;

    /// Every run's design, oldest first: what recovery at launch looks through.
    fn designs(&self) -> Result<Vec<Design>, FlowStoreError>;

    /// Keep the prototype the designer prepared. Set once: every revision of a run is an artifact
    /// of one prototype, which is what makes the history one history.
    fn set_design_prototype(
        &self,
        flow: FlowId,
        prototype: &str,
        at: UnixMs,
    ) -> Result<Design, FlowStoreError>;

    /// Record a revision as its run's next: 1 for the first.
    fn record_design_revision(
        &self,
        revision: &NewDesignRevision,
        at: UnixMs,
    ) -> Result<DesignRevision, FlowStoreError>;

    /// A run's revisions, oldest first. Every one of them, however many came after it.
    fn design_revisions(&self, flow: FlowId) -> Result<Vec<DesignRevision>, FlowStoreError>;

    /// Comment on revision `revision` of `flow`.
    fn comment_on_design(
        &self,
        flow: FlowId,
        revision: u32,
        text: &str,
        at: UnixMs,
    ) -> Result<DesignComment, FlowStoreError>;

    /// A run's comments, oldest first.
    fn design_comments(&self, flow: FlowId) -> Result<Vec<DesignComment>, FlowStoreError>;

    /// Approve revision `revision`: `approval` and the decision that refers to it, in one
    /// transaction.
    fn approve_design(
        &self,
        approval: &NewApproval,
        revision: DesignRevisionId,
        at: UnixMs,
    ) -> Result<(Approved, DesignDecision), FlowStoreError>;

    /// Ask for changes to revision `revision`.
    fn request_design_changes(
        &self,
        revision: DesignRevisionId,
        note: &str,
        at: UnixMs,
    ) -> Result<DesignDecision, FlowStoreError>;

    /// A run's decisions, oldest first.
    fn design_decisions(&self, flow: FlowId) -> Result<Vec<DesignDecision>, FlowStoreError>;
}

/// An Implement run's implementation (migration 16): who writes, the scope its plan holds it to,
/// where its review is, each round of writing, the commits the app made for it, and what the
/// questions it asked are about.
///
/// Rounds and checkpoints are append-only but for a round's end, which is written once: what a
/// round set out to fix is what the bounds count, and a count that could be rewritten would not
/// bound anything.
pub trait ImplementStore: Send + Sync {
    /// Record `implementation`, or return the record the run already has: a resume, or implementing
    /// again after a plan change, is the same implementation, by the same writer, in the same
    /// conversation.
    fn begin_implementation(
        &self,
        implementation: &NewImplementation,
        at: UnixMs,
    ) -> Result<Implementation, FlowStoreError>;

    fn implementation(&self, flow: FlowId) -> Result<Option<Implementation>, FlowStoreError>;

    /// Every run's implementation, oldest first: what recovery at launch looks through.
    fn implementations(&self) -> Result<Vec<Implementation>, FlowStoreError>;

    /// The implementation whose review run is `review`, if one is.
    fn implementation_reviewed_by(
        &self,
        review: FlowId,
    ) -> Result<Option<Implementation>, FlowStoreError>;

    /// # Errors
    ///
    /// [`FlowStoreError::ImplementationEnded`] for any change to one you stopped; nothing is written
    /// then.
    fn change_implementation(
        &self,
        flow: FlowId,
        change: &ImplementChange,
        at: UnixMs,
    ) -> Result<Implementation, FlowStoreError>;

    /// Record the run's next round: 1 for the first.
    fn start_round(&self, round: &NewRound, at: UnixMs) -> Result<Round, FlowStoreError>;

    /// End round `round`, with where it left the worktree, what it changed and what the writer
    /// handed in. Once.
    fn end_round(
        &self,
        flow: FlowId,
        round: u32,
        head_sha: Option<&str>,
        changed: &[String],
        handoff: Option<&Handoff>,
        at: UnixMs,
    ) -> Result<Round, FlowStoreError>;

    /// A run's rounds, oldest first.
    fn rounds(&self, flow: FlowId) -> Result<Vec<Round>, FlowStoreError>;

    /// Keep a commit the app made for round `round`.
    #[allow(clippy::too_many_arguments)]
    fn record_implement_checkpoint(
        &self,
        flow: FlowId,
        round: u32,
        commit: &str,
        message: &str,
        paths: &[String],
        by: CheckpointBy,
        at: UnixMs,
    ) -> Result<ImplementCheckpoint, FlowStoreError>;

    /// A run's commits, oldest first.
    fn implement_checkpoints(
        &self,
        flow: FlowId,
    ) -> Result<Vec<ImplementCheckpoint>, FlowStoreError>;

    /// Keep what a question the implementation asked is about.
    fn link_implement_question(&self, link: &ImplementQuestion) -> Result<(), FlowStoreError>;

    /// What the run's questions are about, oldest first.
    fn implement_questions(&self, flow: FlowId) -> Result<Vec<ImplementQuestion>, FlowStoreError>;

    /// Keep each review of the issue's pull request read for the first time, and return those: a
    /// review already kept is kept as it was first read.
    fn keep_feedback(&self, feedback: &[PrFeedback]) -> Result<Vec<PrFeedback>, FlowStoreError>;

    /// The reviews kept for a run, oldest first.
    fn feedback(&self, flow: FlowId) -> Result<Vec<PrFeedback>, FlowStoreError>;

    /// Say that revision round `round` took `reviews` in. Each review is taken in once.
    fn feedback_revised(
        &self,
        flow: FlowId,
        reviews: &[u64],
        round: u32,
    ) -> Result<(), FlowStoreError>;
}
