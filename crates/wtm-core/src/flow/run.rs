//! A review run's working records: its steps, the sessions each step opened, and what it needs
//! you for — plus the rules for what happens after an attempt fails or a provider runs dry.
//!
//! # Steps and invocations
//!
//! A **step** is one role of one run: "the conventions role of run 12". It is what the Reviews tab
//! lists, and it holds the role's result once there is one. An **invocation** is one attempt at a
//! step: one headless session, opened with a profile, against a snapshot, that either submitted a
//! result or did not. A step that timed out and was retried has two invocations. Keeping both is
//! what lets "retried once, then stopped" be said from the record rather than remembered.
//!
//! # Why a failure, a limit and an interruption are different
//!
//! They look alike from inside a session — the turn ends without a result — and they mean opposite
//! things about what to do next.
//!
//! - A **failure** is the role's: it timed out, its session died, it never submitted. That spends
//!   one of the role's retries, and after the last one its configured fallback runs, or you are asked.
//! - A **limit** is the account's. Retrying spends nothing useful until the window resets, and it is
//!   not the role's fault, so it spends no retry. The run waits, or switches to the role's
//!   configured fallback on another provider; see [`on_limit`].
//! - An **interruption** is the app's: it quit, or crashed, with the session mid-turn. Nothing is
//!   re-sent by itself, because a turn sent twice can do its work twice; you resume it.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::UnixMs;
use super::recipe::{RoleProfile, RoleProvider, SkillProblem, Workspace};
use super::state::FlowId;
use super::submission::Submission;
use crate::model::ProjectId;

id! {
    /// One role of one run, as the store numbers it.
    StepId
}

id! {
    /// One attempt at a step.
    InvocationId
}

id! {
    /// A snapshot a run recorded.
    SnapshotId
}

id! {
    /// Something a run needs you to know or decide.
    InboxItemId
}

id! {
    /// One run of a check, as the store numbers it.
    CheckRunId
}

/// What a review run is of: which worktree, which recipe, measured from which base.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewRun {
    pub flow: FlowId,
    pub project: ProjectId,
    /// The worktree's id, which the app's lookups take.
    pub worktree: String,
    /// Where it was on disk when the run started, so the record still says what was reviewed after
    /// the worktree is gone.
    pub worktree_path: String,
    pub recipe: String,
    /// The ref the change is measured from, as it was asked for.
    pub base: String,
    /// Set once the snapshot is taken; `None` while the run is still resolving.
    pub snapshot: Option<SnapshotId>,
    pub created_at: UnixMs,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewReviewRun {
    pub flow: FlowId,
    pub project: ProjectId,
    pub worktree: String,
    pub worktree_path: String,
    pub recipe: String,
    pub base: String,
}

/// Where a step is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    /// Planned, not started.
    Pending,
    Running,
    /// Its provider's usage limit was reached, and it waits for the reset.
    WaitingCapacity,
    /// Submitted a result.
    Done,
    /// Ran out of attempts, or cannot run at all, and has no fallback left.
    Failed,
    /// Its configured fallback ran in its place. The fallback's own step holds the result.
    Replaced,
    /// The app quit or crashed while it ran.
    Interrupted,
    Cancelled,
    /// The plan left it out: none of its paths changed.
    Skipped,
}

impl StepStatus {
    /// Whether nothing more will happen to it unless someone asks.
    #[must_use]
    pub fn is_settled(self) -> bool {
        matches!(
            self,
            Self::Done | Self::Failed | Self::Replaced | Self::Cancelled | Self::Skipped
        )
    }

    /// Whether resuming a run should run this step again.
    ///
    /// Everything without a result that nothing else stands in for. A step that was `Running` when
    /// the record was last written belongs to a run whose worker is gone, which is what resuming is.
    #[must_use]
    pub fn wants_a_run(self) -> bool {
        matches!(
            self,
            Self::Pending
                | Self::Running
                | Self::WaitingCapacity
                | Self::Failed
                | Self::Interrupted
        )
    }
}

/// One role of one run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Step {
    pub id: StepId,
    pub flow: FlowId,
    /// Which stage of the recipe, from 0. Steps of one stage run together.
    pub stage: u32,
    pub role: String,
    pub provider: RoleProvider,
    pub model: Option<String>,
    pub profile: RoleProfile,
    pub workspace: Workspace,
    pub timeout_ms: u64,
    pub retries: u32,
    pub status: StepStatus,
    /// Attempts that failed, which is what retries are counted against. A limit is not one.
    pub failures: u32,
    /// The role this one runs in place of, for a fallback.
    pub stands_in_for: Option<String>,
    /// What the role it stands in for got to, handed over as text. Never a session: a fallback on
    /// another provider cannot resume the first one's conversation.
    pub handover: Option<String>,
    /// Why it is where it is, in a sentence: the failure, the limit, the skip.
    pub detail: Option<String>,
    /// When a step waiting for capacity may run again, in Unix seconds, when the provider said.
    pub resets_at: Option<u64>,
    /// Set when the role cannot run at all for want of a skill, which no retry fixes: the recipe
    /// has to change. Kept as the typed problem so the Reviews tab can say which skill and which
    /// provider without reading it back out of `detail`.
    #[serde(default)]
    pub skill_problem: Option<SkillProblem>,
    pub result: Option<Submission>,
    pub created_at: UnixMs,
    pub updated_at: UnixMs,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewStep {
    pub stage: u32,
    pub role: String,
    pub provider: RoleProvider,
    pub model: Option<String>,
    pub profile: RoleProfile,
    pub workspace: Workspace,
    pub timeout_ms: u64,
    pub retries: u32,
    pub status: StepStatus,
    pub stands_in_for: Option<String>,
    pub handover: Option<String>,
    pub detail: Option<String>,
}

/// What a change does to a field that may be empty: leave it, set it, or empty it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Field<T> {
    #[default]
    Keep,
    Set(T),
    Clear,
}

impl<T: Clone> Field<T> {
    /// Apply this to `slot`.
    pub fn apply(&self, slot: &mut Option<T>) {
        match self {
            Self::Keep => {}
            Self::Set(value) => *slot = Some(value.clone()),
            Self::Clear => *slot = None,
        }
    }
}

impl<T> From<Option<T>> for Field<T> {
    /// `Some` sets the field and `None` empties it, which is what writing an optional value means.
    fn from(value: Option<T>) -> Self {
        value.map_or(Self::Clear, Self::Set)
    }
}

/// A change to a step. Each `Some` replaces that field; `None` leaves it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StepChange {
    pub status: Option<StepStatus>,
    pub failures: Option<u32>,
    pub detail: Field<String>,
    pub resets_at: Field<u64>,
    pub skill_problem: Field<SkillProblem>,
    pub result: Option<Submission>,
}

impl StepChange {
    #[must_use]
    pub fn status(status: StepStatus) -> Self {
        Self {
            status: Some(status),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Field::Set(detail.into());
        self
    }
}

/// Where an invocation is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvocationStatus {
    /// Preflight, profile, canary and spawn.
    Opening,
    Running,
    /// Handed in a result that passed its schema.
    Submitted,
    Failed,
    TimedOut,
    LimitReached,
    Interrupted,
    Cancelled,
}

impl InvocationStatus {
    #[must_use]
    pub fn is_live(self) -> bool {
        matches!(self, Self::Opening | Self::Running)
    }
}

/// One attempt at a step: one session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Invocation {
    pub id: InvocationId,
    pub flow: FlowId,
    pub step: StepId,
    /// 1 for a step's first attempt.
    pub attempt: u32,
    pub role: String,
    pub provider: RoleProvider,
    pub model: Option<String>,
    pub profile: RoleProfile,
    pub snapshot: Option<SnapshotId>,
    /// Where the session ran: the snapshot checkout or the worktree.
    pub cwd: String,
    /// wtm's id for the session, once it opened.
    pub session: Option<String>,
    /// The CLI's own conversation id, which a resume would need.
    pub provider_session: Option<String>,
    pub status: InvocationStatus,
    pub detail: Option<String>,
    /// The role's last `flow_progress` line.
    pub progress: Option<String>,
    /// The repository instruction files its brief carried, by path, so the Reviews tab can say
    /// which sources a review applied rather than which ones the config would name today.
    #[serde(default)]
    pub instruction_sources: Vec<String>,
    pub started_at: UnixMs,
    pub ended_at: Option<UnixMs>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewInvocation {
    pub flow: FlowId,
    pub step: StepId,
    pub attempt: u32,
    pub role: String,
    pub provider: RoleProvider,
    pub model: Option<String>,
    pub profile: RoleProfile,
    pub snapshot: Option<SnapshotId>,
    pub cwd: String,
}

/// A change to an invocation. Each `Some` replaces that field.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InvocationChange {
    pub status: Option<InvocationStatus>,
    pub session: Option<String>,
    pub provider_session: Option<String>,
    pub detail: Option<String>,
    pub progress: Option<String>,
    pub instruction_sources: Option<Vec<String>>,
    /// Set with a status that ends it.
    pub ended_at: Option<UnixMs>,
}

impl InvocationChange {
    /// End the invocation as `status`, saying why.
    #[must_use]
    pub fn ended(status: InvocationStatus, detail: Option<String>, at: UnixMs) -> Self {
        Self {
            status: Some(status),
            detail,
            ended_at: Some(at),
            ..Self::default()
        }
    }
}

/// What an inbox item is about.
///
/// The minimal set a review run raises. Questions and approvals arrive with the items that first
/// ask them; until then a provider's own permission prompt is the only thing a run session can ask
/// you, and it lands here rather than on a pane nobody is looking at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InboxKind {
    /// A role failed for good: retries spent, no fallback left.
    Failure,
    /// A provider's usage limit paused a role, or switched it to its fallback.
    Capacity,
    /// A run session's CLI asked for a permission. Under a run's profile it should never need to,
    /// so this is worth knowing about whether or not anyone answers it.
    Permission,
    /// The app went away mid-run, and the run waits for you to resume it.
    Interrupted,
}

/// Something a run needs you to know or decide. Kept until resolved; dismissing a notification
/// about it resolves nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InboxItem {
    pub id: InboxItemId,
    pub flow: FlowId,
    pub kind: InboxKind,
    /// What within the run it is about, such as a role id. One open item per `(flow, kind,
    /// subject)`: a role that fails twice is one item, not two.
    pub subject: String,
    pub title: String,
    pub body: String,
    pub created_at: UnixMs,
    pub resolved_at: Option<UnixMs>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewInboxItem {
    pub flow: FlowId,
    pub kind: InboxKind,
    pub subject: String,
    pub title: String,
    pub body: String,
}

/// How a check the app ran for a role came out.
///
/// There is no "unknown" and nothing that reads as a pass unless the command exited 0: a check that
/// could not start is an error, one that ran too long timed out, and one whose services are not
/// running was skipped, with the reason. A check nobody ran has no record at all, which the Reviews
/// tab shows as not run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Passed,
    Failed,
    TimedOut,
    /// Not run, on purpose, for the reason in the record.
    Skipped,
    /// It could not be started at all, such as a program not on the PATH.
    Error,
}

/// One run of a configured check, at a role's request. Its record is the evidence: a role can say
/// a check passed only by pointing at one of these.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckRun {
    pub id: CheckRunId,
    pub flow: FlowId,
    /// The check's id in `[workflow.check]`.
    pub check: String,
    pub step: Option<StepId>,
    pub invocation: Option<InvocationId>,
    pub role: Option<String>,
    pub status: CheckStatus,
    /// Why it was skipped or could not start, or the end of what it printed when it failed.
    pub detail: Option<String>,
    pub exit_code: Option<i32>,
    /// The tree it ran against: the snapshot the asking role read.
    pub tree_sha: String,
    /// Where it ran.
    pub cwd: String,
    /// Everything it printed, under the flow's directory.
    pub output: Option<super::review::BlobRef>,
    pub started_at: UnixMs,
    pub ended_at: UnixMs,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewCheckRun {
    pub flow: FlowId,
    pub check: String,
    pub step: Option<StepId>,
    pub invocation: Option<InvocationId>,
    pub role: Option<String>,
    pub status: CheckStatus,
    pub detail: Option<String>,
    pub exit_code: Option<i32>,
    pub tree_sha: String,
    pub cwd: String,
    pub output: Option<super::review::BlobRef>,
    pub started_at: UnixMs,
}

/// What follows an attempt that ended without a result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AfterFailure {
    /// Try the same role again.
    Retry,
    /// Run this configured fallback role in its place.
    Fallback(String),
    /// Stop, and put it in front of you.
    Ask,
}

/// Decide what follows a failed attempt, given how many attempts have failed (this one included),
/// the role's retry budget, its fallback chain, and the roles this run has already run.
///
/// A fallback already run in this flow is passed over rather than run twice: it either has its own
/// result or has failed on its own account.
#[must_use]
pub fn after_failure(
    failures: u32,
    retries: u32,
    fallbacks: &[String],
    already_run: &BTreeSet<String>,
) -> AfterFailure {
    if failures <= retries {
        return AfterFailure::Retry;
    }
    fallbacks
        .iter()
        .find(|role| !already_run.contains(*role))
        .map_or(AfterFailure::Ask, |role| {
            AfterFailure::Fallback(role.clone())
        })
}

/// What a usage limit on `limited` means for a role whose fallback chain is `fallbacks`, each with
/// its provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OnLimit {
    /// Hand over to this fallback, which runs on a provider that is not out of capacity.
    SwitchTo(String),
    /// Wait for the reset. No fallback is configured on another provider, and nothing switches by
    /// itself: switching provider changes what a review is worth.
    Wait,
}

/// Decide what a limit on `limited` does to a role. Only a fallback the role's own config names
/// is ever used, and only one on a provider that is not the one that ran out — a fallback on the
/// same account would meet the same limit. Never an API key: the run's profile scrubs them all.
#[must_use]
pub fn on_limit(
    limited: RoleProvider,
    fallbacks: &[(String, RoleProvider)],
    already_run: &BTreeSet<String>,
) -> OnLimit {
    fallbacks
        .iter()
        .find(|(role, provider)| *provider != limited && !already_run.contains(role))
        .map_or(OnLimit::Wait, |(role, _)| OnLimit::SwitchTo(role.clone()))
}

/// When a step waiting for `resets_at` (Unix seconds) may run again, in the clock's milliseconds.
/// `None` when the provider did not say: an unknown reset is waited on until you resume it, never
/// guessed at.
#[must_use]
pub fn resume_at(resets_at: Option<u64>) -> Option<UnixMs> {
    resets_at.map(|seconds| seconds.saturating_mul(1000))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(roles: &[&str]) -> BTreeSet<String> {
        roles.iter().map(|r| (*r).to_owned()).collect()
    }

    fn chain(roles: &[&str]) -> Vec<String> {
        roles.iter().map(|r| (*r).to_owned()).collect()
    }

    #[test]
    fn a_failed_attempt_is_retried_until_the_roles_retries_are_spent() {
        assert_eq!(after_failure(1, 1, &[], &set(&[])), AfterFailure::Retry);
        assert_eq!(after_failure(2, 1, &[], &set(&[])), AfterFailure::Ask);
        assert_eq!(after_failure(1, 0, &[], &set(&[])), AfterFailure::Ask);
    }

    #[test]
    fn after_the_last_retry_the_first_fallback_not_yet_run_takes_over() {
        let fallbacks = chain(&["ui-codex", "ui-haiku"]);
        assert_eq!(
            after_failure(1, 0, &fallbacks, &set(&["ui"])),
            AfterFailure::Fallback("ui-codex".to_owned())
        );
        assert_eq!(
            after_failure(1, 0, &fallbacks, &set(&["ui", "ui-codex"])),
            AfterFailure::Fallback("ui-haiku".to_owned())
        );
        assert_eq!(
            after_failure(1, 0, &fallbacks, &set(&["ui", "ui-codex", "ui-haiku"])),
            AfterFailure::Ask
        );
    }

    #[test]
    fn a_limit_switches_only_to_a_configured_fallback_on_another_provider() {
        let fallbacks = vec![
            ("same-account".to_owned(), RoleProvider::Codex),
            ("on-claude".to_owned(), RoleProvider::Claude),
        ];
        assert_eq!(
            on_limit(RoleProvider::Codex, &fallbacks, &set(&[])),
            OnLimit::SwitchTo("on-claude".to_owned())
        );
        assert_eq!(on_limit(RoleProvider::Codex, &[], &set(&[])), OnLimit::Wait);
        assert_eq!(
            on_limit(RoleProvider::Claude, &fallbacks[1..], &set(&[])),
            OnLimit::Wait,
            "a fallback on the provider that ran out would meet the same limit"
        );
    }

    #[test]
    fn an_unknown_reset_is_never_guessed() {
        assert_eq!(resume_at(None), None);
        assert_eq!(resume_at(Some(1_800_000_000)), Some(1_800_000_000_000));
    }

    #[test]
    fn a_step_cut_off_mid_run_is_run_again_on_resume_and_a_finished_one_is_not() {
        for status in [
            StepStatus::Pending,
            StepStatus::Running,
            StepStatus::WaitingCapacity,
            StepStatus::Failed,
            StepStatus::Interrupted,
        ] {
            assert!(status.wants_a_run(), "{status:?}");
        }
        for status in [
            StepStatus::Done,
            StepStatus::Replaced,
            StepStatus::Cancelled,
            StepStatus::Skipped,
        ] {
            assert!(!status.wants_a_run(), "{status:?}");
        }
    }
}
