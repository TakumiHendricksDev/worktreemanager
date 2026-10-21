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
//! decided, plus recipes and their planning, snapshot staleness, review knowledge (rules, their
//! retrieval, and the repository's instruction sources), a review run's steps and invocations with
//! the rules for retries, fallbacks and usage limits, their consolidation into findings, the record
//! of the checks the app ran for them, a teammate's pull request followed across its heads
//! ([`pull_review`]), and the inbox that is the one thing about a run that tells you anything, with
//! the questions it carries, and publishing a review through the outbox under an approval of exactly
//! what posts ([`publish`], [`outbox`], [`approval`]). The engine and the outbox's executor are in
//! the app (`src-tauri/src/flow/`), because one opens sessions and the other reaches the forge.

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
pub mod consolidate;
pub mod event;
pub mod forge;
pub mod glob;
pub mod inbox;
pub mod instructions;
pub mod knowledge;
pub mod outbox;
pub mod plan;
pub mod publish;
pub mod pull_review;
pub mod question;
pub mod recipe;
pub mod review;
pub mod rule;
pub mod run;
pub mod schema;
pub mod snapshot;
pub mod state;
pub mod submission;
pub mod work;

pub use approval::{
    Approval, ApprovalError, ApprovalId, ApprovalIntent, ApprovalSubject, Binding, NewApproval,
    PinChange, Pinned, Stale, review_binding,
};
pub use consolidate::{
    Evidence, EvidenceKey, FindingRecord, Merged, NewFinding, Reconciled, Source, consolidate,
    finding_is_stale, reconcile, role_is_affected,
};
pub use event::{FlowEvent, RecordedEvent};
pub use forge::{
    BranchProtection, ChecksState, ChecksSummary, ForgeAccount, ForgeKind, GITHUB_HOST, Permission,
    PrBase, PrHead, PrState, PrTarget, PrTargetError, PullRequest, RepoSlug, Reviewer,
    default_host, host_for, remote_for,
};
pub use inbox::{
    InboxItem, InboxItemId, InboxKind, InboxState, InboxTarget, NewInboxItem, review_ready,
};
pub use instructions::{
    DEFAULT_INSTRUCTION_SOURCES, InstructionDocument, InstructionSet, SkippedDocument, SourceMatch,
};
pub use knowledge::{
    DEFAULT_MAX_RULES, MatchReason, RecordedApplication, ReviewInvocation, RuleQuery,
    RuleSelection, RuleUsage, SelectedRule,
};
pub use outbox::{
    Activity, Attention, NewOperation, Operation, OperationChange, OperationId, OperationKey,
    OperationKind, OperationPayload, OperationResult, OperationState, OutboxError, Publication,
    PublicationState, Resolution,
};
pub use plan::{PlanError, PlannedRole, PlannedStage, RecipePlan, SkipReason, SkippedRole, plan};
pub use publish::{
    ChangedLines, DraftComment, FileLines, FoldReason, FoldedComment, InlineComment, LineRange,
    Marker, PendingFound, PendingReviewPayload, PublishContext, PublishError, RemoteComment,
    RemoteReview, RemoteReviewState, ReviewDraft, ReviewEvent, ReviewPayload, SubmitFound,
    SubmitReviewPayload, carries_marker, find_pending, find_submitted, missing_comments, signed,
};
pub use pull_review::{
    EarlierFate, MirroredPr, NewOutcome, NewReviewedHead, Outcome, OutcomeError, OutcomeKind,
    PushToBranch, ReviewClaim, ReviewedHead, base_ref, fate, push_to_branch,
};
pub use question::{Answer, ContextLink, NewQuestion, Question, QuestionError, QuestionId};
pub use recipe::{
    CheckSpec, Fallback, KnowledgeScope, OutputKind, RecipeSpec, RoleInput, RoleProfile,
    RoleProvider, RoleSpec, SkillCatalog, SkillError, SkillProblem, StageSpec, WorkflowError,
    WorkflowSpec, Workspace,
};
pub use review::{
    BlobRef, Dismissal, Finding, FindingClass, FindingId, Location, Severity, Snapshot,
};
pub use rule::{
    ExampleStance, NewRule, Rule, RuleApproval, RuleChange, RuleDraft, RuleError, RuleExample,
    RuleId, RuleKind, RuleOrigin, RuleRevision, RuleScope, RuleSource, RuleStatus,
};
pub use run::{
    AfterFailure, CheckRun, CheckRunId, CheckStatus, Field, Invocation, InvocationChange,
    InvocationId, InvocationStatus, NewCheckRun, NewInvocation, NewReviewRun, NewStep, OnLimit,
    ReviewRun, SnapshotId, Step, StepChange, StepId, StepStatus, after_failure, on_limit,
    resume_at,
};
pub use schema::SchemaProblem;
pub use snapshot::{FlowDir, ScratchLabel, TreeCheck};
pub use state::{
    BlockCause, End, Flow, FlowId, FlowKind, FlowState, Hold, HoldKind, InterruptCause, Phase,
    Status, TransitionError,
};
pub use submission::{
    FindingsReport, Handoff, PlanReport, ReportedClass, ReportedFinding, RuleCitation, Submission,
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
