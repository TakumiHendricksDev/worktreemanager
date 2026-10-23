//! What wtm keeps about a teammate's pull request, against a real SQLite file.
//!
//! The Reviews tab draws each of these back, and two of them are promises: a head a review moved on
//! from, and an outcome you chose, are history, so the database refuses to change or delete either
//! rather than trusting every caller not to. The mirror is the opposite, the last reading and
//! nothing more, so a second read replaces the first.

#![allow(clippy::unwrap_used)]

use std::path::Path;

use pretty_assertions::assert_eq;
use wtm_appdb::AppDb;
use wtm_core::flow::{
    BlobRef, CheckRunId, ChecksSummary, FindingId, FlowId, FlowKind, MirroredPr, NewOutcome,
    NewReviewRun, NewReviewedHead, NewWorkItem, OutcomeKind, Permission, PrBase, PrHead, PrState,
    PullRequest, PullRequestRef, RepoSlug, Snapshot, WorkItemId,
};
use wtm_core::model::{CommitId, ProjectId};
use wtm_core::ports::{FlowStore, FlowStoreError, PullReviewStore, RunStore};

fn open() -> (tempfile::TempDir, AppDb) {
    let dir = tempfile::tempdir().unwrap();
    let db = AppDb::open(&dir.path().join("flow.db")).unwrap();
    (dir, db)
}

fn pull_request(head: &str) -> PullRequest {
    PullRequest {
        number: 4031,
        url: "https://github.com/acme/webapp/pull/4031".to_owned(),
        title: "Export units as CSV".to_owned(),
        author: Some("sam-rivera".to_owned()),
        state: PrState::Open,
        draft: false,
        head: PrHead {
            repo: Some(RepoSlug::new("github.com", "sam-rivera", "webapp")),
            branch: "feature/csv".to_owned(),
            sha: CommitId::new(head),
        },
        base: PrBase {
            repo: RepoSlug::new("github.com", "acme", "webapp"),
            branch: "main".to_owned(),
            sha: None,
        },
        cross_repository: true,
        maintainer_can_modify: false,
        requested_reviewers: Vec::new(),
        review_decision: None,
        merge_state: None,
        checks: ChecksSummary::default(),
    }
}

/// A pull request's work item and a review flow of it with one snapshot.
fn a_pr_review(db: &AppDb) -> (WorkItemId, FlowId, wtm_core::flow::SnapshotId) {
    let project = ProjectId::from_root(Path::new("/work/webapp"));
    let item = db
        .create_work_item(
            &NewWorkItem {
                project: project.clone(),
                title: "Export units as CSV".to_owned(),
                issue: None,
                pull_request: Some(PullRequestRef {
                    repo: "acme/webapp".to_owned(),
                    number: 4031,
                }),
            },
            1_000,
        )
        .unwrap();
    let flow = db.start_flow(item.id, FlowKind::ReviewPr, 1_000).unwrap();
    db.create_review(
        &NewReviewRun {
            flow: flow.id,
            project,
            worktree: "/work/webapp-pr-4031".to_owned(),
            worktree_path: "/work/webapp-pr-4031".to_owned(),
            recipe: "standard".to_owned(),
            base: "origin/main".to_owned(),
        },
        1_000,
    )
    .unwrap();
    let snapshot = db
        .record_snapshot(
            flow.id,
            &Snapshot {
                head_sha: "a".repeat(40),
                tree_sha: "b".repeat(40),
                base_sha: "c".repeat(40),
                patch: BlobRef {
                    path: "patches/x.patch".to_owned(),
                    sha256: "d".repeat(64),
                    bytes: 10,
                },
            },
            1_100,
        )
        .unwrap();
    (item.id, flow.id, snapshot)
}

#[test]
fn a_second_reading_of_a_pull_request_replaces_the_first() {
    let (_dir, db) = open();
    let (item, _, _) = a_pr_review(&db);
    assert_eq!(db.mirrored_pr(item).unwrap(), None);

    let first = MirroredPr {
        item,
        pull_request: pull_request(&"1".repeat(40)),
        permission: Permission::Write,
        read_at: 2_000,
    };
    db.mirror_pr(&first).unwrap();
    let second = MirroredPr {
        pull_request: pull_request(&"2".repeat(40)),
        permission: Permission::Read,
        read_at: 3_000,
        ..first.clone()
    };
    db.mirror_pr(&second).unwrap();
    assert_eq!(db.mirrored_pr(item).unwrap(), Some(second));

    let unknown = MirroredPr {
        item: WorkItemId(99),
        ..first
    };
    assert!(matches!(
        db.mirror_pr(&unknown),
        Err(FlowStoreError::UnknownWorkItem(WorkItemId(99)))
    ));
}

#[test]
fn im_reviewing_this_keeps_the_time_it_was_first_set_and_clears_completely() {
    let (_dir, db) = open();
    let (item, _, _) = a_pr_review(&db);
    assert_eq!(db.claim(item).unwrap(), None);

    let set = db.set_claim(item, Some(5_000)).unwrap().unwrap();
    assert_eq!(set.since, 5_000);
    let again = db.set_claim(item, Some(9_000)).unwrap().unwrap();
    assert_eq!(
        again.since, 5_000,
        "setting it twice is still since the first"
    );

    assert_eq!(db.set_claim(item, None).unwrap(), None);
    assert_eq!(db.claim(item).unwrap(), None);
    assert_eq!(
        db.set_claim(item, Some(9_000)).unwrap().unwrap().since,
        9_000
    );
}

#[test]
fn a_head_a_review_moved_on_from_is_kept_with_its_findings_and_never_rewritten() {
    let (dir, db) = open();
    let (_, flow, snapshot) = a_pr_review(&db);
    let recorded = db
        .record_reviewed_head(
            &NewReviewedHead {
                flow,
                head_sha: "a".repeat(40),
                snapshot,
                findings: vec![FindingId(3), FindingId(4)],
                checks: vec![CheckRunId(7)],
                next_head: "e".repeat(40),
            },
            6_000,
        )
        .unwrap();
    assert_eq!(recorded.left_at, 6_000);
    assert_eq!(db.reviewed_heads(flow).unwrap(), vec![recorded.clone()]);
    assert_eq!(db.reviewed_heads(FlowId(99)).unwrap(), Vec::new());
    drop(db);

    // Not through the port, which has no way to ask: straight at the file, as a bug elsewhere
    // or a hand with sqlite3 would.
    let raw = rusqlite::Connection::open(dir.path().join("flow.db")).unwrap();
    let update = raw.execute("UPDATE reviewed_heads SET findings = '[]'", []);
    assert!(update.is_err(), "a reviewed head was rewritten");
    let delete = raw.execute("DELETE FROM reviewed_heads", []);
    assert!(delete.is_err(), "a reviewed head was deleted");
}

#[test]
fn every_outcome_is_kept_in_order_and_the_newest_is_the_current_one() {
    let (dir, db) = open();
    let (_, flow, _) = a_pr_review(&db);
    let first = db
        .record_outcome(
            &NewOutcome::private(flow, &"a".repeat(40), "N+1 in export", vec![FindingId(3)])
                .unwrap(),
            7_000,
        )
        .unwrap();
    let second = db
        .record_outcome(
            &NewOutcome::private(flow, &"e".repeat(40), "fixed at the new head", Vec::new())
                .unwrap(),
            8_000,
        )
        .unwrap();
    let outcomes = db.outcomes(flow).unwrap();
    assert_eq!(outcomes, vec![first, second.clone()]);
    assert_eq!(outcomes.last().unwrap().decision.kind, OutcomeKind::Private);
    assert_eq!(second.decision.notes, "fixed at the new head");
    drop(db);

    let raw = rusqlite::Connection::open(dir.path().join("flow.db")).unwrap();
    assert!(
        raw.execute("UPDATE review_outcomes SET notes = ''", [])
            .is_err()
    );
    assert!(raw.execute("DELETE FROM review_outcomes", []).is_err());
    assert!(
        raw.execute(
            "INSERT INTO review_outcomes (flow_id, kind, head_sha, notes, findings, decided_at)
             VALUES (1, 'publish', 'x', '', '[]', 1)",
            [],
        )
        .is_err(),
        "an outcome kind this build cannot record was accepted"
    );
}
