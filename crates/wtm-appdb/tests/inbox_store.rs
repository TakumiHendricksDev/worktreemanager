//! The inbox against a real SQLite file.
//!
//! The inbox is the one thing about a run that tells you anything, so the properties a person would
//! notice are these: the same problem reported twice is one item and one notification; an item is
//! still there after the app quits and opens again, with everything a click needs; and only an
//! answer, a resolution or a person's dismissal closes one — each closing exactly what it is about.

#![allow(clippy::unwrap_used)]

use std::path::Path;

use pretty_assertions::assert_eq;
use wtm_appdb::AppDb;
use wtm_core::flow::{
    Answer, ContextLink, FindingId, FlowKind, InboxKind, InboxState, InboxTarget, NewInboxItem,
    NewQuestion, NewReviewRun, NewStep, NewWorkItem, QuestionError, RoleProfile, RoleProvider,
    StepId, StepStatus, WorkLink, Workspace,
};
use wtm_core::model::ProjectId;
use wtm_core::ports::{FlowStore, FlowStoreError, InboxStore, RunStore};

fn project() -> ProjectId {
    ProjectId::from_root(Path::new("/work/webapp"))
}

/// A review flow of `path`, with one step per role.
fn a_review(db: &AppDb, path: &str, roles: &[&str]) -> (wtm_core::flow::FlowId, Vec<StepId>) {
    let item = db
        .create_work_item(
            &NewWorkItem {
                project: project(),
                title: path.to_owned(),
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
            worktree: format!("{path}#id"),
            worktree_path: path.to_owned(),
            recipe: "standard".to_owned(),
            base: "origin/main".to_owned(),
        },
        1_000,
    )
    .unwrap();
    let steps = db
        .add_steps(
            flow.id,
            &roles
                .iter()
                .map(|role| NewStep {
                    stage: 0,
                    role: (*role).to_owned(),
                    provider: RoleProvider::Codex,
                    model: None,
                    profile: RoleProfile::Reviewer,
                    workspace: Workspace::Snapshot,
                    timeout_ms: 60_000,
                    retries: 0,
                    status: StepStatus::Pending,
                    stands_in_for: None,
                    handover: None,
                    detail: None,
                })
                .collect::<Vec<_>>(),
            1_000,
        )
        .unwrap();
    (flow.id, steps.into_iter().map(|s| s.id).collect())
}

fn item(flow: wtm_core::flow::FlowId, kind: InboxKind, target: InboxTarget) -> NewInboxItem {
    NewInboxItem {
        flow,
        kind,
        title: format!("{kind:?} about {}", target.subject()),
        body: "Why it matters.".to_owned(),
        target,
    }
}

fn role(step: StepId, name: &str) -> InboxTarget {
    InboxTarget::Step {
        step,
        role: name.to_owned(),
    }
}

fn a_question(flow: wtm_core::flow::FlowId, step: StepId) -> NewQuestion {
    NewQuestion {
        flow,
        step: Some(step),
        role: Some("correctness".to_owned()),
        text: "Is an empty export an error?".to_owned(),
        why: "It decides whether the empty case is a bug or a feature.".to_owned(),
        unblocks: "Reporting the empty case one way or the other".to_owned(),
        blocks: None,
        context: vec![ContextLink::from_ref("src/export.py:41")],
        recommendation: Some("Treat it as valid, and write a header row".to_owned()),
        options: vec!["An error".to_owned(), "Valid, header only".to_owned()],
    }
}

#[test]
fn the_same_kind_about_the_same_target_is_filed_once_while_it_is_open() {
    let dir = tempfile::tempdir().unwrap();
    let db = AppDb::open(&dir.path().join("flow.db")).unwrap();
    let (flow, steps) = a_review(&db, "/work/webapp-wt", &["correctness", "ui"]);

    let failed = item(flow, InboxKind::Failure, role(steps[0], "correctness"));
    assert!(db.file_inbox(&failed, 2_000).unwrap().is_some());
    assert_eq!(
        db.file_inbox(&failed, 2_500).unwrap(),
        None,
        "a second report of the same failure is the same item"
    );
    // Another target, or another kind about the same target, is something else to know.
    assert!(
        db.file_inbox(&item(flow, InboxKind::Failure, role(steps[1], "ui")), 2_600)
            .unwrap()
            .is_some()
    );
    assert!(
        db.file_inbox(
            &item(flow, InboxKind::Capacity, role(steps[0], "correctness")),
            2_700
        )
        .unwrap()
        .is_some()
    );
    assert_eq!(db.inbox(Some(flow), None).unwrap().len(), 3);
}

#[test]
fn a_closed_item_lets_the_same_thing_be_filed_again_as_news() {
    let dir = tempfile::tempdir().unwrap();
    let db = AppDb::open(&dir.path().join("flow.db")).unwrap();
    let (flow, steps) = a_review(&db, "/work/webapp-wt", &["correctness"]);
    let limit = item(flow, InboxKind::Capacity, role(steps[0], "correctness"));

    let first = db.file_inbox(&limit, 2_000).unwrap().unwrap();
    db.dismiss_inbox_item(first.id, 3_000).unwrap().unwrap();
    let again = db.file_inbox(&limit, 4_000).unwrap().unwrap();

    assert_ne!(again.id, first.id);
    assert_eq!(again.created_at, 4_000);
}

#[test]
fn items_are_still_open_after_the_database_is_closed_and_opened_again() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("flow.db");
    let (flow, filed_ids) = {
        let db = AppDb::open(&path).unwrap();
        let (flow, steps) = a_review(&db, "/work/webapp-wt", &["correctness"]);
        let failed = db
            .file_inbox(
                &item(flow, InboxKind::Failure, role(steps[0], "correctness")),
                2_000,
            )
            .unwrap()
            .unwrap();
        let ready = db
            .file_inbox(
                &item(flow, InboxKind::ReviewReady, InboxTarget::Flow),
                3_000,
            )
            .unwrap()
            .unwrap();
        let (_, asked) = db.ask(&a_question(flow, steps[0]), "Asked", 4_000).unwrap();
        (flow, vec![failed.id, ready.id, asked.unwrap().id])
    };

    let db = AppDb::open(&path).unwrap();
    let open = db.inbox(None, None).unwrap();

    assert_eq!(
        open.iter().map(|i| i.id).collect::<Vec<_>>(),
        filed_ids.iter().rev().copied().collect::<Vec<_>>(),
        "newest first"
    );
    for listed in &open {
        assert_eq!(listed.state, InboxState::Open);
        assert_eq!(listed.flow, flow);
        // What a click needs to arrive, with no second read of the run.
        assert_eq!(listed.project.as_ref(), Some(&project()));
        assert_eq!(listed.worktree.as_deref(), Some("/work/webapp-wt#id"));
    }
    let question = open[0]
        .question
        .as_ref()
        .expect("the question rides its item");
    assert_eq!(question.text, "Is an empty export an error?");
    assert_eq!(
        question.context,
        vec![ContextLink::File {
            path: "src/export.py".to_owned(),
            line: Some(41)
        }]
    );
    assert_eq!(question.answer, None);
}

#[test]
fn dismissing_closes_only_that_item_and_it_stays_readable() {
    let dir = tempfile::tempdir().unwrap();
    let db = AppDb::open(&dir.path().join("flow.db")).unwrap();
    let (flow, steps) = a_review(&db, "/work/webapp-wt", &["correctness", "ui"]);
    let one = db
        .file_inbox(
            &item(flow, InboxKind::Failure, role(steps[0], "correctness")),
            2_000,
        )
        .unwrap()
        .unwrap();
    db.file_inbox(&item(flow, InboxKind::Failure, role(steps[1], "ui")), 2_000)
        .unwrap();

    let dismissed = db.dismiss_inbox_item(one.id, 3_000).unwrap().unwrap();
    assert_eq!(dismissed.state, InboxState::Dismissed);
    assert_eq!(dismissed.closed_at, Some(3_000));
    assert_eq!(
        db.dismiss_inbox_item(one.id, 3_100).unwrap(),
        None,
        "a closed item does not close twice"
    );
    assert_eq!(
        db.resolve_inbox_item(one.id, 3_200).unwrap(),
        None,
        "nor change how it was closed"
    );
    assert_eq!(
        db.inbox_item(one.id).unwrap().unwrap().state,
        InboxState::Dismissed
    );
    let open = db.inbox(Some(flow), None).unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].target, role(steps[1], "ui"));
}

#[test]
fn resolving_a_kind_closes_it_for_the_target_named_and_leaves_the_rest_open() {
    let dir = tempfile::tempdir().unwrap();
    let db = AppDb::open(&dir.path().join("flow.db")).unwrap();
    let (flow, steps) = a_review(&db, "/work/webapp-wt", &["correctness", "ui"]);
    let (other, other_steps) = a_review(&db, "/work/webapp-two", &["correctness"]);
    for (flow, step, name) in [
        (flow, steps[0], "correctness"),
        (flow, steps[1], "ui"),
        (other, other_steps[0], "correctness"),
    ] {
        db.file_inbox(&item(flow, InboxKind::Capacity, role(step, name)), 2_000)
            .unwrap();
    }
    db.file_inbox(
        &item(flow, InboxKind::Failure, role(steps[0], "correctness")),
        2_000,
    )
    .unwrap();

    let closed = db
        .resolve_inbox(
            flow,
            InboxKind::Capacity,
            Some(&role(steps[0], "correctness")),
            3_000,
        )
        .unwrap();
    assert_eq!(closed, 1);
    let kinds: Vec<(InboxKind, String)> = db
        .inbox(Some(flow), None)
        .unwrap()
        .into_iter()
        .map(|i| (i.kind, i.target.subject()))
        .collect();
    assert_eq!(
        kinds,
        vec![
            (InboxKind::Failure, "correctness".to_owned()),
            (InboxKind::Capacity, "ui".to_owned()),
        ]
    );

    // With no target, every item of the kind in that run, and none in another run.
    assert_eq!(
        db.resolve_inbox(flow, InboxKind::Capacity, None, 3_100)
            .unwrap(),
        1
    );
    assert_eq!(db.inbox(Some(other), None).unwrap().len(), 1);
}

#[test]
fn answering_a_question_resolves_its_item_and_a_second_answer_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let db = AppDb::open(&dir.path().join("flow.db")).unwrap();
    let (flow, steps) = a_review(&db, "/work/webapp-wt", &["correctness"]);
    let (asked, filed) = db
        .ask(
            &a_question(flow, steps[0]),
            "Is an empty export an error?",
            2_000,
        )
        .unwrap();
    let filed = filed.unwrap();
    assert_eq!(filed.kind, InboxKind::Question);
    assert_eq!(filed.target, InboxTarget::Question { question: asked.id });

    let first = Answer {
        choice: Some(1),
        text: "And say so in the docs.".to_owned(),
        at: 3_000,
    };
    let answered = db.answer_question(asked.id, &first).unwrap();
    assert_eq!(answered.answer.as_ref(), Some(&first));
    let closed = db.inbox_item(filed.id).unwrap().unwrap();
    assert_eq!(closed.state, InboxState::Resolved);
    assert_eq!(closed.closed_at, Some(3_000));
    assert!(db.inbox(Some(flow), None).unwrap().is_empty());

    // Answered from the inbox, then from somewhere else: the first answer is the record's.
    let second = db.answer_question(
        asked.id,
        &Answer {
            choice: Some(0),
            text: String::new(),
            at: 4_000,
        },
    );
    assert!(matches!(
        second,
        Err(FlowStoreError::Unanswerable {
            reason: QuestionError::AlreadyAnswered
        })
    ));
    assert_eq!(
        db.question(asked.id).unwrap().unwrap().answer,
        Some(first),
        "the stored answer is the first one"
    );
}

#[test]
fn an_answer_the_question_refuses_writes_nothing_and_keeps_its_item_open() {
    let dir = tempfile::tempdir().unwrap();
    let db = AppDb::open(&dir.path().join("flow.db")).unwrap();
    let (flow, steps) = a_review(&db, "/work/webapp-wt", &["correctness"]);
    let (asked, filed) = db.ask(&a_question(flow, steps[0]), "Asked", 2_000).unwrap();

    let refused = db.answer_question(
        asked.id,
        &Answer {
            choice: Some(5),
            text: String::new(),
            at: 3_000,
        },
    );

    assert!(matches!(
        refused,
        Err(FlowStoreError::Unanswerable {
            reason: QuestionError::NoSuchOption {
                choice: 5,
                offered: 2
            }
        })
    ));
    assert_eq!(db.question(asked.id).unwrap().unwrap().answer, None);
    assert_eq!(
        db.inbox_item(filed.unwrap().id).unwrap().unwrap().state,
        InboxState::Open
    );
}

#[test]
fn a_finding_target_is_its_own_target_however_many_runs_share_the_kind() {
    // Two items of one kind about different findings are two things to know; the same finding
    // twice is one.
    let dir = tempfile::tempdir().unwrap();
    let db = AppDb::open(&dir.path().join("flow.db")).unwrap();
    let (flow, _) = a_review(&db, "/work/webapp-wt", &[]);
    let about = |finding: i64| {
        item(
            flow,
            InboxKind::ReviewReady,
            InboxTarget::Finding {
                finding: FindingId(finding),
            },
        )
    };
    assert!(db.file_inbox(&about(1), 2_000).unwrap().is_some());
    assert!(db.file_inbox(&about(2), 2_000).unwrap().is_some());
    assert!(db.file_inbox(&about(1), 2_100).unwrap().is_none());
    // The flow itself is a different target from any finding in it.
    assert!(
        db.file_inbox(
            &item(flow, InboxKind::ReviewReady, InboxTarget::Flow),
            2_200
        )
        .unwrap()
        .is_some()
    );
}
