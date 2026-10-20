//! Flows: the durable record of one workflow execution against a work item.
//!
//! # Why "Flow" and not "Run"
//!
//! The UI calls one of these a run. The code cannot: a shell Run and a `spawn_agents` run already
//! own that word, and a third meaning would make every `run` in a grep ambiguous. A [`Flow`] is one
//! execution of Implement, Review mine, Review PR or Revise against a [`WorkItem`].
//!
//! # Why this lives in `wtm-core`
//!
//! The entities, the phase and hold rules, approval and finding staleness, and the outbox states
//! are rules with no I/O, so they belong where the `wasm32` check proves they stay that way.
//! ARCHITECTURE §2 has already rejected a separate domain crate for the same reason: code that
//! always changes together is one crate. The SQLite store that persists these is `wtm-appdb`,
//! behind [`FlowStore`](crate::ports::FlowStore).
//!
//! # Events are the record, and the flow is their projection
//!
//! A flow's state changes only by appending a [`FlowEvent`]. The stored flow is what
//! [`FlowState::replay`] makes of its events, and a store applies each new event through
//! [`FlowState::apply`] before it writes either one, so a transition these rules refuse never
//! reaches the database.
//!
//! # Ids are integers the store assigns
//!
//! A UUID would need a random source, and `wasm32-unknown-unknown` has none, so minting ids here
//! would break the check that keeps this crate honest. The store never reuses an id, which matters
//! because a flow's files live in a directory named after it.
//!
//! # What is here, and what is not yet
//!
//! This is the skeleton the Work view is built on: the types and the rules that are already
//! decided. Recipes, consolidation, rule retrieval and the engine arrive with the backlog items that
//! first need them, and so do the store tables for questions, approvals, findings, rules and
//! operations.

/// Declares an integer id assigned by the store.
macro_rules! id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash,
            ::serde::Serialize, ::serde::Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub i64);

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

pub mod approval;
pub mod event;
pub mod outbox;
pub mod question;
pub mod review;
pub mod rule;
pub mod state;
pub mod work;

pub use approval::{
    Approval, ApprovalError, ApprovalId, ApprovalIntent, Binding, PinChange, Pinned, Stale,
};
pub use event::{FlowEvent, RecordedEvent};
pub use outbox::{
    Operation, OperationId, OperationKey, OperationKind, OperationState, OutboxError,
};
pub use question::{Answer, ContextLink, Question, QuestionError, QuestionId};
pub use review::{
    BlobRef, Dismissal, Finding, FindingClass, FindingId, Location, Severity, Snapshot,
};
pub use rule::{
    Rule, RuleApproval, RuleDraft, RuleError, RuleId, RuleKind, RuleOrigin, RuleRevision,
    RuleScope, RuleStatus,
};
pub use state::{
    BlockCause, End, Flow, FlowId, FlowKind, FlowState, Hold, HoldKind, InterruptCause, Phase,
    Status, TransitionError,
};
pub use work::{
    Backing, IssueRef, NewWorkItem, PullRequestRef, RecordedLink, WorkItem, WorkItemId, WorkKey,
    WorkLink,
};

/// A milliseconds-since-epoch timestamp, as the [`Clock`](crate::ports::Clock) port reports it.
///
/// Named only so a signature says which clock a number came from. Every time in this module
/// arrives from the caller, because reading the clock here would make the rules untestable at a
/// fixed instant.
pub type UnixMs = u64;
