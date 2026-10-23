//! Designing over IPC: the issue's Design tab, its revisions and their preview, your comments and
//! decisions, and Start implementing behind its gate.
//!
//! Every action needs the Work view on, as planning does (`plan_commands.rs`).

use std::sync::Arc;

use wtm_core::flow::{DesignComment, DesignDecision, FlowId};
use wtm_core::ports::DesignStore;

use super::Refusal;
use super::commands::EngineState;
use super::designer::DesignView;
use crate::commands::{AppState, Reply, blocking};
use crate::view::{BrowserView, ErrorView};

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
            "designing is in the Work view, which is turned off",
        ))
    }
}

/// An issue's design, for its Design tab. `None` before it has a run.
#[tauri::command]
pub async fn issue_design(
    engine: EngineState<'_>,
    project: String,
    key: String,
) -> Reply<Option<DesignView>> {
    let engine = Arc::clone(&engine);
    blocking(move || engine.issue_design(&project, &key).map_err(refused)).await
}

/// Request a design for a planned issue whose plan you approved.
#[tauri::command]
pub async fn request_design(
    app: AppState<'_>,
    engine: EngineState<'_>,
    flow: i64,
    request: String,
) -> Reply<()> {
    gate(&app)?;
    let engine = Arc::clone(&engine);
    blocking(move || {
        engine
            .request_design(FlowId(flow), &request)
            .map_err(refusal)
    })
    .await
}

/// Run the designer of a run waiting at Designing, or carry it on in the same conversation.
#[tauri::command]
pub async fn start_designer(app: AppState<'_>, engine: EngineState<'_>, flow: i64) -> Reply<()> {
    gate(&app)?;
    let engine = Arc::clone(&engine);
    blocking(move || engine.start_designer(FlowId(flow)).map_err(refusal)).await
}

/// Comment on one revision.
#[tauri::command]
pub async fn comment_on_design(
    app: AppState<'_>,
    engine: EngineState<'_>,
    flow: i64,
    revision: u32,
    text: String,
) -> Reply<DesignComment> {
    gate(&app)?;
    let engine = Arc::clone(&engine);
    blocking(move || {
        engine
            .comment_on_design(FlowId(flow), revision, &text)
            .map_err(refused)
    })
    .await
}

/// Approve the newest revision, as you saw it.
#[tauri::command]
pub async fn approve_design(
    app: AppState<'_>,
    engine: EngineState<'_>,
    flow: i64,
    revision: u32,
    fingerprint: String,
) -> Reply<DesignDecision> {
    gate(&app)?;
    let engine = Arc::clone(&engine);
    blocking(move || {
        engine
            .approve_design(FlowId(flow), revision, &fingerprint)
            .map_err(refused)
    })
    .await
}

/// Ask for changes to the newest revision: your note and its comments go to the designer.
#[tauri::command]
pub async fn request_design_changes(
    app: AppState<'_>,
    engine: EngineState<'_>,
    flow: i64,
    revision: u32,
    note: String,
) -> Reply<DesignDecision> {
    gate(&app)?;
    let engine = Arc::clone(&engine);
    blocking(move || {
        engine
            .request_design_changes(FlowId(flow), revision, &note)
            .map_err(refused)
    })
    .await
}

/// Open one revision in the existing sandboxed review pane, in the issue's worktree.
///
/// Any revision the run published, not only the newest: an earlier one is still something to look
/// at and comment on, it is only no longer something to approve.
#[tauri::command]
pub async fn preview_design_revision(
    handle: tauri::AppHandle,
    app: AppState<'_>,
    flow: i64,
    revision: u32,
) -> Reply<BrowserView> {
    gate(&app)?;
    let app = Arc::clone(&app);
    blocking(move || {
        let db = app.appdb.get()?;
        let flow = FlowId(flow);
        let design = db
            .design(flow)
            .map_err(|e| refused(e.to_string()))?
            .ok_or_else(|| refused("this run has no design"))?;
        let record = db
            .design_revisions(flow)
            .map_err(|e| refused(e.to_string()))?
            .into_iter()
            .find(|r| r.revision == revision)
            .ok_or_else(|| refused(format!("this run has no design revision {revision}")))?;
        let mut prototype = crate::prototype_commands::run_record(
            &app,
            flow.0,
            design.project.as_str(),
            &design.worktree,
            &record.prototype,
        )?;
        crate::prototype_commands::show_revision(
            &handle,
            &app,
            &mut prototype,
            &record.review,
            None,
        )
    })
    .await
}

/// Move an issue's run to Implementing, through the gate that wants the approved plan and design.
#[tauri::command]
pub async fn start_implementing(
    app: AppState<'_>,
    engine: EngineState<'_>,
    flow: i64,
) -> Reply<()> {
    gate(&app)?;
    let engine = Arc::clone(&engine);
    blocking(move || engine.start_implementing(FlowId(flow)).map_err(refusal)).await
}
