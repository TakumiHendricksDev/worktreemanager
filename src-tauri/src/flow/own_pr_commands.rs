//! The issue's own pull request over IPC: its view, Retry, the Ready + Needs Review sheet, following
//! it, a revision from its feedback, and the Merge sheet.
//!
//! Every action needs the Work view on. Approving a merge is a command of its own, sent only by the
//! Merge sheet's button; nothing else here, and no agent tool anywhere, can merge.

use std::sync::Arc;

use wtm_core::flow::{ApprovalId, FlowId, MergeMethod};

use super::Refusal;
use super::commands::EngineState;
use super::own_pr::{MergeSheet, OwnPrView, ReadySheet};
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
            "publishing is in the Work view, which is turned off",
        ))
    }
}

/// The issue's own pull request, from the record. Reads nothing remote.
#[tauri::command]
pub async fn issue_pull_request(engine: EngineState<'_>, flow: i64) -> Reply<OwnPrView> {
    let engine = Arc::clone(&engine);
    blocking(move || engine.own_pr_view(FlowId(flow)).map_err(refused)).await
}

/// Read the pull request again: on demand, or because the window came back into focus.
#[tauri::command]
pub async fn sync_pull_request(
    app: AppState<'_>,
    engine: EngineState<'_>,
    flow: i64,
) -> Reply<OwnPrView> {
    gate(&app)?;
    let engine = Arc::clone(&engine);
    blocking(move || engine.sync_pull_request(FlowId(flow)).map_err(refused)).await
}

/// Send a draft, a publish or a merge again, reading first.
#[tauri::command]
pub async fn retry_pull_request_write(
    app: AppState<'_>,
    engine: EngineState<'_>,
    approval: i64,
) -> Reply<()> {
    gate(&app)?;
    let engine = Arc::clone(&engine);
    blocking(move || engine.retry_own(ApprovalId(approval)).map_err(refused)).await
}

/// What Mark ready + Needs Review would do, read fresh.
#[tauri::command]
pub async fn ready_sheet(engine: EngineState<'_>, flow: i64) -> Reply<ReadySheet> {
    let engine = Arc::clone(&engine);
    blocking(move || engine.ready_sheet(FlowId(flow)).map_err(refused)).await
}

/// Approve both writes of Mark ready + Needs Review, at the head the sheet showed.
#[tauri::command]
pub async fn approve_ready(
    app: AppState<'_>,
    engine: EngineState<'_>,
    flow: i64,
    head: String,
) -> Reply<()> {
    gate(&app)?;
    let engine = Arc::clone(&engine);
    blocking(move || engine.approve_ready(FlowId(flow), &head).map_err(refusal)).await
}

/// Start a revision from the pull request's feedback.
#[tauri::command]
pub async fn revise_from_feedback(
    app: AppState<'_>,
    engine: EngineState<'_>,
    flow: i64,
) -> Reply<()> {
    gate(&app)?;
    let engine = Arc::clone(&engine);
    blocking(move || engine.revise_from_feedback(FlowId(flow)).map_err(refusal)).await
}

/// What the Merge sheet shows, read fresh.
#[tauri::command]
pub async fn merge_sheet(engine: EngineState<'_>, flow: i64) -> Reply<MergeSheet> {
    let engine = Arc::clone(&engine);
    blocking(move || engine.merge_sheet(FlowId(flow)).map_err(refused)).await
}

/// Merge at the head the sheet showed, by the method picked there.
#[tauri::command]
pub async fn approve_merge(
    app: AppState<'_>,
    engine: EngineState<'_>,
    flow: i64,
    head: String,
    method: MergeMethod,
) -> Reply<()> {
    gate(&app)?;
    let engine = Arc::clone(&engine);
    blocking(move || {
        engine
            .approve_merge(FlowId(flow), &head, method)
            .map_err(refusal)
    })
    .await
}
