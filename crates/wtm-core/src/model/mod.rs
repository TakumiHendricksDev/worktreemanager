//! The domain model.
//!
//! Split by lifecycle rather than by noun:
//!
//! - [`worktree`] — what git tells us exists right now.
//! - [`project`] — what a project's config *declares*: its form, its lookups, its
//!   naming rules, its commands.
//! - [`value`] — what the user typed into the form.
//! - [`naming`] — which template tokens are legal at each stage.
//! - [`plan`] — what we intend to do, and what happened.
//! - [`agent`] — what an agent session reports, normalized across providers.
//!
//! Note that [`plan`] owns the name `Plan` and has since v0.1, for the create pipeline's
//! preview. An agent's plan is an `Agenda` while it is still moving and a `Brief` once it is a
//! document — two English words rather than an `AgentPlan` prefix, which is the same
//! disambiguation smell `ProjectMeta` exists to avoid.

pub mod agent;
pub mod naming;
pub mod plan;
pub mod project;
pub mod value;
pub mod worktree;

pub use agent::{
    AgendaStatus, AgendaStep, AgentAttachment, AgentCapability, AgentEvent, AgentMode, AgentModel,
    AgentSkill, ApprovalAnswer, ApprovalRequest, Effort, EffortOption, ModeRisk, NoticeLevel,
    Usage, UserInputOption, UserInputQuestion,
};
pub use naming::{RESERVED_PREFIXES, TokenScope, TokenSet, namespace_of, shadows_reserved_prefix};
pub use plan::{
    BranchChoice, BranchPlan, CreateOutcome, CreatePlan, ExitOutcome, PlanPreview, PlanWarning,
    PreflightItem, PreflightSeverity, Remedy, SessionId, TrackMode,
};
pub use project::{
    ActionSpec, AgentSpec, BranchScope, CommandSpec, ComputedSpec, Concurrency, ConditionalArgs,
    CreateSpec, CwdBase, DatabaseAccess, DatabaseEngine, DatabaseEnvironment, DatabaseScope,
    DatabaseSpec, DatabaseTls, DirBase, DisplayBadge, DisplayLink, DisplaySource,
    DisplaySourceKind, DisplaySpec, DisplayTable, ExistingBranchBehavior, ExistingBranchMatch,
    FieldDefault, FieldKind, FieldSpec, ForbidRule, GuardSpec, LookupErrorPolicy, LookupFormat,
    LookupMapping, LookupSpec, McpServerSpec, NamingSpec, OnFailure, OptionsParse, OptionsSource,
    Project, ProjectId, ProjectMeta, RemoveSpec, RemoveStrategy, Rewrite, SUPPORTED_SCHEMA_VERSION,
    SetupSpec, TrackModeSpec,
};
pub use value::{FieldValue, FormValues};
pub use worktree::{
    BranchRef, Checkout, CommitId, RepoFacts, WorkingTreeStatus, Worktree, WorktreeId,
};
