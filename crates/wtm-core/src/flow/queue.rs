//! The sprint queue: what a sync keeps, what changed since the last one, and which section of the
//! Work view's queue an issue sits in.
//!
//! # Two states, side by side and never mixed
//!
//! An issue has a team status, which the tracker owns and a sync mirrors, and it may have runs,
//! which wtm owns. The queue's sections (Needs you, Running, Ready to test, Sprint not started) are
//! decided by the runs alone, through [`QueueSection::of`]; the board's columns by the team status
//! alone, through [`StatusMap`](super::StatusMap). Neither ever decides the other: an issue moved to
//! "Done" in the tracker while a review of its worktree waits on you is still in Needs you, and
//! Start Work never follows from a status change (plan §4.1).

use serde::{Deserialize, Serialize};

use super::tracker::{Issue, IssueStatus, Sprint, TrackerKind};
use super::{FlowState, HoldKind, Phase, UnixMs};
use crate::model::ProjectId;

/// One issue in a queue reading, with the sprint it was found in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuedIssue {
    pub issue: Issue,
    /// The sprint whose search returned it. `None` when the queue names no board.
    pub sprint: Option<u64>,
}

/// What one successful sync of a project's queue read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueReading {
    pub project: ProjectId,
    pub tracker: TrackerKind,
    /// The query that was run, kept so a changed config is not shown under the old one's results.
    pub query: String,
    pub board: Option<u64>,
    /// The board's active sprints, when the queue names a board.
    pub sprints: Vec<Sprint>,
    /// In the tracker's order.
    pub issues: Vec<QueuedIssue>,
    pub read_at: UnixMs,
}

/// A sync that did not finish, kept beside the last one that did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncFailure {
    pub at: UnixMs,
    pub message: String,
}

/// A project's queue as the mirror holds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MirroredQueue {
    /// The last sync that finished. `None` when every attempt so far failed.
    pub reading: Option<QueueReading>,
    /// The last attempt, when it failed after that reading.
    pub failure: Option<SyncFailure>,
}

/// An issue whose team status differs from the last reading's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusChange {
    pub key: String,
    pub from: IssueStatus,
    pub to: IssueStatus,
}

/// What a sync changed in the mirror. Data for the window to redraw from, and nothing else: it is
/// not an event any part of the engine listens for (see `usecase::tracker_sync`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueChanges {
    pub moved: Vec<StatusChange>,
    /// Keys new to the queue.
    pub joined: Vec<String>,
    /// Keys the query no longer returns.
    pub left: Vec<String>,
}

impl QueueChanges {
    /// Compare a new reading with the one before it. Order changes are not changes: a tracker
    /// re-ranks a sprint all day, and the queue shows its order without remarking on it.
    #[must_use]
    pub fn between(before: Option<&QueueReading>, after: &QueueReading) -> Self {
        let old: &[QueuedIssue] = before.map_or(&[], |reading| &reading.issues);
        let find = |issues: &[QueuedIssue], key: &str| -> Option<IssueStatus> {
            issues
                .iter()
                .find(|queued| queued.issue.key == key)
                .map(|queued| queued.issue.status.clone())
        };
        let mut changes = Self::default();
        for queued in &after.issues {
            match find(old, &queued.issue.key) {
                None => changes.joined.push(queued.issue.key.clone()),
                Some(from) if from.name != queued.issue.status.name => {
                    changes.moved.push(StatusChange {
                        key: queued.issue.key.clone(),
                        from,
                        to: queued.issue.status.clone(),
                    });
                }
                Some(_) => {}
            }
        }
        for queued in old {
            if find(&after.issues, &queued.issue.key).is_none() {
                changes.left.push(queued.issue.key.clone());
            }
        }
        changes
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.moved.is_empty() && self.joined.is_empty() && self.left.is_empty()
    }
}

/// Where an issue sits in the Work view's queue, by its runs.
///
/// Declared in the order the queue shows them, which is also urgency: an issue with two worktrees,
/// one waiting on you and one running, is shown once, in Needs you.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueueSection {
    NeedsYou,
    Running,
    ReadyToTest,
    /// No run is under way. A finished review of the issue's worktree does not start the issue:
    /// starting is Start Work's, and nothing about a review changes the issue.
    NotStarted,
}

impl QueueSection {
    /// The section a run puts its issue in, from the run's state and how many inbox items it has
    /// open.
    ///
    /// An open inbox item is the strongest signal: it is the one durable record that a run needs
    /// you, and it outlives the hold that filed it (a finished review stays in Needs you until you
    /// have looked at it). Then a hold that waits on a person or on the world. Then the phases a
    /// run hands back to you in. Anything else that has not ended is running.
    #[must_use]
    pub fn of(state: Option<&FlowState>, open_items: usize) -> Self {
        if open_items > 0 {
            return Self::NeedsYou;
        }
        let Some(state) = state else {
            return Self::NotStarted;
        };
        if state.end.is_some() {
            return Self::NotStarted;
        }
        if let Some(hold) = &state.hold {
            return match hold.kind() {
                HoldKind::AwaitingAnswer
                | HoldKind::AwaitingApproval
                | HoldKind::WaitingCapacity
                | HoldKind::Paused
                | HoldKind::Interrupted
                | HoldKind::Blocked => Self::NeedsYou,
            };
        }
        match state.phase {
            Phase::ReadyToTest | Phase::FixReady => Self::ReadyToTest,
            // Settled phases: the run is not doing anything until someone acts on it.
            Phase::Consolidated | Phase::Following | Phase::InReview => Self::NotStarted,
            Phase::Resolving
            | Phase::Preparing
            | Phase::Investigating
            | Phase::Planned
            | Phase::Designing
            | Phase::DesignReady
            | Phase::Implementing
            | Phase::Verifying
            | Phase::Reviewing
            | Phase::Fixing
            | Phase::Revising
            | Phase::Publishing
            | Phase::Submitting => Self::Running,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::tracker::StatusCategory;
    use crate::flow::{End, FlowKind, Hold, InterruptCause};

    fn issue(key: &str, status: &str) -> QueuedIssue {
        QueuedIssue {
            issue: Issue {
                key: key.to_owned(),
                summary: format!("{key} summary"),
                status: IssueStatus {
                    name: status.to_owned(),
                    category: StatusCategory::InProgress,
                },
                kind: None,
                priority: None,
                assignee: None,
                labels: Vec::new(),
            },
            sprint: Some(7),
        }
    }

    fn reading(issues: Vec<QueuedIssue>) -> QueueReading {
        QueueReading {
            project: ProjectId::from_root(std::path::Path::new("/work/webapp")),
            tracker: TrackerKind::Jira,
            query: "assignee = currentUser()".to_owned(),
            board: Some(3),
            sprints: Vec::new(),
            issues,
            read_at: 0,
        }
    }

    fn state(phase: Phase, hold: Option<Hold>, end: Option<End>) -> FlowState {
        FlowState {
            kind: FlowKind::ReviewMine,
            phase,
            hold,
            end,
        }
    }

    #[test]
    fn an_issue_moved_in_the_tracker_is_reported_with_both_statuses() {
        let before = reading(vec![issue("ACME-1", "To Do"), issue("ACME-2", "Doing")]);
        let after = reading(vec![issue("ACME-1", "Doing"), issue("ACME-2", "Doing")]);
        let changes = QueueChanges::between(Some(&before), &after);
        assert_eq!(changes.moved.len(), 1);
        assert_eq!(changes.moved[0].key, "ACME-1");
        assert_eq!(changes.moved[0].from.name, "To Do");
        assert_eq!(changes.moved[0].to.name, "Doing");
        assert!(changes.joined.is_empty() && changes.left.is_empty());
    }

    #[test]
    fn issues_that_join_or_leave_the_query_are_reported_and_reordering_is_not() {
        let before = reading(vec![issue("ACME-1", "To Do"), issue("ACME-2", "To Do")]);
        let after = reading(vec![issue("ACME-3", "To Do"), issue("ACME-1", "To Do")]);
        let changes = QueueChanges::between(Some(&before), &after);
        assert_eq!(changes.joined, ["ACME-3"]);
        assert_eq!(changes.left, ["ACME-2"]);
        assert!(changes.moved.is_empty());

        let reordered = reading(vec![issue("ACME-2", "To Do"), issue("ACME-1", "To Do")]);
        assert!(QueueChanges::between(Some(&before), &reordered).is_empty());
    }

    #[test]
    fn the_first_sync_reports_every_issue_as_joining() {
        let after = reading(vec![issue("ACME-1", "To Do")]);
        assert_eq!(QueueChanges::between(None, &after).joined, ["ACME-1"]);
    }

    #[test]
    fn an_issue_with_no_run_has_not_started() {
        assert_eq!(QueueSection::of(None, 0), QueueSection::NotStarted);
    }

    #[test]
    fn an_open_inbox_item_puts_an_issue_in_needs_you_whatever_its_run_is_doing() {
        let reviewing = state(Phase::Reviewing, None, None);
        assert_eq!(
            QueueSection::of(Some(&reviewing), 1),
            QueueSection::NeedsYou
        );
        let finished = state(Phase::Consolidated, None, None);
        assert_eq!(QueueSection::of(Some(&finished), 1), QueueSection::NeedsYou);
    }

    #[test]
    fn a_held_run_needs_you_and_a_moving_one_is_running() {
        let waiting = state(Phase::Reviewing, Some(Hold::AwaitingAnswer), None);
        assert_eq!(QueueSection::of(Some(&waiting), 0), QueueSection::NeedsYou);
        let interrupted = state(
            Phase::Reviewing,
            Some(Hold::Interrupted {
                cause: InterruptCause::Quit,
            }),
            None,
        );
        assert_eq!(
            QueueSection::of(Some(&interrupted), 0),
            QueueSection::NeedsYou
        );
        let reviewing = state(Phase::Reviewing, None, None);
        assert_eq!(QueueSection::of(Some(&reviewing), 0), QueueSection::Running);
    }

    #[test]
    fn a_run_handed_back_for_testing_is_ready_to_test() {
        let ready = state(Phase::ReadyToTest, None, None);
        assert_eq!(QueueSection::of(Some(&ready), 0), QueueSection::ReadyToTest);
        let fix = state(Phase::FixReady, None, None);
        assert_eq!(QueueSection::of(Some(&fix), 0), QueueSection::ReadyToTest);
    }

    #[test]
    fn a_settled_or_ended_run_leaves_its_issue_not_started() {
        let reviewed = state(Phase::Consolidated, None, None);
        assert_eq!(
            QueueSection::of(Some(&reviewed), 0),
            QueueSection::NotStarted
        );
        let done = state(Phase::Consolidated, None, Some(End::Done));
        assert_eq!(QueueSection::of(Some(&done), 0), QueueSection::NotStarted);
    }

    #[test]
    fn the_sections_order_by_urgency() {
        assert!(QueueSection::NeedsYou < QueueSection::Running);
        assert!(QueueSection::Running < QueueSection::ReadyToTest);
        assert!(QueueSection::ReadyToTest < QueueSection::NotStarted);
    }
}
