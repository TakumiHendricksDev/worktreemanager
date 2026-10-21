//! A review run's records against a real SQLite file.
//!
//! These protect what the Work view and recovery at launch read back after the engine that wrote
//! them is gone: a step's result is exactly the submission that passed its schema, a role has one
//! step per run, an inbox item is filed once however many times its cause is reported, and only
//! runs whose flows are still open come back for recovery.

#![allow(clippy::unwrap_used)]

use std::path::Path;

use pretty_assertions::assert_eq;
use serde_json::json;
use wtm_appdb::AppDb;
use wtm_core::flow::{
    BlobRef, FlowEvent, FlowKind, InboxKind, InvocationChange, InvocationStatus, NewInboxItem,
    NewInvocation, NewReviewRun, NewStep, NewWorkItem, OutputKind, RoleProfile, RoleProvider,
    Snapshot, StepChange, StepStatus, WorkLink, Workspace,
};
use wtm_core::model::ProjectId;
use wtm_core::ports::{FlowStore, RunStore};

fn open() -> (tempfile::TempDir, AppDb) {
    let dir = tempfile::tempdir().unwrap();
    let db = AppDb::open(&dir.path().join("flow.db")).unwrap();
    (dir, db)
}

fn project() -> ProjectId {
    ProjectId::from_root(Path::new("/work/webapp"))
}

/// An ad hoc work item with a worktree link and a review flow, and its run row.
fn a_review(db: &AppDb, path: &str) -> wtm_core::flow::ReviewRun {
    let item = db
        .create_work_item(
            &NewWorkItem {
                project: project(),
                title: "fix/date-picker".to_owned(),
                issue: None,
                pull_request: None,
            },
            1_000,
        )
        .unwrap();
    db.link(
        item.id,
        &WorkLink::Worktree {
            path: path.to_owned(),
        },
        1_000,
    )
    .unwrap();
    let flow = db.start_flow(item.id, FlowKind::ReviewMine, 1_000).unwrap();
    db.create_review(
        &NewReviewRun {
            flow: flow.id,
            project: project(),
            worktree: path.to_owned(),
            worktree_path: path.to_owned(),
            recipe: "standard".to_owned(),
            base: "origin/main".to_owned(),
        },
        1_000,
    )
    .unwrap()
}

fn step(role: &str, stage: u32) -> NewStep {
    NewStep {
        stage,
        role: role.to_owned(),
        provider: RoleProvider::Codex,
        model: Some("gpt-6".to_owned()),
        profile: RoleProfile::Reviewer,
        workspace: Workspace::Snapshot,
        timeout_ms: 900_000,
        retries: 1,
        status: StepStatus::Pending,
        stands_in_for: None,
        handover: None,
        detail: None,
    }
}

fn snapshot() -> Snapshot {
    Snapshot {
        head_sha: "a".repeat(40),
        tree_sha: "b".repeat(40),
        base_sha: "c".repeat(40),
        patch: BlobRef {
            path: format!("snapshots/{}-{}.patch", "c".repeat(40), "b".repeat(40)),
            sha256: "d".repeat(64),
            bytes: 1_234,
        },
    }
}

#[test]
fn a_run_records_its_snapshot_and_reads_it_back_as_the_one_it_reads() {
    let (_dir, db) = open();
    let run = a_review(&db, "/work/webapp-wt");

    let id = db.record_snapshot(run.flow, &snapshot(), 2_000).unwrap();

    assert_eq!(db.snapshot(id).unwrap(), Some(snapshot()));
    assert_eq!(db.review(run.flow).unwrap().unwrap().snapshot, Some(id));
}

#[test]
fn a_worktree_finds_the_work_item_it_was_reviewed_as_before() {
    let (_dir, db) = open();
    let run = a_review(&db, "/work/webapp-wt");
    let item = db.flow(run.flow).unwrap().unwrap().work_item;

    assert_eq!(
        db.work_item_for_worktree(&project(), "/work/webapp-wt")
            .unwrap(),
        Some(item)
    );
    assert_eq!(
        db.work_item_for_worktree(&project(), "/work/elsewhere")
            .unwrap(),
        None
    );
}

#[test]
fn a_role_has_one_step_per_run() {
    let (_dir, db) = open();
    let run = a_review(&db, "/work/webapp-wt");
    db.add_steps(run.flow, &[step("correctness", 0)], 2_000)
        .unwrap();

    let again = db.add_steps(run.flow, &[step("correctness", 1)], 2_000);

    assert!(again.is_err_and(|e| e.to_string().contains("correctness")));
    assert_eq!(db.steps(run.flow).unwrap().len(), 1);
}

#[test]
fn a_steps_result_reads_back_as_exactly_the_submission_that_passed() {
    let (_dir, db) = open();
    let run = a_review(&db, "/work/webapp-wt");
    let added = db
        .add_steps(run.flow, &[step("correctness", 0)], 2_000)
        .unwrap();
    let submission = wtm_core::flow::submission::read(
        OutputKind::Findings,
        &json!({ "findings": [{
            "title": "N+1 in the export loop",
            "severity": "notable",
            "class": "confirmed_bug",
            "verification": "query-count test",
            "body": "Each unit loads its parent.",
            "path": "app/export.py",
            "line": 88,
        }]}),
    )
    .unwrap();

    db.update_step(
        added[0].id,
        &StepChange {
            result: Some(submission.clone()),
            ..StepChange::status(StepStatus::Done)
        },
        3_000,
    )
    .unwrap();

    let read = db.steps(run.flow).unwrap().remove(0);
    assert_eq!(read.status, StepStatus::Done);
    assert_eq!(read.result, Some(submission));
    assert_eq!(read.updated_at, 3_000);
}

#[test]
fn a_step_cannot_be_done_without_a_result() {
    let (_dir, db) = open();
    let run = a_review(&db, "/work/webapp-wt");
    let added = db
        .add_steps(run.flow, &[step("correctness", 0)], 2_000)
        .unwrap();

    let refused = db.update_step(added[0].id, &StepChange::status(StepStatus::Done), 3_000);

    assert!(refused.is_err());
    assert_eq!(db.steps(run.flow).unwrap()[0].status, StepStatus::Pending);
}

#[test]
fn an_invocation_keeps_its_session_progress_and_how_it_ended() {
    let (_dir, db) = open();
    let run = a_review(&db, "/work/webapp-wt");
    let step = db
        .add_steps(run.flow, &[step("correctness", 0)], 2_000)
        .unwrap()
        .remove(0);
    let started = db
        .start_invocation(
            &NewInvocation {
                flow: run.flow,
                step: step.id,
                attempt: 1,
                role: "correctness".to_owned(),
                provider: RoleProvider::Codex,
                model: None,
                profile: RoleProfile::Reviewer,
                snapshot: None,
                cwd: "/data/flow/1/checkouts/b".to_owned(),
            },
            2_000,
        )
        .unwrap();
    assert_eq!(started.status, InvocationStatus::Opening);

    db.update_invocation(
        started.id,
        &InvocationChange {
            status: Some(InvocationStatus::Running),
            session: Some("s-1".to_owned()),
            progress: Some("reading export.py".to_owned()),
            ..InvocationChange::default()
        },
        2_100,
    )
    .unwrap();
    db.update_invocation(
        started.id,
        &InvocationChange::ended(
            InvocationStatus::TimedOut,
            Some("no result in 15 minutes".to_owned()),
            3_000,
        ),
        3_000,
    )
    .unwrap();

    let read = db.invocations(run.flow).unwrap().remove(0);
    assert_eq!(read.session.as_deref(), Some("s-1"));
    assert_eq!(read.progress.as_deref(), Some("reading export.py"));
    assert_eq!(read.status, InvocationStatus::TimedOut);
    assert_eq!(read.ended_at, Some(3_000));
}

#[test]
fn an_inbox_item_is_filed_once_until_it_is_resolved() {
    let (_dir, db) = open();
    let run = a_review(&db, "/work/webapp-wt");
    let limit = NewInboxItem {
        flow: run.flow,
        kind: InboxKind::Capacity,
        subject: "correctness".to_owned(),
        title: "Codex limit reached".to_owned(),
        body: "Resets at 3:00 PM.".to_owned(),
    };

    let first = db.file_inbox(&limit, 2_000).unwrap();
    assert!(first.is_some());
    assert_eq!(db.file_inbox(&limit, 2_500).unwrap(), None, "deduplicated");
    assert_eq!(db.inbox(Some(run.flow), None).unwrap().len(), 1);

    assert_eq!(
        db.resolve_inbox(run.flow, InboxKind::Capacity, Some("correctness"), 3_000)
            .unwrap(),
        1
    );
    assert!(db.inbox(None, Some(&project())).unwrap().is_empty());
    assert!(
        db.file_inbox(&limit, 4_000).unwrap().is_some(),
        "a new limit after the last one was resolved is news again"
    );
}

#[test]
fn a_person_resolving_an_item_resolves_only_that_one() {
    let (_dir, db) = open();
    let run = a_review(&db, "/work/webapp-wt");
    let failed = |role: &str| NewInboxItem {
        flow: run.flow,
        kind: InboxKind::Failure,
        subject: role.to_owned(),
        title: format!("{role} failed"),
        body: String::new(),
    };
    let ui = db.file_inbox(&failed("ui"), 2_000).unwrap().unwrap();
    db.file_inbox(&failed("correctness"), 2_000).unwrap();

    assert!(db.resolve_inbox_item(ui.id, 3_000).unwrap());
    assert!(!db.resolve_inbox_item(ui.id, 3_100).unwrap());
    let open = db.inbox(Some(run.flow), None).unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].subject, "correctness");
}

#[test]
fn only_runs_whose_flows_are_still_open_come_back_for_recovery() {
    let (_dir, db) = open();
    let open_run = a_review(&db, "/work/one");
    let ended = a_review(&db, "/work/two");
    db.record(ended.flow, &FlowEvent::Cancelled, 2_000).unwrap();

    let open: Vec<_> = db
        .open_reviews()
        .unwrap()
        .into_iter()
        .map(|run| run.flow)
        .collect();

    assert_eq!(open, [open_run.flow]);
    assert_eq!(db.reviews_of(&project(), "/work/two").unwrap().len(), 1);
}
