//! An issue's design against a real SQLite file: the design record, its revisions, the comments on
//! each, and your decisions (migration 15).
//!
//! What a design approval rests on is the plan's: the revision it pins means one thing forever. So
//! revisions, comments and decisions are append-only in the database itself, a revision is numbered
//! by the store in the transaction that writes it, and an approval and the decision that refers to
//! it are written together. What is new is that every revision stays addressable, with what it was
//! made from and what was said about it, however many came after it.

#![allow(clippy::unwrap_used)]

use std::path::Path;

use pretty_assertions::assert_eq;
use wtm_appdb::AppDb;
use wtm_core::flow::{
    ApprovalIntent, ApprovalSubject, DesignApproval, DesignOrigin, DesignRevision, DesignVerdict,
    FlowId, FlowKind, IssueRef, NewApproval, NewDesign, NewDesignRevision, NewWorkItem, Pinned,
    SourcePin,
};
use wtm_core::model::ProjectId;
use wtm_core::ports::{DesignStore, FlowStore, FlowStoreError, OutboxStore};

struct Store {
    dir: tempfile::TempDir,
    db: AppDb,
    flow: FlowId,
}

fn project() -> ProjectId {
    ProjectId::from_root(Path::new("/work/webapp"))
}

fn design(flow: FlowId, role: &str) -> NewDesign {
    NewDesign {
        flow,
        project: project(),
        worktree: "/work/ACME-12".to_owned(),
        key: "ACME-12".to_owned(),
        role: role.to_owned(),
        origin: DesignOrigin::Plan,
        request: None,
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
                    title: "Unit export screen".to_owned(),
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

    /// Like [`Self::new`], with the run's design begun and its prototype kept.
    fn designing() -> Self {
        let s = Self::new();
        s.db.begin_design(&design(s.flow, "designer"), 1_500)
            .unwrap();
        s.db.set_design_prototype(s.flow, "proto-1", 1_600).unwrap();
        s
    }

    fn raw(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.dir.path().join("flow.db")).unwrap()
    }

    fn publish(&self, review: &str, fingerprint: &str, feedback: Option<&str>) -> DesignRevision {
        self.db
            .record_design_revision(
                &NewDesignRevision {
                    flow: self.flow,
                    prototype: "proto-1".to_owned(),
                    review: review.to_owned(),
                    fingerprint: fingerprint.to_owned(),
                    brief: "A unit export screen".to_owned(),
                    feedback: feedback.map(str::to_owned),
                    reuse: format!("Reuse notes for {review}"),
                    sources: vec![SourcePin {
                        path: "src/components/PageHeader.svelte".to_owned(),
                        fingerprint: "f1".to_owned(),
                    }],
                    plan_revision: 1,
                    step: None,
                    invocation: None,
                },
                2_000,
            )
            .unwrap()
    }
}

#[test]
fn a_runs_design_is_kept_once_with_the_role_and_origin_it_was_first_given() {
    let s = Store::new();
    let first =
        s.db.begin_design(&design(s.flow, "designer"), 1_500)
            .unwrap();
    let again =
        s.db.begin_design(
            &NewDesign {
                origin: DesignOrigin::You,
                ..design(s.flow, "another")
            },
            1_600,
        )
        .unwrap();
    assert_eq!(again, first);
    assert_eq!(first.origin, DesignOrigin::Plan);
    assert_eq!(s.db.designs().unwrap(), [first]);
}

#[test]
fn a_design_keeps_one_prototype_so_its_revisions_are_one_history() {
    let s = Store::designing();
    assert_eq!(
        s.db.set_design_prototype(s.flow, "proto-1", 1_700)
            .unwrap()
            .prototype
            .as_deref(),
        Some("proto-1")
    );
    assert!(s.db.set_design_prototype(s.flow, "proto-2", 1_800).is_err());
    assert!(matches!(
        s.db.set_design_prototype(FlowId(404), "proto-1", 1_800),
        Err(FlowStoreError::UnknownDesign(_))
    ));
}

#[test]
fn every_revision_stays_addressable_with_what_it_was_made_from_and_what_it_answered() {
    let s = Store::designing();
    let one = s.publish("r-aa", "sha256:aa", None);
    let two = s.publish("r-bb", "sha256:bb", Some("Header: use PageHeader"));
    let three = s.publish("r-cc", "sha256:cc", Some("Tighter spacing"));
    assert_eq!(
        (one.revision, two.revision, three.revision),
        (1, 2, 3),
        "the store numbers revisions"
    );
    let all = s.db.design_revisions(s.flow).unwrap();
    assert_eq!(all, [one.clone(), two, three]);
    // The first is still all there after two more: its artifact, fingerprint, brief and notes.
    assert_eq!(all[0].review, "r-aa");
    assert_eq!(all[0].fingerprint, "sha256:aa");
    assert_eq!(all[0].reuse, "Reuse notes for r-aa");
    assert_eq!(all[0].sources[0].path, "src/components/PageHeader.svelte");
    assert_eq!(all[1].feedback.as_deref(), Some("Header: use PageHeader"));
}

#[test]
fn a_design_revision_cannot_be_rewritten_or_removed_once_published() {
    let s = Store::designing();
    let one = s.publish("r-aa", "sha256:aa", None);
    let rewritten = s.raw().execute(
        "UPDATE design_revisions SET fingerprint = 'sha256:ff' WHERE id = ?1",
        rusqlite::params![one.id.0],
    );
    assert!(rewritten.unwrap_err().to_string().contains("append-only"));
    let removed = s.raw().execute("DELETE FROM design_revisions", []);
    assert!(removed.unwrap_err().to_string().contains("append-only"));
}

#[test]
fn a_revision_needs_a_design_to_belong_to() {
    let s = Store::new();
    let refused = s.db.record_design_revision(
        &NewDesignRevision {
            flow: s.flow,
            prototype: "proto-1".to_owned(),
            review: "r-aa".to_owned(),
            fingerprint: "sha256:aa".to_owned(),
            brief: "b".to_owned(),
            feedback: None,
            reuse: "r".to_owned(),
            sources: Vec::new(),
            plan_revision: 1,
            step: None,
            invocation: None,
        },
        2_000,
    );
    assert!(matches!(refused, Err(FlowStoreError::UnknownDesign(_))));
}

#[test]
fn comments_stay_with_the_revision_they_were_made_on() {
    let s = Store::designing();
    s.publish("r-aa", "sha256:aa", None);
    s.publish("r-bb", "sha256:bb", None);
    s.db.comment_on_design(s.flow, 1, "Header: use PageHeader", 3_000)
        .unwrap();
    s.db.comment_on_design(s.flow, 2, "Better", 3_100).unwrap();
    s.db.comment_on_design(s.flow, 1, "The old header was taller", 3_200)
        .unwrap();
    let comments = s.db.design_comments(s.flow).unwrap();
    assert_eq!(
        comments
            .iter()
            .map(|c| (c.revision, c.text.as_str()))
            .collect::<Vec<_>>(),
        [
            (1, "Header: use PageHeader"),
            (2, "Better"),
            (1, "The old header was taller"),
        ]
    );
    assert!(matches!(
        s.db.comment_on_design(s.flow, 9, "Nothing to say it on", 3_300),
        Err(FlowStoreError::NoSuchRevision { revision: 9, .. })
    ));
    let erased = s.raw().execute("DELETE FROM design_comments", []);
    assert!(erased.unwrap_err().to_string().contains("append-only"));
}

#[test]
fn approving_a_revision_writes_the_approval_and_the_decision_together() {
    let s = Store::designing();
    let one = s.publish("r-aa", "sha256:aa", None);
    let subject = DesignApproval::of(&one);
    let (approved, decision) =
        s.db.approve_design(&NewApproval::design(s.flow, &subject), one.id, 4_000)
            .unwrap();
    assert_eq!(approved.approval.intent, ApprovalIntent::Design);
    assert_eq!(
        approved.approval.binding.0.get(&Pinned::DesignRevision),
        Some(&"1".to_owned())
    );
    assert_eq!(
        approved.approval.binding.0.get(&Pinned::Payload),
        Some(&"sha256:aa".to_owned())
    );
    assert!(approved.approval.binding.0.contains_key(&Pinned::Sources));
    assert!(matches!(approved.subject, ApprovalSubject::Design(_)));
    assert!(approved.operations.is_empty());
    assert_eq!(
        decision.verdict,
        DesignVerdict::Approved {
            approval: approved.approval.id
        }
    );
    assert_eq!(s.db.design_decisions(s.flow).unwrap(), [decision]);
    assert_eq!(s.db.approvals(s.flow).unwrap().len(), 1);
}

#[test]
fn asking_for_changes_after_approving_leaves_the_approval_in_the_record() {
    let s = Store::designing();
    let one = s.publish("r-aa", "sha256:aa", None);
    s.db.approve_design(
        &NewApproval::design(s.flow, &DesignApproval::of(&one)),
        one.id,
        4_000,
    )
    .unwrap();
    let changes =
        s.db.request_design_changes(one.id, "Header: use PageHeader", 5_000)
            .unwrap();
    assert_eq!(changes.revision, 1);
    let all = s.db.design_decisions(s.flow).unwrap();
    assert_eq!(all.len(), 2);
    assert!(matches!(all[0].verdict, DesignVerdict::Approved { .. }));
    assert_eq!(
        all[1].verdict,
        DesignVerdict::ChangesRequested {
            note: "Header: use PageHeader".to_owned()
        }
    );
    let erased = s.raw().execute("DELETE FROM design_decisions", []);
    assert!(erased.unwrap_err().to_string().contains("append-only"));
}

#[test]
fn a_revision_of_another_run_cannot_be_approved_under_this_one() {
    let s = Store::designing();
    let one = s.publish("r-aa", "sha256:aa", None);
    let refused = s.db.approve_design(
        &NewApproval::design(FlowId(404), &DesignApproval::of(&one)),
        one.id,
        4_000,
    );
    assert!(matches!(
        refused,
        Err(FlowStoreError::UnknownDesignRevision(_))
    ));
    assert!(s.db.design_decisions(s.flow).unwrap().is_empty());
}
