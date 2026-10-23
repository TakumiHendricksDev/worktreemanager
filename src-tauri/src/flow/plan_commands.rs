//! Planning over IPC: the issue's Plan tab, a question's thread, Discuss, the plan decisions, and
//! Send to Jira's two clicks.
//!
//! Every action needs the Work view on, as starting work does (`start_commands.rs`). Answering is
//! `answer_question` in `commands.rs`, the inbox's own, because the same question is answered from
//! either place.

use std::sync::Arc;

use wtm_core::flow::{ApprovalId, FlowId, IssueText, PlanDecision, QuestionId};

use super::Refusal;
use super::commands::EngineState;
use super::comments::{CommentDraft, CommentView};
use super::planner::{PlanView, QuestionThread};
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
        Err(refused("planning is in the Work view, which is turned off"))
    }
}

/// An issue's planning, for its Plan tab. `None` before it has a run.
#[tauri::command]
pub async fn issue_plan(
    engine: EngineState<'_>,
    project: String,
    key: String,
) -> Reply<Option<PlanView>> {
    let engine = Arc::clone(&engine);
    blocking(move || engine.issue_plan(&project, &key).map_err(refused)).await
}

/// Plan an issue's run that is ready to plan.
#[tauri::command]
pub async fn start_planning(app: AppState<'_>, engine: EngineState<'_>, flow: i64) -> Reply<()> {
    gate(&app)?;
    let engine = Arc::clone(&engine);
    blocking(move || engine.start_planning(FlowId(flow)).map_err(refusal)).await
}

/// Carry a planner on after an interruption, a failure or a limit, in the same conversation.
#[tauri::command]
pub async fn resume_planning(app: AppState<'_>, engine: EngineState<'_>, flow: i64) -> Reply<()> {
    gate(&app)?;
    let engine = Arc::clone(&engine);
    blocking(move || engine.resume(FlowId(flow)).map_err(refusal)).await
}

/// Read the issue whole again, as Check the issue does: what the approval is measured against.
#[tauri::command]
pub async fn read_planned_issue(
    app: AppState<'_>,
    engine: EngineState<'_>,
    flow: i64,
) -> Reply<IssueText> {
    gate(&app)?;
    let engine = Arc::clone(&engine);
    blocking(move || engine.read_planned_issue(FlowId(flow)).map_err(refused)).await
}

/// Approve the newest plan for the issue whose revision you saw.
#[tauri::command]
pub async fn approve_plan(
    app: AppState<'_>,
    engine: EngineState<'_>,
    flow: i64,
    revision: u32,
    issue_revision: String,
    skip_design: bool,
) -> Reply<PlanDecision> {
    gate(&app)?;
    let engine = Arc::clone(&engine);
    blocking(move || {
        engine
            .approve_plan(FlowId(flow), revision, &issue_revision, skip_design)
            .map_err(refused)
    })
    .await
}

/// Ask the planner for changes to the newest plan, in the same conversation.
#[tauri::command]
pub async fn request_plan_changes(
    app: AppState<'_>,
    engine: EngineState<'_>,
    flow: i64,
    revision: u32,
    note: String,
) -> Reply<PlanDecision> {
    gate(&app)?;
    let engine = Arc::clone(&engine);
    blocking(move || {
        engine
            .request_plan_changes(FlowId(flow), revision, &note)
            .map_err(refused)
    })
    .await
}

/// One question with its discussion and what was sent, for its card.
#[tauri::command]
pub async fn question_thread(engine: EngineState<'_>, question: i64) -> Reply<QuestionThread> {
    let engine = Arc::clone(&engine);
    blocking(move || {
        engine
            .question_thread(QuestionId(question))
            .map_err(refused)
    })
    .await
}

/// Talk a planner's open question over with it before answering.
#[tauri::command]
pub async fn discuss_question(
    app: AppState<'_>,
    engine: EngineState<'_>,
    question: i64,
    text: String,
) -> Reply<()> {
    gate(&app)?;
    let engine = Arc::clone(&engine);
    blocking(move || engine.discuss(QuestionId(question), &text).map_err(refused)).await
}

/// Send to Jira…'s first click: the draft, built from the question and your answer. Records,
/// reads and sends nothing.
#[tauri::command]
pub async fn question_comment_draft(engine: EngineState<'_>, question: i64) -> Reply<CommentDraft> {
    let engine = Arc::clone(&engine);
    blocking(move || engine.comment_draft(QuestionId(question)).map_err(refused)).await
}

/// Send to Jira's second click: post exactly `body` to the question's issue.
#[tauri::command]
pub async fn post_question_comment(
    app: AppState<'_>,
    engine: EngineState<'_>,
    question: i64,
    body: String,
) -> Reply<CommentView> {
    gate(&app)?;
    let engine = Arc::clone(&engine);
    blocking(move || {
        engine
            .post_comment(QuestionId(question), &body)
            .map_err(refused)
    })
    .await
}

/// Try a comment that did not post again, reading the issue first.
#[tauri::command]
pub async fn retry_question_comment(
    app: AppState<'_>,
    engine: EngineState<'_>,
    approval: i64,
) -> Reply<()> {
    gate(&app)?;
    let engine = Arc::clone(&engine);
    blocking(move || engine.retry_comment(ApprovalId(approval)).map_err(refused)).await
}
