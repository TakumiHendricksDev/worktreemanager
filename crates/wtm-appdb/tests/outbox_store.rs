//! Approvals and the outbox against a real SQLite file.
//!
//! The outbox's promise is that a write happens at most once however often it is approved, retried
//! or interrupted, so the properties here are about that: the same payload approved twice is one row
//! per write; a write recorded as done can never be queued again, by the store's rules or by a stray
//! `UPDATE`; an approval that would take over a write whose send is unsettled is refused; and an
//! approval, once given, is never changed.

#![allow(clippy::unwrap_used)]

use std::path::Path;

use pretty_assertions::assert_eq;
use rusqlite::{Connection, params};
use wtm_appdb::AppDb;
use wtm_core::flow::{
    Attention, FlowId, FlowKind, NewApproval, NewWorkItem, OperationChange, OperationResult,
    OperationState, Publication, RepoSlug, ReviewEvent, ReviewPayload, UnixMs,
};
use wtm_core::model::ProjectId;
use wtm_core::ports::{FlowStore, FlowStoreError, OutboxStore};

struct Store {
    dir: tempfile::TempDir,
    db: AppDb,
    flow: FlowId,
}

impl Store {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let db = AppDb::open(&dir.path().join("flow.db")).unwrap();
        let item = db
            .create_work_item(
                &NewWorkItem {
                    project: ProjectId::from_root(Path::new("/work/webapp")),
                    title: "Export units as CSV".to_owned(),
                    issue: None,
                    pull_request: None,
                },
                1_000,
            )
            .unwrap();
        let flow = db
            .start_flow(item.id, FlowKind::ReviewPr, 1_000)
            .unwrap()
            .id;
        Self { dir, db, flow }
    }

    fn approve(&self, review: &ReviewPayload, at: UnixMs) -> Publication {
        let approval = NewApproval::review(self.flow, review, &review.commit_id).unwrap();
        self.db
            .approve(&approval, &review.operations(self.flow), at)
            .unwrap()
    }

    fn raw(&self) -> Connection {
        Connection::open(self.dir.path().join("flow.db")).unwrap()
    }
}

fn review(summary: &str) -> ReviewPayload {
    ReviewPayload {
        repo: RepoSlug::new("github.com", "acme", "webapp"),
        number: 4031,
        commit_id: "7c9e1d".to_owned(),
        event: ReviewEvent::Comment,
        comments: Vec::new(),
        folded: Vec::new(),
        summary: summary.to_owned(),
    }
}

fn posted() -> OperationResult {
    OperationResult::Review {
        review: 1,
        node_id: "PRR_1".to_owned(),
        url: "https://github.com/acme/webapp/pull/4031#pullrequestreview-1".to_owned(),
        adopted: false,
    }
}

#[test]
fn an_approval_queues_its_operations_in_order_and_keeps_exactly_what_it_covered() {
    let s = Store::new();
    let payload = review("Two things before merge.");
    let published = s.approve(&payload, 10);
    assert_eq!(published.review, payload);
    assert_eq!(
        published
            .operations
            .iter()
            .map(|op| (op.position, op.state))
            .collect::<Vec<_>>(),
        [(0, OperationState::Pending), (1, OperationState::Pending)]
    );
    assert_eq!(
        s.db.publications(s.flow).unwrap(),
        std::slice::from_ref(&published)
    );

    // Still there for the next launch.
    let reopened = AppDb::open(&s.dir.path().join("flow.db")).unwrap();
    assert_eq!(
        reopened.publication(published.approval.id).unwrap(),
        Some(published)
    );
}

#[test]
fn approving_the_same_text_again_queues_nothing_twice_and_a_done_write_stays_done() {
    let s = Store::new();
    let payload = review("Same words.");
    let first = s.approve(&payload, 10);
    let comments = first.operations[0].id;
    let submit = first.operations[1].id;
    s.db.change_operation(comments, &OperationChange::sending(), 11)
        .unwrap();
    s.db.change_operation(comments, &OperationChange::done(posted()), 12)
        .unwrap();
    s.db.change_operation(
        submit,
        &OperationChange::stopped(Attention::HeadMoved {
            approved: "7c9e1d".to_owned(),
            current: "91ab44".to_owned(),
        }),
        13,
    )
    .unwrap();

    let again = s.approve(&payload, 20);
    assert_ne!(again.approval.id, first.approval.id);
    assert_eq!(
        again
            .operations
            .iter()
            .map(|op| (op.id, op.state, op.attention.clone()))
            .collect::<Vec<_>>(),
        [
            (comments, OperationState::Done, None),
            (submit, OperationState::Pending, None)
        ]
    );
    assert!(
        s.db.publication(first.approval.id)
            .unwrap()
            .unwrap()
            .operations
            .is_empty(),
        "the operations run under the newer approval now"
    );
    let rows: i64 = s
        .raw()
        .query_row("SELECT count(*) FROM operations", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 2);
}

#[test]
fn an_approval_cannot_take_over_a_write_whose_send_is_unsettled() {
    let s = Store::new();
    let payload = review("In flight.");
    let first = s.approve(&payload, 10);
    s.db.change_operation(first.operations[0].id, &OperationChange::sending(), 11)
        .unwrap();
    let approval = NewApproval::review(s.flow, &payload, "7c9e1d").unwrap();
    assert!(matches!(
        s.db.approve(&approval, &payload.operations(s.flow), 12),
        Err(FlowStoreError::StillSending { operation }) if operation == first.operations[0].id
    ));
    assert_eq!(
        s.db.publications(s.flow).unwrap().len(),
        1,
        "nothing written"
    );
}

#[test]
fn an_operation_moves_only_as_the_outbox_allows_and_counts_each_send() {
    let s = Store::new();
    let id = s.approve(&review("x"), 10).operations[0].id;
    assert!(matches!(
        s.db.change_operation(id, &OperationChange::to(OperationState::Pending), 11),
        Err(FlowStoreError::Outbox { .. })
    ));
    let sent =
        s.db.change_operation(id, &OperationChange::sending(), 12)
            .unwrap();
    assert_eq!((sent.state, sent.attempts), (OperationState::Executing, 1));
    assert!(matches!(
        s.db.change_operation(id, &OperationChange::sending(), 13),
        Err(FlowStoreError::Outbox { .. })
    ));
    assert_eq!(s.db.executing_operations().unwrap(), [sent]);

    let failed =
        s.db.change_operation(id, &OperationChange::failed("no answer"), 14)
            .unwrap();
    assert_eq!(failed.last_error.as_deref(), Some("no answer"));
    assert!(s.db.executing_operations().unwrap().is_empty());
    let again =
        s.db.change_operation(id, &OperationChange::sending(), 15)
            .unwrap();
    assert_eq!((again.attempts, again.last_error), (2, None));
}

#[test]
fn what_stopped_an_operation_is_cleared_as_soon_as_it_moves_on() {
    let s = Store::new();
    let id = s.approve(&review("x"), 10).operations[0].id;
    let stopped =
        s.db.change_operation(
            id,
            &OperationChange::stopped(Attention::Refused {
                message: "Line could not be resolved".to_owned(),
            }),
            11,
        )
        .unwrap();
    assert!(stopped.attention.is_some());
    let cancelled =
        s.db.change_operation(id, &OperationChange::to(OperationState::Cancelled), 12)
            .unwrap();
    assert_eq!(cancelled.attention, None);
}

#[test]
fn a_done_write_and_a_given_approval_are_refused_any_change_by_the_tables_themselves() {
    let s = Store::new();
    let id = s.approve(&review("x"), 10).operations[0].id;
    s.db.change_operation(id, &OperationChange::done(posted()), 11)
        .unwrap();
    let raw = s.raw();
    let undone = raw.execute(
        "UPDATE operations SET state = 'pending' WHERE id = ?1",
        params![id.0],
    );
    assert!(undone.unwrap_err().to_string().contains("stays done"));
    let edited = raw.execute("UPDATE approvals SET binding = '{}'", []);
    assert!(edited.unwrap_err().to_string().contains("append-only"));
    let deleted = raw.execute("DELETE FROM operations", []);
    assert!(deleted.unwrap_err().to_string().contains("never deleted"));
}
