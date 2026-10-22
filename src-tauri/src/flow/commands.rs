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
    ApprovalId, FindingId, FindingRecord, FlowId, InboxItem, InboxItemId, Outcome, Question,
    QuestionId, ReviewDraft, RoleProvider, WorkItemId,
};

use super::findings::{Rerun, Staleness};
use super::publish::{PublicationView, PublishPreview};
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

/// The refusal every publishing command gives while the Work view is off.
fn work_view_off() -> ErrorView {
    refused("publishing happens from the Work view, which is turned off".to_owned())
}

/// Exactly what a draft of a review would post on its pull request, markers and all, and the hash
/// an approval of it would pin. Reads the pull request and its diff; posts nothing.
#[tauri::command]
pub async fn preview_review_publish(
    app: AppState<'_>,
    engine: EngineState<'_>,
    flow: i64,
    draft: ReviewDraft,
) -> Reply<PublishPreview> {
    if !app.work_view_enabled() {
        return Err(work_view_off());
    }
    let engine = Arc::clone(&engine);
    blocking(move || engine.preview_publish(FlowId(flow), &draft)).await
}

/// Approve exactly what the preview showed, by its hash, and start posting it.
///
/// There is no agent tool that reaches this, or anything else that posts: only the window calls it,
/// from the outcome sheet's Approve and publish.
#[tauri::command]
pub async fn approve_review_publish(
    app: AppState<'_>,
    engine: EngineState<'_>,
    flow: i64,
    draft: ReviewDraft,
    hash: String,
    against_head: Option<String>,
) -> Reply<PublicationView> {
    if !app.work_view_enabled() {
        return Err(work_view_off());
    }
    let engine = Arc::clone(&engine);
    blocking(move || engine.approve_publish(FlowId(flow), &draft, &hash, against_head.as_deref()))
        .await
}

/// One publish of a review, as it stands.
#[tauri::command]
pub async fn review_publication(engine: EngineState<'_>, approval: i64) -> Reply<PublicationView> {
    let engine = Arc::clone(&engine);
    blocking(move || engine.publication_view(ApprovalId(approval))).await
}

/// Send what is left of a publish that failed or stopped, after checking GitHub for it.
#[tauri::command]
pub async fn retry_publication(
    app: AppState<'_>,
    engine: EngineState<'_>,
    approval: i64,
) -> Reply<PublicationView> {
    if !app.work_view_enabled() {
        return Err(work_view_off());
    }
    let engine = Arc::clone(&engine);
    blocking(move || engine.retry_publication(ApprovalId(approval))).await
}

/// Put the review's comments on the pending review that was in the way, then submit it.
#[tauri::command]
pub async fn add_to_pending_review(
    app: AppState<'_>,
    engine: EngineState<'_>,
    approval: i64,
) -> Reply<PublicationView> {
    if !app.work_view_enabled() {
        return Err(work_view_off());
    }
    let engine = Arc::clone(&engine);
    blocking(move || engine.add_to_pending_review(ApprovalId(approval))).await
}

/// Stop a publish where it is. What was posted stays posted.
#[tauri::command]
pub async fn cancel_publication(engine: EngineState<'_>, approval: i64) -> Reply<PublicationView> {
    let engine = Arc::clone(&engine);
    blocking(move || engine.cancel_publication(ApprovalId(approval))).await
}

/// The refusal every local-fix command gives while the Work view is off.
fn fixes_off() -> ErrorView {
    refused("local fixes happen from the Work view, which is turned off".to_owned())
}

/// Start a local fix of a finished review's findings, by a writer role, in its review worktree.
/// Takes the writer lease; nothing leaves the Mac.
#[tauri::command]
pub async fn start_local_fix(
    app: AppState<'_>,
    engine: EngineState<'_>,
    flow: i64,
    request: super::fix::StartFix,
) -> Reply<FlowView> {
    if !app.work_view_enabled() {
        return Err(fixes_off());
    }
    let engine = Arc::clone(&engine);
    blocking(move || {
        engine.start_fix(FlowId(flow), &request)?;
        engine.view(FlowId(flow)).map_err(refused)
    })
    .await
}

/// Stop the local fix being written. What it committed stays in the review worktree.
#[tauri::command]
pub async fn cancel_local_fix(engine: EngineState<'_>, flow: i64) -> Reply<FlowView> {
    let engine = Arc::clone(&engine);
    blocking(move || {
        engine.cancel_fix(FlowId(flow))?;
        engine.view(FlowId(flow)).map_err(refused)
    })
    .await
}

/// The current local fix's diff, for reading before choosing where it goes.
#[tauri::command]
pub async fn local_fix_diff(engine: EngineState<'_>, flow: i64) -> Reply<super::fix::FixDiffView> {
    let engine = Arc::clone(&engine);
    blocking(move || engine.fix_diff(FlowId(flow))).await
}

/// Every route for the run's fix, open or not, from a fresh read of the pull request. Sends
/// nothing.
#[tauri::command]
pub async fn fix_routes(
    app: AppState<'_>,
    engine: EngineState<'_>,
    flow: i64,
) -> Reply<super::routes::RouteOptions> {
    if !app.work_view_enabled() {
        return Err(fixes_off());
    }
    let engine = Arc::clone(&engine);
    blocking(move || engine.route_options(FlowId(flow))).await
}

/// Exactly what a route would send, and the hash an approval of it pins. Sends nothing.
#[tauri::command]
pub async fn preview_fix_route(
    app: AppState<'_>,
    engine: EngineState<'_>,
    flow: i64,
    request: super::routes::RouteRequest,
) -> Reply<super::routes::RoutePreview> {
    if !app.work_view_enabled() {
        return Err(fixes_off());
    }
    let engine = Arc::clone(&engine);
    blocking(move || engine.preview_route(FlowId(flow), &request)).await
}

/// Approve exactly what the preview showed, by its hash, and start sending it.
///
/// Like publishing, only the window calls this: no run tool reaches a push or a pull request.
#[tauri::command]
pub async fn approve_fix_route(
    app: AppState<'_>,
    engine: EngineState<'_>,
    flow: i64,
    request: super::routes::RouteRequest,
    hash: String,
) -> Reply<super::routes::ApprovedRoute> {
    if !app.work_view_enabled() {
        return Err(fixes_off());
    }
    let engine = Arc::clone(&engine);
    blocking(move || engine.approve_route(FlowId(flow), &request, &hash)).await
}

/// Send what is left of a route that failed or stopped, after reading the remote for it.
#[tauri::command]
pub async fn retry_fix_route(
    app: AppState<'_>,
    engine: EngineState<'_>,
    approval: i64,
) -> Reply<super::routes::RouteSendView> {
    if !app.work_view_enabled() {
        return Err(fixes_off());
    }
    let engine = Arc::clone(&engine);
    blocking(move || engine.retry_route(ApprovalId(approval))).await
}

/// Stop a route where it is. What was pushed stays pushed.
#[tauri::command]
pub async fn cancel_fix_route(
    engine: EngineState<'_>,
    approval: i64,
) -> Reply<super::routes::RouteSendView> {
    let engine = Arc::clone(&engine);
    blocking(move || engine.cancel_route(ApprovalId(approval))).await
}

// ═══════════════════════════════ environments ═══════════════════════════════

/// A worktree's environment changed: a service started, answered, stopped or exited, or a command
/// ran. Carries the place only; the window asks for the rest.
pub const ENV_CHANGED_EVENT: &str = "env:changed";

pub type SupervisorState<'a> = State<'a, Arc<super::env::Supervisor>>;

/// The window's half of [`super::env::EnvNotice`].
pub struct TauriEnvNotice {
    handle: AppHandle,
}

impl std::fmt::Debug for TauriEnvNotice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TauriEnvNotice").finish_non_exhaustive()
    }
}

impl TauriEnvNotice {
    #[must_use]
    pub fn new(handle: AppHandle) -> Self {
        Self { handle }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct EnvChanged<'a> {
    project: &'a str,
    worktree: &'a str,
}

impl super::env::EnvNotice for TauriEnvNotice {
    fn changed(&self, project: &str, worktree: &str) {
        let _ = self
            .handle
            .emit(ENV_CHANGED_EVENT, EnvChanged { project, worktree });
    }
}

fn environment_off() -> ErrorView {
    refused("the Environment tab is part of the Work view, which is turned off".to_owned())
}

/// A worktree's environment: its services, resources, commands and recent runs.
#[tauri::command]
pub async fn environment(
    app: AppState<'_>,
    supervisor: SupervisorState<'_>,
    project: String,
    worktree: String,
) -> Reply<super::env::EnvironmentView> {
    if !app.work_view_enabled() {
        return Err(environment_off());
    }
    let supervisor = Arc::clone(&supervisor);
    blocking(move || supervisor.view(&project, &worktree).map_err(refused)).await
}

/// Start one service, or every one that is not running. A port in use refuses the start and names
/// its owner; nothing is ended to free it.
#[tauri::command]
pub async fn start_services(
    app: AppState<'_>,
    supervisor: SupervisorState<'_>,
    project: String,
    worktree: String,
    service: Option<String>,
) -> Reply<super::env::EnvironmentView> {
    if !app.work_view_enabled() {
        return Err(environment_off());
    }
    let supervisor = Arc::clone(&supervisor);
    blocking(move || {
        supervisor
            .start(&project, &worktree, service.as_deref())
            .map_err(refused)
    })
    .await
}

/// Stop one service, or every running one.
#[tauri::command]
pub async fn stop_services(
    supervisor: SupervisorState<'_>,
    project: String,
    worktree: String,
    service: Option<String>,
) -> Reply<super::env::EnvironmentView> {
    let supervisor = Arc::clone(&supervisor);
    blocking(move || {
        supervisor
            .stop(&project, &worktree, service.as_deref())
            .map_err(refused)
    })
    .await
}

#[tauri::command]
pub async fn restart_services(
    app: AppState<'_>,
    supervisor: SupervisorState<'_>,
    project: String,
    worktree: String,
    service: Option<String>,
) -> Reply<super::env::EnvironmentView> {
    if !app.work_view_enabled() {
        return Err(environment_off());
    }
    let supervisor = Arc::clone(&supervisor);
    blocking(move || {
        supervisor
            .restart(&project, &worktree, service.as_deref())
            .map_err(refused)
    })
    .await
}

/// Ask a running service once whether it is up.
#[tauri::command]
pub async fn check_service(
    supervisor: SupervisorState<'_>,
    project: String,
    worktree: String,
    service: String,
) -> Reply<super::env::EnvironmentView> {
    let supervisor = Arc::clone(&supervisor);
    blocking(move || {
        supervisor
            .check(&project, &worktree, &service)
            .map_err(refused)
    })
    .await
}

/// A healthy service's preview, fetched just now. The window opens only what this returns.
#[tauri::command]
pub async fn verify_service_preview(
    app: AppState<'_>,
    supervisor: SupervisorState<'_>,
    project: String,
    worktree: String,
    service: String,
) -> Reply<super::env::VerifiedPreview> {
    if !app.work_view_enabled() {
        return Err(environment_off());
    }
    let supervisor = Arc::clone(&supervisor);
    blocking(move || {
        supervisor
            .preview(&project, &worktree, &service)
            .map_err(refused)
    })
    .await
}

/// The end of a service's log.
#[tauri::command]
pub async fn service_log(
    supervisor: SupervisorState<'_>,
    project: String,
    worktree: String,
    service: String,
) -> Reply<super::env::ServiceLog> {
    let supervisor = Arc::clone(&supervisor);
    blocking(move || {
        supervisor
            .log(&project, &worktree, &service)
            .map_err(refused)
    })
    .await
}

/// Run one of a worktree's commands, or ask for approval in the inbox when it is destructive and
/// reaches a shared resource.
#[tauri::command]
pub async fn run_environment_command(
    app: AppState<'_>,
    supervisor: SupervisorState<'_>,
    project: String,
    worktree: String,
    command: String,
) -> Reply<super::env::EnvironmentView> {
    if !app.work_view_enabled() {
        return Err(environment_off());
    }
    let supervisor = Arc::clone(&supervisor);
    blocking(move || {
        supervisor
            .run_command(&project, &worktree, &command)
            .map_err(refused)
    })
    .await
}

/// The command waiting for your approval in an environment's flow, if one is.
#[tauri::command]
pub async fn pending_environment_command(
    supervisor: SupervisorState<'_>,
    flow: i64,
) -> Reply<Option<super::env::PendingCommandView>> {
    let supervisor = Arc::clone(&supervisor);
    blocking(move || supervisor.pending(FlowId(flow)).map_err(refused)).await
}

/// Approve one run of a waiting command. Only the window calls this; no agent tool reaches it.
#[tauri::command]
pub async fn approve_environment_command(
    app: AppState<'_>,
    supervisor: SupervisorState<'_>,
    run: i64,
) -> Reply<super::env::CommandRunView> {
    if !app.work_view_enabled() {
        return Err(environment_off());
    }
    let supervisor = Arc::clone(&supervisor);
    blocking(move || {
        supervisor
            .approve(wtm_core::flow::CommandRunId(run))
            .map_err(refused)
    })
    .await
}

#[tauri::command]
pub async fn decline_environment_command(
    supervisor: SupervisorState<'_>,
    run: i64,
) -> Reply<super::env::CommandRunView> {
    let supervisor = Arc::clone(&supervisor);
    blocking(move || {
        supervisor
            .decline(wtm_core::flow::CommandRunId(run))
            .map_err(refused)
    })
    .await
}

/// Approve starting a review's environment, as the Reviews tab or the inbox showed it, and run what
/// it blocks. `binding` is the pin the shown plan carried; a different one runs nothing. Only the
/// window calls this; no agent tool reaches it.
#[tauri::command]
pub async fn start_review_environment(
    app: AppState<'_>,
    engine: EngineState<'_>,
    flow: i64,
    binding: String,
) -> Reply<FlowView> {
    if !app.work_view_enabled() {
        return Err(environment_off());
    }
    let engine = Arc::clone(&engine);
    blocking(move || {
        engine
            .start_environment(FlowId(flow), &binding)
            .map_err(refused)?;
        engine.view(FlowId(flow)).map_err(refused)
    })
    .await
}

/// Decline starting a review's environment: what it blocks stays blocked.
#[tauri::command]
pub async fn decline_review_environment(engine: EngineState<'_>, flow: i64) -> Reply<FlowView> {
    let engine = Arc::clone(&engine);
    blocking(move || {
        engine.decline_environment(FlowId(flow)).map_err(refused)?;
        engine.view(FlowId(flow)).map_err(refused)
    })
    .await
}

/// A runtime role's screenshot, as base64 PNG: the window cannot read the data directory.
#[tauri::command]
pub async fn review_screenshot(engine: EngineState<'_>, flow: i64, id: i64) -> Reply<String> {
    let engine = Arc::clone(&engine);
    blocking(move || {
        engine
            .screenshot_png(FlowId(flow), wtm_core::flow::ScreenshotId(id))
            .map_err(refused)
    })
    .await
}

/// Stop a project's services after this many minutes with wtm in the background, or never.
#[tauri::command]
pub async fn set_idle_stop(
    supervisor: SupervisorState<'_>,
    project: String,
    minutes: Option<u32>,
) -> Reply<()> {
    let supervisor = Arc::clone(&supervisor);
    blocking(move || supervisor.set_idle_stop(&project, minutes).map_err(refused)).await
}
