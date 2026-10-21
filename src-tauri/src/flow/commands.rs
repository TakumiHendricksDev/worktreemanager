//! The review engine over IPC, and its notices to the window.
//!
//! Starting a review is the Work view's action, so it needs the view on, the same gate R1 put on
//! everything Work does. Reading needs nothing: with no database yet, every read answers "nothing
//! here" without creating one.
//!
//! # Events, never polling
//!
//! The Reviews tab learns that a run moved from `flow:changed`, which carries only the flow's id,
//! and asks for the run again. ARCHITECTURE §8 bans polling, and a run is exactly the kind of slow,
//! irregular thing a poll gets wrong in both directions. A newly filed inbox item is its own event,
//! `flow:inbox`, because it is the one thing that may notify.

use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};
use wtm_core::flow::{FlowId, InboxItem, InboxItemId, RoleProvider};

use super::{Engine, FlowView, Notice, StartReview};
use crate::commands::{AppState, Reply, blocking};
use crate::view::ErrorView;

/// A run's state changed. Carries the flow only; the window asks for the rest.
pub const FLOW_CHANGED_EVENT: &str = "flow:changed";

/// An inbox item was filed for the first time.
pub const FLOW_INBOX_EVENT: &str = "flow:inbox";

pub type EngineState<'a> = State<'a, Arc<Engine>>;

/// The window's half of [`Notice`].
pub struct TauriNotice {
    handle: AppHandle,
}

impl std::fmt::Debug for TauriNotice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TauriNotice").finish_non_exhaustive()
    }
}

impl TauriNotice {
    #[must_use]
    pub fn new(handle: AppHandle) -> Self {
        Self { handle }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Changed {
    flow: FlowId,
}

impl Notice for TauriNotice {
    fn changed(&self, flow: FlowId) {
        // A failed emit means the window is gone; the record is already written.
        let _ = self.handle.emit(FLOW_CHANGED_EVENT, Changed { flow });
    }

    fn filed(&self, item: &InboxItem) {
        let _ = self.handle.emit(FLOW_INBOX_EVENT, item);
    }
}

fn refused(message: String) -> ErrorView {
    ErrorView::new("flow", message)
}

/// A recipe the Reviews tab can offer, with who would run it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecipeChoice {
    pub id: String,
    pub label: String,
    pub roles: Vec<RoleChoice>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoleChoice {
    pub id: String,
    pub provider: RoleProvider,
    pub model: Option<String>,
}

/// The recipes a project's workflow config declares, in the order the config lists them.
#[tauri::command]
pub async fn review_recipes(app: AppState<'_>, project: String) -> Reply<Vec<RecipeChoice>> {
    let app = Arc::clone(&app);
    blocking(move || {
        let project = app.project(&project)?;
        let Some(workflow) = project.workflow else {
            return Ok(Vec::new());
        };
        Ok(workflow
            .recipe
            .iter()
            .map(|(id, recipe)| RecipeChoice {
                id: id.clone(),
                label: recipe.label.clone().unwrap_or_else(|| id.clone()),
                roles: recipe
                    .stages
                    .iter()
                    .flat_map(|stage| &stage.roles)
                    .filter_map(|role| {
                        let spec = workflow.role.get(role)?;
                        Some(RoleChoice {
                            id: role.clone(),
                            provider: spec.provider,
                            model: spec.model.clone(),
                        })
                    })
                    .collect(),
            })
            .collect())
    })
    .await
}

/// Start a review of a worktree, or focus the one it already has.
#[tauri::command]
pub async fn start_review(
    app: AppState<'_>,
    engine: EngineState<'_>,
    request: StartReview,
) -> Reply<FlowView> {
    if !app.work_view_enabled() {
        return Err(refused(
            "reviews run from the Work view, which is turned off".to_owned(),
        ));
    }
    let engine = Arc::clone(&engine);
    blocking(move || {
        let flow = engine.start(&request).map_err(refused)?;
        engine.view(flow).map_err(refused)
    })
    .await
}

/// One review run, whole.
#[tauri::command]
pub async fn review_run(engine: EngineState<'_>, flow: i64) -> Reply<FlowView> {
    let engine = Arc::clone(&engine);
    blocking(move || engine.view(FlowId(flow)).map_err(refused)).await
}

/// A worktree's newest review run, if it has one.
#[tauri::command]
pub async fn latest_review(
    engine: EngineState<'_>,
    project: String,
    worktree: String,
) -> Reply<Option<FlowView>> {
    let engine = Arc::clone(&engine);
    blocking(move || engine.latest(&project, &worktree).map_err(refused)).await
}

/// Run what is left of a review: interrupted, waiting for capacity, or with a failed role.
#[tauri::command]
pub async fn resume_review(engine: EngineState<'_>, flow: i64) -> Reply<FlowView> {
    let engine = Arc::clone(&engine);
    blocking(move || {
        engine.resume(FlowId(flow)).map_err(refused)?;
        engine.view(FlowId(flow)).map_err(refused)
    })
    .await
}

/// Stop a review wherever it is.
#[tauri::command]
pub async fn cancel_review(engine: EngineState<'_>, flow: i64) -> Reply<FlowView> {
    let engine = Arc::clone(&engine);
    blocking(move || {
        engine.cancel(FlowId(flow)).map_err(refused)?;
        engine.view(FlowId(flow)).map_err(refused)
    })
    .await
}

/// Open inbox items, newest first: one project's, or every project's.
#[tauri::command]
pub async fn work_inbox(engine: EngineState<'_>, project: Option<String>) -> Reply<Vec<InboxItem>> {
    let engine = Arc::clone(&engine);
    blocking(move || engine.inbox(project.as_deref()).map_err(refused)).await
}

/// Resolve one inbox item, as a person does.
#[tauri::command]
pub async fn resolve_inbox_item(engine: EngineState<'_>, item: i64) -> Reply<bool> {
    let engine = Arc::clone(&engine);
    blocking(move || engine.resolve(InboxItemId(item)).map_err(refused)).await
}
