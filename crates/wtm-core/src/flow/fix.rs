//! Local fixes: the writer lease that gives one writer a worktree to itself, and the scope its
//! commits are held to.
//!
//! # One writer per worktree, and never beside a reviewer
//!
//! A writer edits the worktree; a reviewer reads it. A review reading a tree mid-edit reports on
//! something nobody wrote, and two writers build on each other's half-done work. So a worktree is
//! held by a [`Lease`]: at most one open writer lease, and a writer lease never beside an open
//! reviewer lease, in either order. [`lease_conflict`] is that rule, and the store enforces it in
//! the same transaction that grants a lease, so two clicks cannot both win.
//!
//! # A lease outlives the process that took it
//!
//! A writer lease is a row, not a lock in memory. A fix interrupted by a quit or a crash still holds
//! its worktree after the relaunch, because the worktree may hold the writer's half-done edits, and
//! a review or a second writer would read or build on them. The fix's own end releases it: ready,
//! failed or cancelled. Recovery at launch releases only what nothing can come back for — a lease
//! whose flow has ended, or a reviewer lease, whose review thread cannot have survived the process.
//!
//! # A fix is held to the files its findings name
//!
//! You choose findings, and the writer fixes those. Its commits are the app's
//! (`flow_checkpoint`), and before each one the changed paths are checked against the selected
//! findings' files ([`FixScope`]). A file outside them is an outlier: the commit is refused and you
//! are asked, because a writer that wandered is either right about something nobody told it or
//! wrong about its task, and only you can tell which. Allowing an outlier adds it to the fix.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::UnixMs;
use super::consolidate::FindingRecord;
use super::question::QuestionId;
use super::review::FindingId;
use super::run::{Field, StepId};
use super::state::FlowId;

id! {
    /// One lease on a worktree, as the store numbers it.
    LeaseId
}

id! {
    /// One local fix, as the store numbers it.
    FixId
}

// ═══════════════════════════════ the lease ═══════════════════════════════

/// What a lease lets its holder do to the worktree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseKind {
    /// Edits it. Exclusive.
    Writer,
    /// Reads it. Any number at once, but never beside a writer.
    Reviewer,
}

/// Why a lease was let go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeaseRelease {
    /// The work it was taken for finished.
    Completed,
    /// You stopped that work.
    Cancelled,
    /// The work failed for good.
    Failed,
    /// Recovery at launch found nothing left that could use it.
    Recovered,
}

/// A lease to take, not yet numbered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewLease {
    /// The worktree's path, as the review run recorded it.
    pub worktree: String,
    pub flow: FlowId,
    pub kind: LeaseKind,
    /// Who holds it: a writer's role, or the review's recipe.
    pub holder: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Lease {
    pub id: LeaseId,
    pub worktree: String,
    pub flow: FlowId,
    pub kind: LeaseKind,
    pub holder: String,
    pub acquired_at: UnixMs,
    pub released_at: Option<UnixMs>,
    pub released_why: Option<LeaseRelease>,
}

impl Lease {
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.released_at.is_none()
    }
}

/// The open lease that stops a `kind` lease being taken on the same worktree, if any.
///
/// A writer is stopped by any open lease, a reviewer only by an open writer. `open` is every open
/// lease on that worktree; a released one in it is ignored.
#[must_use]
pub fn lease_conflict(kind: LeaseKind, open: &[Lease]) -> Option<&Lease> {
    open.iter()
        .filter(|lease| lease.is_open())
        .find(|lease| kind == LeaseKind::Writer || lease.kind == LeaseKind::Writer)
}

// ═══════════════════════════════ the scope ═══════════════════════════════

/// Why a fix was not started, or a scope not made. Nothing was recorded.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize)]
#[error("{0}")]
pub struct FixError(pub String);

/// The files a fix may change: those its selected findings are about.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FixScope {
    pub paths: BTreeSet<String>,
}

impl FixScope {
    /// The scope of fixing `picks` among a review's `findings`.
    ///
    /// # Errors
    ///
    /// When nothing is picked, a pick is not one of the review's open findings, or a pick is about
    /// the change as a whole: a finding with no file gives the writer no file it may change, so a
    /// scope check could only refuse everything it did.
    pub fn of(findings: &[FindingRecord], picks: &[FindingId]) -> Result<Self, FixError> {
        if picks.is_empty() {
            return Err(FixError(
                "Pick at least one finding for the writer to fix.".to_owned(),
            ));
        }
        let mut paths = BTreeSet::new();
        for pick in picks {
            let record = findings
                .iter()
                .find(|record| record.finding.id == *pick)
                .filter(|record| record.finding.dismissed.is_none())
                .ok_or_else(|| {
                    FixError(format!(
                        "Finding {pick} is not one of this review's open findings any more; read \
                         the review again."
                    ))
                })?;
            let location = record.finding.location.as_ref().ok_or_else(|| {
                FixError(format!(
                    "“{}” is about the change as a whole, so it names no file a fix could be held \
                     to. Leave it out, or publish it as a comment.",
                    record.finding.title
                ))
            })?;
            paths.insert(location.path.clone());
        }
        Ok(Self { paths })
    }

    /// The paths in `changed` that are neither in the scope nor in `allowed`, in order.
    #[must_use]
    pub fn outliers(&self, changed: &[String], allowed: &[String]) -> Vec<String> {
        let mut seen = BTreeSet::new();
        changed
            .iter()
            .filter(|path| !self.paths.contains(*path) && !allowed.contains(path))
            .filter(|path| seen.insert((*path).clone()))
            .cloned()
            .collect()
    }
}

// ═══════════════════════════════ the fix ═══════════════════════════════

/// Where a fix is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixState {
    /// The writer has the worktree.
    Writing,
    /// The writer finished, and the diff waits for you.
    Ready,
    /// The writer failed for good.
    Failed,
    /// You stopped it.
    Cancelled,
}

impl FixState {
    #[must_use]
    pub fn is_over(self) -> bool {
        self != Self::Writing
    }
}

/// A question asked about files a writer changed outside its scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutlierQuestion {
    pub question: QuestionId,
    pub paths: Vec<String>,
}

/// The option an outlier question offers first: the files join the fix.
pub const ALLOW_OUTLIERS: usize = 0;

/// A fix to record, not yet numbered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewFix {
    pub flow: FlowId,
    pub step: StepId,
    /// The writer role that does it.
    pub role: String,
    pub findings: Vec<FindingId>,
    pub scope: FixScope,
    /// The commit the fix starts from: the head the review read, or the last fix's.
    pub base_sha: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Fix {
    pub id: FixId,
    pub flow: FlowId,
    pub step: StepId,
    pub role: String,
    pub findings: Vec<FindingId>,
    pub scope: FixScope,
    pub base_sha: String,
    /// The fix's newest commit, once there is one.
    pub head_sha: Option<String>,
    /// The hash of `git diff --binary base head`, once the writer has finished: what a route's
    /// approval pins as the diff it covers.
    pub diff_hash: Option<String>,
    pub state: FixState,
    /// Why it failed or stopped, or what it left behind.
    pub detail: Option<String>,
    /// Questions asked about files outside the scope.
    pub asked: Vec<OutlierQuestion>,
    /// Files you let into the fix by answering one.
    pub allowed: Vec<String>,
    pub started_at: UnixMs,
    pub ended_at: Option<UnixMs>,
}

impl Fix {
    /// The outlier question about `paths` already asked, if one was: asking once is enough.
    #[must_use]
    pub fn asked_about(&self, path: &str) -> bool {
        self.asked
            .iter()
            .any(|asked| asked.paths.iter().any(|p| p == path))
    }
}

/// A change to a fix. `None` and [`Field::Keep`] leave a field as it is; `asked` and `allowed`
/// append.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FixChange {
    pub state: Option<FixState>,
    pub head_sha: Field<String>,
    pub diff_hash: Field<String>,
    pub detail: Field<String>,
    pub asked: Option<OutlierQuestion>,
    pub allowed: Vec<String>,
}

impl FixChange {
    #[must_use]
    pub fn state(state: FixState) -> Self {
        Self {
            state: Some(state),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Field::Set(detail.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::review::{Dismissal, Finding, FindingClass, Location, Severity};

    fn lease(id: i64, kind: LeaseKind, released: bool) -> Lease {
        Lease {
            id: LeaseId(id),
            worktree: "/w/webapp-pr-4031".to_owned(),
            flow: FlowId(1),
            kind,
            holder: "fixer".to_owned(),
            acquired_at: 0,
            released_at: released.then_some(5),
            released_why: released.then_some(LeaseRelease::Completed),
        }
    }

    #[test]
    fn a_writer_is_stopped_by_any_open_lease_and_a_reviewer_only_by_an_open_writer() {
        let reviewer = [lease(1, LeaseKind::Reviewer, false)];
        let writer = [lease(2, LeaseKind::Writer, false)];
        assert_eq!(
            lease_conflict(LeaseKind::Writer, &reviewer).map(|l| l.id),
            Some(LeaseId(1)),
            "a writer never edits a tree a review is reading"
        );
        assert_eq!(
            lease_conflict(LeaseKind::Writer, &writer).map(|l| l.id),
            Some(LeaseId(2))
        );
        assert_eq!(
            lease_conflict(LeaseKind::Reviewer, &writer).map(|l| l.id),
            Some(LeaseId(2)),
            "a review never reads a tree a writer is editing"
        );
        assert_eq!(lease_conflict(LeaseKind::Reviewer, &reviewer), None);
    }

    #[test]
    fn a_released_lease_stops_nothing() {
        let released = [
            lease(1, LeaseKind::Writer, true),
            lease(2, LeaseKind::Reviewer, true),
        ];
        assert_eq!(lease_conflict(LeaseKind::Writer, &released), None);
        assert_eq!(lease_conflict(LeaseKind::Reviewer, &released), None);
    }

    fn record(id: i64, path: Option<&str>, dismissed: bool) -> FindingRecord {
        FindingRecord {
            finding: Finding {
                id: FindingId(id),
                flow: FlowId(1),
                class: FindingClass::Suggestion,
                severity: Severity::Notable,
                location: path.map(|path| Location {
                    path: path.to_owned(),
                    start_line: 3,
                    end_line: 4,
                }),
                title: format!("finding {id}"),
                rationale: "because".to_owned(),
                requirement: None,
                tree_sha: "t".to_owned(),
                dismissed: dismissed.then(|| Dismissal {
                    reason: "fine".to_owned(),
                    at: 1,
                }),
            },
            lines: true,
            evidence: Vec::new(),
        }
    }

    #[test]
    fn a_fixs_scope_is_the_files_its_picked_findings_name() {
        let findings = [
            record(1, Some("src/export.rs"), false),
            record(2, Some("src/export.rs"), false),
            record(3, Some("docs/guide.md"), false),
            record(4, Some("src/other.rs"), false),
        ];
        let scope = FixScope::of(&findings, &[FindingId(1), FindingId(2), FindingId(3)]).unwrap();
        assert_eq!(
            scope.paths.iter().map(String::as_str).collect::<Vec<_>>(),
            ["docs/guide.md", "src/export.rs"]
        );
    }

    #[test]
    fn a_fix_cannot_be_scoped_to_nothing_to_a_dismissed_finding_or_to_the_whole_change() {
        let findings = [
            record(1, None, false),
            record(2, Some("src/a.rs"), true),
            record(3, Some("src/b.rs"), false),
        ];
        assert!(FixScope::of(&findings, &[]).is_err());
        assert!(
            FixScope::of(&findings, &[FindingId(1)])
                .is_err_and(|e| e.0.contains("change as a whole"))
        );
        assert!(
            FixScope::of(&findings, &[FindingId(2)]).is_err_and(|e| e.0.contains("not one of"))
        );
        assert!(FixScope::of(&findings, &[FindingId(9)]).is_err());
        assert!(FixScope::of(&findings, &[FindingId(3)]).is_ok());
    }

    #[test]
    fn a_changed_file_outside_the_scope_is_an_outlier_until_you_allow_it() {
        let scope = FixScope {
            paths: BTreeSet::from(["src/export.rs".to_owned()]),
        };
        let changed = [
            "src/export.rs".to_owned(),
            "src/lib.rs".to_owned(),
            "Cargo.toml".to_owned(),
            "src/lib.rs".to_owned(),
        ];
        assert_eq!(scope.outliers(&changed, &[]), ["src/lib.rs", "Cargo.toml"]);
        assert_eq!(
            scope.outliers(&changed, &["src/lib.rs".to_owned()]),
            ["Cargo.toml"]
        );
        assert!(scope.outliers(&changed[..1], &[]).is_empty());
    }
}
