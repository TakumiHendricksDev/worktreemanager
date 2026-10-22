//! The sprint queue through the real `App`: its gates, what a sync shows, and the plan's acceptance
//! scenario for I1, "an issue moved in Jira updates the board and starts nothing".
//!
//! The tracker is a fake that counts its calls; the config, the repository with its worktrees and
//! the app database are real. Counting matters twice over. ARCHITECTURE §1 and §6a promise that a
//! project causes no tracker call until its own config names a tracker, and a call that did not
//! happen shows nothing on screen. And plan §4.1 promises that no sync starts a run, which here is
//! every table a run writes counted, empty, after the tracker moved issues under it.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use wtm_app_lib::app::{App, WORK_VIEW_PREF};
use wtm_app_lib::tracker;
use wtm_config::AppPaths;
use wtm_core::flow::{
    FlowEvent, FlowKind, Hold, Issue, IssueStatus, NewReviewRun, NewWorkItem, QueueSection, Sprint,
    SprintState, StatusCategory, StatusRole,
};
use wtm_core::model::ProjectId;
use wtm_core::ports::config::ConfigStore;
use wtm_core::ports::{FlowStore, RunStore, Tracker};
use wtm_core::usecase::SyncReason;
use wtm_testkit::{FakeTracker, GitFixture};

fn issue(key: &str, status: &str, category: StatusCategory) -> Issue {
    Issue {
        key: key.to_owned(),
        summary: format!("Summary of {key}"),
        status: IssueStatus {
            name: status.to_owned(),
            category,
        },
        kind: Some("Story".to_owned()),
        priority: None,
        assignee: Some("Sam Rivera".to_owned()),
        labels: Vec::new(),
    }
}

fn sprint() -> Sprint {
    Sprint {
        id: 41,
        name: "Sprint 41".to_owned(),
        state: SprintState::Active,
        goal: Some("Ship the bulk export".to_owned()),
        starts_at: None,
        ends_at: None,
        board: Some(3),
    }
}

/// The project's workflow: a tracker, a status mapping, and a queue on board 3.
const WORKFLOW: &str = "tracker = \"jira\"\n\
     [projects.\"{root}\".config.workflow.statuses]\n\
     todo = \"To Do\"\n\
     started = \"In Progress\"\n\
     needs_review = \"In Review\"\n\
     done = [\"Done\"]\n\
     [projects.\"{root}\".config.workflow.queue]\n\
     query = \"assignee = currentUser()\"\n\
     board = 3\n";

struct Harness {
    app: App,
    tracker: Arc<FakeTracker>,
    project: String,
    flow_db: PathBuf,
    fixture: GitFixture,
    config: tempfile::TempDir,
}

impl Harness {
    fn new(work_view: bool, tracker_configured: bool) -> Self {
        let fixture = GitFixture::new();
        let config = tempfile::tempdir().unwrap();
        let paths = AppPaths::rooted(config.path());
        let flow_db = paths.flow_db.clone();
        let config_file = paths.config_file.clone();

        let mut app = App::with_paths(paths).unwrap();
        let tracker = Arc::new(
            FakeTracker::new()
                .with_sprint(sprint())
                .with_issue(issue("ACME-1", "To Do", StatusCategory::ToDo), &[41])
                .with_issue(
                    issue("ACME-2", "In Progress", StatusCategory::InProgress),
                    &[41],
                )
                .with_issue(issue("ACME-3", "Triage", StatusCategory::ToDo), &[41]),
        );
        app.tracker = Arc::clone(&tracker) as Arc<dyn Tracker>;

        let root = app.register(fixture.root()).unwrap();
        app.config
            .set_user_pref(WORK_VIEW_PREF, if work_view { "on" } else { "off" })
            .unwrap();
        if tracker_configured {
            let mut text = std::fs::read_to_string(&config_file).unwrap_or_default();
            let root = root.display().to_string();
            write!(
                text,
                "\n[projects.\"{root}\".config.workflow]\n{}",
                WORKFLOW.replace("{root}", &root)
            )
            .unwrap();
            std::fs::write(&config_file, text).unwrap();
        }

        Self {
            project: ProjectId::from_root(&root).to_string(),
            app,
            tracker,
            flow_db,
            fixture,
            config,
        }
    }

    fn sync(&self) -> tracker::SyncedView {
        tracker::sync(&self.app, &self.project, SyncReason::Demand).unwrap()
    }
}

/// Every table's row count, the mirror's left out.
fn counts_outside_the_mirror(path: &Path) -> BTreeMap<String, i64> {
    let raw = rusqlite::Connection::open(path).unwrap();
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
fn a_project_whose_workflow_names_no_tracker_reads_no_issue_and_creates_no_database() {
    let h = Harness::new(true, false);

    let error = tracker::sync(&h.app, &h.project, SyncReason::Open).unwrap_err();
    assert_eq!(error.kind, "trackerOff");
    assert!(
        error.message.contains("tracker = \"jira\""),
        "{}",
        error.message
    );
    let queue = tracker::queue(&h.app, &h.project).unwrap();
    assert!(queue.tracker.is_none());
    assert!(queue.issues.is_empty());

    assert_eq!(h.tracker.calls(), Vec::<String>::new());
    assert!(
        !h.flow_db.exists(),
        "looking at an empty queue created the database"
    );
}

#[test]
fn with_the_work_view_off_no_issue_is_read_even_for_a_project_with_a_tracker() {
    let h = Harness::new(false, true);

    assert_eq!(
        tracker::sync(&h.app, &h.project, SyncReason::Focus)
            .unwrap_err()
            .kind,
        "trackerOff"
    );
    assert_eq!(
        tracker::issue(&h.app, &h.project, "ACME-1", true)
            .unwrap_err()
            .kind,
        "trackerOff"
    );
    assert_eq!(h.tracker.calls(), Vec::<String>::new());
}

#[test]
fn looking_at_the_queue_reads_the_mirror_and_never_the_tracker() {
    let h = Harness::new(true, true);

    let queue = tracker::queue(&h.app, &h.project).unwrap();

    assert!(queue.read_at.is_none(), "nothing has been synced yet");
    assert_eq!(queue.query.as_deref(), Some("assignee = currentUser()"));
    assert_eq!(h.tracker.calls(), Vec::<String>::new());
}

#[test]
fn a_sync_fills_the_queue_with_the_sprint_and_places_each_issue_in_a_mapped_column() {
    let h = Harness::new(true, true);

    let synced = h.sync();

    let queue = synced.queue;
    assert_eq!(queue.sprints.len(), 1);
    assert_eq!(
        queue.sprints[0].goal.as_deref(),
        Some("Ship the bulk export")
    );
    let columns: Vec<_> = queue.columns.iter().map(|c| c.status.as_str()).collect();
    assert_eq!(columns, ["To Do", "In Progress", "In Review", "Done"]);
    let rows: Vec<_> = queue
        .issues
        .iter()
        .map(|row| (row.issue.key.as_str(), row.column, row.role, row.section))
        .collect();
    assert_eq!(
        rows,
        [
            (
                "ACME-1",
                Some(0),
                Some(StatusRole::Todo),
                QueueSection::NotStarted
            ),
            (
                "ACME-2",
                Some(1),
                Some(StatusRole::Started),
                QueueSection::NotStarted
            ),
            // A status nobody mapped is still in the queue, with no column: the board shows it
            // apart rather than dropping it.
            ("ACME-3", None, None, QueueSection::NotStarted),
        ]
    );
    assert_eq!(synced.report.unwrap().account.site, "acme.example.net");
}

#[test]
fn an_issue_moved_in_jira_moves_on_the_board_and_starts_nothing() {
    let h = Harness::new(true, true);
    h.sync();
    let before = counts_outside_the_mirror(&h.flow_db);

    h.tracker
        .move_issue("ACME-1", "In Review", StatusCategory::InProgress);
    let synced = h.sync();

    let report = synced.report.unwrap();
    assert_eq!(report.changes.moved.len(), 1);
    assert_eq!(report.changes.moved[0].key, "ACME-1");
    let row = &synced.queue.issues[0];
    assert_eq!(row.issue.status.name, "In Review");
    assert_eq!(row.column, Some(2));
    assert_eq!(row.role, Some(StatusRole::NeedsReview));
    assert!(row.run.is_none());

    let after = counts_outside_the_mirror(&h.flow_db);
    assert_eq!(after, before, "a sync wrote outside the mirror tables");
    for table in [
        "work_items",
        "flows",
        "flow_events",
        "steps",
        "invocations",
        "inbox_items",
        "operations",
    ] {
        assert_eq!(
            after[table], 0,
            "{table} after a status change in the tracker"
        );
    }
}

#[test]
fn a_worktree_carrying_an_issues_key_links_to_it_and_its_waiting_run_puts_it_in_needs_you() {
    let h = Harness::new(true, true);
    let worktree = h
        .fixture
        .add_worktree("webapp-acme-2", "feature/ACME-2-export");
    // As git lists it, which is how the app names a worktree: the temporary directory resolved.
    let worktree_id = std::fs::canonicalize(&worktree)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let db = h.app.appdb.open().unwrap();
    // A review of the worktree that stopped to ask a question, as the engine records one.
    let item = db
        .create_work_item(
            &NewWorkItem {
                project: ProjectId::from_root(Path::new(&h.project)),
                title: "feature/ACME-2-export".to_owned(),
                issue: None,
                pull_request: None,
            },
            1,
        )
        .unwrap();
    let flow = db.start_flow(item.id, FlowKind::ReviewMine, 2).unwrap();
    db.create_review(
        &NewReviewRun {
            flow: flow.id,
            project: ProjectId::from_root(Path::new(&h.project)),
            worktree: worktree_id.clone(),
            worktree_path: worktree_id.clone(),
            recipe: "standard".to_owned(),
            base: "main".to_owned(),
        },
        3,
    )
    .unwrap();
    db.record(
        flow.id,
        &FlowEvent::Held {
            hold: Hold::AwaitingAnswer,
        },
        4,
    )
    .unwrap();

    let queue = h.sync().queue;

    let linked = queue
        .issues
        .iter()
        .find(|row| row.issue.key == "ACME-2")
        .unwrap();
    assert_eq!(linked.worktrees, std::slice::from_ref(&worktree_id));
    assert_eq!(linked.section, QueueSection::NeedsYou);
    let run = linked.run.as_ref().unwrap();
    assert_eq!(run.flow, flow.id);
    assert_eq!(run.worktree, worktree_id);
    // Its team status is its own, whatever the run is doing.
    assert_eq!(linked.issue.status.name, "In Progress");

    let unlinked = queue
        .issues
        .iter()
        .find(|row| row.issue.key == "ACME-1")
        .unwrap();
    assert!(unlinked.worktrees.is_empty());
    assert_eq!(unlinked.section, QueueSection::NotStarted);
}

#[test]
fn an_issue_backed_work_item_is_named_on_its_row_and_none_is_made_by_looking() {
    let h = Harness::new(true, true);
    h.sync();
    let db = h.app.appdb.open().unwrap();
    assert!(
        db.work_items(&ProjectId::from_root(Path::new(&h.project)))
            .unwrap()
            .is_empty(),
        "a sync made a work item"
    );
    let item = db
        .create_work_item(
            &NewWorkItem {
                project: ProjectId::from_root(Path::new(&h.project)),
                title: "Summary of ACME-1".to_owned(),
                issue: Some(wtm_core::flow::IssueRef {
                    tracker: "jira".to_owned(),
                    key: "ACME-1".to_owned(),
                }),
                pull_request: None,
            },
            1,
        )
        .unwrap();

    let queue = tracker::queue(&h.app, &h.project).unwrap();

    assert_eq!(queue.issues[0].work_item, Some(item.id));
    assert_eq!(queue.issues[1].work_item, None);
}

#[test]
fn a_failed_sync_keeps_the_last_queue_and_says_why() {
    let h = Harness::new(true, true);
    h.sync();
    h.tracker
        .fail_next(wtm_core::error::TrackerError::SignedOut {
            program: "acli".to_owned(),
            message: "token expired".to_owned(),
        });

    let synced = h.sync();

    assert!(synced.report.is_none());
    assert_eq!(
        synced.queue.issues.len(),
        3,
        "the last reading is still shown"
    );
    let failure = synced.queue.failure.unwrap();
    assert!(
        failure.message.contains("not signed in"),
        "{}",
        failure.message
    );
}

#[test]
fn a_queue_synced_under_another_query_is_marked_outdated_until_the_next_sync() {
    let h = Harness::new(true, true);
    h.sync();
    let config_file = AppPaths::rooted(h.config.path()).config_file;
    let text = std::fs::read_to_string(&config_file)
        .unwrap()
        .replace("assignee = currentUser()", "project = ACME");
    std::fs::write(&config_file, text).unwrap();

    assert!(tracker::queue(&h.app, &h.project).unwrap().outdated);
    assert!(!h.sync().queue.outdated);
}

#[test]
fn an_opened_issue_is_read_whole_kept_and_shown_from_the_mirror_after() {
    let h = Harness::new(true, true);
    assert!(
        tracker::issue(&h.app, &h.project, "ACME-2", false)
            .unwrap()
            .is_none()
    );

    let fresh = tracker::issue(&h.app, &h.project, "ACME-2", true)
        .unwrap()
        .unwrap();
    assert_eq!(fresh.detail.issue.key, "ACME-2");
    assert_eq!(fresh.column, Some(1));
    assert!(fresh.account.is_some());

    let calls = h.tracker.calls().len();
    let kept = tracker::issue(&h.app, &h.project, "ACME-2", false)
        .unwrap()
        .unwrap();
    assert_eq!(kept.detail, fresh.detail);
    assert!(kept.account.is_none());
    assert_eq!(
        h.tracker.calls().len(),
        calls,
        "the mirror read asked the tracker"
    );
}
