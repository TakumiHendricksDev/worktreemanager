//! What we intend to do, and what happened.
//!
//! # The boundary these types encode
//!
//! > Stages 1–6 of the create pipeline perform **zero** mutations. Every mutating
//! > operation is in stage 7 or later.
//!
//! [`PlanPreview`] is everything known at the end of stage 6b. Because producing
//! it cannot change anything, `preview` and `execute` are the same code with a
//! stop-after parameter, a failed preview is infinitely retryable with nothing to
//! clean up, and the review screen can show the *exact* argv that will run before
//! anything has happened.
//!
//! # Why there is no rollback variant
//!
//! [`CreateOutcome::SetupFailed`] is a success value, not an error. By the time a
//! project's setup command fails it may have written an environment file,
//! allocated ports, copied editor config and cloned a multi-gigabyte database
//! volume. Quietly removing the worktree to leave a tidy-looking failure would
//! leak those resources and destroy work that is usually one command from fixed.
//! So the pipeline keeps what exists and returns [`Remedy`] options instead.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::worktree::{BranchRef, CommitId, Worktree};

/// How the new branch relates to its base — the resolved decision, not the config
/// preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackMode {
    /// `--no-track -b`: branch off the base without inheriting it as upstream.
    NoTrack,
    /// `--track -b`: used when adopting a remote-only branch, where an upstream is
    /// exactly what you want.
    Track,
    /// `--detach`: no branch at all.
    Detach,
}

/// What to do about the branch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BranchPlan {
    /// Create a new branch from the base.
    Create { branch: BranchRef, track: TrackMode },
    /// Check out a branch that already exists locally.
    UseLocal { branch: BranchRef },
    /// Create a local tracking branch from a remote-only branch.
    AdoptRemote { branch: BranchRef, remote: String },
    /// No branch.
    Detach,
}

impl BranchPlan {
    #[must_use]
    pub fn branch(&self) -> Option<&BranchRef> {
        match self {
            Self::Create { branch, .. }
            | Self::UseLocal { branch }
            | Self::AdoptRemote { branch, .. } => Some(branch),
            Self::Detach => None,
        }
    }
}

/// Everything decided before anything is touched.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreatePlan {
    pub branch_plan: BranchPlan,
    /// Absolute, normalized target directory.
    pub directory: PathBuf,
    /// The base ref as the user chose it (`origin/develop`).
    pub base_ref: String,
    /// What that ref resolved to, if it resolved.
    pub base_commit: Option<CommitId>,
    /// Whether a fetch will be attempted first.
    pub will_fetch: bool,
    /// The literal `git worktree add …` argv. Shown to the user verbatim, because
    /// "trust me" is not a review.
    pub git_argv: Vec<String>,
    /// The setup argv, if the project declares one.
    pub setup_argv: Option<Vec<String>>,
    /// Where setup will run. Surfaced deliberately: a project's setup often has to
    /// run from the repo root rather than the new worktree, and that is surprising
    /// enough to show rather than hide.
    pub setup_cwd: Option<PathBuf>,
}

/// A non-blocking observation from planning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanWarning {
    /// Stable identifier, for tests and for suppression.
    pub id: String,
    pub message: String,
}

impl PlanWarning {
    pub fn new(id: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreflightSeverity {
    /// Blocks Create outright.
    Error,
    /// Requires an explicit acknowledgement.
    Warn,
    /// Informational.
    Info,
}

/// One preflight check's result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreflightItem {
    pub id: String,
    pub severity: PreflightSeverity,
    pub message: String,
    /// Whether the user may proceed anyway. An `Error` that is overridable becomes
    /// a "force" checkbox; one that isn't stays fatal.
    pub overridable: bool,
    /// What to do about it, when there is a concrete answer.
    #[serde(default)]
    pub hint: Option<String>,
}

impl PreflightItem {
    pub fn error(id: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            severity: PreflightSeverity::Error,
            message: message.into(),
            overridable: false,
            hint: None,
        }
    }

    pub fn warn(id: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            severity: PreflightSeverity::Warn,
            message: message.into(),
            overridable: true,
            hint: None,
        }
    }

    #[must_use]
    pub fn overridable(mut self) -> Self {
        self.overridable = true;
        self
    }

    #[must_use]
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    #[must_use]
    pub fn blocks(&self) -> bool {
        self.severity == PreflightSeverity::Error && !self.overridable
    }
}

/// The result of stages 1–6b: the review screen's entire contents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanPreview {
    pub plan: CreatePlan,
    pub preflight: Vec<PreflightItem>,
    pub warnings: Vec<PlanWarning>,
    /// Resolved lookup tokens, so the user can see what Jira actually returned.
    pub lookups: std::collections::BTreeMap<String, String>,
    /// Resolved `[computed]` values.
    pub computed: std::collections::BTreeMap<String, String>,
    /// Existing branches matching the configured pattern, when the project asked
    /// to be offered a choice. This is the GUI form of a numbered stdin picker.
    pub branch_choices: Vec<BranchChoice>,
}

impl PlanPreview {
    /// Whether Create may proceed without any override.
    #[must_use]
    pub fn is_clear(&self) -> bool {
        !self
            .preflight
            .iter()
            .any(|i| i.severity == PreflightSeverity::Error)
    }

    /// Whether Create is possible at all, even with overrides ticked.
    #[must_use]
    pub fn is_blocked(&self) -> bool {
        self.preflight.iter().any(PreflightItem::blocks)
    }
}

/// An existing branch the user could adopt instead of creating a new one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchChoice {
    pub branch: BranchRef,
    /// True when it exists only on the remote.
    pub remote_only: bool,
    /// Directory that would be used if this branch is adopted.
    pub directory: PathBuf,
}

/// How a command finished.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExitOutcome {
    Success,
    Failed {
        code: i32,
    },
    /// Killed by a signal — includes the case where we killed the process group
    /// ourselves on cancel or timeout.
    Signalled {
        signal: i32,
    },
    TimedOut {
        after_ms: u64,
    },
    Cancelled,
}

impl ExitOutcome {
    #[must_use]
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Success)
    }
}

/// A PTY session identifier.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(String);

impl SessionId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What the user can do about a partially-created worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Remedy {
    /// Re-run the setup command against the existing worktree.
    ///
    /// The same code path as "adopt an existing worktree", so this is one
    /// implementation with two callers rather than a bespoke retry.
    RetrySetup,
    /// Open an interactive shell in the worktree to fix it by hand.
    OpenShell,
    /// Remove it, routed through the normal remove pipeline so the project's
    /// configured teardown steps still run.
    RemoveWorktree,
}

/// The result of a create attempt.
///
/// Note that a failed setup is a *successful* return: the operation produced a
/// real worktree and a diagnosis. See the module docs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CreateOutcome {
    Created {
        worktree: Worktree,
        /// The setup session, kept so its transcript stays readable afterwards.
        setup_session: Option<SessionId>,
    },
    SetupFailed {
        worktree: Worktree,
        session: SessionId,
        outcome: ExitOutcome,
        remedies: Vec<Remedy>,
    },
    /// Cancelled. The worktree is `Some` when cancellation happened after stage 8,
    /// in which case the same remedies apply.
    Cancelled {
        worktree: Option<Worktree>,
        session: Option<SessionId>,
    },
}

impl CreateOutcome {
    #[must_use]
    pub fn worktree(&self) -> Option<&Worktree> {
        match self {
            Self::Created { worktree, .. } | Self::SetupFailed { worktree, .. } => Some(worktree),
            Self::Cancelled { worktree, .. } => worktree.as_ref(),
        }
    }

    /// The standard remedy set offered whenever a worktree exists but is not
    /// known-good.
    #[must_use]
    pub fn default_remedies() -> Vec<Remedy> {
        vec![
            Remedy::RetrySetup,
            Remedy::OpenShell,
            Remedy::RemoveWorktree,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn preview_with(items: Vec<PreflightItem>) -> PlanPreview {
        PlanPreview {
            plan: CreatePlan {
                branch_plan: BranchPlan::Detach,
                directory: PathBuf::from("/tmp/x"),
                base_ref: "HEAD".to_owned(),
                base_commit: None,
                will_fetch: false,
                git_argv: vec![],
                setup_argv: None,
                setup_cwd: None,
            },
            preflight: items,
            warnings: vec![],
            lookups: std::collections::BTreeMap::default(),
            computed: std::collections::BTreeMap::default(),
            branch_choices: vec![],
        }
    }

    #[test]
    fn a_clear_preview_has_no_errors() {
        assert!(preview_with(vec![]).is_clear());
        assert!(preview_with(vec![PreflightItem::warn("w", "careful")]).is_clear());
        assert!(!preview_with(vec![PreflightItem::error("e", "no")]).is_clear());
    }

    #[test]
    fn an_overridable_error_is_not_blocked() {
        // A dirty worktree is an error you may force past; an existing directory
        // is not.
        let overridable = preview_with(vec![PreflightItem::error("e", "dirty").overridable()]);
        assert!(!overridable.is_clear(), "still needs an acknowledgement");
        assert!(!overridable.is_blocked(), "but must remain forceable");

        let hard = preview_with(vec![PreflightItem::error("e", "path exists")]);
        assert!(hard.is_blocked());
    }

    #[test]
    fn branch_plan_exposes_its_branch_except_when_detached() {
        let b = BranchRef::new("task/x");
        assert_eq!(
            BranchPlan::Create {
                branch: b.clone(),
                track: TrackMode::NoTrack
            }
            .branch(),
            Some(&b)
        );
        assert_eq!(
            BranchPlan::UseLocal { branch: b.clone() }.branch(),
            Some(&b)
        );
        assert_eq!(BranchPlan::Detach.branch(), None);
    }

    #[test]
    fn setup_failure_still_reports_the_worktree_it_made() {
        // The whole point: a failed setup must not lose track of what exists.
        let wt = Worktree {
            id: super::super::worktree::WorktreeId::from_path(std::path::Path::new("/tmp/w")),
            path: PathBuf::from("/tmp/w"),
            head: None,
            checkout: super::super::worktree::Checkout::Detached,
            is_main: false,
            is_bare: false,
            locked: None,
            prunable: None,
        };
        let outcome = CreateOutcome::SetupFailed {
            worktree: wt,
            session: SessionId::new("s1"),
            outcome: ExitOutcome::Failed { code: 1 },
            remedies: CreateOutcome::default_remedies(),
        };
        assert!(outcome.worktree().is_some());
        assert_eq!(CreateOutcome::default_remedies().len(), 3);
    }

    #[test]
    fn timed_out_is_not_success() {
        assert!(ExitOutcome::Success.is_success());
        assert!(!ExitOutcome::TimedOut { after_ms: 1 }.is_success());
        assert!(!ExitOutcome::Cancelled.is_success());
    }
}
