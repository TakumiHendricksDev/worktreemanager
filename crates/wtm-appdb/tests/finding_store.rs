//! A review's findings and check records against a real SQLite file.
//!
//! These protect what the Reviews tab reads back and what a rule's provenance points at: a finding
//! keeps every report it was made of, a superseded finding leaves the list but not the record, a
//! dismissal keeps its reason, and a check record reads as a pass only when its command exited 0 —
//! the store refuses to write one that says otherwise.

#![allow(clippy::unwrap_used)]

use std::path::Path;

use pretty_assertions::assert_eq;
use wtm_appdb::AppDb;
use wtm_core::flow::{
    CheckStatus, Evidence, FindingClass, FindingId, FlowKind, InvocationId, Location, NewCheckRun,
    NewFinding, NewInvocation, NewReviewRun, NewStep, NewWorkItem, Reconciled, ReportedClass,
    ReportedFinding, RoleProfile, RoleProvider, Severity, Source, StepStatus, Workspace,
};
use wtm_core::model::ProjectId;
use wtm_core::ports::{FlowStore, FlowStoreError, RunStore};

fn open() -> (tempfile::TempDir, AppDb) {
    let dir = tempfile::tempdir().unwrap();
    let db = AppDb::open(&dir.path().join("flow.db")).unwrap();
    (dir, db)
}

/// A review flow with one step and one invocation, which a report needs to point at.
fn a_review(db: &AppDb) -> (wtm_core::flow::FlowId, Source) {
    let project = ProjectId::from_root(Path::new("/work/webapp"));
    let item = db
        .create_work_item(
            &NewWorkItem {
                project: project.clone(),
                title: "fix/export".to_owned(),
                issue: None,
                pull_request: None,
            },
            1_000,
        )
        .unwrap();
    let flow = db.start_flow(item.id, FlowKind::ReviewMine, 1_000).unwrap();
    db.create_review(
        &NewReviewRun {
            flow: flow.id,
            project,
            worktree: "/work/webapp-wt".to_owned(),
            worktree_path: "/work/webapp-wt".to_owned(),
            recipe: "standard".to_owned(),
            base: "origin/main".to_owned(),
        },
        1_000,
    )
    .unwrap();
    let step = db
        .add_steps(
            flow.id,
            &[NewStep {
                stage: 0,
                role: "correctness".to_owned(),
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
            }],
            1_000,
        )
        .unwrap()
        .remove(0);
    let invocation = db
        .start_invocation(
            &NewInvocation {
                flow: flow.id,
                step: step.id,
                attempt: 1,
                role: "correctness".to_owned(),
                provider: RoleProvider::Codex,
                model: None,
                profile: RoleProfile::Reviewer,
                snapshot: None,
                cwd: "/data/flow/1/checkouts/t1".to_owned(),
            },
            1_000,
        )
        .unwrap();
    let source = Source {
        step: step.id,
        invocation: invocation.id,
        ordinal: 0,
        role: "correctness".to_owned(),
        provider: RoleProvider::Codex,
        model: None,
        workspace: Workspace::Snapshot,
        tree_sha: "t1".to_owned(),
    };
    (flow.id, source)
}

fn reported(title: &str, line: u32) -> ReportedFinding {
    ReportedFinding {
        title: title.to_owned(),
        severity: Severity::Notable,
        class: ReportedClass::ConfirmedBug,
        body: "The loop stops one row early.".to_owned(),
        path: Some("src/export.rs".to_owned()),
        line: Some(line),
        end_line: None,
        verification: Some("ran the range test".to_owned()),
        rule: None,
        between: Vec::new(),
        requirement: None,
        evidence: vec!["range test: 4 rows of 5".to_owned()],
    }
}

fn new_finding(evidence: Vec<Evidence>) -> NewFinding {
    let lead = &evidence[0].reported;
    NewFinding {
        class: lead.finding_class(),
        severity: lead.severity,
        location: lead.location(),
        lines: true,
        title: lead.title.clone(),
        rationale: lead.body.clone(),
        requirement: None,
        tree_sha: "t1".to_owned(),
        dismissed: None,
        evidence,
    }
}

#[test]
fn a_consolidation_saves_each_finding_with_every_report_it_was_made_of() {
    let (_dir, db) = open();
    let (flow, source) = a_review(&db);
    let second = Source {
        ordinal: 1,
        ..source.clone()
    };
    let evidence = vec![
        Evidence {
            source: source.clone(),
            reported: reported("Off by one", 3),
        },
        Evidence {
            source: second,
            reported: reported("Stops a row early", 4),
        },
    ];

    let saved = db
        .save_consolidation(
            flow,
            &Reconciled {
                create: vec![new_finding(evidence.clone())],
                ..Reconciled::default()
            },
            2_000,
        )
        .unwrap();

    assert_eq!(saved.len(), 1);
    assert_eq!(
        saved[0].evidence, evidence,
        "every report, exactly as submitted"
    );
    assert_eq!(
        saved[0].finding.class,
        FindingClass::ConfirmedBug {
            verification: "ran the range test".to_owned()
        }
    );
    assert_eq!(
        saved[0].finding.location,
        Some(Location {
            path: "src/export.rs".to_owned(),
            start_line: 3,
            end_line: 3
        })
    );
}

#[test]
fn a_superseded_finding_leaves_the_list_and_stays_readable_by_its_id() {
    let (_dir, db) = open();
    let (flow, source) = a_review(&db);
    let first = db
        .save_consolidation(
            flow,
            &Reconciled {
                create: vec![new_finding(vec![Evidence {
                    source,
                    reported: reported("Off by one", 3),
                }])],
                ..Reconciled::default()
            },
            2_000,
        )
        .unwrap()
        .remove(0);

    let after = db
        .save_consolidation(
            flow,
            &Reconciled {
                supersede: vec![first.finding.id],
                ..Reconciled::default()
            },
            3_000,
        )
        .unwrap();

    assert!(after.is_empty());
    assert_eq!(
        db.finding(first.finding.id)
            .unwrap()
            .map(|r| r.finding.title),
        Some("Off by one".to_owned()),
        "a rule saved from it still names a finding that exists"
    );
}

#[test]
fn a_dismissal_keeps_its_reason_and_can_be_taken_back() {
    let (_dir, db) = open();
    let (flow, source) = a_review(&db);
    let id = db
        .save_consolidation(
            flow,
            &Reconciled {
                create: vec![new_finding(vec![Evidence {
                    source,
                    reported: reported("Off by one", 3),
                }])],
                ..Reconciled::default()
            },
            2_000,
        )
        .unwrap()[0]
        .finding
        .id;

    let dismissed = db
        .dismiss_finding(id, Some("the last row is a header"), 4_000)
        .unwrap();
    let dismissal = dismissed.finding.dismissed.unwrap();
    assert_eq!(dismissal.reason, "the last row is a header");
    assert_eq!(dismissal.at, 4_000);
    assert_eq!(
        db.findings(flow).unwrap().len(),
        1,
        "a dismissed finding is still listed"
    );

    let restored = db.dismiss_finding(id, None, 5_000).unwrap();
    assert_eq!(restored.finding.dismissed, None);
}

#[test]
fn dismissing_a_finding_that_does_not_exist_is_refused_by_name() {
    let (_dir, db) = open();
    assert!(matches!(
        db.dismiss_finding(FindingId(99), Some("x"), 1),
        Err(FlowStoreError::UnknownFinding(FindingId(99)))
    ));
}

fn check(status: CheckStatus, exit_code: Option<i32>) -> NewCheckRun {
    NewCheckRun {
        flow: wtm_core::flow::FlowId(1),
        check: "unit".to_owned(),
        step: None,
        invocation: Some(InvocationId(1)),
        role: Some("correctness".to_owned()),
        status,
        detail: None,
        exit_code,
        tree_sha: "t1".to_owned(),
        cwd: "/data/flow/1/checkouts/t1".to_owned(),
        output: None,
        started_at: 1_000,
    }
}

#[test]
fn a_check_record_reads_as_a_pass_only_when_its_command_exited_zero() {
    let (_dir, db) = open();
    let (flow, _) = a_review(&db);
    let at = |run: NewCheckRun| NewCheckRun { flow, ..run };

    let passed = db
        .record_check(&at(check(CheckStatus::Passed, Some(0))), 2_000)
        .unwrap();
    assert_eq!(passed.status, CheckStatus::Passed);
    db.record_check(&at(check(CheckStatus::Failed, Some(2))), 2_000)
        .unwrap();
    let skipped = db
        .record_check(
            &at(NewCheckRun {
                detail: Some("needs `web`, and wtm starts no services yet".to_owned()),
                ..check(CheckStatus::Skipped, None)
            }),
            2_000,
        )
        .unwrap();
    assert_eq!(skipped.status, CheckStatus::Skipped);

    for refused in [
        check(CheckStatus::Passed, Some(1)),
        check(CheckStatus::Passed, None),
        check(CheckStatus::Failed, Some(0)),
        check(CheckStatus::Skipped, Some(0)),
    ] {
        assert!(
            db.record_check(&at(refused.clone()), 2_000).is_err(),
            "{:?} with exit {:?} must not be written",
            refused.status,
            refused.exit_code
        );
    }
    assert_eq!(db.checks(flow).unwrap().len(), 3);
}
