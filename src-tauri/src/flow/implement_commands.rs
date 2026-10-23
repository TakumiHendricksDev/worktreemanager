//! Implementing over IPC: the issue's Implement tab, Start implementing behind the gate, Resume,
//! the hand-over and Revise.
//!
//! Every action needs the Work view on, as planning and designing do.

use std::sync::Arc;

use wtm_core::flow::FlowId;

use super::Refusal;
use super::commands::EngineState;
use super::handoff::HandoffView;
use super::implement::ImplementView;
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
            "implementing is in the Work view, which is turned off",
        ))
    }
}

/// An issue's implementation, for its Implement tab. `None` before it has a run.
#[tauri::command]
pub async fn issue_implementation(
    engine: EngineState<'_>,
    project: String,
    key: String,
) -> Reply<Option<ImplementView>> {
    let engine = Arc::clone(&engine);
    blocking(move || engine.issue_implementation(&project, &key).map_err(refused)).await
}

/// Carry an implementation on after a quit, a crash, a limit or a failure.
#[tauri::command]
pub async fn resume_implementing(
    app: AppState<'_>,
    engine: EngineState<'_>,
    flow: i64,
) -> Reply<()> {
    gate(&app)?;
    let engine = Arc::clone(&engine);
    blocking(move || engine.resume_implementation(FlowId(flow)).map_err(refusal)).await
}

/// What an implementation handed over, for the hand-over panel.
#[tauri::command]
pub async fn implementation_handoff(engine: EngineState<'_>, flow: i64) -> Reply<HandoffView> {
    let engine = Arc::clone(&engine);
    blocking(move || engine.handoff_view(FlowId(flow)).map_err(refused)).await
}

/// Send the implementation back to its writer with what testing it showed.
#[tauri::command]
pub async fn revise_implementation(
    app: AppState<'_>,
    engine: EngineState<'_>,
    flow: i64,
    note: String,
) -> Reply<()> {
    gate(&app)?;
    let engine = Arc::clone(&engine);
    blocking(move || engine.revise(FlowId(flow), &note).map_err(refusal)).await
}
