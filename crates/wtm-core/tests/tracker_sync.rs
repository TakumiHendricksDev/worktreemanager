//! Syncing a project's queue from its tracker into the mirror.
//!
//! The queue is what the Work view's sprint sections and board are drawn from, and a sync is the
//! only thing that changes it, so these hold what a sync reads and what it keeps: the board's
//! active sprint scoping the query, the fallback when a board has none, a failure recorded beside
//! the last good reading instead of over it, and a status change in the tracker reported as one.
//! The tracker and the mirror are fakes; `wtm-appdb` and `src-tauri` test the same rules against
//! the real database.

#![allow(clippy::unwrap_used)]

use std::path::Path;

use wtm_core::error::TrackerError;
use wtm_core::flow::{
    Issue, IssueStatus, QueueSpec, Sprint, SprintState, StatusCategory, TrackerKind,
};
use wtm_core::model::ProjectId;
use wtm_core::ports::TrackerMirror;
use wtm_core::usecase::{SyncError, SyncReason, SyncRequest, sync_queue};
use wtm_testkit::{FakeTracker, MemoryTrackerMirror};

fn issue(key: &str, status: &str) -> Issue {
    Issue {
        key: key.to_owned(),
        summary: format!("Summary of {key}"),
        status: IssueStatus {
            name: status.to_owned(),
            category: StatusCategory::ToDo,
        },
        kind: Some("Task".to_owned()),
        priority: None,
        assignee: None,
        labels: Vec::new(),
    }
}

fn sprint(id: u64, state: SprintState) -> Sprint {
    Sprint {
        id,
        name: format!("Sprint {id}"),
        state,
        goal: Some("Ship the export".to_owned()),
        starts_at: None,
        ends_at: None,
        board: Some(3),
    }
}

fn project() -> ProjectId {
    ProjectId::from_root(Path::new("/work/webapp"))
}

fn queue(board: Option<u64>) -> QueueSpec {
    QueueSpec {
        query: "assignee = currentUser()".to_owned(),
        board,
        limit: None,
    }
}

fn request(queue: &QueueSpec, now: u64) -> SyncRequest<'_> {
    SyncRequest {
        project: project(),
        tracker: TrackerKind::Jira,
        cwd: Path::new("/work/webapp"),
        queue,
        reason: SyncReason::Demand,
        now,
    }
}

#[test]
fn a_board_scopes_the_queue_to_its_active_sprint_and_names_it() {
    let tracker = FakeTracker::new()
        .with_sprint(sprint(10, SprintState::Closed))
        .with_sprint(sprint(11, SprintState::Active))
        .with_issue(issue("ACME-1", "To Do"), &[11])
        .with_issue(issue("ACME-2", "To Do"), &[10]);
    let mirror = MemoryTrackerMirror::new();
    let spec = queue(Some(3));

    let report = sync_queue(&tracker, &mirror, &request(&spec, 1_000)).unwrap();

    assert_eq!(report.issues, 1);
    assert_eq!(report.sprints.len(), 1);
    assert_eq!(report.sprints[0].goal.as_deref(), Some("Ship the export"));
    let reading = mirror
        .mirrored_queue(&project())
        .unwrap()
        .unwrap()
        .reading
        .unwrap();
    assert_eq!(reading.issues[0].issue.key, "ACME-1");
    assert_eq!(reading.issues[0].sprint, Some(11));
    assert_eq!(
        tracker.calls(),
        [
            "check",
            "sprints 3",
            "search assignee = currentUser() sprint 11"
        ]
    );
}

#[test]
fn a_board_with_no_active_sprint_falls_back_to_the_query_alone() {
    let tracker = FakeTracker::new()
        .with_sprint(sprint(12, SprintState::Future))
        .with_issue(issue("ACME-1", "To Do"), &[12]);
    let mirror = MemoryTrackerMirror::new();
    let spec = queue(Some(3));

    let report = sync_queue(&tracker, &mirror, &request(&spec, 1_000)).unwrap();

    assert_eq!(report.issues, 1);
    assert!(report.sprints.is_empty());
    let reading = mirror
        .mirrored_queue(&project())
        .unwrap()
        .unwrap()
        .reading
        .unwrap();
    assert_eq!(reading.issues[0].sprint, None);
}

#[test]
fn an_issue_in_two_active_sprints_is_listed_once_under_the_first() {
    let tracker = FakeTracker::new()
        .with_sprint(sprint(11, SprintState::Active))
        .with_sprint(sprint(12, SprintState::Active))
        .with_issue(issue("ACME-1", "To Do"), &[11, 12])
        .with_issue(issue("ACME-2", "To Do"), &[12]);
    let mirror = MemoryTrackerMirror::new();
    let spec = queue(Some(3));

    sync_queue(&tracker, &mirror, &request(&spec, 1_000)).unwrap();

    let reading = mirror
        .mirrored_queue(&project())
        .unwrap()
        .unwrap()
        .reading
        .unwrap();
    let keys: Vec<_> = reading
        .issues
        .iter()
        .map(|q| (q.issue.key.as_str(), q.sprint))
        .collect();
    assert_eq!(keys, [("ACME-1", Some(11)), ("ACME-2", Some(12))]);
}

#[test]
fn an_issue_moved_in_the_tracker_is_reported_by_the_next_sync() {
    let tracker = FakeTracker::new().with_issue(issue("ACME-1", "To Do"), &[]);
    let mirror = MemoryTrackerMirror::new();
    let spec = queue(None);
    sync_queue(&tracker, &mirror, &request(&spec, 1_000)).unwrap();

    tracker.move_issue("ACME-1", "In Progress", StatusCategory::InProgress);
    let report = sync_queue(&tracker, &mirror, &request(&spec, 2_000)).unwrap();

    assert_eq!(report.changes.moved.len(), 1);
    assert_eq!(report.changes.moved[0].from.name, "To Do");
    assert_eq!(report.changes.moved[0].to.name, "In Progress");
    let reading = mirror
        .mirrored_queue(&project())
        .unwrap()
        .unwrap()
        .reading
        .unwrap();
    assert_eq!(reading.issues[0].issue.status.name, "In Progress");
    assert_eq!(reading.read_at, 2_000);
}

#[test]
fn a_failed_sync_is_recorded_beside_the_last_reading_and_does_not_replace_it() {
    let tracker = FakeTracker::new().with_issue(issue("ACME-1", "To Do"), &[]);
    let mirror = MemoryTrackerMirror::new();
    let spec = queue(None);
    sync_queue(&tracker, &mirror, &request(&spec, 1_000)).unwrap();

    tracker.fail_next(TrackerError::SignedOut {
        program: "acli".to_owned(),
        message: "token expired".to_owned(),
    });
    let err = sync_queue(&tracker, &mirror, &request(&spec, 2_000)).unwrap_err();

    assert!(matches!(
        err,
        SyncError::Tracker(TrackerError::SignedOut { .. })
    ));
    let mirrored = mirror.mirrored_queue(&project()).unwrap().unwrap();
    assert_eq!(mirrored.reading.unwrap().read_at, 1_000);
    let failure = mirrored.failure.unwrap();
    assert_eq!(failure.at, 2_000);
    assert!(
        failure.message.contains("not signed in"),
        "{}",
        failure.message
    );
}

#[test]
fn a_success_after_a_failure_clears_it() {
    let tracker = FakeTracker::new().with_issue(issue("ACME-1", "To Do"), &[]);
    let mirror = MemoryTrackerMirror::new();
    let spec = queue(None);
    tracker.fail_next(TrackerError::Timeout {
        argv: "acli jira auth status".to_owned(),
        timeout_ms: 20_000,
    });
    assert!(sync_queue(&tracker, &mirror, &request(&spec, 1_000)).is_err());

    sync_queue(&tracker, &mirror, &request(&spec, 2_000)).unwrap();

    let mirrored = mirror.mirrored_queue(&project()).unwrap().unwrap();
    assert!(mirrored.failure.is_none());
    assert!(mirrored.reading.is_some());
}

#[test]
fn a_changed_query_does_not_report_every_issue_as_leaving_and_joining() {
    let tracker = FakeTracker::new().with_issue(issue("ACME-1", "To Do"), &[]);
    let mirror = MemoryTrackerMirror::new();
    sync_queue(&tracker, &mirror, &request(&queue(None), 1_000)).unwrap();

    let other = QueueSpec {
        query: "project = ACME".to_owned(),
        ..queue(None)
    };
    let report = sync_queue(&tracker, &mirror, &request(&other, 2_000)).unwrap();

    // Compared with nothing, so everything joins once; nothing is reported as having left.
    assert_eq!(report.changes.joined, ["ACME-1"]);
    assert!(report.changes.left.is_empty());
}
