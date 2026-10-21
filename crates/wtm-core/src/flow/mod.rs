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
//! decided, plus recipes and their planning, snapshot staleness, and review knowledge (rules,
//! their retrieval, and the repository's instruction sources). Consolidation and the engine arrive
//! with the backlog items that first need them, and so do the store tables for snapshots,
//! questions, approvals, findings and operations.

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
pub mod forge;
pub mod glob;
pub mod instructions;
pub mod knowledge;
pub mod outbox;
pub mod plan;
pub mod question;
pub mod recipe;
pub mod review;
pub mod rule;
pub mod snapshot;
pub mod state;
pub mod work;

pub use approval::{
    Approval, ApprovalError, ApprovalId, ApprovalIntent, Binding, PinChange, Pinned, Stale,
};
pub use event::{FlowEvent, RecordedEvent};
pub use forge::{
    BranchProtection, ChecksState, ChecksSummary, ForgeAccount, ForgeKind, GITHUB_HOST, Permission,
    PrBase, PrHead, PrState, PrTarget, PrTargetError, PullRequest, RepoSlug, Reviewer,
    default_host, host_for, remote_for,
};
pub use instructions::{
    DEFAULT_INSTRUCTION_SOURCES, InstructionDocument, InstructionSet, SkippedDocument, SourceMatch,
};
pub use knowledge::{
    DEFAULT_MAX_RULES, MatchReason, RecordedApplication, ReviewInvocation, RuleQuery,
    RuleSelection, RuleUsage, SelectedRule,
};
pub use outbox::{
    Operation, OperationId, OperationKey, OperationKind, OperationState, OutboxError,
};
pub use plan::{PlanError, PlannedRole, PlannedStage, RecipePlan, SkipReason, SkippedRole, plan};
pub use question::{Answer, ContextLink, Question, QuestionError, QuestionId};
pub use recipe::{
    Fallback, KnowledgeScope, OutputKind, RecipeSpec, RoleInput, RoleProfile, RoleProvider,
    RoleSpec, SkillCatalog, SkillError, SkillProblem, StageSpec, WorkflowError, WorkflowSpec,
    Workspace,
};
pub use review::{
    BlobRef, Dismissal, Finding, FindingClass, FindingId, Location, Severity, Snapshot,
};
pub use rule::{
    ExampleStance, NewRule, Rule, RuleApproval, RuleChange, RuleDraft, RuleError, RuleExample,
    RuleId, RuleKind, RuleOrigin, RuleRevision, RuleScope, RuleSource, RuleStatus,
};
pub use snapshot::{FlowDir, ScratchLabel, TreeCheck};
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
