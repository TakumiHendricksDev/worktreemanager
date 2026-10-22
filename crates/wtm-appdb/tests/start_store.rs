//! Start Work's records against a real SQLite file: the job keyed by its work item, the move
//! approvals migration 11 makes room for, and the inbox naming where an issue's run is.
//!
//! The job's promise is the one the plan's I2 scenario rests on: two Starts of one issue make one
//! job, and so one worktree. So the first property is that a second job for a work item whose run
//! is open is refused, in the store, naming the run that is under way.

#![allow(clippy::unwrap_used)]

use std::path::Path;

use pretty_assertions::assert_eq;
use wtm_appdb::AppDb;
use wtm_core::flow::{
    Candidate, FlowEvent, FlowId, FlowKind, InboxKind, InboxTarget, IssueRef, MoveCause,
    MoveIssuePayload, MoveRequest, NewApproval, NewInboxItem, NewOperation, NewStartJob,
    NewWorkItem, OperationChange, OperationPayload, OperationResult, OperationState, Place,
    StartJobChange, StartJobError, StartJobState, TrackerKind, WorkItemId,
};
use wtm_core::model::ProjectId;
use wtm_core::ports::{FlowStore, FlowStoreError, InboxStore, OutboxStore, StartStore};

struct Store {
    _dir: tempfile::TempDir,
    db: AppDb,
    item: WorkItemId,
}

fn project() -> ProjectId {
    ProjectId::from_root(Path::new("/work/webapp"))
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
        Self {
            _dir: dir,
            db,
            item,
        }
    }

    fn flow(&self) -> FlowId {
        self.db
            .start_flow(self.item, FlowKind::Implement, 1_000)
            .unwrap()
            .id
    }

    fn job(&self, flow: FlowId, state: StartJobState) -> NewStartJob {
        NewStartJob {
            work_item: self.item,
            flow,
            project: project(),
            key: "ACME-12".to_owned(),
            candidate: Candidate::New {
                branch: Some("ACME-12".to_owned()),
                directory: "/work/ACME-12".to_owned(),
                base: "origin/main".to_owned(),
            },
            worktree: "/work/ACME-12".to_owned(),
            branch: Some("ACME-12".to_owned()),
            state,
        }
    }
}

#[test]
fn a_second_start_job_for_an_issue_whose_run_is_open_is_refused_naming_that_run() {
    let s = Store::new();
    let flow = s.flow();
    s.db.record_start_job(&s.job(flow, StartJobState::Creating), 1_100)
        .unwrap();

    let again =
        s.db.record_start_job(&s.job(flow, StartJobState::Creating), 1_200)
            .unwrap_err();
    assert!(
        matches!(again, FlowStoreError::StartUnderway { item, flow: running }
            if item == s.item && running == flow),
        "{again:?}"
    );
    assert_eq!(s.db.start_jobs(None).unwrap().len(), 1);
}

#[test]
fn starting_an_issue_again_after_its_run_ended_replaces_the_job_with_the_new_runs() {
    let s = Store::new();
    let first = s.flow();
    s.db.record_start_job(&s.job(first, StartJobState::Ready), 1_100)
        .unwrap();
    s.db.record(first, &FlowEvent::Cancelled, 1_150).unwrap();

    let second = s.flow();
    let job =
        s.db.record_start_job(&s.job(second, StartJobState::Creating), 1_200)
            .unwrap();

    assert_eq!(job.flow, second);
    assert_eq!(job.state, StartJobState::Creating);
    assert_eq!(s.db.start_job(s.item).unwrap().unwrap().flow, second);
    assert_eq!(s.db.start_job_of(first).unwrap(), None);
    assert_eq!(s.db.start_jobs(Some(&project())).unwrap().len(), 1);
}

#[test]
fn a_start_job_moves_only_as_its_rules_allow_and_keeps_its_terminal_and_detail() {
    let s = Store::new();
    let flow = s.flow();
    s.db.record_start_job(&s.job(flow, StartJobState::Creating), 1_100)
        .unwrap();

    let mut setting_up = StartJobChange::to(StartJobState::SettingUp);
    setting_up.setup_session = wtm_core::flow::Field::Set("pty-7".to_owned());
    s.db.change_start_job(s.item, &setting_up, 1_200).unwrap();
    let failed =
        s.db.change_start_job(
            s.item,
            &StartJobChange::to(StartJobState::SetupFailed).detail("exited with 2"),
            1_300,
        )
        .unwrap();
    assert_eq!(failed.setup_session.as_deref(), Some("pty-7"));
    assert_eq!(failed.detail.as_deref(), Some("exited with 2"));

    let refused =
        s.db.change_start_job(s.item, &StartJobChange::to(StartJobState::Creating), 1_400)
            .unwrap_err();
    assert!(
        matches!(
            refused,
            FlowStoreError::StartJob {
                reason: StartJobError::NoSuchStep { .. }
            }
        ),
        "{refused:?}"
    );
    assert_eq!(
        s.db.start_job(s.item).unwrap().unwrap().state,
        StartJobState::SetupFailed,
        "a refused change wrote nothing"
    );
}

#[test]
fn an_issue_runs_inbox_item_names_the_project_and_the_worktree_its_start_is_giving_it() {
    let s = Store::new();
    let flow = s.flow();
    let mut job = s.job(flow, StartJobState::Ready);
    job.candidate = Candidate::KeyInName {
        worktree: Place {
            id: "/work/ACME-12-export".to_owned(),
            branch: Some("feature/ACME-12-export".to_owned()),
            key: Some("ACME-12".to_owned()),
        },
    };
    job.worktree = "/work/ACME-12-export".to_owned();
    s.db.record_start_job(&job, 1_100).unwrap();
    s.db.file_inbox(
        &NewInboxItem {
            flow,
            kind: InboxKind::Approval,
            target: InboxTarget::Flow,
            title: "Pause the run?".to_owned(),
            body: String::new(),
        },
        1_200,
    )
    .unwrap()
    .unwrap();

    let listed = s.db.inbox(None, Some(&project())).unwrap();
    assert_eq!(listed.len(), 1, "found by the project its start names");
    assert_eq!(listed[0].project, Some(project()));
    assert_eq!(listed[0].worktree.as_deref(), Some("/work/ACME-12-export"));
}

#[test]
fn a_move_is_approved_and_queued_once_however_often_it_is_asked_for() {
    let s = Store::new();
    let flow = s.flow();
    let request = MoveRequest {
        payload: MoveIssuePayload {
            tracker: TrackerKind::Jira,
            key: "ACME-12".to_owned(),
            from: "To Do".to_owned(),
            to: "In Progress".to_owned(),
        },
        cause: MoveCause::Start,
    };
    let op = || {
        NewOperation::new(
            flow,
            0,
            OperationPayload::MoveIssue(request.payload.clone()),
        )
    };
    let first =
        s.db.approve(&NewApproval::move_issue(flow, &request), &[op()], 1_100)
            .unwrap();
    let moved = first.clone().into_move().unwrap();
    assert_eq!(moved.request, request);
    assert_eq!(moved.operations[0].key.target, "jira:ACME-12");

    let op_id = moved.operations[0].id;
    s.db.change_operation(op_id, &OperationChange::sending(), 1_200)
        .unwrap();
    s.db.change_operation(
        op_id,
        &OperationChange::done(OperationResult::Moved {
            key: "ACME-12".to_owned(),
            status: "In Progress".to_owned(),
            adopted: false,
        }),
        1_300,
    )
    .unwrap();

    let again =
        s.db.approve(&NewApproval::move_issue(flow, &request), &[op()], 1_400)
            .unwrap();
    assert_eq!(again.operations.len(), 1);
    assert_eq!(
        again.operations[0].id, op_id,
        "the same move is the same row"
    );
    assert_eq!(again.operations[0].state, OperationState::Done);
    assert_eq!(s.db.issue_moves(flow).unwrap().len(), 2);
}
