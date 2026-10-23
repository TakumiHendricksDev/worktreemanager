//! A teammate's pull request under review: your own flag on it, what the forge last said about it,
//! the heads a review of it followed, and what you chose to do with the result.
//!
//! # The flag is yours, not the forge's
//!
//! "I'm reviewing this" is a row in wtm's own store. GitHub's requested reviewers say who was
//! asked and a tracker's assignee says who owns the work; neither says that you picked this one up,
//! and setting either would be a write to somebody else's system. So the flag is kept apart from
//! both, read by nothing but the Work view, and never sent anywhere.
//!
//! # What the forge last said is a mirror
//!
//! A pull request is read when it is added, opened, refreshed, or the window comes back into focus
//! (§8: never on a timer), and each read is kept as a [`MirroredPr`]. The mirror is how a review
//! learns that the pull request moved on without asking the forge again: the head it reviewed is
//! its snapshot's, and the head the mirror holds is what the author has pushed since. Nothing but
//! a forge read writes it, and nothing it holds starts anything.
//!
//! # A head the review left behind is kept
//!
//! Re-review changes moves the review to the new head and runs again only the roles whose files
//! changed. Before it does, what the review said at the head it leaves is recorded as a
//! [`ReviewedHead`]: the findings that stood there and the checks run on it. Findings are never
//! deleted, so that record is enough to show each earlier head's results later, and [`fate`] says
//! what became of each finding since.
//!
//! # Private is an outcome; a publish is an approval
//!
//! Keeping notes private writes nothing outside wtm, and is recorded here as an [`Outcome`], pinned
//! to the head it was decided at and kept, so a later head shows that the earlier decision was
//! about earlier code. Publishing a review writes to GitHub, so its record is the approval of
//! exactly what posts and the outbox's operations that post it ([`super::publish`],
//! [`super::outbox`]), not a row here. Sending local fixes somewhere waits for the writer lease.

use serde::{Deserialize, Serialize};

use super::UnixMs;
use super::consolidate::{EvidenceKey, FindingRecord};
use super::forge::{Permission, PullRequest, remote_for};
use super::review::FindingId;
use super::run::{CheckRunId, SnapshotId};
use super::state::FlowId;
use super::work::WorkItemId;
use crate::ports::git::RemoteUrl;

/// What the forge said about a pull request the last time wtm read it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MirroredPr {
    pub item: WorkItemId,
    pub pull_request: PullRequest,
    /// Your permission on the base repository, read at the same time.
    pub permission: Permission,
    pub read_at: UnixMs,
}

/// "I'm reviewing this", set on a pull request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewClaim {
    pub item: WorkItemId,
    pub since: UnixMs,
}

/// A head a review read, recorded when Re-review changes moved the review to the next one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewReviewedHead {
    pub flow: FlowId,
    pub head_sha: String,
    /// The snapshot the review last read at that head.
    pub snapshot: SnapshotId,
    /// The findings that stood at that head, dismissed ones included.
    pub findings: Vec<FindingId>,
    /// The checks run on that head's snapshot.
    pub checks: Vec<CheckRunId>,
    /// The head the review moved to.
    pub next_head: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewedHead {
    pub id: i64,
    #[serde(flatten)]
    pub head: NewReviewedHead,
    /// When the review moved on from it.
    pub left_at: UnixMs,
}

/// What you chose to do with a review, when it writes nothing outside wtm. A publish is recorded as
/// its approval instead; see the module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeKind {
    /// Notes for yourself. Nothing leaves wtm.
    Private,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewOutcome {
    pub flow: FlowId,
    pub kind: OutcomeKind,
    /// The head the review had read when you decided.
    pub head_sha: String,
    pub notes: String,
    /// The findings that stood when you decided, dismissed ones included.
    pub findings: Vec<FindingId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    pub id: i64,
    #[serde(flatten)]
    pub decision: NewOutcome,
    pub decided_at: UnixMs,
}

/// Why an outcome was refused. Nothing was recorded.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize)]
#[error("{0}")]
pub struct OutcomeError(pub String);

impl NewOutcome {
    /// Notes kept in wtm, about the review as it stands at `head_sha`.
    ///
    /// # Errors
    ///
    /// When there is nothing to keep: no notes and no findings. A private outcome is the notes and
    /// the findings they sit beside; an empty one would record a decision about nothing.
    pub fn private(
        flow: FlowId,
        head_sha: &str,
        notes: &str,
        findings: Vec<FindingId>,
    ) -> Result<Self, OutcomeError> {
        let notes = notes.trim();
        if notes.is_empty() && findings.is_empty() {
            return Err(OutcomeError(
                "There is nothing to keep: this review has no findings, so write a note."
                    .to_owned(),
            ));
        }
        if head_sha.trim().is_empty() {
            return Err(OutcomeError(
                "This review has not read a head yet, so there is nothing to keep notes on."
                    .to_owned(),
            ));
        }
        Ok(Self {
            flow,
            kind: OutcomeKind::Private,
            head_sha: head_sha.to_owned(),
            notes: notes.to_owned(),
            findings,
        })
    }
}

/// What became of a finding from an earlier head, by the review's current findings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EarlierFate {
    /// It is still one of the review's findings, unchanged: none of its files moved, so no role
    /// read it again.
    Unchanged,
    /// Still reported at a later head: a role asked to check it named it in `repeats`, or a report
    /// it was made of still stands in a current finding.
    StillReported,
    /// The roles run again at a later head were asked to check it and did not report it again.
    /// That is their reading that it was addressed; it is worded as what happened, not as a
    /// verdict.
    NotReportedAgain,
    /// You dismissed it.
    Dismissed,
}

/// What became of `earlier`, given the review's `current` findings.
///
/// Only the review's own links count: the same finding row, a report carried over unchanged, or a
/// current report whose `repeats` names one of `earlier`'s reports by the id the brief gave it
/// ([`EvidenceKey::label`]). Places are not compared. Between two heads the author's commits move
/// lines, and the first real re-review showed what comparing them does: a different problem now on
/// overlapping lines read as the fixed one "reported again".
#[must_use]
pub fn fate(earlier: &FindingRecord, current: &[FindingRecord]) -> EarlierFate {
    if earlier.finding.dismissed.is_some() {
        return EarlierFate::Dismissed;
    }
    if current.iter().any(|c| c.finding.id == earlier.finding.id) {
        return EarlierFate::Unchanged;
    }
    let keys: Vec<EvidenceKey> = earlier.evidence.iter().map(|e| e.source.key()).collect();
    let labels: Vec<String> = keys.iter().map(EvidenceKey::label).collect();
    let again = current.iter().flat_map(|now| &now.evidence).any(|report| {
        keys.contains(&report.source.key())
            || report
                .reported
                .repeats
                .as_deref()
                .is_some_and(|named| labels.iter().any(|label| label == named.trim()))
    });
    if again {
        EarlierFate::StillReported
    } else {
        EarlierFate::NotReportedAgain
    }
}

/// Whether wtm could push a fix to the pull request's own branch, and why not when it could not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PushToBranch {
    /// Your access allows it. Pushing still needs your approval of that destination, every time.
    Allowed,
    /// The branch is in the author's fork, and they have not let maintainers push to it.
    ForkWithoutMaintainerEdits,
    /// The fork the branch was in has been deleted.
    HeadRepositoryGone,
    /// Your permission on the base repository does not include pushing.
    NoWriteAccess,
}

/// Whether a push to `pr`'s own branch is open to someone with `permission` on its base.
///
/// A fork's branch takes both the author's leave (`maintainer_can_modify`) and write access to the
/// base repository; a branch of the base repository takes write access. The fork comes first
/// because it is the reason that no amount of your own access fixes.
#[must_use]
pub fn push_to_branch(pr: &PullRequest, permission: Permission) -> PushToBranch {
    if pr.cross_repository && pr.head.repo.is_none() {
        PushToBranch::HeadRepositoryGone
    } else if pr.cross_repository && !pr.maintainer_can_modify {
        PushToBranch::ForkWithoutMaintainerEdits
    } else if !permission.can_push() {
        PushToBranch::NoWriteAccess
    } else {
        PushToBranch::Allowed
    }
}

/// The base branch a review of `pr` is measured from, as a remote-tracking ref of the remote that
/// points at its base repository: `(remote, "remote/branch")`.
///
/// A remote-tracking ref rather than the base commit the forge reported, because the forge's is a
/// commit that may not be here yet, and fetching the branch brings it and keeps the ref current for
/// the next head. `None` when no remote points there.
#[must_use]
pub fn base_ref(pr: &PullRequest, remotes: &[RemoteUrl]) -> Option<(String, String)> {
    let remote = remote_for(&pr.base.repo, remotes)?;
    let reference = format!("{remote}/{}", pr.base.branch);
    Some((remote, reference))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::consolidate::{Evidence, Source};
    use crate::flow::forge::{ChecksSummary, PrBase, PrHead, PrState, RepoSlug};
    use crate::flow::recipe::{RoleProvider, Workspace};
    use crate::flow::review::{Dismissal, Finding, FindingClass, Location, Severity};
    use crate::flow::run::{InvocationId, StepId};
    use crate::flow::submission::{ReportedClass, ReportedFinding};
    use crate::model::CommitId;

    fn pr(cross_repository: bool, maintainer_can_modify: bool) -> PullRequest {
        PullRequest {
            number: 4031,
            url: "https://github.com/acme/webapp/pull/4031".to_owned(),
            title: "CSV export".to_owned(),
            author: Some("sam".to_owned()),
            state: PrState::Open,
            draft: false,
            head: PrHead {
                repo: Some(RepoSlug::new(
                    "github.com",
                    if cross_repository { "sam" } else { "acme" },
                    "webapp",
                )),
                branch: "feature/csv".to_owned(),
                sha: CommitId::new("7c9e1d0000000000000000000000000000000000"),
            },
            base: PrBase {
                repo: RepoSlug::new("github.com", "acme", "webapp"),
                branch: "develop".to_owned(),
                sha: None,
            },
            cross_repository,
            maintainer_can_modify,
            requested_reviewers: Vec::new(),
            review_decision: None,
            merge_state: None,
            checks: ChecksSummary::default(),
        }
    }

    #[test]
    fn a_fork_without_maintainer_edits_is_closed_to_a_push_whatever_your_access() {
        assert_eq!(
            push_to_branch(&pr(true, false), Permission::Admin),
            PushToBranch::ForkWithoutMaintainerEdits
        );
        assert_eq!(
            push_to_branch(&pr(true, true), Permission::Write),
            PushToBranch::Allowed
        );
        assert_eq!(
            push_to_branch(&pr(true, true), Permission::Read),
            PushToBranch::NoWriteAccess
        );
        assert_eq!(
            push_to_branch(&pr(false, false), Permission::Triage),
            PushToBranch::NoWriteAccess
        );
        assert_eq!(
            push_to_branch(&pr(false, false), Permission::Maintain),
            PushToBranch::Allowed,
            "maintainer edits are a fork's setting; a branch of the base needs only access"
        );
        let mut gone = pr(true, true);
        gone.head.repo = None;
        assert_eq!(
            push_to_branch(&gone, Permission::Admin),
            PushToBranch::HeadRepositoryGone
        );
    }

    #[test]
    fn the_base_is_the_remote_tracking_branch_of_the_remote_that_points_at_the_base_repository() {
        let remotes = [
            RemoteUrl {
                name: "mine".to_owned(),
                url: "git@github.com:takumi/webapp.git".to_owned(),
            },
            RemoteUrl {
                name: "upstream".to_owned(),
                url: "https://github.com/acme/webapp.git".to_owned(),
            },
        ];
        assert_eq!(
            base_ref(&pr(true, false), &remotes),
            Some(("upstream".to_owned(), "upstream/develop".to_owned()))
        );
        assert_eq!(base_ref(&pr(true, false), &remotes[..1]), None);
    }

    fn reported(path: &str, line: u32) -> ReportedFinding {
        serde_json::from_value(serde_json::json!({
            "title": format!("{path}:{line}"),
            "severity": "notable",
            "class": "suggestion",
            "path": path,
            "line": line,
            "body": "because",
        }))
        .unwrap()
    }

    fn record(id: i64, invocation: i64, reports: &[ReportedFinding]) -> FindingRecord {
        let first = &reports[0];
        FindingRecord {
            finding: Finding {
                id: FindingId(id),
                flow: FlowId(1),
                class: FindingClass::Suggestion,
                severity: Severity::Notable,
                location: first.path.as_ref().map(|path| Location {
                    path: path.clone(),
                    start_line: first.line.unwrap_or(1),
                    end_line: first.line.unwrap_or(1),
                }),
                title: first.title.clone(),
                rationale: first.body.clone(),
                requirement: None,
                tree_sha: "t".to_owned(),
                dismissed: None,
            },
            lines: true,
            evidence: (0u32..)
                .zip(reports)
                .map(|(ordinal, reported)| Evidence {
                    source: Source {
                        step: StepId(1),
                        invocation: InvocationId(invocation),
                        ordinal,
                        role: "correctness".to_owned(),
                        provider: RoleProvider::Codex,
                        model: None,
                        workspace: Workspace::Snapshot,
                        tree_sha: "t".to_owned(),
                    },
                    reported: reported.clone(),
                })
                .collect(),
        }
    }

    #[test]
    fn an_earlier_finding_is_unchanged_still_reported_not_reported_again_or_dismissed() {
        let kept = record(1, 10, &[reported("src/a.rs", 4)]);
        let fixed = record(2, 20, &[reported("src/b.rs", 9)]);
        let still = record(3, 30, &[reported("src/c.rs", 20)]);
        let mut waved = record(4, 40, &[reported("src/d.rs", 1)]);
        waved.finding.dismissed = Some(Dismissal {
            reason: "intended".to_owned(),
            at: 5,
        });
        // At the new head the first is untouched, and the role run again re-reported the third,
        // now further down the file, naming it by the id its brief gave it.
        let mut again = reported("src/c.rs", 31);
        again.repeats = Some("E30.0".to_owned());
        let current = [kept.clone(), record(9, 11, &[again])];

        assert_eq!(fate(&kept, &current), EarlierFate::Unchanged);
        assert_eq!(fate(&fixed, &current), EarlierFate::NotReportedAgain);
        assert_eq!(fate(&still, &current), EarlierFate::StillReported);
        assert_eq!(fate(&waved, &current), EarlierFate::Dismissed);
    }

    #[test]
    fn a_different_problem_on_the_same_lines_at_the_new_head_is_not_the_old_one_still_reported() {
        // What the first real re-review hit: the fixed off-by-one at lines 7–8, and a new report
        // about CSV escaping that the push moved onto line 8.
        let fixed = record(2, 20, &[reported("src/export.py", 8)]);
        let elsewhere = record(9, 21, &[reported("src/export.py", 8)]);
        assert_eq!(fate(&fixed, &[elsewhere]), EarlierFate::NotReportedAgain);
        assert!(matches!(
            reported("src/b.rs", 9).class,
            ReportedClass::Suggestion
        ));
    }

    #[test]
    fn a_report_carried_over_unchanged_keeps_an_earlier_finding_still_reported() {
        // Two roles' reports made one finding; one role was run again and the other's report
        // stands, merged into a new finding with the rerun role's.
        let both = record(5, 50, &[reported("src/e.rs", 3), reported("src/e.rs", 3)]);
        let mut carried = record(9, 50, &[reported("src/e.rs", 3)]);
        carried.evidence[0].source.ordinal = 1;
        assert_eq!(fate(&both, &[carried]), EarlierFate::StillReported);
    }

    #[test]
    fn a_private_outcome_needs_a_note_or_a_finding_and_a_head() {
        assert!(NewOutcome::private(FlowId(1), "7c9e1d", "  ", Vec::new()).is_err());
        assert!(NewOutcome::private(FlowId(1), "", "fine", Vec::new()).is_err());
        let kept = NewOutcome::private(
            FlowId(1),
            "7c9e1d",
            "  ask Sam about the N+1 \n",
            Vec::new(),
        )
        .unwrap();
        assert_eq!(kept.notes, "ask Sam about the N+1");
        assert_eq!(kept.kind, OutcomeKind::Private);
        assert!(NewOutcome::private(FlowId(1), "7c9e1d", "", vec![FindingId(3)]).is_ok());
    }
}
