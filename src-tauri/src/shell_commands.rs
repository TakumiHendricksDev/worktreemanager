//! Human IPC and visible-pane admission for reviewed shell runs.
//!
//! New shells require the main window to place a placeholder first. A missing
//! window or a full layout leaves an approved request waiting, never a hidden job.

use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tauri::Emitter;
use wtm_core::model::SessionId;

use crate::app::App;
use crate::commands::{AppState, Reply, blocking};
use crate::shell_admission::Availability;
use crate::shell_control;
use crate::shell_runs::{Phase, Prepared, Request, Run, Source, Target, View};
use crate::view::ErrorView;

fn error(message: impl Into<String>) -> ErrorView {
    ErrorView::new("shellRun", message)
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellView {
    pub session: String,
    pub project: String,
    pub worktree: String,
    pub availability: Availability,
    pub opened_by: Option<String>,
}

pub fn list_shells(app: &App, project: &str, worktree: &str) -> Vec<ShellView> {
    app.live_shells()
        .into_iter()
        .filter(|shell| shell.project == project && shell.worktree == worktree)
        .map(|shell| {
            let integration = app.shell_admission.get(&shell.session);
            let foreground = app
                .pty
                .shell_owns_foreground(&SessionId::new(&shell.session));
            ShellView {
                session: shell.session,
                project: shell.project,
                worktree: shell.worktree,
                availability: integration.as_ref().map_or(Availability::Unknown, |shell| {
                    shell.availability(foreground)
                }),
                opened_by: integration.and_then(|shell| shell.owner.clone()),
            }
        })
        .collect()
}

#[tauri::command]
pub async fn list_run_shells(
    app: AppState<'_>,
    project_id: String,
    worktree_id: String,
) -> Reply<Vec<ShellView>> {
    let app = Arc::clone(&app);
    blocking(move || {
        Target::resolve(&app, &project_id, &worktree_id).map_err(error)?;
        Ok(list_shells(&app, &project_id, &worktree_id))
    })
    .await
}

#[tauri::command]
pub async fn prepare_shell_run(app: AppState<'_>, request: Request) -> Reply<View> {
    let app = Arc::clone(&app);
    blocking(move || {
        let prepared = Prepared::create(&app, request, Source::Human).map_err(error)?;
        Ok(app.shell_runs.prepare(prepared).map_err(error)?.view())
    })
    .await
}

#[tauri::command]
pub async fn list_shell_runs(app: AppState<'_>) -> Reply<Vec<View>> {
    let app = Arc::clone(&app);
    blocking(move || Ok(app.shell_runs.all().iter().map(|run| run.view()).collect())).await
}

#[tauri::command]
pub async fn approve_shell_run(
    handle: tauri::AppHandle,
    app: AppState<'_>,
    run_id: String,
) -> Reply<View> {
    let app = Arc::clone(&app);
    blocking(move || {
        let run = app.shell_runs.get(&run_id).map_err(error)?;
        start(&handle, &app, &run, true).map_err(error)?;
        Ok(run.view())
    })
    .await
}

pub fn start(
    handle: &tauri::AppHandle,
    app: &Arc<App>,
    run: &Arc<Run>,
    human: bool,
) -> Result<(), String> {
    let result = start_inner(handle, app, run, human);
    if let Err(reason) = &result {
        shell_control::cancel(handle, app, run, reason);
    }
    result
}

fn start_inner(
    handle: &tauri::AppHandle,
    app: &Arc<App>,
    run: &Arc<Run>,
    human: bool,
) -> Result<(), String> {
    shell_control::validate(app, run)?;
    if !human && !run.state.lock().grant {
        return Err("This command is waiting for the user's approval.".into());
    }
    if !run.approve()? {
        return Ok(());
    }
    let choice = &run.prepared.request.shell;
    if choice != "new" {
        let candidates = if choice == "auto" {
            app.shell_admission.candidates(
                &run.prepared.target.project,
                &run.prepared.target.worktree,
                run.prepared.source.home(),
            )
        } else {
            let shell = app
                .shell_admission
                .get(choice)
                .ok_or("This shell has no verified integration. Choose a new shell.")?;
            if shell.project != run.prepared.target.project
                || shell.worktree != run.prepared.target.worktree
            {
                return fail(handle, app, run, "This shell belongs to another worktree.");
            }
            if !human && run.prepared.source.home() != shell.owner.as_deref() {
                return fail(
                    handle,
                    app,
                    run,
                    "Home needs per-run approval to use a user-owned shell.",
                );
            }
            vec![(choice.clone(), shell)]
        };
        for (session, shell) in candidates {
            let foreground = app.pty.shell_owns_foreground(&SessionId::new(&session));
            if shell.availability(foreground) == Availability::Ready {
                return dispatch(handle, app, run, &session);
            }
        }
        if choice != "auto" {
            return fail(handle, app, run, "The selected shell is busy. Nothing ran.");
        }
    }
    shell_control::announce(handle, run);
    handle
        .emit_to(
            crate::pane_windows::MAIN_WINDOW,
            shell_control::PANE_EVENT,
            run.view(),
        )
        .map_err(|e| e.to_string())
}

fn fail(handle: &tauri::AppHandle, app: &App, run: &Run, reason: &str) -> Result<(), String> {
    shell_control::cancel(handle, app, run, reason);
    Err(reason.into())
}

fn dispatch(
    handle: &tauri::AppHandle,
    app: &Arc<App>,
    run: &Arc<Run>,
    session: &str,
) -> Result<(), String> {
    shell_control::validate(app, run)?;
    let shell = app
        .shell_admission
        .get(session)
        .ok_or("The shell closed before Run.")?;
    let capability = app.shell_runs.reserve(run, session)?;
    if let Err(reason) = shell.reserve(
        &capability,
        app.pty.shell_owns_foreground(&SessionId::new(session)),
    ) {
        return fail(handle, app, run, &reason);
    }
    shell_control::announce(handle, run);
    let app = Arc::clone(app);
    let handle = handle.clone();
    let run = Arc::clone(run);
    std::thread::Builder::new()
        .name("wtm-shell-admission".into())
        .spawn(move || {
            let deadline = app.clock.monotonic_ms() + 5_000;
            let mut state = run.state.lock();
            while state.phase == Phase::Reserved && app.clock.monotonic_ms() < deadline {
                run.changed.wait_for(&mut state, Duration::from_millis(50));
            }
            let expired = state.phase == Phase::Reserved;
            drop(state);
            if expired {
                shell_control::cancel(
                    &handle,
                    &app,
                    &run,
                    "The shell did not accept Run in time. Nothing ran.",
                );
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn capacity(app: &App, project: &str, worktree: &str) -> Result<(), String> {
    let shells = app.live_shells();
    let agents: Vec<_> = app
        .live_agents()
        .into_iter()
        .filter(|agent| !agent.ephemeral && app.handoff.parent_of(&agent.session).is_none())
        .collect();
    let browsers = app.browsers.list(None);
    let total = shells.len() + agents.len() + browsers.len();
    let here = shells
        .iter()
        .filter(|s| s.project == project && s.worktree == worktree)
        .count()
        + agents
            .iter()
            .filter(|a| a.scope.place() == Some((project, worktree)))
            .count()
        + browsers
            .iter()
            .filter(|b| b.project == project && b.worktree == worktree)
            .count();
    if total >= 40 || here >= 20 {
        return Err("There is no room for another shell (20 panes per worktree, 40 total). Close a pane first.".into());
    }
    Ok(())
}

#[tauri::command]
pub async fn admit_shell_run(
    window: tauri::WebviewWindow,
    handle: tauri::AppHandle,
    app: AppState<'_>,
    run_id: String,
    rows: u16,
    cols: u16,
) -> Reply<View> {
    if window.label() != crate::pane_windows::MAIN_WINDOW {
        return Err(error("The main window must place this shell first."));
    }
    let app = Arc::clone(&app);
    blocking(move || {
        let run = app.shell_runs.get(&run_id).map_err(error)?;
        let result = open_admitted(&handle, &app, &run, rows, cols);
        if let Err(reason) = result {
            shell_control::cancel(&handle, &app, &run, &reason);
            return Err(error(reason));
        }
        Ok(run.view())
    })
    .await
}

fn open_admitted(
    handle: &tauri::AppHandle,
    app: &Arc<App>,
    run: &Arc<Run>,
    rows: u16,
    cols: u16,
) -> Result<(), String> {
    {
        let mut state = run.state.lock();
        if state.phase != Phase::AwaitingPane {
            return Ok(());
        }
        state.phase = Phase::OpeningShell;
    }
    shell_control::validate(app, run)?;
    let target = &run.prepared.target;
    let session = {
        // Counts and spawns of managed shells are serialized. The main window
        // has already reserved a real layout leaf, including detached panes.
        let _opening = app.shell_creation.lock();
        capacity(app, &target.project, &target.worktree)?;
        let project = app.project(&target.project).map_err(|e| e.to_string())?;
        let worktree = app
            .worktree(&project, &target.worktree)
            .map_err(|e| e.to_string())?;
        let sink = crate::pty_bridge::EventSink::recording(handle.clone(), Arc::clone(app));
        app.open_run_shell(
            &worktree,
            &target.project,
            run.prepared.source.home().map(str::to_owned),
            rows.max(1),
            cols.max(1),
            sink,
        )
        .map_err(|e| e.to_string())?
    };
    let deadline = app.clock.monotonic_ms() + 5_000;
    loop {
        if run.state.lock().phase != Phase::OpeningShell {
            app.close_shell(session.as_str());
            return Err("The request was cancelled while its shell was opening.".into());
        }
        if app
            .shell_admission
            .get(session.as_str())
            .is_some_and(|shell| {
                shell.availability(app.pty.shell_owns_foreground(&session)) == Availability::Ready
            })
        {
            break;
        }
        if app.clock.monotonic_ms() >= deadline {
            app.close_shell(session.as_str());
            return Err("The new shell could not establish a verified prompt. Nothing ran.".into());
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    {
        let mut state = run.state.lock();
        if state.phase != Phase::OpeningShell {
            drop(state);
            app.close_shell(session.as_str());
            return Err("The request was cancelled while its shell was opening.".into());
        }
        state.phase = Phase::AwaitingPane;
    }
    if let Err(error) = dispatch(handle, app, run, session.as_str()) {
        app.close_shell(session.as_str());
        return Err(error);
    }
    Ok(())
}

#[tauri::command]
pub async fn deny_shell_run(
    handle: tauri::AppHandle,
    app: AppState<'_>,
    run_id: String,
) -> Reply<()> {
    let app = Arc::clone(&app);
    blocking(move || {
        let run = app.shell_runs.get(&run_id).map_err(error)?;
        run.deny().map_err(error)?;
        shell_control::announce(&handle, &run);
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn cancel_shell_run(
    handle: tauri::AppHandle,
    app: AppState<'_>,
    run_id: String,
) -> Reply<()> {
    let app = Arc::clone(&app);
    blocking(move || {
        let run = app.shell_runs.get(&run_id).map_err(error)?;
        shell_control::cancel(&handle, &app, &run, "The user cancelled this run.");
        {
            let mut state = run.state.lock();
            if state.phase == Phase::Interrupted {
                state.phase = Phase::Cancelled;
            }
        }
        shell_control::announce(&handle, &run);
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn set_home_shell_grant(
    app: AppState<'_>,
    home: String,
    project_id: String,
    worktree_id: String,
    allow: bool,
) -> Reply<()> {
    let app = Arc::clone(&app);
    blocking(move || {
        if !app
            .overview_of(&home)
            .is_some_and(|home| home.scope.is_home())
        {
            return Err(error("That Home session is no longer running."));
        }
        let target = Target::resolve(&app, &project_id, &worktree_id).map_err(error)?;
        app.shell_runs.set_grant(&home, target, allow);
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn focus_run_shell(app: AppState<'_>, session: String) -> Reply<()> {
    let app = Arc::clone(&app);
    blocking(move || {
        if let Some(shell) = app.shell_admission.get(&session) {
            shell.focus(app.clock.monotonic_ms());
        }
        Ok(())
    })
    .await
}
