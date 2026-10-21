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
//! `flow:inbox`, because it is the one thing that may notify. An item closing — answered, resolved,
//! dismissed, or settled by a resume — is a `flow:changed` like any other change to its run.

use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};
use wtm_core::flow::{
    FindingId, FindingRecord, FlowId, InboxItem, InboxItemId, Outcome, Question, QuestionId,
    RoleProvider, WorkItemId,
};

use super::findings::{Rerun, Staleness};
use super::pull_request;
use super::{Engine, FlowView, Notice, Refusal, StartReview};
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

/// A start, rerun or resume refused. A refusal over billing has its own kind and names the
/// providers, because the Reviews tab draws it as its own state rather than as a line of red text.
fn refused_start(refusal: Refusal) -> ErrorView {
    if refusal.billing.is_empty() {
        ErrorView::new("flow", refusal.message)
    } else {
        ErrorView::new("billing", refusal.message)
            .with_detail(serde_json::json!({ "providers": refusal.billing }))
    }
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
    /// Skills it names, without the leading `/`.
    pub skills: Vec<String>,
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
                            skills: spec.skill_names().map(str::to_owned).collect(),
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
        let flow = engine.start(&request).map_err(refused_start)?;
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
        engine.resume(FlowId(flow)).map_err(refused_start)?;
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

/// Open inbox items, newest first: one project's, or every project's. Each names the project and
/// worktree its run is of, so Home, the dock badge and a notification click need nothing else.
#[tauri::command]
pub async fn work_inbox(engine: EngineState<'_>, project: Option<String>) -> Reply<Vec<InboxItem>> {
    let engine = Arc::clone(&engine);
    blocking(move || engine.inbox(project.as_deref()).map_err(refused)).await
}

/// Resolve one inbox item: what it was about is settled. `null` when it was not open.
#[tauri::command]
pub async fn resolve_inbox_item(engine: EngineState<'_>, item: i64) -> Reply<Option<InboxItem>> {
    let engine = Arc::clone(&engine);
    blocking(move || engine.resolve(InboxItemId(item)).map_err(refused)).await
}

/// Dismiss one inbox item, as a person does. `null` when it was not open.
#[tauri::command]
pub async fn dismiss_inbox_item(engine: EngineState<'_>, item: i64) -> Reply<Option<InboxItem>> {
    let engine = Arc::clone(&engine);
    blocking(move || engine.dismiss_item(InboxItemId(item)).map_err(refused)).await
}

/// Answer a question a run asked: an option by its index, words, or both.
#[tauri::command]
pub async fn answer_question(
    engine: EngineState<'_>,
    question: i64,
    choice: Option<usize>,
    text: String,
) -> Reply<Question> {
    let engine = Arc::clone(&engine);
    blocking(move || {
        engine
            .answer(QuestionId(question), choice, &text)
            .map_err(refused)
    })
    .await
}

/// Whether a review's results still describe its worktree: what changed since, which findings and
/// checks that makes stale, and which roles a rerun would run. Asks git, so the window calls it when
/// the worktree's listing changes and when a run settles, never on a timer.
#[tauri::command]
pub async fn review_staleness(engine: EngineState<'_>, flow: i64) -> Reply<Staleness> {
    let engine = Arc::clone(&engine);
    blocking(move || engine.staleness(FlowId(flow)).map_err(refused)).await
}

/// Run a finished review's affected roles, or all of them, against a new snapshot.
#[tauri::command]
pub async fn rerun_review(
    app: AppState<'_>,
    engine: EngineState<'_>,
    flow: i64,
    scope: Rerun,
) -> Reply<FlowView> {
    if !app.work_view_enabled() {
        return Err(refused(
            "reviews run from the Work view, which is turned off".to_owned(),
        ));
    }
    let engine = Arc::clone(&engine);
    blocking(move || {
        engine.rerun(FlowId(flow), scope).map_err(refused_start)?;
        engine.view(FlowId(flow)).map_err(refused)
    })
    .await
}

/// Dismiss a finding with a reason, or take the dismissal back when `reason` is absent.
#[tauri::command]
pub async fn dismiss_finding(
    engine: EngineState<'_>,
    finding: i64,
    reason: Option<String>,
) -> Reply<FindingRecord> {
    let engine = Arc::clone(&engine);
    blocking(move || {
        engine
            .dismiss(FindingId(finding), reason.as_deref())
            .map_err(refused)
    })
    .await
}

/// The base a review of a worktree would be measured from, before it starts.
#[tauri::command]
pub async fn review_base(
    engine: EngineState<'_>,
    project: String,
    worktree: String,
) -> Reply<String> {
    let engine = Arc::clone(&engine);
    blocking(move || engine.default_base(&project, &worktree).map_err(refused)).await
}

/// Run Review on a queued pull request's review worktree, against the pull request's base.
///
/// Reads the pull request again first and refuses a head other than the one shown, then brings the
/// review worktree to the head if a review put it somewhere behind it (`pull_request.rs`).
#[tauri::command]
pub async fn start_pull_request_review(
    app: AppState<'_>,
    engine: EngineState<'_>,
    project: String,
    item: WorkItemId,
    recipe: String,
    expected_head: Option<String>,
) -> Reply<FlowView> {
    let app = Arc::clone(&app);
    let engine = Arc::clone(&engine);
    blocking(move || {
        let flow = pull_request::start(
            &app,
            &engine,
            &project,
            item,
            &recipe,
            expected_head.as_deref(),
        )?;
        engine.view(flow).map_err(refused)
    })
    .await
}

/// Re-review changes: move a pull request's review to its new head and rerun the roles it affects.
#[tauri::command]
pub async fn rereview_pull_request(
    app: AppState<'_>,
    engine: EngineState<'_>,
    project: String,
    flow: i64,
    expected_head: Option<String>,
) -> Reply<FlowView> {
    let app = Arc::clone(&app);
    let engine = Arc::clone(&engine);
    blocking(move || {
        pull_request::rereview(
            &app,
            &engine,
            &project,
            FlowId(flow),
            expected_head.as_deref(),
        )?;
        engine.view(FlowId(flow)).map_err(refused)
    })
    .await
}

/// Keep a review's result private: notes kept in wtm beside its findings. Writes nowhere else.
#[tauri::command]
pub async fn keep_review_private(
    engine: EngineState<'_>,
    flow: i64,
    notes: String,
) -> Reply<Outcome> {
    let engine = Arc::clone(&engine);
    blocking(move || engine.keep_private(FlowId(flow), &notes).map_err(refused)).await
}
