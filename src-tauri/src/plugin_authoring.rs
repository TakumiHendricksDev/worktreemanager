//! Authoring is an ordinary, visible agent conversation. Only the package manager may activate
//! its output, so an agent finishing a turn never grants trust to the files it wrote.
use crate::{
    app::App,
    commands::{AppState, Reply, blocking},
    view::ErrorView,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};
use tauri::Emitter;
use wtm_core::{
    model::AgentEvent,
    ports::exec::{CancelToken, Invocation},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    pub id: String,
    pub provider: String,
    pub brief: String,
    pub folder: String,
    pub session: Option<String>,
    pub conversation: Option<String>,
    pub submitted: bool,
    pub problem: Option<String>,
    #[serde(default)]
    pub validation_error: Option<String>,
}

/// Separate from installed packages: abandoning a draft must not uninstall anything, and
/// recovering a plugin registry must not lose the conversation that can repair a package.
#[derive(Debug)]
pub struct Jobs {
    path: PathBuf,
    records: Mutex<BTreeMap<String, Job>>,
    problem: Option<String>,
    operations: Mutex<()>,
}
impl Jobs {
    pub fn new(path: PathBuf) -> Self {
        let (records, problem) = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(records) => (records, None),
                Err(e) => (
                    BTreeMap::new(),
                    Some(format!("Could not read plugin drafts: {e}")),
                ),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (BTreeMap::new(), None),
            Err(e) => (BTreeMap::new(), Some(e.to_string())),
        };
        Self {
            path,
            records: Mutex::new(records),
            problem,
            operations: Mutex::new(()),
        }
    }
    pub fn list(&self) -> Reply<Vec<Job>> {
        if let Some(e) = &self.problem {
            return Err(error(e));
        }
        Ok(self.records.lock().values().cloned().collect())
    }
    pub fn get(&self, id: &str) -> Reply<Job> {
        self.list()?
            .into_iter()
            .find(|j| j.id == id)
            .ok_or_else(|| error("Plugin draft not found"))
    }
    fn edit(&self, id: &str, change: impl FnOnce(&mut Job)) -> Reply<Job> {
        let mut records = self.records.lock();
        let mut next = records.clone();
        let job = next
            .get_mut(id)
            .ok_or_else(|| error("Plugin draft not found"))?;
        change(job);
        let job = job.clone();
        self.persist(&next)?;
        *records = next;
        Ok(job)
    }
    fn persist(&self, records: &BTreeMap<String, Job>) -> Reply<()> {
        if let Some(e) = &self.problem {
            return Err(error(e));
        }
        let parent = self
            .path
            .parent()
            .ok_or_else(|| error("Invalid drafts path"))?;
        std::fs::create_dir_all(parent).map_err(error)?;
        let temp = parent.join(format!("plugin-drafts-{}.tmp", uuid::Uuid::new_v4()));
        std::fs::write(&temp, serde_json::to_vec_pretty(records).map_err(error)?).map_err(error)?;
        std::fs::rename(&temp, &self.path).map_err(error)
    }
    /// Called only for identity/failure events, never for streamed tokens.
    pub fn observe(&self, app: &App, session: &str, event: &AgentEvent) -> Reply<bool> {
        if !matches!(
            event,
            AgentEvent::SessionReady { .. }
                | AgentEvent::TurnStarted { .. }
                | AgentEvent::Failed { .. }
                | AgentEvent::LimitReached { .. }
        ) {
            return Ok(false);
        }
        let Some(facts) = app.live_agents().into_iter().find(|f| f.session == session) else {
            return Ok(false);
        };
        let Some(job) = self.list()?.into_iter().find(|j| {
            facts.provider == j.provider
                && facts.scope.worktree_id() == Some(j.folder.as_str())
                && (j.session.as_deref() == Some(session)
                    || j.conversation.as_deref() == Some(facts.provider_session.as_str()))
        }) else {
            return Ok(false);
        };
        self.edit(&job.id, |j| {
            j.session = Some(session.into());
            match event {
                AgentEvent::SessionReady {
                    provider_session_id,
                    ..
                } => j.conversation = Some(provider_session_id.clone()),
                AgentEvent::TurnStarted { .. } => {
                    j.problem = None;
                    j.validation_error = None;
                }
                AgentEvent::Failed { message } | AgentEvent::LimitReached { message, .. } => {
                    j.problem = Some(message.clone());
                }
                _ => {}
            }
        })?;
        Ok(true)
    }
}
fn error(message: impl std::fmt::Display) -> ErrorView {
    ErrorView::new("plugin", message.to_string())
}

/// Only new or empty directories qualify. In particular, a picker cannot turn the active
/// repository into a plugin workspace or overwrite a previously generated draft.
pub fn prepare(app: &App, id: &str, provider: &str, brief: &str, folder: &str) -> Reply<Job> {
    let _operation = app.plugin_jobs.operations.lock();
    if uuid::Uuid::parse_str(id).is_err()
        || !["claude", "codex"].contains(&provider)
        || brief.trim().is_empty()
        || brief.len() > 32_768
    {
        return Err(error(
            "Choose Claude or Codex and enter a brief of at most 32 KiB",
        ));
    }
    let path = crate::app::expand_tilde(Path::new(folder));
    if !path.is_absolute()
        || path
            .components()
            .any(|p| matches!(p, std::path::Component::ParentDir))
    {
        return Err(error("Choose an absolute plugin folder path"));
    }
    if let Some(job) = app.plugin_jobs.list()?.into_iter().find(|j| j.id == id) {
        if job.provider != provider || job.brief != brief || Path::new(&job.folder) != path {
            return Err(error(
                "This creation request already belongs to another draft",
            ));
        }
        return Ok(job);
    }
    let entry = wtm_agent::entry(provider).ok_or_else(|| error("Unknown provider"))?;
    if app.agent_executable(entry).is_none() {
        return Err(error(format!("{provider} is not installed on WTM's PATH")));
    }
    if !app.plugins.enabled("wtm.chat") {
        return Err(error("Enable WTM Chat in Plugins to create with an agent"));
    }
    for parent in path.ancestors() {
        if let Ok(meta) = std::fs::symlink_metadata(parent)
            && meta.file_type().is_symlink()
        {
            return Err(error("Choose a plugin folder without symbolic links"));
        }
    }
    if path.exists()
        && (!path.is_dir() || std::fs::read_dir(&path).map_err(error)?.next().is_some())
    {
        return Err(error(
            "Choose a new or empty folder. Existing files will not be overwritten.",
        ));
    }
    std::fs::create_dir_all(&path).map_err(error)?;
    let path = path.canonicalize().map_err(error)?;
    let invocation = Invocation::new(
        vec![
            "git".into(),
            "-c".into(),
            "core.hooksPath=/dev/null".into(),
            "init".into(),
            "--template=".into(),
            "--initial-branch=main".into(),
        ],
        &path,
        10_000,
    );
    let result = app
        .runner
        .run(&invocation, &CancelToken::new())
        .map_err(error)?;
    if !result.is_success() {
        return Err(error(result.stderr));
    }
    // Git metadata is omitted from reviewed packages. The guide is bundled into the app rather
    // than fetched or resolved against a developer's source checkout.
    std::fs::write(path.join(".git/wtm-plugin-guide.md"), authoring_guide()).map_err(error)?;
    std::fs::write(
        path.join(".git/wtm-plugin-operations.json"),
        include_str!("../../crates/wtm-plugins/contracts/operations.json"),
    )
    .map_err(error)?;
    let root = app.register(&path).map_err(error)?;
    let job = Job {
        id: id.into(),
        provider: provider.into(),
        brief: brief.into(),
        folder: root.to_string_lossy().into_owned(),
        session: None,
        conversation: None,
        submitted: false,
        problem: None,
        validation_error: None,
    };
    let mut records = app.plugin_jobs.records.lock();
    let mut next = records.clone();
    next.insert(id.into(), job.clone());
    app.plugin_jobs.persist(&next)?;
    *records = next;
    Ok(job)
}
fn authoring_guide() -> String {
    format!(
        "{}\n\n## Complete example manifest\n```toml\n{}\n```\n\n## Complete example view\n```html\n{}\n```\n",
        format_args!(
            "{}\n\n## Host contract inventory (API 2)\n```json\n{}\n```\n\n## In-feature example manifest\n```toml\n{}\n```",
            include_str!("../../docs/plugins.md"),
            include_str!("../../crates/wtm-plugins/contracts/catalog.json"),
            include_str!("../../examples/plugins/workbench-toolkit/wtm-plugin.toml")
        ),
        include_str!("../../examples/plugins/project-tools/wtm-plugin.toml"),
        include_str!("../../examples/plugins/project-tools/summary.html")
    )
}
fn prompt(job: &Job) -> String {
    format!(
        "Create a WTM plugin for this request:\n\n{}\n\nWork only in this plugin workspace: {}. Read .git/wtm-plugin-guide.md for the versioned WTM plugin API and working examples. Write wtm-plugin.toml at the workspace root and include all declared files. Use the public contribution contracts; do not modify WTM source, private internals, or the currently installed plugin registry. Explain unsupported capabilities instead of inventing APIs. Validate and test the package, then summarize the files, access required, and what was actually tested. Do not install or enable the plugin: the user will review an immutable snapshot in Settings. Keep using this conversation for refinements and ordinary permission or design approvals.",
        job.brief, job.folder
    )
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobView {
    #[serde(flatten)]
    pub job: Job,
    pub status: String,
    pub installed: Option<String>,
}
pub fn list(app: &App) -> Reply<Vec<JobView>> {
    let active = app.overview();
    let identities = app.live_agents();
    let installed = app.plugins.snapshot();
    Ok(app
        .plugin_jobs
        .list()?
        .into_iter()
        .map(|mut job| {
            if let Some(live) = identities.iter().find(|s| {
                s.provider == job.provider
                    && s.scope.worktree_id() == Some(job.folder.as_str())
                    && (job.session.as_deref() == Some(s.session.as_str())
                        || (!s.provider_session.is_empty()
                            && job.conversation.as_deref() == Some(s.provider_session.as_str())))
            }) {
                job.session = Some(live.session.clone());
            }
            let session = active
                .iter()
                .find(|s| s.session == job.session.as_deref().unwrap_or_default());
            let status = if let Some(s) = session {
                match s.status {
                    crate::app::AgentStatus::Starting => "starting",
                    crate::app::AgentStatus::Working => "building",
                    crate::app::AgentStatus::NeedsYou => "waiting",
                    crate::app::AgentStatus::Failed => "error",
                    crate::app::AgentStatus::Idle => {
                        if job.problem.is_some() || job.validation_error.is_some() {
                            "error"
                        } else {
                            "idle"
                        }
                    }
                }
            } else {
                "stopped"
            }
            .into();
            let package = installed
                .registry
                .installed
                .values()
                .find(|p| p.current.kind == "local" && p.current.source == job.folder)
                .map(|p| p.current.manifest.id.clone());
            JobView {
                job,
                status,
                installed: package,
            }
        })
        .collect())
}

pub fn open(handle: &tauri::AppHandle, app: &Arc<App>, id: &str, fix: bool) -> Reply<Job> {
    let _operation = app.plugin_jobs.operations.lock();
    let mut job = app.plugin_jobs.get(id)?;
    let live = app.live_agents().into_iter().find(|s| {
        s.provider == job.provider
            && s.scope.worktree_id() == Some(job.folder.as_str())
            && (Some(s.session.as_str()) == job.session.as_deref()
                || (!s.provider_session.is_empty()
                    && Some(s.provider_session.as_str()) == job.conversation.as_deref()))
    });
    let session = if let Some(live) = live {
        live.session
    } else {
        let session = crate::commands::open_agent_process(
            handle.clone(),
            app,
            &job.folder,
            &job.folder,
            &job.provider,
            crate::commands::SessionOptions {
                resume: job.conversation.clone(),
                typed: Some(true),
                ..Default::default()
            },
            None,
        )?;
        app.plugin_jobs
            .edit(id, |j| j.session = Some(session.clone()))?;
        // A provider can announce its identity inside open(), before the job knows the process id.
        for event in app.agent_replay(&session) {
            app.plugin_jobs.observe(app, &session, &event.event)?;
        }
        session
    };
    job = app
        .plugin_jobs
        .edit(id, |j| j.session = Some(session.clone()))?;
    let project = app.project(&job.folder).map_err(error)?;
    let spec = project.agent_spec(&job.provider);
    crate::agent_bridge::announce_spawn(
        handle,
        &crate::agent_bridge::SpawnedSession {
            session: session.clone(),
            project: job.folder.clone(),
            worktree: job.folder.clone(),
            provider: job.provider.clone(),
            model: spec.model,
            effort: spec.effort,
            mode: spec.mode,
            parent_session: None,
            run: None,
            title: Some("Plugin development".into()),
            opened_by: None,
        },
    );
    let text = if !job.submitted {
        Some(prompt(&job))
    } else if fix {
        job.validation_error.as_ref().map(|e| format!("Fix this plugin validation error in the existing plugin folder, then validate again. Do not install it.\n\n{e}"))
    } else {
        None
    };
    if let Some(text) = text {
        if app.overview_of(&session).is_some_and(|s| {
            matches!(
                s.status,
                crate::app::AgentStatus::Working | crate::app::AgentStatus::NeedsYou
            )
        }) {
            return Err(error(
                "The agent is busy. Open its chat to reply or stop the current turn first.",
            ));
        }
        // Persist before sending: after a crash an uncertain prompt must never be replayed
        // automatically. The visible conversation is where the user can choose to retry it.
        app.plugin_jobs.edit(id, |j| {
            j.submitted = true;
            j.problem = None;
        })?;
        if let Err(e) = app.send_agent_turn(&session, &text, &[]) {
            app.plugin_jobs.edit(id, |j| {
                j.problem = Some(format!(
                    "Could not send the request. Open the agent chat to retry: {e}"
                ));
            })?;
            return Err(error(e));
        }
        app.note_user_turn(&session);
    }
    app.plugin_jobs.get(id)
}

#[tauri::command]
pub async fn plugin_drafts(app: AppState<'_>) -> Reply<Vec<JobView>> {
    let app = Arc::clone(&app);
    blocking(move || list(&app)).await
}
#[tauri::command]
pub async fn plugin_create(
    handle: tauri::AppHandle,
    app: AppState<'_>,
    id: String,
    provider: String,
    brief: String,
    folder: String,
) -> Reply<Job> {
    let app = Arc::clone(&app);
    blocking(move || {
        let job = prepare(&app, &id, &provider, &brief, &folder)?;
        let result = open(&handle, &app, &job.id, false);
        if let Err(e) = &result {
            app.plugin_jobs
                .edit(&id, |j| j.problem = Some(e.message.clone()))?;
        }
        let _ = handle.emit("plugins:drafts", ());
        // A failed launch is still a durable draft with an actionable error, not a lost task.
        app.plugin_jobs.get(&id)
    })
    .await
}
#[tauri::command]
pub async fn plugin_open_draft(
    handle: tauri::AppHandle,
    app: AppState<'_>,
    id: String,
    fix: bool,
) -> Reply<Job> {
    let app = Arc::clone(&app);
    blocking(move || {
        let result = open(&handle, &app, &id, fix);
        let _ = handle.emit("plugins:drafts", ());
        result
    })
    .await
}
#[tauri::command]
pub async fn plugin_review_draft(
    handle: tauri::AppHandle,
    app: AppState<'_>,
    id: String,
) -> Reply<wtm_plugins::Review> {
    let app = Arc::clone(&app);
    blocking(move || {
        let _operation = app.plugin_jobs.operations.lock();
        let job = app.plugin_jobs.get(&id)?;
        let live = app.live_agents().into_iter().find(|s| {
            s.provider == job.provider
                && s.scope.worktree_id() == Some(job.folder.as_str())
                && (job.session.as_deref() == Some(s.session.as_str())
                    || (!s.provider_session.is_empty()
                        && job.conversation.as_deref() == Some(s.provider_session.as_str())))
        });
        if live
            .as_ref()
            .and_then(|s| app.overview_of(&s.session))
            .is_some_and(|s| {
                matches!(
                    s.status,
                    crate::app::AgentStatus::Working
                        | crate::app::AgentStatus::NeedsYou
                        | crate::app::AgentStatus::Starting
                )
            })
        {
            return Err(error(
                "Wait for the agent to finish or stop its turn before reviewing files",
            ));
        }
        let result = app.plugins.prepare(&job.folder, "local", "").map_err(error);
        app.plugin_jobs.edit(&id, |j| {
            j.validation_error = result.as_ref().err().map(|e| e.message.clone());
        })?;
        let _ = handle.emit("plugins:drafts", ());
        result
    })
    .await
}
