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
    Flow, FlowEvent, FlowId, FlowKind, NewWorkItem, RecordedEvent, RecordedLink, TransitionError,
    UnixMs, WorkItem, WorkItemId, WorkKey, WorkLink,
};
use crate::flow::{
    InboxItem, InboxItemId, InboxKind, Invocation, InvocationChange, InvocationId, NewInboxItem,
    NewInvocation, NewReviewRun, NewStep, ReviewRun, Snapshot, SnapshotId, Step, StepChange,
    StepId,
};
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

    /// The flow rules refused the event. Nothing was written.
    #[error(transparent)]
    Refused(#[from] TransitionError),

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

/// A review run's working records: what it is of, its snapshot, its steps and their attempts, and
/// its inbox.
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

    /// File an inbox item, unless an open one already covers the same `(flow, kind, subject)`.
    /// `None` when it was already there, which is what keeps a notification from firing twice.
    fn file_inbox(
        &self,
        item: &NewInboxItem,
        at: UnixMs,
    ) -> Result<Option<InboxItem>, FlowStoreError>;

    /// Resolve a run's open items of `kind`, about `subject` when one is given. Returns how many.
    fn resolve_inbox(
        &self,
        flow: FlowId,
        kind: InboxKind,
        subject: Option<&str>,
        at: UnixMs,
    ) -> Result<usize, FlowStoreError>;

    /// Resolve one item, as a person does. `false` if it was already resolved or never existed.
    fn resolve_inbox_item(&self, item: InboxItemId, at: UnixMs) -> Result<bool, FlowStoreError>;

    /// Open inbox items, newest first: one run's, one project's, or everyone's.
    fn inbox(
        &self,
        flow: Option<FlowId>,
        project: Option<&ProjectId>,
    ) -> Result<Vec<InboxItem>, FlowStoreError>;
}
