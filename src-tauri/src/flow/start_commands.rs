//! Start Work over IPC: the Start button, ⌘↵ and the board's drops, the issue workspace's reads,
//! and what the inbox offers about an issue's run.
//!
//! Every action needs the Work view on, as starting a review does (`commands.rs`); every read is
//! gated by the tracker's two gates inside the engine (`tracker.rs`).

use std::sync::Arc;

use wtm_core::flow::{ApprovalId, FlowId};

use super::Refusal;
use super::commands::EngineState;
use super::moves::MoveAction;
use super::start::{DropOutcome, IssueStartView, StartOutcome, StartPreview, StartWork};
use crate::commands::{AppState, Reply, blocking};
use crate::view::ErrorView;

fn refused(message: impl Into<String>) -> ErrorView {
    ErrorView::new("flow", message)
}

fn refusal(refusal: Refusal) -> ErrorView {
    refused(refusal.message)
}

fn gate(app: &AppState<'_>) -> Result<(), ErrorView> {
    if app.work_view_enabled() {
        Ok(())
    } else {
        Err(refused(
            "work starts from the Work view, which is turned off",
        ))
    }
}

/// Where an issue's work would happen, and what starting it would do. Changes nothing.
#[tauri::command]
pub async fn start_work_preview(
    engine: EngineState<'_>,
    project: String,
    key: String,
    by_drop: bool,
) -> Reply<StartPreview> {
    let engine = Arc::clone(&engine);
    blocking(move || {
        engine
            .preview_start(&project, &key, by_drop)
            .map_err(refused)
    })
    .await
}

/// Start work on an issue, or focus the run it has.
#[tauri::command]
pub async fn start_work(
    app: AppState<'_>,
    engine: EngineState<'_>,
    request: StartWork,
) -> Reply<StartOutcome> {
    gate(&app)?;
    let engine = Arc::clone(&engine);
    blocking(move || engine.start_work(&request).map_err(refusal)).await
}

/// An issue's start, run, moves and inbox, for its workspace.
#[tauri::command]
pub async fn issue_start(
    engine: EngineState<'_>,
    project: String,
    key: String,
) -> Reply<IssueStartView> {
    let engine = Arc::clone(&engine);
    blocking(move || engine.issue_start(&project, &key).map_err(refused)).await
}

/// Carry on with a start a crash or a failed setup stopped: Retry setup, or use the worktree as
/// it is.
#[tauri::command]
pub async fn resume_start(
    app: AppState<'_>,
    engine: EngineState<'_>,
    flow: i64,
    as_is: bool,
) -> Reply<()> {
    gate(&app)?;
    let engine = Arc::clone(&engine);
    blocking(move || engine.resume_start(FlowId(flow), as_is).map_err(refusal)).await
}

/// Stop an issue's run. Its worktree and branch stay.
#[tauri::command]
pub async fn cancel_issue_run(app: AppState<'_>, engine: EngineState<'_>, flow: i64) -> Reply<()> {
    gate(&app)?;
    let engine = Arc::clone(&engine);
    blocking(move || engine.cancel_start(FlowId(flow)).map_err(refused)).await
}

/// A card dropped on a column other than the start column: a team status change, and only that.
#[tauri::command]
pub async fn move_issue(
    app: AppState<'_>,
    engine: EngineState<'_>,
    project: String,
    key: String,
    to: String,
    confirmed: bool,
) -> Reply<DropOutcome> {
    gate(&app)?;
    let engine = Arc::clone(&engine);
    blocking(move || {
        engine
            .move_issue(&project, &key, &to, confirmed)
            .map_err(refusal)
    })
    .await
}

/// Retry, force or leave a move that stopped or failed.
#[tauri::command]
pub async fn settle_move(
    app: AppState<'_>,
    engine: EngineState<'_>,
    approval: i64,
    action: MoveAction,
) -> Reply<()> {
    gate(&app)?;
    let engine = Arc::clone(&engine);
    blocking(move || {
        engine
            .settle_move(ApprovalId(approval), action)
            .map_err(refused)
    })
    .await
}

/// Hold an issue's run `Paused`.
#[tauri::command]
pub async fn pause_run(app: AppState<'_>, engine: EngineState<'_>, flow: i64) -> Reply<()> {
    gate(&app)?;
    let engine = Arc::clone(&engine);
    blocking(move || engine.pause_run(FlowId(flow)).map_err(refused)).await
}

/// Release a run you paused.
#[tauri::command]
pub async fn unpause_run(app: AppState<'_>, engine: EngineState<'_>, flow: i64) -> Reply<()> {
    gate(&app)?;
    let engine = Arc::clone(&engine);
    blocking(move || engine.unpause_run(FlowId(flow)).map_err(refused)).await
}
