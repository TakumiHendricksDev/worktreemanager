//! The UI and MCP share one conversation-scoped lifecycle. Nothing here starts a provider or
//! changes its model, environment, permissions or credentials.

use crate::app::{AgentSessionFacts, AgentStatus, App};
use crate::commands::{AppState, Reply, blocking};
use crate::native_sessions::delivery::{DeliveryStatus, Resolution};
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
    app.conversation_sessions()
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
    /// What became of this design's native delivery, if a native conversation was asked to
    /// implement it. `None` in WTM Chat, whose turn is the delivery.
    pub delivery: Option<PrototypeDelivery>,
}

/// The receipt for a design's approval, as the review card needs it. The status is the receipt's
/// own, so the card can say Delivered rather than repeat a phase written when it was sent.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PrototypeDelivery {
    pub receipt: String,
    pub review: String,
    pub status: DeliveryStatus,
    pub error: Option<String>,
    pub resolution: Option<Resolution>,
}

pub fn state(app: &App, mut prototype: Prototype) -> PrototypeState {
    let review = prototype
        .implemented_review
        .clone()
        .or_else(|| prototype.review.as_ref().map(|r| r.id.clone()));
    let delivery = review.and_then(|review| {
        let found = app
            .native_sessions
            .deliveries
            .for_review(&prototype.id, &review)?;
        Some(PrototypeDelivery {
            receipt: found.id,
            review,
            status: found.status,
            error: found.error,
            resolution: found.resolution,
        })
    });
    // A rejected native delivery leaves the design unimplemented, whoever rejected it, so the card
    // offers Implement again. Derived here rather than written at each place a receipt can be
    // rejected — a provider reply, a stale hook, the user — so none of them can forget.
    if let Some(delivery) = &delivery
        && delivery.status == DeliveryStatus::Rejected
        && prototype.implemented_review.as_ref() == Some(&delivery.review)
    {
        prototype.implemented_review = None;
    }
    PrototypeState {
        stale: stale(app, &prototype),
        prototype,
        delivery,
    }
}

/// Re-announce one prototype after its delivery changed outside this module. Best effort: the
/// next listing says the same thing.
pub fn refresh(handle: &tauri::AppHandle, app: &App, session: &str, id: &str) {
    if let Ok(prototype) = record(app, session, id) {
        announce(handle, app, session, &prototype);
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
            dismissed: false,
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
pub async fn dismiss_prototype(
    handle: tauri::AppHandle,
    app: AppState<'_>,
    session: String,
    id: String,
) -> Reply<PrototypeState> {
    let app = Arc::clone(&app);
    blocking(move || {
        let _guard = MUTATING.lock();
        let mut prototype = record(&app, &session, &id)?;
        prototypes::dismiss(&mut prototype)?;
        announce(&handle, &app, &session, &prototype);
        Ok(state(&app, prototype))
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
        if let Some(native) = app.native_sessions.get(&session) {
            return implement_native(&handle, &app, &native, &session, &id, &review_id);
        }
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
            |prototype| close_previews(&handle, &app, prototype),
        )?;
        app.note_user_turn(&session);
        announce(&handle, &app, &session, &prototype);
        Ok(())
    })
    .await
}

fn close_previews(handle: &tauri::AppHandle, app: &Arc<App>, prototype: &Prototype) {
    // Session ownership alone also includes live app browsers, which must stay open.
    for browser in app.browsers.list(Some(&prototype.worktree_id)) {
        if preview_belongs(prototype, &browser) {
            crate::browser::close(handle, app, &browser.id);
        }
    }
}

/// Implement through a native conversation, whose delivery is a receipt rather than a turn.
///
/// `MUTATING` is released while the instruction is sent: Codex may take the whole reply timeout
/// to answer, and every other design in every conversation would wait on it. The record is read
/// again afterwards rather than trusted across that gap.
fn implement_native(
    handle: &tauri::AppHandle,
    app: &Arc<App>,
    native: &crate::native_sessions::Session,
    session: &str,
    id: &str,
    review_id: &str,
) -> Reply<()> {
    let attempted = app.native_sessions.deliveries.for_review(id, review_id);
    let prompt = {
        let _guard = MUTATING.lock();
        let mut prototype = record(app, session, id)?;
        match &attempted {
            // One exact review is one instruction, whichever conversation it went to.
            Some(receipt) if receipt.status != DeliveryStatus::Rejected => None,
            rejected => {
                // Only a rejected attempt leaves this set without a live receipt; clearing it is
                // what lets an explicit retry through `handoff`, never an automatic one.
                if rejected.is_some() && prototype.implemented_review.as_deref() == Some(review_id)
                {
                    prototype.implemented_review = None;
                }
                if stale(app, &prototype) {
                    return Err(failure(
                        "Project context changed. Refresh and review the design first.",
                    ));
                }
                Some(prototypes::handoff(&prototype, review_id)?)
            }
        }
    };
    let (status, error) = match prompt {
        Some(prompt) => {
            let receipt = native
                .send("Implement reviewed design", &prompt, Some((id, review_id)))
                .map_err(failure)?;
            (receipt.status, receipt.error)
        }
        None => attempted.map_or((DeliveryStatus::Pending, None), |d| (d.status, d.error)),
    };
    let _guard = MUTATING.lock();
    let mut prototype = record(app, session, id)?;
    if status == DeliveryStatus::Rejected {
        announce(handle, app, session, &prototype);
        return Err(failure(format!(
            "Implementation was not sent: {}. You can retry.",
            error.unwrap_or_else(|| "the provider rejected it".into())
        )));
    }
    // A newer revision prepared while this one was sent keeps its own review card.
    let current = prototype.review.as_ref().is_some_and(|r| r.id == review_id);
    if current && prototype.implemented_review.as_deref() != Some(review_id) {
        prototype.implemented_review = Some(review_id.to_owned());
        "implementation approved".clone_into(&mut prototype.phase);
        prototypes::save(&prototype)?;
    }
    close_previews(handle, app, &prototype);
    announce(handle, app, session, &prototype);
    Ok(())
}

/// What cancelling a design did to the turn generating it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelOutcome {
    /// WTM stopped the running turn itself.
    pub interrupted: bool,
    /// What is left for the user to do when it could not. The design is cancelled either way,
    /// which is why this is not an error.
    pub notice: Option<String>,
}

#[tauri::command]
pub async fn cancel_prototype(
    handle: tauri::AppHandle,
    app: AppState<'_>,
    session: String,
    id: String,
) -> Reply<CancelOutcome> {
    let app = Arc::clone(&app);
    blocking(move || {
        {
            let _guard = MUTATING.lock();
            let mut prototype = record(&app, &session, &id)?;
            prototypes::cancel(&mut prototype)?;
            announce(&handle, &app, &session, &prototype);
        }
        // Outside `MUTATING`: an interrupt waits for the provider's answer.
        if let Some(native) = app.native_sessions.get(&session) {
            let notice = if native.capabilities().stop {
                native
                    .interrupt()
                    .err()
                    .map(|e| format!("Design cancelled. {e}"))
            } else {
                Some(crate::native_sessions::CLAUDE_STOP.to_owned())
            };
            return Ok(CancelOutcome {
                interrupted: notice.is_none(),
                notice,
            });
        }
        app.with_agent(&session, wtm_agent::AgentSession::interrupt)
            .map_err(|e| {
                failure(format!(
                    "Design cancelled; this chat could not be interrupted: {e}"
                ))
            })?;
        Ok(CancelOutcome {
            interrupted: true,
            notice: None,
        })
    })
    .await
}
