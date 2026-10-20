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
