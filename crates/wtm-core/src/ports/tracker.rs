//! Reading an issue tracker, the writes a later item will need, and the mirror a sync fills.
//!
//! # Three capabilities, three traits
//!
//! [`Tracker`] only reads: a search, one issue, a board's sprints, one sprint. [`TrackerWriter`]
//! moves an issue and comments on one, and is declared here only so its shape is settled before
//! the item that sends them; nothing implements it yet, so no code in this build can write to a
//! tracker. [`TrackerMirror`] is the app database's half: the tables a sync writes.
//!
//! The split is what makes "a sync never starts a run" a property of types rather than of care.
//! The sync use-case is handed a [`Tracker`] and a [`TrackerMirror`] and nothing else, and neither
//! has a method that creates a flow, a step or an invocation, or that leads to anything that
//! could. `usecase::tracker_sync` holds the rest of that argument.
//!
//! # `cwd`
//!
//! A tracker CLI does not care where it runs, but every command through
//! [`CommandRunner`](super::CommandRunner) runs somewhere. The project's root keeps it out of the
//! app's own directory and makes a trace read like a terminal opened in the project.

use std::collections::BTreeMap;
use std::path::Path;

pub use crate::error::TrackerError;
use crate::flow::{
    IssueDetail, IssueQuery, MirroredQueue, QueueReading, Sprint, SprintState, SyncFailure,
    TrackerAccount, TrackerKind, Transition, UnixMs,
};
use crate::model::ProjectId;
use crate::ports::flow::FlowStoreError;

/// What the Work view reads from a tracker.
pub trait Tracker: Send + Sync {
    /// Whether the CLI is installed, new enough and signed in, and to which site.
    fn check(&self, cwd: &Path) -> Result<TrackerAccount, TrackerError>;

    /// The issues `query` matches, in the tracker's order, at most `query.limit` of them.
    fn search(
        &self,
        cwd: &Path,
        query: &IssueQuery,
    ) -> Result<Vec<crate::flow::Issue>, TrackerError>;

    /// One issue, read whole, with the project's mapped `fields` (name → the tracker's field id).
    /// The field named [`SPRINT_FIELD`](crate::flow::SPRINT_FIELD) is read as the issue's sprints.
    fn issue(
        &self,
        cwd: &Path,
        key: &str,
        fields: &BTreeMap<String, String>,
    ) -> Result<IssueDetail, TrackerError>;

    /// A board's sprints in any of `states`.
    fn sprints(
        &self,
        cwd: &Path,
        board: u64,
        states: &[SprintState],
    ) -> Result<Vec<Sprint>, TrackerError>;

    /// One sprint, with its goal.
    fn sprint(&self, cwd: &Path, id: u64) -> Result<Sprint, TrackerError>;

    /// The transitions an issue's workflow offers from its current status.
    ///
    /// # Errors
    ///
    /// [`TrackerError::Unsupported`] from a CLI that cannot list them. A move by status name does
    /// not need the list, so this is information for a person, not a step a write depends on.
    fn transitions(&self, cwd: &Path, key: &str) -> Result<Vec<Transition>, TrackerError>;
}

/// Moving an issue and commenting on one. Declared, and implemented by nothing in this build.
///
/// Both are approvals in the plan (§4.2): a move to the mapped "needs review" status rides on the
/// publish approval, and a comment is sent only from its own second click. The outbox's executor
/// will be the only holder, as it is of the forge's writers, and it reads the issue's status first
/// because a tracker has no compare-and-swap for a transition (§4.4).
pub trait TrackerWriter: Send + Sync {
    /// Move `key` to `status` by name. The tracker picks the transition that reaches it.
    fn move_to(&self, cwd: &Path, key: &str, status: &str) -> Result<(), TrackerError>;

    /// Add a comment to `key`.
    fn comment(&self, cwd: &Path, key: &str, body: &str) -> Result<(), TrackerError>;
}

/// The mirror tables: the last thing the tracker said, per project's queue and per issue.
///
/// Every method writes or reads those tables only. That is the whole contract, and the reason a
/// sync can be handed one of these without being able to start anything.
pub trait TrackerMirror: Send + Sync {
    /// Replace the project's queue with `reading` and update the mirror of each issue and sprint in
    /// it, in one transaction, clearing any failure recorded since the last success.
    fn mirror_queue(&self, reading: &QueueReading) -> Result<(), FlowStoreError>;

    /// Record that a sync of the project's queue failed, keeping the last reading that did not.
    fn record_sync_failure(
        &self,
        project: &ProjectId,
        tracker: TrackerKind,
        failure: &SyncFailure,
    ) -> Result<(), FlowStoreError>;

    /// The project's queue as the last sync left it. `None` before the first attempt.
    fn mirrored_queue(&self, project: &ProjectId) -> Result<Option<MirroredQueue>, FlowStoreError>;

    /// Keep an issue read whole, and its row's fields with it.
    fn mirror_issue(
        &self,
        tracker: TrackerKind,
        detail: &IssueDetail,
        at: UnixMs,
    ) -> Result<(), FlowStoreError>;

    /// An issue as last read whole, with when.
    fn mirrored_issue(
        &self,
        tracker: TrackerKind,
        key: &str,
    ) -> Result<Option<(IssueDetail, UnixMs)>, FlowStoreError>;
}
