//! A conversation edit forks provider history. No display text or provider transcript file is patched.
use crate::{
    app::{AgentStatus, App, SeqEvent},
    commands::{AppState, Reply, SessionOptions, blocking},
    view::ErrorView,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::Path,
    sync::Arc,
};
use wtm_core::{
    model::AgentEvent,
    ports::exec::{CancelToken, Invocation},
};

#[allow(clippy::needless_pass_by_value)]
fn error(message: impl ToString) -> ErrorView {
    ErrorView::new("chatEdit", message.to_string())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: String,
    pub turn: String,
    pub text: String,
    pub previous: Option<String>,
    pub problem: Option<String>,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub session: String,
    pub revision: u64,
    pub messages: Vec<Message>,
    pub problem: Option<String>,
    pub status: &'static str,
}

pub fn messages(events: &[SeqEvent]) -> Vec<Message> {
    let mut complete = false;
    let mut turns = Vec::<String>::new();
    let mut finished = BTreeSet::new();
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for row in events {
        match &row.event {
            AgentEvent::HistoryBoundary { complete: full } => complete = *full,
            AgentEvent::HistoricalTurn { turn, complete } => {
                if !turns.contains(turn) {
                    turns.push(turn.clone());
                }
                if *complete {
                    finished.insert(turn.clone());
                }
            }
            AgentEvent::TurnStarted { turn } => {
                if !turns.contains(turn) {
                    turns.push(turn.clone());
                }
            }
            AgentEvent::TurnFinished { turn, .. } => {
                finished.insert(turn.clone());
            }
            AgentEvent::MessageIdentity {
                message,
                turn,
                text,
                attachments,
                truncated,
            } => {
                if !seen.insert(message.clone()) {
                    continue;
                }
                let position = turns.iter().position(|t| t == turn);
                let previous = position
                    .and_then(|at| at.checked_sub(1))
                    .and_then(|at| turns.get(at))
                    .cloned();
                let problem = if *truncated {
                    Some(
                        "The retained message text was truncated. Editing it would lose part of the original input.",
                    )
                } else if *attachments {
                    Some(
                        "This provider item contains attachments. Editing it without losing those attachments is not supported yet.",
                    )
                } else if out.iter().any(|m: &Message| m.turn == *turn) {
                    Some(
                        "This message was sent inside a running turn. The provider can fork at turn boundaries only.",
                    )
                } else if position.is_none() || (previous.is_none() && !complete) {
                    Some(
                        "The retained history does not prove the exact start of this turn. Reopen the complete conversation first.",
                    )
                } else {
                    None
                };
                out.push(Message {
                    id: message.clone(),
                    turn: turn.clone(),
                    text: text.clone(),
                    previous,
                    problem: problem.map(str::to_owned),
                });
            }
            _ => {}
        }
    }
    for message in &mut out {
        if message.problem.is_none()
            && (!finished.contains(&message.turn)
                || message
                    .previous
                    .as_ref()
                    .is_some_and(|t| !finished.contains(t)))
        {
            message.problem = Some("The turn boundary is not completed yet.".into());
        }
    }
    out
}
pub fn snapshot(app: &App, session: &str) -> Reply<Snapshot> {
    let source = app
        .agent_fork_source(session)
        .ok_or_else(|| error("Session is unavailable or still starting"))?;
    let events = app.agent_replay(session);
    let overview = app
        .overview_of(session)
        .ok_or_else(|| error("Session is not live"))?;
    let problem = if overview.status != AgentStatus::Idle {
        Some("Session is busy. Wait for it to finish or stop it first.")
    } else if source.provider != "codex" {
        Some(
            "This provider adapter does not yet negotiate an exact message boundary. The original conversation is unchanged.",
        )
    } else if source.scope.place().is_none() {
        Some("Editing Home conversations is not supported by this provider adapter yet.")
    } else {
        None
    };
    Ok(Snapshot {
        session: session.into(),
        revision: events.last().map_or(0, |e| e.seq),
        messages: messages(&events),
        problem: problem.map(str::to_owned),
        status: if overview.status != AgentStatus::Idle {
            "busy"
        } else if problem.is_some() {
            "unsupported"
        } else {
            "ready"
        },
    })
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Edit {
    pub id: String,
    pub session: String,
    pub message: String,
    pub revision: u64,
    pub text: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    pub session: String,
    pub original: String,
    pub submitted: bool,
}

fn probe(app: &App) -> Reply<()> {
    let entry = wtm_agent::entry("codex").ok_or_else(|| error("Codex is unavailable"))?;
    let executable = app
        .agent_executable(entry)
        .ok_or_else(|| error("Codex is not installed"))?;
    let path = app
        .config
        .paths()
        .config_dir
        .join("plugin-probes")
        .join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&path).map_err(error)?;
    let invocation = Invocation::new(
        vec![
            executable.to_string_lossy().into_owned(),
            "app-server".into(),
            "generate-json-schema".into(),
            "--out".into(),
            path.to_string_lossy().into_owned(),
        ],
        &path,
        15_000,
    );
    let result=app.runner.run(&invocation,&CancelToken::new()).map_err(error).and_then(|output|{
        if !output.is_success() { return Err(error("Installed Codex could not report its fork capabilities.")); }
        let bytes=std::fs::read(path.join("v2/ThreadForkParams.json")).map_err(error)?;
        let schema:Value=serde_json::from_slice(&bytes).map_err(error)?;
        if schema["properties"]["lastTurnId"].is_object(){Ok(())}else{Err(error("Installed Codex does not advertise exact-boundary forks. Update Codex before editing."))}
    });
    let _ = std::fs::remove_dir_all(&path);
    result
}

fn save_outcome(path: &Path, edit: &Edit, outcome: &Outcome) -> Reply<()> {
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(error)?;
        file.write_all(
            json!({"request":edit,"outcome":outcome})
                .to_string()
                .as_bytes(),
        )
        .map_err(error)?;
        file.sync_all().map_err(error)?;
        std::fs::rename(&temporary, path).map_err(error)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(temporary);
    }
    result
}

#[tauri::command]
pub async fn chat_edit_snapshot(app: AppState<'_>, session: String) -> Reply<Snapshot> {
    let app = Arc::clone(&app);
    blocking(move || snapshot(&app, &session)).await
}

#[tauri::command]
pub async fn chat_edit(handle: tauri::AppHandle, app: AppState<'_>, edit: Edit) -> Reply<Outcome> {
    let app = Arc::clone(&app);
    blocking(move||{
        uuid::Uuid::parse_str(&edit.id).map_err(|_|error("Edit request must have a UUID"))?;
        if edit.text.trim().is_empty()||edit.text.len()>1_048_576{return Err(error("Edited message is empty or too large"));}
        // A create-new journal fences retries even across a host restart. A pending record is
        // deliberately never replayed: the provider may already have accepted its turn.
        let folder=app.config.paths().config_dir.join("chat-edits");
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&folder).map_err(error)?;
        let path=folder.join(format!("{}.json",edit.id));
        if path.exists(){
            let record:Value=serde_json::from_slice(&std::fs::read(&path).map_err(error)?).map_err(error)?;
            if record["request"]!=serde_json::to_value(&edit).map_err(error)?{return Err(error("This edit id belongs to a different request"));}
            return serde_json::from_value(record["outcome"].clone()).map_err(|_|error("The previous edit has an unknown outcome. It will not be submitted again. Check the conversation history."));
        }
        let before=snapshot(&app,&edit.session)?;
        if let Some(problem)=before.problem{return Err(error(problem));}
        if before.revision!=edit.revision{return Err(error("Conversation changed. Reopen Edit to review its latest history."));}
        let message=before.messages.iter().find(|m|m.id==edit.message).ok_or_else(||error("Message is no longer in retained history"))?;
        if let Some(problem)=&message.problem{return Err(error(problem));}
        probe(&app)?;
        let source=app.agent_fork_source(&edit.session).ok_or_else(||error("Original session closed"))?;
        let (project,worktree)=source.scope.place().ok_or_else(||error("No workspace"))?;
        let events=app.agent_replay(&edit.session);
        let mut options=SessionOptions::default();
        for row in &events {match &row.event {
            AgentEvent::SessionReady {model,effort,mode,..}=>{options.model.clone_from(model);options.effort.clone_from(effort);options.mode.clone_from(mode);},
            AgentEvent::ModeChanged {mode}=>options.mode=Some(mode.clone()),
            _=>{},
        }}
        if options.mode.is_none(){return Err(error("The current provider permission mode is unknown; it cannot be inherited safely."));}
        let current=snapshot(&app,&edit.session)?;
        if current.revision!=edit.revision||current.problem.is_some(){return Err(error("Conversation changed while checking provider support. Reopen Edit."));}
        let mut journal=std::fs::OpenOptions::new().create_new(true).write(true).mode(0o600).open(&path).map_err(error)?;
        let pending=json!({"request":edit,"outcome":null});
        journal.write_all(pending.to_string().as_bytes()).map_err(error)?;
        journal.sync_all().map_err(error)?;
        let fork=message.previous.as_ref().map(|_|source.provider_session.clone());
        let branch=crate::commands::open_agent_branch_process(handle,&app,project,worktree,&source.provider,options,fork,message.previous.clone(),true)?;
        let outcome=Outcome{session:branch.clone(),original:edit.session.clone(),submitted:false};
        save_outcome(&path,&edit,&outcome)?;
        app.send_agent_turn(&branch,&edit.text,&[]).map_err(error)?;
        app.note_user_turn(&branch);
        let outcome=Outcome{submitted:true,..outcome};
        save_outcome(&path,&edit,&outcome)?;
        Ok(outcome)
    }).await
}
