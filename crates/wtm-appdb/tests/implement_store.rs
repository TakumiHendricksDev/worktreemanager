//! An issue's implementation against a real SQLite file: the record, its rounds, the commits the app
//! made for it, and what its questions are about (migration 16).
//!
//! The fix loop is bounded by what its rounds set out to fix, so that record has to be one nothing
//! can rewrite: a round's task is append-only in the database itself, its end is written once, and
//! an implementation that ended stays ended. What is protected here is that the bounds count a
//! history, not a number someone can reset.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::path::Path;

use pretty_assertions::assert_eq;
use wtm_appdb::AppDb;
use wtm_core::flow::{
    AttemptedFinding, CheckpointBy, Field, FindingId, FixBounds, FlowId, FlowKind, ImplementAsk,
    ImplementChange, ImplementOutcome, ImplementQuestion, ImplementScope, IssueRef, LeftBecause,
    NewImplementation, NewRound, NewWorkItem, QuestionId, Remaining, RoundKind, StopReason,
};
use wtm_core::model::ProjectId;
use wtm_core::ports::{FlowStore, FlowStoreError, ImplementStore, InboxStore};

struct Store {
    dir: tempfile::TempDir,
    db: AppDb,
    flow: FlowId,
}

fn project() -> ProjectId {
    ProjectId::from_root(Path::new("/work/webapp"))
}

fn scope() -> ImplementScope {
    ImplementScope {
        areas: vec!["src/export/**".to_owned()],
        files: BTreeSet::from(["tests/export_test.py".to_owned()]),
    }
}

fn implementation(flow: FlowId, role: &str) -> NewImplementation {
    NewImplementation {
        flow,
        project: project(),
        worktree: "/work/ACME-12".to_owned(),
        key: "ACME-12".to_owned(),
        role: role.to_owned(),
        recipe: "standard".to_owned(),
        base_sha: "a1b2c3".to_owned(),
        plan_revision: 1,
        scope: scope(),
        bounds: FixBounds::default(),
    }
}

fn attempted(id: i64) -> AttemptedFinding {
    AttemptedFinding {
        finding: FindingId(id),
        title: "Off by one".to_owned(),
        location: None,
        labels: vec![format!("E{id}.0")],
        lead: None,
    }
}

impl Store {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = AppDb::open(&dir.path().join("flow.db")).unwrap();
        let item = db
            .create_work_item(
                &NewWorkItem {
                    project: project(),
                    title: "Unit export".to_owned(),
                    issue: Some(IssueRef {
                        tracker: "jira".to_owned(),
                        key: "ACME-12".to_owned(),
                    }),
                    pull_request: None,
                },
                1_000,
            )
            .unwrap()
            .id;
        let flow = db.start_flow(item, FlowKind::Implement, 1_000).unwrap().id;
        Self { dir, db, flow }
    }

    fn implementing() -> Self {
        let s = Self::new();
        s.db.begin_implementation(&implementation(s.flow, "implementer"), 2_000)
            .unwrap();
        s
    }

    fn raw(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.dir.path().join("flow.db")).unwrap()
    }
}

#[test]
fn beginning_again_returns_the_implementation_the_run_already_has() {
    let s = Store::implementing();
    // A resume, or implementing again after a plan change, is the same writer and conversation.
    let again =
        s.db.begin_implementation(&implementation(s.flow, "someone-else"), 3_000)
            .unwrap();
    assert_eq!(again.role, "implementer");
    assert_eq!(again.scope, scope());
    assert_eq!(again.bounds, FixBounds::default());
    assert_eq!(again.started_at, 2_000);
    assert_eq!(s.db.implementations().unwrap().len(), 1);
}

#[test]
fn rounds_are_numbered_by_the_store_and_a_fix_rounds_task_is_kept_as_it_was_set() {
    let s = Store::implementing();
    let first =
        s.db.start_round(
            &NewRound {
                flow: s.flow,
                kind: RoundKind::Implement,
                plan_revision: 1,
                findings: Vec::new(),
                checks: Vec::new(),
                note: None,
            },
            2_100,
        )
        .unwrap();
    let fix =
        s.db.start_round(
            &NewRound {
                flow: s.flow,
                kind: RoundKind::Fix,
                plan_revision: 1,
                findings: vec![attempted(7)],
                checks: vec!["unit".to_owned()],
                note: None,
            },
            2_200,
        )
        .unwrap();
    assert_eq!((first.round, fix.round), (1, 2));
    let ended =
        s.db.end_round(
            s.flow,
            2,
            Some("d4e5f6"),
            &["src/export/csv.rs".to_owned()],
            None,
            2_300,
        )
        .unwrap();
    assert_eq!(ended.head_sha.as_deref(), Some("d4e5f6"));
    assert_eq!(ended.findings, vec![attempted(7)]);

    // What a round set out to fix is what the bounds count: the database refuses to change it,
    // and an ended round stays as it ended.
    let raw = s.raw();
    let rewrite = raw.execute(
        "UPDATE implement_rounds SET findings = '[]' WHERE round = 2",
        [],
    );
    assert!(rewrite.is_err(), "a round's task was rewritten");
    let again = raw.execute(
        "UPDATE implement_rounds SET head_sha = 'other' WHERE round = 2",
        [],
    );
    assert!(again.is_err(), "an ended round was ended again");
    assert!(
        raw.execute("DELETE FROM implement_rounds", []).is_err(),
        "rounds were deleted"
    );
    let rounds = s.db.rounds(s.flow).unwrap();
    assert_eq!(rounds.len(), 2);
    assert_eq!(rounds[1].checks, ["unit"]);
}

#[test]
fn an_implement_round_carries_no_task_of_a_fix() {
    let s = Store::implementing();
    let refused = s.raw().execute(
        "INSERT INTO implement_rounds (flow_id, round, kind, plan_revision, findings, started_at)
         VALUES (?1, 1, 'implement', 1, '[7]', 0)",
        [s.flow.0],
    );
    assert!(refused.is_err());
}

#[test]
fn a_handed_over_implementation_opens_again_and_a_stopped_one_stays_stopped() {
    let s = Store::implementing();
    let remaining = vec![Remaining {
        finding: FindingId(9),
        title: "Guide out of date".to_owned(),
        location: None,
        because: LeftBecause::OutsidePlan {
            paths: vec!["docs/guide.md".to_owned()],
        },
    }];
    let ready =
        s.db.change_implementation(
            s.flow,
            &ImplementChange {
                outcome: Field::Set(ImplementOutcome::Ready),
                remaining: Some(remaining.clone()),
                stopped: Field::Set(StopReason::NothingFixable),
                ..ImplementChange::default()
            },
            4_000,
        )
        .unwrap();
    assert_eq!(ready.outcome, Some(ImplementOutcome::Ready));
    assert_eq!(ready.remaining, remaining);
    assert_eq!(ready.stopped, Some(StopReason::NothingFixable));

    // A plan change you agree to after the hand-over opens it again.
    let reopened =
        s.db.change_implementation(
            s.flow,
            &ImplementChange {
                outcome: Field::Clear,
                stopped: Field::Clear,
                ..ImplementChange::default()
            },
            4_500,
        )
        .unwrap();
    assert_eq!((reopened.outcome, reopened.stopped), (None, None));

    s.db.change_implementation(
        s.flow,
        &ImplementChange {
            outcome: Field::Set(ImplementOutcome::Cancelled),
            ..ImplementChange::default()
        },
        5_000,
    )
    .unwrap();
    let refused = s.db.change_implementation(
        s.flow,
        &ImplementChange {
            outcome: Field::Clear,
            ..ImplementChange::default()
        },
        6_000,
    );
    assert!(matches!(
        refused,
        Err(FlowStoreError::ImplementationEnded(flow)) if flow == s.flow
    ));
    // And by the database, whatever wrote it.
    assert!(
        s.raw()
            .execute("UPDATE implementations SET outcome = NULL", [])
            .is_err()
    );
}

#[test]
fn a_plan_change_you_approved_moves_the_implementation_to_its_revision_and_scope() {
    let s = Store::implementing();
    let mut wider = scope();
    wider.areas.push("docs/guide.md".to_owned());
    let moved =
        s.db.change_implementation(
            s.flow,
            &ImplementChange {
                plan: Some((2, wider.clone())),
                ..ImplementChange::default()
            },
            3_000,
        )
        .unwrap();
    assert_eq!(moved.plan_revision, 2);
    assert_eq!(moved.scope, wider);
}

#[test]
fn the_review_run_names_its_implementation_and_only_one() {
    let s = Store::implementing();
    let item = s.db.flow(s.flow).unwrap().unwrap().work_item;
    let review =
        s.db.start_flow(item, FlowKind::ReviewMine, 2_500)
            .unwrap()
            .id;
    s.db.change_implementation(
        s.flow,
        &ImplementChange {
            review_flow: Some(review),
            ..ImplementChange::default()
        },
        2_600,
    )
    .unwrap();
    assert_eq!(
        s.db.implementation_reviewed_by(review)
            .unwrap()
            .map(|i| i.flow),
        Some(s.flow)
    );
    assert_eq!(s.db.implementation_reviewed_by(s.flow).unwrap(), None);
}

#[test]
fn checkpoints_and_question_links_are_kept_as_written() {
    let s = Store::implementing();
    s.db.record_implement_checkpoint(
        s.flow,
        1,
        "d4e5f6",
        "Export units as CSV",
        &["src/export/csv.rs".to_owned()],
        CheckpointBy::Writer,
        2_200,
    )
    .unwrap();
    s.db.record_implement_checkpoint(
        s.flow,
        1,
        "e5f6a7",
        "What the writer left",
        &["tests/export_test.py".to_owned()],
        CheckpointBy::App,
        2_300,
    )
    .unwrap();
    let kept = s.db.implement_checkpoints(s.flow).unwrap();
    assert_eq!(
        kept.iter().map(|c| c.by).collect::<Vec<_>>(),
        [CheckpointBy::Writer, CheckpointBy::App]
    );
    assert!(
        s.raw()
            .execute("UPDATE implement_checkpoints SET message = 'x'", [])
            .is_err()
    );

    let question =
        s.db.ask(
            &wtm_core::flow::NewQuestion {
                flow: s.flow,
                step: None,
                role: Some("implementer".to_owned()),
                text: "Change the plan to include docs/guide.md?".to_owned(),
                why: "It is outside the plan".to_owned(),
                unblocks: "Committing it".to_owned(),
                blocks: None,
                context: Vec::new(),
                recommendation: None,
                options: vec!["Change the plan".to_owned(), "Keep to the plan".to_owned()],
            },
            "ACME-12: outside the plan",
            2_400,
        )
        .unwrap()
        .0;
    let link = ImplementQuestion {
        question: question.id,
        flow: s.flow,
        kind: ImplementAsk::Outliers,
        paths: vec!["docs/guide.md".to_owned()],
        findings: Vec::new(),
        plan_revision: 1,
    };
    s.db.link_implement_question(&link).unwrap();
    // Linking twice is the same link.
    s.db.link_implement_question(&link).unwrap();
    assert_eq!(s.db.implement_questions(s.flow).unwrap(), vec![link]);
    assert!(
        s.db.link_implement_question(&ImplementQuestion {
            question: QuestionId(999),
            ..s.db.implement_questions(s.flow).unwrap()[0].clone()
        })
        .is_err(),
        "a question that does not exist is not linked"
    );
}

#[test]
fn a_revision_keeps_your_note_as_it_was_asked_and_every_round_keeps_its_hand_in() {
    let s = Store::implementing();
    let note = "An empty building should still export its header row";
    let revision =
        s.db.start_round(
            &NewRound {
                flow: s.flow,
                kind: RoundKind::Revise,
                plan_revision: 1,
                findings: Vec::new(),
                checks: Vec::new(),
                note: Some(note.to_owned()),
            },
            2_100,
        )
        .unwrap();
    assert_eq!(revision.note.as_deref(), Some(note));
    let handoff = wtm_core::flow::Handoff {
        summary: "Exported the header for an empty building".to_owned(),
        changed: Vec::new(),
        checks: Vec::new(),
        open_questions: Vec::new(),
        acceptance: vec![wtm_core::flow::CriterionClaim {
            criterion: 2,
            met: true,
            evidence: "tests/test_export.py::test_empty".to_owned(),
        }],
    };
    let ended =
        s.db.end_round(
            s.flow,
            revision.round,
            Some("a1b2c3"),
            &[],
            Some(&handoff),
            2_200,
        )
        .unwrap();
    assert_eq!(ended.handoff, Some(handoff));

    let raw = s.raw();
    assert!(
        raw.execute("UPDATE implement_rounds SET note = 'something else'", [])
            .is_err(),
        "a revision's note was rewritten"
    );
    // A revision always says what it is for, and only a revision has a note.
    for (kind, note) in [
        ("revise", "NULL"),
        ("revise", "'  '"),
        ("implement", "'a note'"),
    ] {
        assert!(
            raw.execute(
                &format!(
                    "INSERT INTO implement_rounds (flow_id, round, kind, plan_revision, note, \
                     started_at) VALUES ({}, 9, '{kind}', 1, {note}, 3000)",
                    s.flow.0
                ),
                [],
            )
            .is_err(),
            "a {kind} round with note {note} was kept"
        );
    }
}

#[test]
fn feedback_is_news_once_and_taken_into_one_revision() {
    let s = Store::implementing();
    let review = |id: u64, body: &str| wtm_core::flow::PrFeedback {
        flow: s.flow,
        review: id,
        author: Some("sam".to_owned()),
        state: wtm_core::flow::RemoteReviewState::ChangesRequested,
        body: body.to_owned(),
        url: format!("https://github.com/acme/units/pull/40#pullrequestreview-{id}"),
        commit_id: Some("a1b2c3".to_owned()),
        comments: vec![wtm_core::flow::FeedbackComment {
            path: "src/export.rs".to_owned(),
            body: "Sort these".to_owned(),
        }],
        seen_at: 3_000,
        revised_in: None,
    };
    let first = s.db.keep_feedback(&[review(1, "Sort by floor")]).unwrap();
    assert_eq!(first.len(), 1);
    // Read again, with the reviewer's text edited since: it was news once, and is kept as first read.
    let again =
        s.db.keep_feedback(&[review(1, "Edited"), review(2, "And name the file")])
            .unwrap();
    assert_eq!(again.iter().map(|f| f.review).collect::<Vec<_>>(), [2]);
    s.db.feedback_revised(s.flow, &[1, 2], 4).unwrap();
    s.db.feedback_revised(s.flow, &[1], 6).unwrap();
    let kept = s.db.feedback(s.flow).unwrap();
    assert_eq!(kept[0].body, "Sort by floor");
    assert_eq!(kept[0].comments[0].body, "Sort these");
    assert_eq!(
        kept.iter().map(|f| f.revised_in).collect::<Vec<_>>(),
        [Some(4), Some(4)],
        "a review is taken into one revision"
    );
    assert!(
        s.raw()
            .execute("DELETE FROM pull_request_feedback", [])
            .is_err()
    );
}
