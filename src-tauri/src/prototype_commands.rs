//! The UI and MCP share one conversation-scoped lifecycle. Nothing here starts a provider or
//! changes its model, environment, permissions or credentials.

use crate::app::{AgentSessionFacts, AgentStatus, App};
use crate::commands::{AppState, Reply, blocking};
use crate::prototype_context::{self as context, Discovery, SourceChoice, failure};
use crate::prototypes::{self, Prototype};
use crate::view::{BrowserView, ErrorView};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::Emitter;

static MUTATING: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
pub const EVENT: &str = "prototype:changed";

pub fn root_of(app: &App, project_id: &str, worktree_id: &str) -> Result<PathBuf, ErrorView> {
    let project = app.project(project_id)?;
    app.worktree(&project, worktree_id)?
        .path
        .canonicalize()
        .map_err(|_| failure("Selected worktree is missing"))
}

pub fn storage(app: &App) -> Result<PathBuf, ErrorView> {
    let paths = app.config.paths();
    let data = paths
        .home_dir
        .parent()
        .ok_or_else(|| failure("WTM data directory is missing"))?;
    let storage = prototypes::storage(data);
    prototypes::private_dir(&storage)?;
    storage.canonicalize().map_err(|_| {
        failure("Cannot prepare prototype storage. Check the WTM data directory and retry.")
    })
}

pub fn owner(app: &App, session: &str) -> Result<AgentSessionFacts, ErrorView> {
    app.live_agents()
        .into_iter()
        .find(|a| a.session == session && !a.ephemeral && a.scope.place().is_some())
        .ok_or_else(|| failure("Open or resume this worktree's chat to work with its prototypes"))
}

pub fn belongs(prototype: &Prototype, owner: &AgentSessionFacts) -> bool {
    owner
        .scope
        .place()
        .is_some_and(|(p, w)| p == prototype.project_id && w == prototype.worktree_id)
        && prototype.provider == owner.provider
        && (prototype.session == owner.session
            || (!prototype.conversation.is_empty()
                && prototype.conversation == owner.provider_session))
}

pub fn record(app: &App, session: &str, id: &str) -> Result<Prototype, ErrorView> {
    let owner = owner(app, session)?;
    let (project, worktree) = owner
        .scope
        .place()
        .ok_or_else(|| failure("A prototype needs a worktree"))?;
    let root = root_of(app, project, worktree)?;
    let record = prototypes::load(&storage(app)?, id, project, worktree)?;
    if Path::new(&record.root) != root || !belongs(&record, &owner) {
        return Err(failure(
            "This prototype belongs to another worktree or conversation",
        ));
    }
    Ok(record)
}

pub fn stale(app: &App, prototype: &Prototype) -> bool {
    let Ok(names) = app.git.files(Path::new(&prototype.root)) else {
        return true;
    };
    let choices = prototype
        .sources
        .iter()
        .map(|s| SourceChoice {
            path: s.path.clone(),
            fingerprint: s.fingerprint.clone(),
        })
        .collect::<Vec<_>>();
    if choices.is_empty() {
        return false;
    }
    context::capture(Path::new(&prototype.root), &names.paths, &choices).is_err()
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrototypeState {
    pub prototype: Prototype,
    pub stale: bool,
}

pub fn state(app: &App, prototype: Prototype) -> PrototypeState {
    PrototypeState {
        stale: stale(app, &prototype),
        prototype,
    }
}

pub fn announce(handle: &tauri::AppHandle, app: &App, session: &str, prototype: &Prototype) {
    let _ = handle.emit(
        EVENT,
        serde_json::json!({ "session": session, "state": state(app, prototype.clone()) }),
    );
}

pub fn prepare(
    app: &App,
    session: &str,
    brief: &str,
    id: Option<&str>,
    paths: Option<&[String]>,
) -> Result<Prototype, ErrorView> {
    let _guard = MUTATING.lock();
    if brief.trim().is_empty() || brief.len() > 12_000 {
        return Err(failure("Describe the design in up to 12,000 characters"));
    }
    let owner = owner(app, session)?;
    let (project, worktree) = owner
        .scope
        .place()
        .ok_or_else(|| failure("A prototype needs a worktree"))?;
    let root = root_of(app, project, worktree)?;
    let storage = storage(app)?;
    if storage.starts_with(&root) {
        return Err(failure(
            "WTM's data directory must be outside the application worktree",
        ));
    }
    let names = app.git.files(&root)?;
    let discovery = context::discover(&root, &names.paths, names.truncated);
    let sources = if let Some(paths) = paths {
        let choices = paths
            .iter()
            .map(|path| {
                discovery
                    .sources
                    .iter()
                    .find(|s| s.path == *path)
                    .map(|s| SourceChoice {
                        path: s.path.clone(),
                        fingerprint: s.fingerprint.clone(),
                    })
                    .ok_or_else(|| failure("Choose sources from the discovered project context"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        context::capture(&root, &names.paths, &choices)?
    } else {
        context::select(&discovery, brief)
    };
    let mut prototype = if let Some(id) = id {
        record(app, session, id)?
    } else {
        let id = uuid::Uuid::new_v4().to_string();
        Prototype {
            directory: storage.join(&id).to_string_lossy().into_owned(),
            id,
            project_id: project.to_owned(),
            worktree_id: worktree.to_owned(),
            root: root.to_string_lossy().into_owned(),
            session: session.to_owned(),
            provider: owner.provider.clone(),
            conversation: owner.provider_session.clone(),
            title: brief
                .lines()
                .next()
                .unwrap_or("Prototype")
                .chars()
                .take(80)
                .collect(),
            brief: brief.to_owned(),
            created: app.clock.now_iso(),
            upstream: prototypes::UPSTREAM.to_owned(),
            sources: vec![],
            revision: String::new(),
            phase: String::new(),
            feedback: vec![],
            review: None,
            implemented_review: None,
        }
    };
    prototype.sources = sources;
    session.clone_into(&mut prototype.session);
    prototype.conversation = owner.provider_session;
    prototypes::begin(&mut prototype, brief)?;
    Ok(prototype)
}

pub fn publish(
    app: &App,
    session: &str,
    id: &str,
    revision: &str,
    html: &str,
    reuse: &str,
) -> Result<Prototype, ErrorView> {
    let _guard = MUTATING.lock();
    let mut prototype = record(app, session, id)?;
    if stale(app, &prototype) {
        return Err(failure(
            "Project context changed during generation. Prepare a fresh revision before publishing.",
        ));
    }
    prototypes::publish(&mut prototype, revision, html, reuse)?;
    Ok(prototype)
}

pub fn preview(
    handle: &tauri::AppHandle,
    app: &Arc<App>,
    session: &str,
    id: &str,
    review_id: &str,
) -> Result<BrowserView, ErrorView> {
    let _guard = MUTATING.lock();
    let mut prototype = record(app, session, id)?;
    let (html, _) = prototypes::artifact(&prototype, review_id)?;
    let view = crate::browser::open_prototype(
        handle,
        app,
        &prototype.project_id,
        &prototype.worktree_id,
        &prototypes::review_document(&html),
        session,
        &format!("{id}/{review_id}"),
    )?;
    if let Some(review) = &mut prototype.review {
        review.opened = true;
    }
    prototypes::save(&prototype)?;
    announce(handle, app, session, &prototype);
    Ok(view)
}

#[tauri::command]
pub async fn discover_design_context(
    app: AppState<'_>,
    project_id: String,
    worktree_id: String,
) -> Reply<Discovery> {
    let app = Arc::clone(&app);
    blocking(move || {
        let root = root_of(&app, &project_id, &worktree_id)?;
        let names = app.git.files(&root)?;
        Ok(context::discover(&root, &names.paths, names.truncated))
    })
    .await
}

#[tauri::command]
pub async fn list_prototypes(app: AppState<'_>, session: String) -> Reply<Vec<PrototypeState>> {
    let app = Arc::clone(&app);
    blocking(move || {
        let owner = owner(&app, &session)?;
        let (project, worktree) = owner
            .scope
            .place()
            .ok_or_else(|| failure("A prototype needs a worktree"))?;
        root_of(&app, project, worktree)?;
        let storage = storage(&app)?;
        let mut records = std::fs::read_dir(&storage)
            .map_err(|_| {
                failure("Cannot read prototypes. Retry after checking WTM's data directory.")
            })?
            .filter_map(Result::ok)
            .take(1000)
            .filter_map(|entry| {
                prototypes::load(
                    &storage,
                    &entry.file_name().to_string_lossy(),
                    project,
                    worktree,
                )
                .ok()
            })
            .filter(|p| belongs(p, &owner))
            .map(|p| state(&app, p))
            .collect::<Vec<_>>();
        records.sort_by(|a, b| b.prototype.created.cmp(&a.prototype.created));
        records.truncate(20);
        Ok(records)
    })
    .await
}

#[tauri::command]
pub async fn open_prototype_preview(
    handle: tauri::AppHandle,
    app: AppState<'_>,
    session: String,
    id: String,
    review_id: String,
) -> Reply<BrowserView> {
    let app = Arc::clone(&app);
    blocking(move || preview(&handle, &app, &session, &id, &review_id)).await
}

pub fn preview_belongs(prototype: &Prototype, browser: &BrowserView) -> bool {
    browser.project == prototype.project_id
        && browser.worktree == prototype.worktree_id
        && browser
            .prototype_id
            .as_deref()
            .and_then(|identity| identity.split_once('/'))
            .is_some_and(|(id, _)| id == prototype.id)
}

pub fn send_implementation(
    prototype: &mut Prototype,
    review_id: &str,
    send: impl FnOnce(&str) -> Result<(), String>,
    close_previews: impl FnOnce(&Prototype),
) -> Reply<()> {
    let prompt = prototypes::handoff(prototype, review_id)?;
    prototype.implemented_review = Some(review_id.to_owned());
    "implementation approved".clone_into(&mut prototype.phase);
    prototypes::save(prototype)?;
    if let Err(error) = send(&prompt) {
        prototype.implemented_review = None;
        "ready for review".clone_into(&mut prototype.phase);
        prototypes::save(prototype)?;
        return Err(failure(format!(
            "Implementation was not sent: {error}. You can retry."
        )));
    }
    close_previews(prototype);
    Ok(())
}

#[tauri::command]
pub async fn implement_prototype(
    handle: tauri::AppHandle,
    app: AppState<'_>,
    session: String,
    id: String,
    review_id: String,
) -> Reply<()> {
    let app = Arc::clone(&app);
    blocking(move || {
        let _guard = MUTATING.lock();
        let mut prototype = record(&app, &session, &id)?;
        if app
            .overview_of(&session)
            .is_none_or(|s| s.status != AgentStatus::Idle)
        {
            return Err(failure(
                "Wait for this chat to finish its turn before implementing",
            ));
        }
        if stale(&app, &prototype) {
            return Err(failure(
                "Project context changed. Ask for a refreshed prototype and review it first.",
            ));
        }
        send_implementation(
            &mut prototype,
            &review_id,
            |prompt| {
                app.send_agent_turn(&session, prompt, &[])
                    .map_err(|error| error.to_string())
            },
            |prototype| {
                // Session ownership alone also includes live app browsers, which must stay open.
                for browser in app.browsers.list(Some(&prototype.worktree_id)) {
                    if preview_belongs(prototype, &browser) {
                        crate::browser::close(&handle, &app, &browser.id);
                    }
                }
            },
        )?;
        app.note_user_turn(&session);
        announce(&handle, &app, &session, &prototype);
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn cancel_prototype(
    handle: tauri::AppHandle,
    app: AppState<'_>,
    session: String,
    id: String,
) -> Reply<()> {
    let app = Arc::clone(&app);
    blocking(move || {
        let _guard = MUTATING.lock();
        let mut prototype = record(&app, &session, &id)?;
        prototypes::cancel(&mut prototype)?;
        announce(&handle, &app, &session, &prototype);
        app.with_agent(&session, wtm_agent::AgentSession::interrupt)
            .map_err(|e| {
                failure(format!(
                    "Design cancelled; this chat could not be interrupted: {e}"
                ))
            })?;
        Ok(())
    })
    .await
}
