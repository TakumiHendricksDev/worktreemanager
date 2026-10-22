//! The tracker mirror against a real SQLite file.
//!
//! Two promises. The mirror is the last reading and nothing more: a sync replaces a project's queue
//! whole, keeps its order and sprints, and a failure is recorded beside the last good reading
//! rather than over it. And a sync writes the mirror tables and no others: the plan's acceptance
//! scenario, an issue moved in the tracker updating the board and starting nothing, is run here
//! with the real use-case, a fake tracker and this database, and every other table is counted
//! before and after.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::Path;

use pretty_assertions::assert_eq;
use rusqlite::Connection;
use wtm_appdb::AppDb;
use wtm_core::flow::{
    FlowKind, Issue, IssueDetail, IssueRef, IssueStatus, NewWorkItem, QueueReading, QueueSpec,
    QueuedIssue, Sprint, SprintState, StatusCategory, SyncFailure, TrackerKind,
};
use wtm_core::model::ProjectId;
use wtm_core::ports::{FlowStore, TrackerMirror};
use wtm_core::usecase::{SyncReason, SyncRequest, sync_queue};
use wtm_testkit::FakeTracker;

fn open() -> (tempfile::TempDir, AppDb) {
    let dir = tempfile::tempdir().unwrap();
    let db = AppDb::open(&dir.path().join("flow.db")).unwrap();
    (dir, db)
}

fn project() -> ProjectId {
    ProjectId::from_root(Path::new("/work/webapp"))
}

fn issue(key: &str, status: &str, category: StatusCategory) -> Issue {
    Issue {
        key: key.to_owned(),
        summary: format!("Summary of {key}"),
        status: IssueStatus {
            name: status.to_owned(),
            category,
        },
        kind: Some("Story".to_owned()),
        priority: Some("Medium".to_owned()),
        assignee: Some("Sam Rivera".to_owned()),
        labels: vec!["export".to_owned()],
    }
}

fn sprint(id: u64) -> Sprint {
    Sprint {
        id,
        name: format!("Sprint {id}"),
        state: SprintState::Active,
        goal: Some("Ship the bulk export".to_owned()),
        starts_at: Some("2026-10-12T14:00:00.000Z".to_owned()),
        ends_at: Some("2026-10-23T22:00:00.000Z".to_owned()),
        board: Some(3),
    }
}

fn reading(issues: Vec<QueuedIssue>, read_at: u64) -> QueueReading {
    QueueReading {
        project: project(),
        tracker: TrackerKind::Jira,
        query: "assignee = currentUser()".to_owned(),
        board: Some(3),
        sprints: vec![sprint(41)],
        issues,
        read_at,
    }
}

fn queued(key: &str, status: &str) -> QueuedIssue {
    QueuedIssue {
        issue: issue(key, status, StatusCategory::ToDo),
        sprint: Some(41),
    }
}

/// Every table's row count, the mirror's left out.
fn counts_outside_the_mirror(path: &Path) -> BTreeMap<String, i64> {
    let raw = Connection::open(path).unwrap();
    let tables: Vec<String> = raw
        .prepare(
            "SELECT name FROM sqlite_master
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE 'mirror_%'",
        )
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    tables
        .into_iter()
        .map(|table| {
            let count = raw
                .query_row(&format!("SELECT COUNT(*) FROM \"{table}\""), [], |row| {
                    row.get(0)
                })
                .unwrap();
            (table, count)
        })
        .collect()
}

#[test]
fn a_synced_queue_reads_back_in_the_trackers_order_with_its_sprint() {
    let (_dir, db) = open();
    db.mirror_queue(&reading(
        vec![queued("ACME-2", "Doing"), queued("ACME-1", "To Do")],
        1_000,
    ))
    .unwrap();

    let mirrored = db.mirrored_queue(&project()).unwrap().unwrap();
    let reading = mirrored.reading.unwrap();
    let keys: Vec<_> = reading
        .issues
        .iter()
        .map(|q| q.issue.key.as_str())
        .collect();
    assert_eq!(keys, ["ACME-2", "ACME-1"]);
    assert_eq!(reading.issues[0].sprint, Some(41));
    assert_eq!(
        reading.issues[0].issue.assignee.as_deref(),
        Some("Sam Rivera")
    );
    assert_eq!(reading.sprints, [sprint(41)]);
    assert_eq!(reading.board, Some(3));
    assert_eq!(reading.read_at, 1_000);
    assert!(mirrored.failure.is_none());
}

#[test]
fn the_next_sync_replaces_the_queue_whole() {
    let (_dir, db) = open();
    db.mirror_queue(&reading(
        vec![queued("ACME-1", "To Do"), queued("ACME-2", "To Do")],
        1_000,
    ))
    .unwrap();
    db.mirror_queue(&reading(vec![queued("ACME-3", "To Do")], 2_000))
        .unwrap();

    let reading = db
        .mirrored_queue(&project())
        .unwrap()
        .unwrap()
        .reading
        .unwrap();
    let keys: Vec<_> = reading
        .issues
        .iter()
        .map(|q| q.issue.key.as_str())
        .collect();
    assert_eq!(keys, ["ACME-3"]);
}

#[test]
fn a_failure_is_kept_beside_the_last_reading_and_the_next_success_clears_it() {
    let (_dir, db) = open();
    db.mirror_queue(&reading(vec![queued("ACME-1", "To Do")], 1_000))
        .unwrap();
    db.record_sync_failure(
        &project(),
        TrackerKind::Jira,
        &SyncFailure {
            at: 2_000,
            message: "acli is not signed in".to_owned(),
        },
    )
    .unwrap();

    let mirrored = db.mirrored_queue(&project()).unwrap().unwrap();
    assert_eq!(mirrored.reading.unwrap().issues.len(), 1);
    assert_eq!(
        mirrored.failure,
        Some(SyncFailure {
            at: 2_000,
            message: "acli is not signed in".to_owned()
        })
    );

    db.mirror_queue(&reading(vec![queued("ACME-1", "To Do")], 3_000))
        .unwrap();
    assert!(
        db.mirrored_queue(&project())
            .unwrap()
            .unwrap()
            .failure
            .is_none()
    );
}

#[test]
fn a_first_sync_that_fails_leaves_a_failure_and_no_reading() {
    let (_dir, db) = open();
    db.record_sync_failure(
        &project(),
        TrackerKind::Jira,
        &SyncFailure {
            at: 1_000,
            message: "acli is not installed".to_owned(),
        },
    )
    .unwrap();

    let mirrored = db.mirrored_queue(&project()).unwrap().unwrap();
    assert!(mirrored.reading.is_none());
    assert_eq!(mirrored.failure.unwrap().at, 1_000);
    assert!(
        db.mirrored_queue(&ProjectId::from_root(Path::new("/other")))
            .unwrap()
            .is_none()
    );
}

#[test]
fn opening_an_issue_keeps_it_whole_and_its_queue_row_shows_the_newer_status() {
    let (_dir, db) = open();
    db.mirror_queue(&reading(vec![queued("ACME-1", "To Do")], 1_000))
        .unwrap();
    let detail = IssueDetail {
        issue: issue("ACME-1", "In Review", StatusCategory::InProgress),
        description: Some("Export every unit.".to_owned()),
        parent: None,
        links: Vec::new(),
        updated: None,
        fields: Vec::new(),
        sprints: vec![sprint(41)],
    };

    db.mirror_issue(TrackerKind::Jira, &detail, 2_000).unwrap();

    let (read, at) = db
        .mirrored_issue(TrackerKind::Jira, "ACME-1")
        .unwrap()
        .unwrap();
    assert_eq!(read, detail);
    assert_eq!(at, 2_000);
    let reading = db
        .mirrored_queue(&project())
        .unwrap()
        .unwrap()
        .reading
        .unwrap();
    assert_eq!(reading.issues[0].issue.status.name, "In Review");
    assert!(
        db.mirrored_issue(TrackerKind::Jira, "ACME-404")
            .unwrap()
            .is_none()
    );
}

#[test]
fn an_issue_moved_in_the_tracker_updates_the_mirror_and_starts_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("flow.db");
    let db = AppDb::open(&path).unwrap();
    // The issue already has a work item and a run of its own, as it would once work has started:
    // a sync must leave both exactly as they are, and add no other.
    let item = db
        .create_work_item(
            &NewWorkItem {
                project: project(),
                title: "Summary of ACME-1".to_owned(),
                issue: Some(IssueRef {
                    tracker: "jira".to_owned(),
                    key: "ACME-1".to_owned(),
                }),
                pull_request: None,
            },
            500,
        )
        .unwrap();
    let flow = db.start_flow(item.id, FlowKind::ReviewMine, 600).unwrap();
    let before = counts_outside_the_mirror(&path);
    assert_eq!(before["work_items"], 1);
    assert_eq!(before["flows"], 1);

    let tracker = FakeTracker::new()
        .with_sprint(sprint(41))
        .with_issue(issue("ACME-1", "To Do", StatusCategory::ToDo), &[41])
        .with_issue(issue("ACME-2", "To Do", StatusCategory::ToDo), &[41]);
    let queue = QueueSpec {
        query: "assignee = currentUser()".to_owned(),
        board: Some(3),
        limit: None,
    };
    let request = |now| SyncRequest {
        project: project(),
        tracker: TrackerKind::Jira,
        cwd: Path::new("/work/webapp"),
        queue: &queue,
        reason: SyncReason::Focus,
        now,
    };
    sync_queue(&tracker, &db, &request(1_000)).unwrap();

    // Someone moves both issues in the tracker. wtm hears nothing until it next syncs.
    tracker.move_issue("ACME-1", "In Progress", StatusCategory::InProgress);
    tracker.move_issue("ACME-2", "Done", StatusCategory::Done);
    let report = sync_queue(&tracker, &db, &request(2_000)).unwrap();

    let moved: Vec<_> = report
        .changes
        .moved
        .iter()
        .map(|m| (m.key.as_str(), m.to.name.as_str()))
        .collect();
    assert_eq!(moved, [("ACME-1", "In Progress"), ("ACME-2", "Done")]);
    let reading = db
        .mirrored_queue(&project())
        .unwrap()
        .unwrap()
        .reading
        .unwrap();
    assert_eq!(reading.issues[0].issue.status.name, "In Progress");
    assert_eq!(reading.issues[1].issue.status.name, "Done");

    assert_eq!(
        counts_outside_the_mirror(&path),
        before,
        "a sync wrote outside the mirror tables"
    );
    let unchanged = db.flow(flow.id).unwrap().unwrap();
    assert_eq!(unchanged.state, flow.state);
    assert_eq!(db.events(flow.id).unwrap().len(), 1);
}
