//! An issue's planning against a real SQLite file: the planning record, plan revisions, decisions,
//! and Discuss's messages (migration 13).
//!
//! What a plan approval rests on is that the revision it pins means one thing forever. So the plans
//! and decisions are append-only in the database itself, a revision is numbered by the store in the
//! transaction that writes it, and an approval and the decision that refers to it are written
//! together or not at all.

#![allow(clippy::unwrap_used)]

use std::path::Path;

use pretty_assertions::assert_eq;
use wtm_appdb::AppDb;
use wtm_core::flow::{
    ApprovalIntent, ApprovalSubject, FlowId, FlowKind, IssueRef, IssueText, NewApproval, NewPlan,
    NewPlanning, NewQuestion, NewWorkItem, Pinned, PlanApproval, PlanReport, PlanVerdict, Speaker,
    TrackerKind,
};
use wtm_core::model::ProjectId;
use wtm_core::ports::{FlowStore, FlowStoreError, InboxStore, OutboxStore, PlanStore};

struct Store {
    dir: tempfile::TempDir,
    db: AppDb,
    flow: FlowId,
}

fn project() -> ProjectId {
    ProjectId::from_root(Path::new("/work/webapp"))
}

fn issue(description: &str) -> IssueText {
    IssueText {
        key: "ACME-12".to_owned(),
        summary: "Bulk export".to_owned(),
        kind: Some("Story".to_owned()),
        description: Some(description.to_owned()),
        fields: Vec::new(),
        parent: None,
        links: Vec::new(),
    }
}

fn report(summary: &str) -> PlanReport {
    PlanReport {
        summary: summary.to_owned(),
        steps: vec!["Write the rows".to_owned()],
        acceptance_criteria: vec!["A file downloads".to_owned()],
        verification: vec!["A test reads it back".to_owned()],
        areas: vec!["src/export/**".to_owned()],
        design_needed: false,
        design_reason: None,
        risks: Vec::new(),
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
                    title: "Bulk export".to_owned(),
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

    /// A second connection to the same file, as `sqlite3` would open one, so the database's own
    /// rules are what is tested rather than the store's.
    fn raw(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.dir.path().join("flow.db")).unwrap()
    }

    fn plan(&self, summary: &str, description: &str) -> wtm_core::flow::PlanRecord {
        self.db
            .record_plan(
                &NewPlan {
                    flow: self.flow,
                    step: None,
                    invocation: None,
                    plan: report(summary),
                    issue: issue(description),
                },
                2_000,
            )
            .unwrap()
    }
}

#[test]
fn a_runs_planning_record_is_kept_once_with_the_role_it_was_first_given() {
    let s = Store::new();
    let first =
        s.db.begin_planning(
            &NewPlanning {
                flow: s.flow,
                project: project(),
                worktree: "/work/ACME-12".to_owned(),
                key: "ACME-12".to_owned(),
                tracker: TrackerKind::Jira,
                role: "planner".to_owned(),
            },
            1_500,
        )
        .unwrap();
    let again =
        s.db.begin_planning(
            &NewPlanning {
                role: "another".to_owned(),
                ..NewPlanning {
                    flow: s.flow,
                    project: project(),
                    worktree: "/work/ACME-12".to_owned(),
                    key: "ACME-12".to_owned(),
                    tracker: TrackerKind::Jira,
                    role: String::new(),
                }
            },
            1_600,
        )
        .unwrap();
    assert_eq!(again, first);
    assert_eq!(s.db.plannings().unwrap(), [first]);
}

#[test]
fn plans_are_numbered_by_the_store_and_carry_the_issue_they_answered() {
    let s = Store::new();
    let one = s.plan("CSV", "Export as a spreadsheet.");
    let two = s.plan("CSV, ordered", "Export as a spreadsheet.");
    assert_eq!((one.revision, two.revision), (1, 2));
    assert_eq!(
        one.issue_revision,
        issue("Export as a spreadsheet.").revision()
    );
    assert_eq!(
        s.db.plans(s.flow)
            .unwrap()
            .iter()
            .map(|p| p.plan.summary.as_str())
            .collect::<Vec<_>>(),
        ["CSV", "CSV, ordered"]
    );
}

#[test]
fn a_plan_revision_cannot_be_rewritten_once_handed_in() {
    let s = Store::new();
    let one = s.plan("CSV", "x");
    let rewritten = s.raw().execute(
        "UPDATE plans SET plan = '{}' WHERE id = ?1",
        rusqlite::params![one.id.0],
    );
    assert!(rewritten.unwrap_err().to_string().contains("append-only"));
}

#[test]
fn approving_a_plan_writes_the_approval_and_the_decision_together() {
    let s = Store::new();
    let plan = s.plan("CSV", "x");
    let subject = PlanApproval {
        revision: plan.revision,
        issue: plan.issue.clone(),
        skip_design: true,
        design_needed: true,
    };
    let (approved, decision) =
        s.db.approve_plan(
            &NewApproval::plan(s.flow, &subject, &plan.issue_revision),
            plan.id,
            true,
            3_000,
        )
        .unwrap();
    assert_eq!(approved.approval.intent, ApprovalIntent::Plan);
    assert_eq!(
        approved.approval.binding.0.get(&Pinned::PlanRevision),
        Some(&"1".to_owned())
    );
    assert_eq!(
        approved.approval.binding.0.get(&Pinned::IssueRevision),
        Some(&plan.issue_revision)
    );
    assert!(matches!(approved.subject, ApprovalSubject::Plan(_)));
    assert!(approved.operations.is_empty());
    assert_eq!(
        decision.verdict,
        PlanVerdict::Approved {
            approval: approved.approval.id,
            skip_design: true,
        }
    );
    assert_eq!(s.db.plan_decisions(s.flow).unwrap(), [decision]);
    assert_eq!(s.db.approvals(s.flow).unwrap().len(), 1);
}

#[test]
fn asking_for_changes_keeps_the_note_and_leaves_the_earlier_approval_in_the_record() {
    let s = Store::new();
    let plan = s.plan("CSV", "x");
    let subject = PlanApproval {
        revision: 1,
        issue: plan.issue.clone(),
        skip_design: false,
        design_needed: false,
    };
    s.db.approve_plan(
        &NewApproval::plan(s.flow, &subject, &plan.issue_revision),
        plan.id,
        false,
        3_000,
    )
    .unwrap();
    let changes =
        s.db.request_plan_changes(plan.id, "Keep the column order", 4_000)
            .unwrap();
    assert_eq!(
        changes.verdict,
        PlanVerdict::ChangesRequested {
            note: "Keep the column order".to_owned()
        }
    );
    let all = s.db.plan_decisions(s.flow).unwrap();
    assert_eq!(all.len(), 2);
    assert!(matches!(all[0].verdict, PlanVerdict::Approved { .. }));
    let erased = s.raw().execute("DELETE FROM plan_decisions", []);
    assert!(erased.unwrap_err().to_string().contains("append-only"));
}

#[test]
fn a_discussion_is_kept_per_question_in_the_order_it_was_said() {
    let s = Store::new();
    let (question, _) =
        s.db.ask(
            &NewQuestion {
                flow: s.flow,
                step: None,
                role: Some("planner".to_owned()),
                text: "CSV or XLSX?".to_owned(),
                why: "The issue says spreadsheet".to_owned(),
                unblocks: "the plan".to_owned(),
                blocks: Some(wtm_core::flow::Phase::Planned),
                context: Vec::new(),
                recommendation: None,
                options: vec!["CSV".to_owned(), "XLSX".to_owned()],
            },
            "planner asks: CSV or XLSX?",
            1_000,
        )
        .unwrap();
    s.db.discuss(
        question.id,
        Speaker::You,
        "Does anything else use the writer?",
        1_100,
    )
    .unwrap();
    s.db.discuss(question.id, Speaker::Agent, "Two reports do.", 1_200)
        .unwrap();
    let thread = s.db.discussion(question.id).unwrap();
    assert_eq!(
        thread
            .iter()
            .map(|m| (m.from, m.text.as_str()))
            .collect::<Vec<_>>(),
        [
            (Speaker::You, "Does anything else use the writer?"),
            (Speaker::Agent, "Two reports do."),
        ]
    );
    assert!(matches!(
        s.db.discuss(wtm_core::flow::QuestionId(404), Speaker::You, "?", 1),
        Err(FlowStoreError::UnknownQuestion(_))
    ));
}
