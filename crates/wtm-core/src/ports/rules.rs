//! Durable storage for review rules and the record of which reviews read them.
//!
//! # Separate from [`FlowStore`](super::FlowStore)
//!
//! Settings › Knowledge edits rules and never touches a flow, and the engine records which rules a
//! role read without editing any. One store implements both ports, but a caller that needs only
//! rules depends only on this one, which is the interface segregation ARCHITECTURE §2 asks for.
//!
//! # Every rule change goes through the rules
//!
//! A store creates a rule only from a [`NewRule`], whose constructors decide its origin and
//! approval, and changes one only through [`Rule::change`](crate::flow::Rule::change) or
//! [`Rule::supersede`](crate::flow::Rule::supersede), whose answer it writes whole. It cannot be
//! asked to set a status or an approval directly, so it cannot be asked to activate a proposal
//! nobody approved.
//!
//! # Append-only
//!
//! Revisions and applications are never rewritten or deleted, and neither is a rule. Findings cite
//! revisions, and applications are the evidence of what a review read; both would lie if either
//! could change afterwards.

use serde::Serialize;

use crate::flow::{
    FlowId, NewRule, RecordedApplication, ReviewInvocation, Rule, RuleChange, RuleDraft, RuleError,
    RuleId, RuleUsage, SelectedRule, UnixMs,
};
use crate::model::ProjectId;

#[derive(Debug, thiserror::Error, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuleStoreError {
    #[error("no rule {0}")]
    UnknownRule(RuleId),

    #[error("rule {rule} has no revision {revision}")]
    UnknownRevision { rule: RuleId, revision: u32 },

    #[error("no flow {0}")]
    UnknownFlow(FlowId),

    /// A flow was given a rule from another project. Retrieval never does this, so it is a bug in
    /// the caller, and recording it would make the rule look as though it governed that project.
    #[error("rule {rule} belongs to another project than flow {flow}")]
    OtherProject { rule: RuleId, flow: FlowId },

    /// That invocation's rules are already recorded. A second attempt is a new invocation.
    #[error("the rules {role} read in attempt {attempt} of flow {flow} are already recorded")]
    AlreadyRecorded {
        flow: FlowId,
        role: String,
        attempt: u32,
    },

    /// The rule rules refused the change. Nothing was written.
    #[error(transparent)]
    Refused(#[from] RuleError),

    /// The store itself failed, with the driver's message. Nothing in this store is a secret.
    #[error("rule store: {0}")]
    Storage(String),
}

/// Review rules and their applications.
pub trait RuleStore: Send + Sync {
    /// Save a rule with its first revision.
    fn create_rule(&self, rule: &NewRule, at: UnixMs) -> Result<Rule, RuleStoreError>;

    fn rule(&self, id: RuleId) -> Result<Option<Rule>, RuleStoreError>;

    /// Every rule of the project, whatever its status, oldest first, each with all its revisions.
    fn rules(&self, project: &ProjectId) -> Result<Vec<Rule>, RuleStoreError>;

    /// Apply `change` to the rule and write the result, returning the rule as it now is.
    fn change_rule(
        &self,
        id: RuleId,
        change: &RuleChange,
        at: UnixMs,
    ) -> Result<Rule, RuleStoreError>;

    /// Save `draft` as a new rule of yours and mark `id` superseded by it, in one write. Returns
    /// the old rule and then the new one.
    fn replace_rule(
        &self,
        id: RuleId,
        draft: &RuleDraft,
        at: UnixMs,
    ) -> Result<(Rule, Rule), RuleStoreError>;

    /// Record the rule revisions `invocation` was given. Refused whole if any revision does not
    /// exist, belongs to another project, or the invocation is already recorded.
    fn record_applications(
        &self,
        invocation: &ReviewInvocation,
        rules: &[SelectedRule],
        at: UnixMs,
    ) -> Result<(), RuleStoreError>;

    /// What every invocation in `flow` was given, in the order it was recorded.
    fn applications(&self, flow: FlowId) -> Result<Vec<RecordedApplication>, RuleStoreError>;

    /// How often each of the project's rules has been given to a review. Rules never given are
    /// absent.
    fn usage(&self, project: &ProjectId) -> Result<Vec<RuleUsage>, RuleStoreError>;
}
