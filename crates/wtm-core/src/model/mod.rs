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

pub mod naming;
pub mod plan;
pub mod project;
pub mod value;
pub mod worktree;

pub use naming::{RESERVED_PREFIXES, TokenScope, TokenSet, namespace_of, shadows_reserved_prefix};
pub use plan::{
    BranchChoice, BranchPlan, CreateOutcome, CreatePlan, ExitOutcome, PlanPreview, PlanWarning,
    PreflightItem, PreflightSeverity, Remedy, SessionId, TrackMode,
};
pub use project::{
    ActionSpec, BranchScope, CommandSpec, ComputedSpec, Concurrency, ConditionalArgs, CreateSpec,
    CwdBase, DirBase, DisplayBadge, DisplayLink, DisplaySource, DisplaySourceKind, DisplaySpec,
    DisplayTable, ExistingBranchBehavior, ExistingBranchMatch, FieldDefault, FieldKind, FieldSpec,
    ForbidRule, GuardSpec, LookupErrorPolicy, LookupFormat, LookupMapping, LookupSpec, NamingSpec,
    OnFailure, OptionsParse, OptionsSource, Project, ProjectId, ProjectMeta, RemoveSpec,
    RemoveStrategy, Rewrite, SUPPORTED_SCHEMA_VERSION, SetupSpec, TrackModeSpec,
};
pub use value::{FieldValue, FormValues};
pub use worktree::{
    BranchRef, Checkout, CommitId, RepoFacts, WorkingTreeStatus, Worktree, WorktreeId,
};
