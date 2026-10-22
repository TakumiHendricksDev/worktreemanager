//! The writer lease and local fixes against a real SQLite file.
//!
//! The lease's promise is that a worktree has one writer and that the writer never shares it with a
//! review, between two clicks and across a restart. So the properties here are about that: a second
//! writer is refused with the holder named; a writer and a review refuse each other in either
//! order, while reviews may read together; a lease survives the database being opened again; a
//! released lease frees the worktree and stays released; and the database itself refuses a second
//! writer that reached it without the store. A fix keeps its state once it is over.

#![allow(clippy::unwrap_used)]

use std::path::Path;

use pretty_assertions::assert_eq;
use rusqlite::{Connection, params};
use wtm_appdb::AppDb;
use wtm_core::flow::{
    FindingId, FixChange, FixScope, FixState, FlowId, FlowKind, LeaseKind, LeaseRelease, NewFix,
    NewLease, NewStep, NewWorkItem, OutlierQuestion, QuestionId, RoleProfile, RoleProvider,
    StepStatus, Workspace,
};
use wtm_core::model::ProjectId;
use wtm_core::ports::{FixStore, FlowStore, FlowStoreError, LeaseStore, RunStore};

const TREE: &str = "/work/webapp-pr-4031";

struct Store {
    dir: tempfile::TempDir,
    db: AppDb,
    review: FlowId,
    other: FlowId,
}

impl Store {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = AppDb::open(&dir.path().join("flow.db")).unwrap();
        let flow = |title: &str| {
            let item = db
                .create_work_item(
                    &NewWorkItem {
                        project: ProjectId::from_root(Path::new("/work/webapp")),
                        title: title.to_owned(),
                        issue: None,
                        pull_request: None,
                    },
                    1_000,
                )
                .unwrap();
            db.start_flow(item.id, FlowKind::ReviewPr, 1_000)
                .unwrap()
                .id
        };
        let review = flow("Export units as CSV");
        let other = flow("Something else");
        Self {
            dir,
            db,
            review,
            other,
        }
    }

    fn lease(
        &self,
        flow: FlowId,
        kind: LeaseKind,
    ) -> Result<wtm_core::flow::Lease, FlowStoreError> {
        self.db.acquire_lease(
            &NewLease {
                worktree: TREE.to_owned(),
                flow,
                kind,
                holder: match kind {
                    LeaseKind::Writer => "fixer".to_owned(),
                    LeaseKind::Reviewer => "standard".to_owned(),
                },
            },
            2_000,
        )
    }

    fn raw(&self) -> Connection {
        Connection::open(self.dir.path().join("flow.db")).unwrap()
    }
}

fn held_by(result: Result<wtm_core::flow::Lease, FlowStoreError>) -> (FlowId, LeaseKind) {
    match result {
        Err(FlowStoreError::Leased { lease }) => (lease.flow, lease.kind),
        other => panic!("expected the worktree to be leased, got {other:?}"),
    }
}

#[test]
fn a_second_writer_is_refused_and_told_who_holds_the_worktree() {
    let s = Store::new();
    let first = s.lease(s.review, LeaseKind::Writer).unwrap();
    assert!(first.is_open());
    assert_eq!(
        held_by(s.lease(s.other, LeaseKind::Writer)),
        (s.review, LeaseKind::Writer)
    );
    assert_eq!(
        held_by(s.lease(s.review, LeaseKind::Writer)),
        (s.review, LeaseKind::Writer),
        "the same run asking twice is still one writer"
    );
}

#[test]
fn a_writer_and_a_review_refuse_each_other_in_either_order_and_reviews_read_together() {
    let s = Store::new();
    let reading = s.lease(s.review, LeaseKind::Reviewer).unwrap();
    s.lease(s.other, LeaseKind::Reviewer).unwrap();
    assert_eq!(
        held_by(s.lease(s.review, LeaseKind::Writer)),
        (s.review, LeaseKind::Reviewer),
        "a writer never edits a tree a review is reading"
    );

    let t = Store::new();
    t.lease(t.review, LeaseKind::Writer).unwrap();
    assert_eq!(
        held_by(t.lease(t.other, LeaseKind::Reviewer)),
        (t.review, LeaseKind::Writer),
        "a review never reads a tree a writer is editing"
    );
    assert_eq!(reading.kind, LeaseKind::Reviewer);
}

#[test]
fn a_lease_on_another_worktree_is_no_conflict() {
    let s = Store::new();
    s.lease(s.review, LeaseKind::Writer).unwrap();
    s.db.acquire_lease(
        &NewLease {
            worktree: "/work/webapp-pr-77".to_owned(),
            flow: s.other,
            kind: LeaseKind::Writer,
            holder: "fixer".to_owned(),
        },
        2_000,
    )
    .unwrap();
    assert_eq!(s.db.open_leases(None).unwrap().len(), 2);
    assert_eq!(s.db.open_leases(Some(TREE)).unwrap().len(), 1);
}

#[test]
fn a_writer_lease_survives_the_database_being_opened_again() {
    let s = Store::new();
    let held = s.lease(s.review, LeaseKind::Writer).unwrap();
    drop(s.db);

    let relaunched = AppDb::open(&s.dir.path().join("flow.db")).unwrap();
    assert_eq!(
        relaunched.open_leases(Some(TREE)).unwrap(),
        std::slice::from_ref(&held)
    );
    let refused = relaunched.acquire_lease(
        &NewLease {
            worktree: TREE.to_owned(),
            flow: s.other,
            kind: LeaseKind::Reviewer,
            holder: "standard".to_owned(),
        },
        3_000,
    );
    assert!(matches!(refused, Err(FlowStoreError::Leased { lease }) if lease.id == held.id));
}

#[test]
fn a_released_lease_frees_the_worktree_and_keeps_its_first_reason() {
    let s = Store::new();
    let held = s.lease(s.review, LeaseKind::Writer).unwrap();
    let released =
        s.db.release_lease(held.id, LeaseRelease::Cancelled, 2_500)
            .unwrap()
            .unwrap();
    assert_eq!(released.released_at, Some(2_500));
    assert_eq!(released.released_why, Some(LeaseRelease::Cancelled));
    assert_eq!(
        s.db.release_lease(held.id, LeaseRelease::Recovered, 2_600)
            .unwrap(),
        None,
        "released once"
    );
    assert!(s.db.open_leases(Some(TREE)).unwrap().is_empty());
    s.lease(s.other, LeaseKind::Writer).unwrap();

    let reopened = s.raw().execute(
        "UPDATE worktree_leases SET released_at = NULL, released_why = NULL WHERE id = ?1",
        params![held.id.0],
    );
    assert!(
        reopened.is_err_and(|e| e.to_string().contains("stays released")),
        "a stray UPDATE cannot take a worktree back"
    );
}

#[test]
fn the_database_refuses_a_writer_beside_another_lease_that_reached_it_without_the_store() {
    let s = Store::new();
    s.lease(s.review, LeaseKind::Reviewer).unwrap();
    let planted = s.raw().execute(
        "INSERT INTO worktree_leases (worktree, flow_id, kind, holder, acquired_at)
         VALUES (?1, ?2, 'writer', 'fixer', 1)",
        params![TREE, s.other.0],
    );
    assert!(planted.is_err_and(|e| e.to_string().contains("leased")));
}

fn writer_step(s: &Store) -> wtm_core::flow::StepId {
    s.db.add_steps(
        s.review,
        &[NewStep {
            stage: 1,
            role: "fixer".to_owned(),
            provider: RoleProvider::Claude,
            model: None,
            profile: RoleProfile::Writer,
            workspace: Workspace::Worktree,
            timeout_ms: 60_000,
            retries: 0,
            status: StepStatus::Pending,
            stands_in_for: None,
            handover: None,
            detail: None,
        }],
        1_500,
    )
    .unwrap()[0]
        .id
}

fn new_fix(s: &Store, step: wtm_core::flow::StepId) -> NewFix {
    NewFix {
        flow: s.review,
        step,
        role: "fixer".to_owned(),
        findings: vec![FindingId(3)],
        scope: FixScope {
            paths: ["src/export.rs".to_owned()].into(),
        },
        base_sha: "7c9e1d".to_owned(),
    }
}

#[test]
fn a_run_has_one_fix_being_written_at_a_time() {
    let s = Store::new();
    let step = writer_step(&s);
    let fix = s.db.start_fix(&new_fix(&s, step), 2_000).unwrap();
    assert_eq!(fix.state, FixState::Writing);
    assert_eq!(fix.ended_at, None);
    assert!(matches!(
        s.db.start_fix(&new_fix(&s, step), 2_100),
        Err(FlowStoreError::FixWriting { existing }) if existing == fix.id
    ));
    s.db.change_fix(
        fix.id,
        &FixChange::state(FixState::Failed).detail("the writer timed out"),
        2_200,
    )
    .unwrap();
    s.db.start_fix(&new_fix(&s, step), 2_300).unwrap();
    assert_eq!(s.db.fixes(s.review).unwrap().len(), 2);
}

#[test]
fn a_fix_that_is_over_keeps_its_state_and_can_still_take_its_rest_and_allowed_files() {
    let s = Store::new();
    let step = writer_step(&s);
    let fix = s.db.start_fix(&new_fix(&s, step), 2_000).unwrap();
    let asked =
        s.db.change_fix(
            fix.id,
            &FixChange {
                asked: Some(OutlierQuestion {
                    question: QuestionId(4),
                    paths: vec!["src/lib.rs".to_owned()],
                }),
                ..FixChange::default()
            },
            2_050,
        )
        .unwrap();
    assert!(asked.asked_about("src/lib.rs"));
    let ready =
        s.db.change_fix(
            fix.id,
            &FixChange {
                head_sha: wtm_core::flow::Field::Set("f1a2b3".to_owned()),
                diff_hash: wtm_core::flow::Field::Set("sha256:d1".to_owned()),
                ..FixChange::state(FixState::Ready)
            },
            2_100,
        )
        .unwrap();
    assert_eq!(ready.ended_at, Some(2_100));
    assert!(
        s.db.change_fix(fix.id, &FixChange::state(FixState::Cancelled), 2_200)
            .is_err(),
        "a ready fix is not cancelled after the fact"
    );
    let widened =
        s.db.change_fix(
            fix.id,
            &FixChange {
                allowed: vec!["src/lib.rs".to_owned(), "src/lib.rs".to_owned()],
                head_sha: wtm_core::flow::Field::Set("0a0b0c".to_owned()),
                diff_hash: wtm_core::flow::Field::Set("sha256:d2".to_owned()),
                ..FixChange::default()
            },
            2_300,
        )
        .unwrap();
    assert_eq!(widened.state, FixState::Ready);
    assert_eq!(widened.allowed, ["src/lib.rs"]);
    assert_eq!(widened.head_sha.as_deref(), Some("0a0b0c"));
    assert_eq!(s.db.fix(fix.id).unwrap(), Some(widened));

    let planted = s.raw().execute(
        "UPDATE fixes SET state = 'writing', ended_at = NULL WHERE id = ?1",
        params![fix.id.0],
    );
    assert!(planted.is_err_and(|e| e.to_string().contains("stays finished")));
}
