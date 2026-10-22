//! A review run's records against a real SQLite file.
//!
//! These protect what the Work view and recovery at launch read back after the engine that wrote
//! them is gone: a step's result is exactly the submission that passed its schema, a role has one
//! step per run, and only runs whose flows are still open come back for recovery. The inbox has its
//! own file, `inbox_store.rs`.

#![allow(clippy::unwrap_used)]

use std::path::Path;

use pretty_assertions::assert_eq;
use serde_json::json;
use wtm_appdb::AppDb;
use wtm_core::flow::{
    BlobRef, FlowEvent, FlowKind, InvocationChange, InvocationStatus, NewInvocation, NewReviewRun,
    NewStep, NewWorkItem, OutputKind, RoleProfile, RoleProvider, Snapshot, StepChange, StepStatus,
    WorkLink, Workspace,
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

fn handover(to_role: &str, to: RoleProvider) -> wtm_core::flow::Handover {
    wtm_core::flow::Handover {
        from: wtm_core::flow::Runner {
            role: "correctness".to_owned(),
            provider: RoleProvider::Codex,
            model: Some("gpt-6".to_owned()),
        },
        to: wtm_core::flow::Runner {
            role: to_role.to_owned(),
            provider: to,
            model: None,
        },
        reason: wtm_core::flow::SwitchReason::Limit,
        why: "You've hit your usage limit.".to_owned(),
        objective: "Find bugs".to_owned(),
        stage: 0,
        attempts: 1,
        progress: vec!["reading export.rs".to_owned()],
        plan: None,
        snapshot: None,
        diff: Some(Vec::new()),
        findings: Vec::new(),
        answers: Vec::new(),
    }
}

#[test]
fn a_provider_switch_is_kept_with_its_handover_and_never_rewritten() {
    let (dir, db) = open();
    let review = a_review(&db, "/work/webapp-wt");
    let steps = db
        .add_steps(
            review.flow,
            &[step("correctness", 0), step("correctness-claude", 0)],
            2_000,
        )
        .unwrap();
    let kept = db
        .record_switch(
            &wtm_core::flow::NewProviderSwitch {
                flow: review.flow,
                from_step: steps[0].id,
                to_step: steps[1].id,
                reason: wtm_core::flow::SwitchReason::Limit,
                by: wtm_core::flow::SwitchedBy::Config,
                handover: handover("correctness-claude", RoleProvider::Claude),
            },
            3_000,
        )
        .unwrap();
    assert_eq!(db.switches(review.flow).unwrap(), vec![kept.clone()]);
    assert_eq!(
        db.switches(review.flow).unwrap()[0].handover.progress,
        ["reading export.rs"]
    );

    // A person's "run it once elsewhere" keeps the role's own step; the config's never does.
    let wrong = db.record_switch(
        &wtm_core::flow::NewProviderSwitch {
            flow: review.flow,
            from_step: steps[0].id,
            to_step: steps[0].id,
            reason: wtm_core::flow::SwitchReason::Limit,
            by: wtm_core::flow::SwitchedBy::Config,
            handover: handover("correctness", RoleProvider::Claude),
        },
        4_000,
    );
    assert!(wrong.is_err(), "{wrong:?}");

    let conn = rusqlite::Connection::open(dir.path().join("flow.db")).unwrap();
    let rewritten = conn.execute("UPDATE provider_switches SET reason = 'failure'", []);
    assert!(rewritten.is_err(), "append-only");
    let deleted = conn.execute("DELETE FROM provider_switches", []);
    assert!(deleted.is_err(), "append-only");
}

#[test]
fn running_a_step_once_elsewhere_changes_who_runs_it_and_what_it_is_handed() {
    let (_dir, db) = open();
    let review = a_review(&db, "/work/webapp-wt");
    let added = db
        .add_steps(review.flow, &[step("correctness", 0)], 2_000)
        .unwrap();
    let changed = db
        .update_step(
            added[0].id,
            &StepChange {
                provider: Some(RoleProvider::Claude),
                model: wtm_core::flow::Field::Set("opus".to_owned()),
                handover: wtm_core::flow::Field::Set("where it got to".to_owned()),
                ..StepChange::default()
            },
            3_000,
        )
        .unwrap();
    assert_eq!(changed.provider, RoleProvider::Claude);
    let read = db.steps(review.flow).unwrap().remove(0);
    assert_eq!(read.provider, RoleProvider::Claude);
    assert_eq!(read.model.as_deref(), Some("opus"));
    assert_eq!(read.handover.as_deref(), Some("where it got to"));

    let back = db
        .update_step(
            added[0].id,
            &StepChange {
                provider: Some(RoleProvider::Codex),
                model: wtm_core::flow::Field::Set("gpt-6".to_owned()),
                handover: wtm_core::flow::Field::Clear,
                ..StepChange::default()
            },
            4_000,
        )
        .unwrap();
    assert_eq!(back.provider, RoleProvider::Codex);
    assert_eq!(back.handover, None);
}
