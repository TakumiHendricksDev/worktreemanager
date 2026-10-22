//! Worktree environments against a real SQLite file.
//!
//! What the supervisor relies on the store for: a service is one row per worktree and service, and
//! the pid it holds is never without the start time that proves the process is the one wtm
//! started, across a reopen; one command waits for approval at a time per worktree, and the store
//! names it; an approved run runs once and stays over; an environment's inbox item knows its project
//! and worktree as a review's does; and idle stop stays off until you opt in.

#![allow(clippy::unwrap_used)]

use std::path::Path;

use pretty_assertions::assert_eq;
use wtm_appdb::AppDb;
use wtm_core::flow::{
    ApprovalId, CommandRunChange, CommandRunState, FlowEvent, FlowId, FlowKind, InboxKind,
    InboxTarget, NewCommandRun, NewInboxItem, NewWorkItem, Phase, ProcessIdentity, ServiceRecord,
    ServiceState, command_binding,
};
use wtm_core::model::ProjectId;
use wtm_core::ports::{EnvStore, FlowStore, FlowStoreError, InboxStore};

const PROJECT: &str = "/work/webapp";
const TREE: &str = "/work/webapp-export";

fn project() -> ProjectId {
    ProjectId::from_root(Path::new(PROJECT))
}

fn environment(db: &AppDb) -> FlowId {
    let item = db
        .create_work_item(
            &NewWorkItem {
                project: project(),
                title: "export".to_owned(),
                issue: None,
                pull_request: None,
            },
            1_000,
        )
        .unwrap();
    let flow = db
        .start_flow(item.id, FlowKind::Environment, 1_000)
        .unwrap()
        .id;
    db.record(
        flow,
        &FlowEvent::Advanced {
            to: Phase::Operating,
        },
        1_000,
    )
    .unwrap();
    db.bind_environment_flow(flow, &project(), TREE).unwrap();
    flow
}

fn web(state: ServiceState, process: Option<ProcessIdentity>) -> ServiceRecord {
    ServiceRecord {
        project: PROJECT.to_owned(),
        worktree: TREE.to_owned(),
        service: "web".to_owned(),
        state,
        process,
        detached: false,
        log: "/data/env/web.log".to_owned(),
        detail: None,
        started_at: Some(2_000),
        updated_at: 2_000,
    }
}

fn reset(flow: FlowId, state: CommandRunState) -> NewCommandRun {
    NewCommandRun {
        flow,
        command: "reset".to_owned(),
        label: "Reset the database".to_owned(),
        argv: vec!["just".to_owned(), "reset-db".to_owned()],
        binding: command_binding(&["just".to_owned(), "reset-db".to_owned()], TREE),
        state,
    }
}

#[test]
fn a_service_is_one_row_per_worktree_and_keeps_the_identity_that_proves_its_process_across_a_reopen()
 {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("flow.db");
    let process = ProcessIdentity {
        pid: 4242,
        started: "Tue Oct 21 14:02:03 2026".to_owned(),
    };
    {
        let db = AppDb::open(&path).unwrap();
        db.save_service(&web(ServiceState::Starting, Some(process.clone())))
            .unwrap();
        db.save_service(&web(ServiceState::Healthy, Some(process.clone())))
            .unwrap();
    }

    let db = AppDb::open(&path).unwrap();
    let services = db.services(Some(TREE)).unwrap();
    assert_eq!(services.len(), 1, "saving again replaces the row");
    assert_eq!(services[0].state, ServiceState::Healthy);
    assert_eq!(services[0].process, Some(process));

    db.save_service(&web(ServiceState::Stopped, None)).unwrap();
    assert_eq!(db.services(None).unwrap()[0].process, None);
    assert!(db.services(Some("/elsewhere")).unwrap().is_empty());
}

#[test]
fn a_second_command_waiting_for_approval_is_refused_and_names_the_one_already_waiting() {
    let dir = tempfile::tempdir().unwrap();
    let db = AppDb::open(&dir.path().join("flow.db")).unwrap();
    let flow = environment(&db);

    let first = db
        .record_command_run(&reset(flow, CommandRunState::AwaitingApproval), 3_000)
        .unwrap();
    match db.record_command_run(&reset(flow, CommandRunState::AwaitingApproval), 3_001) {
        Err(FlowStoreError::CommandWaiting { existing }) => assert_eq!(existing, first.id),
        other => panic!("expected CommandWaiting, got {other:?}"),
    }
    // A command that needs no approval is not held up by one that does.
    db.record_command_run(&reset(flow, CommandRunState::Running), 3_002)
        .unwrap();

    db.change_command_run(
        first.id,
        &CommandRunChange {
            state: Some(CommandRunState::Declined),
            ..CommandRunChange::default()
        },
        3_003,
    )
    .unwrap();
    db.record_command_run(&reset(flow, CommandRunState::AwaitingApproval), 3_004)
        .unwrap();
}

#[test]
fn an_approved_run_runs_once_and_a_finished_run_cannot_run_again() {
    let dir = tempfile::tempdir().unwrap();
    let db = AppDb::open(&dir.path().join("flow.db")).unwrap();
    let flow = environment(&db);
    let run = db
        .record_command_run(&reset(flow, CommandRunState::AwaitingApproval), 3_000)
        .unwrap();
    assert_eq!(run.finished_at, None);

    let running = db
        .change_command_run(
            run.id,
            &CommandRunChange {
                state: Some(CommandRunState::Running),
                approval: Some(ApprovalId(9)),
                ..CommandRunChange::default()
            },
            3_100,
        )
        .unwrap_err();
    assert!(
        matches!(running, FlowStoreError::Storage(_)),
        "approval 9 does not exist, and the foreign key says so: {running:?}"
    );

    db.change_command_run(
        run.id,
        &CommandRunChange {
            state: Some(CommandRunState::Running),
            ..CommandRunChange::default()
        },
        3_100,
    )
    .unwrap();
    let done = db
        .change_command_run(
            run.id,
            &CommandRunChange {
                state: Some(CommandRunState::Succeeded),
                exit_code: Some(0),
                output: Some("dropped 3 tables".to_owned()),
                ..CommandRunChange::default()
            },
            3_200,
        )
        .unwrap();
    assert_eq!(done.finished_at, Some(3_200));
    assert_eq!(done.output.as_deref(), Some("dropped 3 tables"));

    match db.change_command_run(
        run.id,
        &CommandRunChange {
            state: Some(CommandRunState::Running),
            ..CommandRunChange::default()
        },
        3_300,
    ) {
        Err(FlowStoreError::CommandRun { reason }) => {
            assert_eq!(reason.from, CommandRunState::Succeeded);
        }
        other => panic!("a finished run must stay finished: {other:?}"),
    }
    assert_eq!(db.command_runs(flow, 10).unwrap().len(), 1);
}

#[test]
fn an_environments_inbox_item_knows_its_project_and_worktree() {
    let dir = tempfile::tempdir().unwrap();
    let db = AppDb::open(&dir.path().join("flow.db")).unwrap();
    let flow = environment(&db);
    assert_eq!(db.environment_flow(TREE).unwrap(), Some(flow));

    let item = db
        .file_inbox(
            &NewInboxItem {
                flow,
                kind: InboxKind::Approval,
                target: InboxTarget::Flow,
                title: "Reset the database?".to_owned(),
                body: "It acts on `db`, which every worktree shares.".to_owned(),
            },
            4_000,
        )
        .unwrap()
        .unwrap();
    assert_eq!(item.project, Some(project()));
    assert_eq!(item.worktree.as_deref(), Some(TREE));
    assert_eq!(db.inbox(None, Some(&project())).unwrap().len(), 1);
}

#[test]
fn idle_stop_is_off_until_opted_into_and_off_again_once_cleared() {
    let dir = tempfile::tempdir().unwrap();
    let db = AppDb::open(&dir.path().join("flow.db")).unwrap();
    assert_eq!(db.idle_stop(&project()).unwrap(), None);
    db.set_idle_stop(&project(), Some(30)).unwrap();
    assert_eq!(db.idle_stop(&project()).unwrap(), Some(30));
    db.set_idle_stop(&project(), None).unwrap();
    assert_eq!(db.idle_stop(&project()).unwrap(), None);
}
