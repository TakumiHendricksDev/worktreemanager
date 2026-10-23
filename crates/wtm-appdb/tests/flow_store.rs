//! The flow store against a real SQLite file.
//!
//! These protect the rules that make starting work idempotent: one work item per issue, one per
//! pull request, and one open flow of each kind per work item. Each is the database's to enforce,
//! because a check in the caller loses the race between two clicks. They also protect the claim
//! the Activity tab and crash recovery rest on: a flow's stored state is exactly the replay of its
//! append-only history, and a transition the rules refuse writes nothing.
//!
//! Every test opens a fresh database in a temporary directory, so these exercise the same WAL file
//! the app does rather than an in-memory stand-in with different durability.

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use pretty_assertions::assert_eq;
use rusqlite::Connection;
use wtm_appdb::AppDb;
use wtm_core::flow::{
    Backing, End, FlowEvent, FlowId, FlowKind, FlowState, Hold, HoldKind, InterruptCause, IssueRef,
    NewWorkItem, Phase, PullRequestRef, RunPart, TransitionError, WorkItem, WorkItemId, WorkKey,
    WorkLink,
};
use wtm_core::model::ProjectId;
use wtm_core::ports::{FlowStore, FlowStoreError};

struct Fixture {
    _dir: tempfile::TempDir,
    path: PathBuf,
    db: AppDb,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("flow.db");
    let db = AppDb::open(&path).unwrap();
    Fixture {
        _dir: dir,
        path,
        db,
    }
}

fn project() -> ProjectId {
    ProjectId::from_root(Path::new("/work/webapp"))
}

fn issue(key: &str) -> IssueRef {
    IssueRef {
        tracker: "jira".to_owned(),
        key: key.to_owned(),
    }
}

fn pull_request(number: u64) -> PullRequestRef {
    PullRequestRef {
        repo: "acme/webapp".to_owned(),
        number,
    }
}

fn new_item(issue: Option<IssueRef>, pull_request: Option<PullRequestRef>) -> NewWorkItem {
    NewWorkItem {
        project: project(),
        title: "Bulk export".to_owned(),
        issue,
        pull_request,
    }
}

fn an_issue_item(db: &AppDb) -> WorkItem {
    db.create_work_item(&new_item(Some(issue("ACME-12")), None), 1_000)
        .unwrap()
}

fn advance(to: Phase) -> FlowEvent {
    FlowEvent::Advanced { to }
}

#[test]
fn a_work_item_reads_back_as_it_was_created() {
    let f = fixture();
    let created =
        f.db.create_work_item(
            &new_item(Some(issue("ACME-12")), Some(pull_request(40))),
            1_000,
        )
        .unwrap();

    assert_eq!(f.db.work_item(created.id).unwrap(), Some(created.clone()));
    assert_eq!(
        f.db.find_work_item(&WorkKey::Issue(issue("ACME-12")))
            .unwrap(),
        Some(created.clone())
    );
    assert_eq!(
        f.db.find_work_item(&WorkKey::PullRequest(pull_request(40)))
            .unwrap(),
        Some(created)
    );
}

#[test]
fn a_projects_work_items_list_in_the_order_they_were_added_and_no_other_projects() {
    let f = fixture();
    let first =
        f.db.create_work_item(&new_item(None, Some(pull_request(41))), 1_000)
            .unwrap();
    let elsewhere = NewWorkItem {
        project: ProjectId::from_root(Path::new("/work/other")),
        ..new_item(None, Some(pull_request(7)))
    };
    f.db.create_work_item(&elsewhere, 1_500).unwrap();
    let second =
        f.db.create_work_item(&new_item(Some(issue("ACME-12")), None), 2_000)
            .unwrap();

    assert_eq!(f.db.work_items(&project()).unwrap(), [first, second]);
    assert!(
        f.db.work_items(&ProjectId::from_root(Path::new("/work/none")))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_second_work_item_for_the_same_issue_is_refused_and_names_the_first() {
    let f = fixture();
    let first = an_issue_item(&f.db);

    let second =
        f.db.create_work_item(&new_item(Some(issue("ACME-12")), None), 2_000);

    assert!(
        matches!(second, Err(FlowStoreError::WorkItemExists { existing }) if existing == first.id),
        "{second:?}"
    );
}

#[test]
fn the_same_key_in_two_trackers_is_two_work_items() {
    let f = fixture();
    an_issue_item(&f.db);

    let other = IssueRef {
        tracker: "linear".to_owned(),
        key: "ACME-12".to_owned(),
    };
    assert!(
        f.db.create_work_item(&new_item(Some(other), None), 2_000)
            .is_ok()
    );
}

#[test]
fn a_second_work_item_for_the_same_pull_request_is_refused_and_names_the_first() {
    let f = fixture();
    let first =
        f.db.create_work_item(&new_item(None, Some(pull_request(40))), 1_000)
            .unwrap();
    assert_eq!(first.backing(), Backing::PullRequest);

    let second =
        f.db.create_work_item(&new_item(None, Some(pull_request(40))), 2_000);

    assert!(
        matches!(second, Err(FlowStoreError::WorkItemExists { existing }) if existing == first.id),
        "{second:?}"
    );
}

#[test]
fn ad_hoc_work_items_never_collide() {
    let f = fixture();
    let first = f.db.create_work_item(&new_item(None, None), 1_000).unwrap();
    let second = f.db.create_work_item(&new_item(None, None), 2_000).unwrap();
    assert_ne!(first.id, second.id);
    assert_eq!(second.backing(), Backing::AdHoc);
}

#[test]
fn linking_a_pull_request_makes_it_current_and_keeps_the_earlier_one_in_history() {
    let f = fixture();
    let item = an_issue_item(&f.db);

    let branch = WorkLink::Branch {
        name: "story/ACME-12-bulk-export".to_owned(),
    };
    f.db.link(item.id, &branch, 1_100).unwrap();
    f.db.link(item.id, &WorkLink::PullRequest(pull_request(40)), 1_200)
        .unwrap();
    f.db.link(item.id, &WorkLink::PullRequest(pull_request(41)), 1_300)
        .unwrap();

    let reread = f.db.work_item(item.id).unwrap().unwrap();
    assert_eq!(reread.pull_request, Some(pull_request(41)));
    assert_eq!(reread.backing(), Backing::Issue);
    assert_eq!(
        f.db.find_work_item(&WorkKey::PullRequest(pull_request(40)))
            .unwrap(),
        None,
        "only the current pull request identifies the item"
    );

    let history: Vec<_> =
        f.db.links(item.id)
            .unwrap()
            .into_iter()
            .map(|recorded| (recorded.link, recorded.at))
            .collect();
    assert_eq!(
        history,
        [
            (branch, 1_100),
            (WorkLink::PullRequest(pull_request(40)), 1_200),
            (WorkLink::PullRequest(pull_request(41)), 1_300),
        ]
    );
}

#[test]
fn linking_a_pull_request_another_work_item_owns_is_refused_and_records_nothing() {
    let f = fixture();
    let reviewed =
        f.db.create_work_item(&new_item(None, Some(pull_request(40))), 1_000)
            .unwrap();
    let mine = an_issue_item(&f.db);

    let linked =
        f.db.link(mine.id, &WorkLink::PullRequest(pull_request(40)), 2_000);

    assert!(
        matches!(linked, Err(FlowStoreError::PullRequestTaken { owner }) if owner == reviewed.id),
        "{linked:?}"
    );
    assert_eq!(f.db.work_item(mine.id).unwrap().unwrap().pull_request, None);
    assert!(f.db.links(mine.id).unwrap().is_empty());
}

#[test]
fn only_one_flow_of_a_kind_may_be_open_per_work_item() {
    let f = fixture();
    let item = an_issue_item(&f.db);
    let first =
        f.db.start_flow(item.id, FlowKind::Implement, 2_000)
            .unwrap();

    let second = f.db.start_flow(item.id, FlowKind::Implement, 3_000);

    assert!(
        matches!(second, Err(FlowStoreError::FlowAlreadyOpen { existing }) if existing == first.id),
        "{second:?}"
    );
    assert_eq!(f.db.open_flows(item.id).unwrap(), [first]);
}

#[test]
fn flows_of_different_kinds_run_side_by_side_on_one_work_item() {
    let f = fixture();
    let item = an_issue_item(&f.db);
    let implement =
        f.db.start_flow(item.id, FlowKind::Implement, 2_000)
            .unwrap();
    let review =
        f.db.start_flow(item.id, FlowKind::ReviewMine, 2_100)
            .unwrap();

    assert_eq!(f.db.open_flows(item.id).unwrap(), [implement, review]);
}

#[test]
fn a_flow_of_the_same_kind_can_start_once_the_previous_one_has_ended() {
    let f = fixture();
    let item = an_issue_item(&f.db);
    let first =
        f.db.start_flow(item.id, FlowKind::ReviewMine, 2_000)
            .unwrap();
    f.db.record(first.id, &FlowEvent::Cancelled, 2_500).unwrap();

    let second =
        f.db.start_flow(item.id, FlowKind::ReviewMine, 3_000)
            .unwrap();

    assert_ne!(second.id, first.id);
    assert_eq!(f.db.open_flows(item.id).unwrap(), [second]);
}

#[test]
fn a_flow_cannot_start_for_a_work_item_that_does_not_exist() {
    let f = fixture();
    assert!(matches!(
        f.db.start_flow(WorkItemId(404), FlowKind::ReviewPr, 1_000),
        Err(FlowStoreError::UnknownWorkItem(WorkItemId(404)))
    ));
}

#[test]
fn recording_events_moves_the_flow_and_numbers_its_history() {
    let f = fixture();
    let item = an_issue_item(&f.db);
    let flow = f.db.start_flow(item.id, FlowKind::ReviewPr, 2_000).unwrap();

    f.db.record(flow.id, &advance(Phase::Preparing), 2_100)
        .unwrap();
    let held =
        f.db.record(
            flow.id,
            &FlowEvent::Held {
                hold: Hold::WaitingCapacity {
                    provider: "codex".to_owned(),
                    resets_at: None,
                },
            },
            2_200,
        )
        .unwrap();

    assert_eq!(held.seq, 3);
    assert_eq!(held.updated_at, 2_200);
    assert_eq!(held.started_at, 2_000);
    assert_eq!(held.state.phase, Phase::Preparing);
    assert_eq!(f.db.flow(flow.id).unwrap(), Some(held));

    let seqs: Vec<u64> =
        f.db.events(flow.id)
            .unwrap()
            .iter()
            .map(|event| event.seq)
            .collect();
    assert_eq!(seqs, [1, 2, 3]);
}

#[test]
fn a_refused_transition_writes_no_event_and_leaves_the_flow_unchanged() {
    let f = fixture();
    let item = an_issue_item(&f.db);
    let flow =
        f.db.start_flow(item.id, FlowKind::Implement, 2_000)
            .unwrap();

    let refused = f.db.record(flow.id, &advance(Phase::Implementing), 2_100);

    assert!(
        matches!(
            refused,
            Err(FlowStoreError::Refused(TransitionError::NoSuchStep { .. }))
        ),
        "{refused:?}"
    );
    assert_eq!(f.db.flow(flow.id).unwrap(), Some(flow.clone()));
    assert_eq!(f.db.events(flow.id).unwrap().len(), 1);
}

#[test]
fn every_stored_flow_is_the_replay_of_its_own_events() {
    let f = fixture();
    let item = an_issue_item(&f.db);
    let flow =
        f.db.start_flow(item.id, FlowKind::ReviewMine, 2_000)
            .unwrap();
    let history = [
        advance(Phase::Preparing),
        advance(Phase::Reviewing),
        FlowEvent::Held {
            hold: Hold::Interrupted {
                cause: InterruptCause::Crash,
            },
        },
        FlowEvent::Released {
            hold: HoldKind::Interrupted,
        },
        advance(Phase::Consolidated),
        FlowEvent::Failed {
            reason: "the worktree was removed".to_owned(),
        },
    ];
    for (offset, event) in (1..).zip(&history) {
        f.db.record(flow.id, event, 2_000 + offset).unwrap();
    }

    let stored = f.db.flow(flow.id).unwrap().unwrap();
    let events: Vec<FlowEvent> =
        f.db.events(flow.id)
            .unwrap()
            .into_iter()
            .map(|recorded| recorded.event)
            .collect();
    assert_eq!(FlowState::replay(&events).unwrap(), stored.state);
    assert_eq!(
        stored.state.end,
        Some(End::Failed {
            reason: "the worktree was removed".to_owned()
        })
    );
    assert!(f.db.open_flows(item.id).unwrap().is_empty());
}

#[test]
fn recording_against_a_flow_that_does_not_exist_is_refused() {
    let f = fixture();
    assert!(matches!(
        f.db.record(FlowId(404), &FlowEvent::Cancelled, 1_000),
        Err(FlowStoreError::UnknownFlow(FlowId(404)))
    ));
}

#[test]
fn a_flows_history_cannot_be_rewritten_or_deleted() {
    let f = fixture();
    let item = an_issue_item(&f.db);
    let flow = f.db.start_flow(item.id, FlowKind::ReviewPr, 2_000).unwrap();

    let raw = Connection::open(&f.path).unwrap();
    let rewrite = raw.execute(
        "UPDATE flow_events SET event = '{\"event\":\"cancelled\"}' WHERE flow_id = ?1",
        [flow.id.0],
    );
    let delete = raw.execute("DELETE FROM flow_events WHERE flow_id = ?1", [flow.id.0]);

    for attempt in [rewrite, delete] {
        let message = attempt.unwrap_err().to_string();
        assert!(message.contains("append-only"), "{message}");
    }
    assert_eq!(f.db.events(flow.id).unwrap().len(), 1);
}

#[test]
fn the_store_survives_being_closed_and_reopened() {
    let f = fixture();
    let item = an_issue_item(&f.db);
    let flow =
        f.db.start_flow(item.id, FlowKind::Implement, 2_000)
            .unwrap();
    let advanced =
        f.db.record(flow.id, &advance(Phase::Preparing), 2_100)
            .unwrap();
    let Fixture { _dir, path, db } = f;
    drop(db);

    let reopened = AppDb::open(&path).unwrap();
    assert_eq!(reopened.work_item(item.id).unwrap(), Some(item.clone()));
    assert_eq!(reopened.open_flows(item.id).unwrap(), [advanced]);
}

#[test]
fn the_database_file_is_in_write_ahead_logging_mode() {
    let f = fixture();
    let raw = Connection::open(&f.path).unwrap();
    let mode: String = raw
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .unwrap();
    // WAL is a property of the file rather than of a connection, which is why a second
    // connection can see it.
    assert_eq!(mode, "wal");
}

fn workflow(json: serde_json::Value) -> wtm_core::flow::WorkflowSpec {
    serde_json::from_value(json).unwrap()
}

/// What a run reads is its own from its start: Settings › Recipes saving a change must reach the
/// next run, never one under way, and recovery after a crash must read what the run started with.
#[test]
fn each_part_of_a_run_keeps_the_first_workflow_it_was_given_and_a_later_one_is_not_taken() {
    let f = fixture();
    let item = an_issue_item(&f.db);
    let flow = f.db.start_flow(item.id, FlowKind::Implement, 1).unwrap();
    assert_eq!(f.db.run_workflow(flow.id, RunPart::Plan).unwrap(), None);

    let first = workflow(serde_json::json!({
        "role": {
            "correctness": {
                "objective": "Find bugs", "provider": "codex", "model": "gpt-6",
                "inputs": ["diff"], "fallback": ["spare"]
            },
            "spare": { "objective": "Find bugs", "provider": "claude", "inputs": ["diff"] }
        },
        "recipe": { "standard": { "stage": [{ "roles": ["correctness"] }] } }
    }));
    assert_eq!(
        f.db.capture_workflow(flow.id, RunPart::Plan, &first, 2)
            .unwrap(),
        first
    );

    let mut edited = first.clone();
    edited.role.get_mut("correctness").unwrap().model = Some("gpt-7".to_owned());
    assert_eq!(
        f.db.capture_workflow(flow.id, RunPart::Plan, &edited, 3)
            .unwrap(),
        first,
        "the second capture returns the run's own"
    );
    assert_eq!(
        f.db.capture_workflow(flow.id, RunPart::Implement, &edited, 4)
            .unwrap(),
        edited,
        "a step that begins later keeps the config as it is then"
    );
    drop(f.db);

    let reopened = AppDb::open(&f.path).unwrap();
    assert_eq!(
        reopened.run_workflow(flow.id, RunPart::Plan).unwrap(),
        Some(first)
    );
    let conn = Connection::open(&f.path).unwrap();
    assert!(
        conn.execute("UPDATE run_workflows SET workflow = '{}'", [])
            .is_err(),
        "the database refuses to change what a run read"
    );
}
