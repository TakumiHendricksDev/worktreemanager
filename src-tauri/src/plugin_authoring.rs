//! Authoring is an ordinary, visible agent conversation. Only the package manager may activate
//! its output, so an agent finishing a turn never grants trust to the files it wrote.
use crate::{
    agent_bridge::{SpawnedSession, Window},
    app::{AgentSessionFacts, AgentStatus, App},
    commands::{AppState, Branch, Reply, SessionOptions, blocking},
    view::ErrorView,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};
use wtm_core::{
    error::ExecError,
    model::AgentEvent,
    ports::{
        config::ConfigStore,
        exec::{CancelToken, Invocation},
    },
};

/// The event the window listens for to re-read the drafts list.
pub const DRAFTS_EVENT: &str = "plugins:drafts";

/// The layout of `plugin-drafts.json` this build reads and writes.
///
/// A file from a different version is refused rather than reinterpreted or set aside: it is not
/// damaged, and a WTM that understands it may run against the same config directory again.
const DRAFTS_VERSION: u32 = 1;

/// A brief is a prompt, not a document. The limit keeps a pasted log from becoming a draft
/// record that is rewritten on every event the conversation emits.
const MAX_BRIEF_BYTES: usize = 32 * 1024;

/// A folder that holds only this is still empty: Finder writes one into any folder it shows.
const FINDER_METADATA: &str = ".DS_Store";

const ENABLE_CHAT: &str = "Enable WTM Chat in Plugins to create with an agent";
const BUSY: &str = "The agent is busy. Open its chat to reply or stop the current turn first.";
const REVIEW_BUSY: &str = "Wait for the agent to finish or stop its turn before reviewing files";
const DELETE_LIVE: &str =
    "Close this draft's agent chat before removing the draft. Its folder is kept either way.";
const SEND_FAILED: &str = "Could not send the request, so it was not submitted. Opening the \
    draft again sends it:";
const SEND_UNKNOWN: &str = "The request may or may not have reached the agent, so it will not \
    be sent again automatically. Open the agent chat to check:";
const FIX_REQUEST: &str = "Fix this plugin validation error in the existing plugin folder, then \
    validate again. Do not install it.";
const BRIEF_RULES: &str = "Read .git/wtm-plugin-guide.md for the versioned WTM plugin API and \
    working examples. Write wtm-plugin.toml at the workspace root and include all declared files. \
    Use the public contribution contracts; do not modify WTM source, private internals, or the \
    currently installed plugin registry. Explain unsupported capabilities instead of inventing \
    APIs. Validate and test the package, then summarize the files, access required, and what was \
    actually tested. Do not install or enable the plugin: the user will review an immutable \
    snapshot in Settings. Keep using this conversation for refinements and ordinary permission or \
    design approvals.";

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

/// The body of a `plugin-drafts.json` whose [`Header`] matched. Written through `json!` instead,
/// from a borrowed map.
#[derive(Deserialize)]
struct Stored {
    drafts: BTreeMap<String, Job>,
}

/// Just enough of the file to tell a damaged one from one written by another version.
#[derive(Deserialize)]
struct Header {
    version: u32,
}

/// Separate from installed packages: abandoning a draft must not uninstall anything, and
/// recovering a plugin registry must not lose the conversation that can repair a package.
#[derive(Debug)]
pub struct Jobs {
    path: PathBuf,
    records: Mutex<BTreeMap<String, Job>>,
    /// Why the drafts can be neither read nor saved: an unreadable file, or one from another
    /// version. Blocks every operation, because saving would overwrite what could not be read.
    problem: Option<String>,
    /// What loading did that the user should hear about but that blocks nothing: a damaged file
    /// was set aside and the list started empty.
    notice: Option<String>,
    /// Serializes prepare, open, review and delete, which each read a draft, act on the world and
    /// then record the result. Two at once could start two conversations for one draft.
    operations: Mutex<()>,
    /// Whether a failed save from the agent sink has been logged. Once is information; once per
    /// event of every session would bury the log.
    save_warned: AtomicBool,
}
impl Jobs {
    pub fn new(path: PathBuf) -> Self {
        let (records, problem, notice) = load(&path);
        Self {
            path,
            records: Mutex::new(records),
            problem,
            notice,
            operations: Mutex::new(()),
            save_warned: AtomicBool::new(false),
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
    /// What loading the drafts had to do that the user should know about, if anything.
    #[must_use]
    pub fn notice(&self) -> Option<String> {
        self.notice.clone()
    }
    /// Apply `change` to a copy, save the copy, and only then let it replace the records, so a
    /// failed save leaves memory agreeing with the file.
    fn change<T>(&self, change: impl FnOnce(&mut BTreeMap<String, Job>) -> Reply<T>) -> Reply<T> {
        let mut records = self.records.lock();
        let mut next = records.clone();
        let out = change(&mut next)?;
        self.persist(&next)?;
        *records = next;
        Ok(out)
    }
    fn edit(&self, id: &str, change: impl FnOnce(&mut Job)) -> Reply<Job> {
        self.change(|records| {
            let job = records
                .get_mut(id)
                .ok_or_else(|| error("Plugin draft not found"))?;
            change(job);
            Ok(job.clone())
        })
    }
    fn insert(&self, job: Job) -> Reply<Job> {
        self.change(|records| {
            records.insert(job.id.clone(), job.clone());
            Ok(job)
        })
    }
    fn remove(&self, id: &str) -> Reply<Job> {
        self.change(|records| {
            records
                .remove(id)
                .ok_or_else(|| error("Plugin draft not found"))
        })
    }
    fn persist(&self, records: &BTreeMap<String, Job>) -> Reply<()> {
        if let Some(e) = &self.problem {
            return Err(error(e));
        }
        let bytes = serde_json::to_vec_pretty(&serde_json::json!({
            "version": DRAFTS_VERSION,
            "drafts": records,
        }))
        .map_err(error)?;
        let saved = self
            .path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| crate::app::write_private_atomically(&self.path, &bytes));
        saved.map_err(|e| error(format!("Could not save plugin drafts: {e}")))
    }
    /// Keep a draft's record in step with its live conversation.
    ///
    /// Acts on the events that change what the record says — the provider naming its
    /// conversation, a turn starting, a turn failing — and returns `Ok(false)` for everything
    /// else, which includes every streamed token, before taking any lock. `Ok(true)` means the
    /// record changed and the drafts list should be re-read.
    pub fn observe(&self, app: &App, session: &str, event: &AgentEvent) -> Reply<bool> {
        if !matches!(
            event,
            AgentEvent::SessionReady { .. }
                | AgentEvent::TurnStarted { .. }
                | AgentEvent::Failed { .. }
                | AgentEvent::LimitReached { .. }
        ) || self.problem.is_some()
        {
            return Ok(false);
        }
        let Some(facts) = app.live_agents().into_iter().find(|f| f.session == session) else {
            return Ok(false);
        };
        let Some(job) = self.list()?.into_iter().find(|j| belongs(j, &facts)) else {
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
    /// [`Self::observe`] for the agent sink, which has nobody to return an error to. The first
    /// failed save is logged; the rest would only repeat it for every turn of every session.
    pub fn observed(&self, app: &App, session: &str, event: &AgentEvent) -> bool {
        match self.observe(app, session, event) {
            Ok(changed) => changed,
            Err(e) => {
                if !self.save_warned.swap(true, Ordering::Relaxed) {
                    tracing::warn!(error = %e.message, "could not save plugin conversation");
                }
                false
            }
        }
    }
}

/// Read the drafts, setting a damaged file aside rather than letting it block authoring forever.
///
/// Returns the records, a blocking problem, and a notice. Only a file that was read and could
/// not be understood is set aside, the way the plugin registry's recovery does it: a file that
/// could not be *read* is not known to be damaged, and one from another version is not damaged.
fn load(path: &Path) -> (BTreeMap<String, Job>, Option<String>, Option<String>) {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (BTreeMap::new(), None, None),
        Err(e) => {
            let problem = format!("Could not read plugin drafts at {}: {e}", path.display());
            return (BTreeMap::new(), Some(problem), None);
        }
    };
    match serde_json::from_slice::<Header>(&bytes) {
        Ok(header) if header.version != DRAFTS_VERSION => {
            let problem = format!(
                "Plugin drafts at {} were saved by a WTM that writes version {}; this one reads \
                 version {DRAFTS_VERSION}. They are left untouched.",
                path.display(),
                header.version
            );
            return (BTreeMap::new(), Some(problem), None);
        }
        Ok(_) => {
            if let Ok(stored) = serde_json::from_slice::<Stored>(&bytes) {
                return (stored.drafts, None, None);
            }
        }
        Err(_) => {}
    }
    let aside = path.with_file_name(format!(
        "plugin-drafts-damaged-{}.json",
        uuid::Uuid::new_v4()
    ));
    match std::fs::rename(path, &aside) {
        Ok(()) => {
            let notice = format!(
                "Plugin drafts could not be read, so WTM moved them to {} and started a new list. \
                 Draft folders were not changed.",
                aside.display()
            );
            tracing::warn!(%notice);
            (BTreeMap::new(), None, Some(notice))
        }
        Err(e) => {
            let problem = format!(
                "Plugin drafts at {} are damaged and could not be moved aside: {e}",
                path.display()
            );
            (BTreeMap::new(), Some(problem), None)
        }
    }
}

fn error(message: impl std::fmt::Display) -> ErrorView {
    ErrorView::new("plugin", message.to_string())
}

/// Whether a live session is this draft's conversation.
///
/// By process while that process lives, and by the provider's conversation id once it has one,
/// which is how the draft finds its conversation again after a restart resumed it. An empty
/// provider id is "not said yet", never a match, or every session that had not said would match
/// a draft that has not either. Provider and folder must agree too, so a second conversation the
/// user opened in the same folder never takes the draft over.
fn belongs(job: &Job, live: &AgentSessionFacts) -> bool {
    live.provider == job.provider
        && live.scope.worktree_id() == Some(job.folder.as_str())
        && (job.session.as_deref() == Some(live.session.as_str())
            || (!live.provider_session.is_empty()
                && job.conversation.as_deref() == Some(live.provider_session.as_str())))
}

fn live_session(app: &App, job: &Job) -> Option<AgentSessionFacts> {
    app.live_agents().into_iter().find(|s| belongs(job, s))
}

fn busy(app: &App, session: &str, starting: bool) -> bool {
    app.overview_of(session).is_some_and(|s| match s.status {
        AgentStatus::Working | AgentStatus::NeedsYou => true,
        AgentStatus::Starting => starting,
        AgentStatus::Idle | AgentStatus::Failed => false,
    })
}

/// `path` with its longest existing prefix resolved and the part that does not exist yet
/// appended unchanged.
///
/// Resolved before any test that looks at links or locations, so `/tmp/plugin` is judged as the
/// `/private/tmp/plugin` it is, and a link into a project cannot pass for a path outside one.
fn resolve(path: &Path) -> Reply<PathBuf> {
    let mut existing = path;
    let mut missing = Vec::new();
    loop {
        match std::fs::symlink_metadata(existing) {
            Ok(_) => break,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let (Some(name), Some(parent)) = (existing.file_name(), existing.parent()) else {
                    return Err(error("Choose an absolute plugin folder path"));
                };
                missing.push(name);
                existing = parent;
            }
            Err(e) => {
                return Err(error(format!(
                    "Could not inspect {}: {e}",
                    existing.display()
                )));
            }
        }
    }
    let mut resolved = existing
        .canonicalize()
        .map_err(|e| error(format!("Could not resolve {}: {e}", existing.display())))?;
    for name in missing.iter().rev() {
        resolved.push(name);
    }
    Ok(resolved)
}

/// The registered project or worktree that `path` lies inside, if any.
///
/// Every worktree, not only the projects' main checkouts: a worktree usually lives beside its
/// repository rather than inside it, and the docs promise the active repository is not changed.
fn inside_a_project(app: &App, path: &Path) -> Option<PathBuf> {
    let roots = app.config.projects().unwrap_or_default();
    for root in roots {
        let mut places = vec![root.clone()];
        // A repository git cannot list is still a place not to write into; it just has no
        // worktrees anyone can find.
        if let Ok(worktrees) = app.git.list_worktrees(&root) {
            places.extend(worktrees.into_iter().map(|w| w.path));
        }
        for place in places {
            let place = place.canonicalize().unwrap_or(place);
            if path.starts_with(&place) {
                return Some(place);
            }
        }
    }
    None
}

/// What `prepare` made, so a failure part-way can take back exactly that.
#[derive(Default)]
struct Made {
    /// The topmost directory created for the folder, if any had to be.
    created: Option<PathBuf>,
    folder: PathBuf,
    git: bool,
    registered: Option<PathBuf>,
}
impl Made {
    /// Undo, newest first. Only ever removes what this request created: `.git` was absent before
    /// (the folder was new or empty), and directories go with `remove_dir`, which refuses to
    /// delete anything that has gained content since.
    fn undo(&self, app: &App) {
        if let Some(root) = &self.registered
            && let Err(e) = app.config.unregister_project(root)
        {
            tracing::warn!(error = %e, "could not unregister a failed draft's folder");
        }
        if self.git {
            let _ = std::fs::remove_dir_all(self.folder.join(".git"));
        }
        if let Some(top) = &self.created {
            for dir in self.folder.ancestors() {
                if std::fs::remove_dir(dir).is_err() || dir == top {
                    break;
                }
            }
        }
    }
}

/// Only new or empty directories qualify. In particular, a picker cannot turn the active
/// repository into a plugin workspace or overwrite a previously generated draft.
///
/// Every refusal says which rule it hit, because the dialog shows it under the folder field and a
/// shared sentence could not tell the user what to change.
pub fn prepare(app: &App, id: &str, provider: &str, brief: &str, folder: &str) -> Reply<Job> {
    let _operation = app.plugin_jobs.operations.lock();
    // The canonical spelling, because the id becomes the record's key: a braced or URN spelling
    // of the same request must find the same draft rather than start a second one.
    let id = uuid::Uuid::parse_str(id)
        .map_err(|_| error("This creation request has no valid id. Close the dialog and retry."))?
        .to_string();
    if !["claude", "codex"].contains(&provider) {
        return Err(error("Choose Claude or Codex to create the plugin"));
    }
    if brief.trim().is_empty() {
        return Err(error("Describe the plugin you want the agent to create"));
    }
    if brief.len() > MAX_BRIEF_BYTES {
        return Err(error("Keep the description under 32 KiB"));
    }
    let requested = crate::app::expand_tilde(Path::new(folder));
    if !requested.is_absolute() {
        return Err(error("Choose an absolute plugin folder path"));
    }
    if requested
        .components()
        .any(|p| matches!(p, Component::ParentDir))
    {
        return Err(error("Choose a plugin folder path without `..`"));
    }
    if let Some(job) = app.plugin_jobs.list()?.into_iter().find(|j| j.id == id) {
        let same_folder = resolve(&requested).is_ok_and(|path| Path::new(&job.folder) == path);
        if job.provider != provider || job.brief != brief || !same_folder {
            return Err(error(
                "This creation request already belongs to another draft",
            ));
        }
        return Ok(job);
    }
    let entry = wtm_agent::entry(provider).ok_or_else(|| error("Unknown provider"))?;
    if app.agent_executable(entry).is_none() {
        return Err(error(format!("{provider} is not installed on wtm's PATH")));
    }
    if !app.plugins.enabled("wtm.chat") {
        return Err(error(ENABLE_CHAT));
    }
    // The folder itself, before resolving: a link the user named would otherwise resolve to its
    // target, and the draft would live somewhere other than the path they chose.
    if std::fs::symlink_metadata(&requested).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(error(
            "Choose the folder itself rather than a symbolic link to it",
        ));
    }
    let path = resolve(&requested)?;
    if let Some(place) = inside_a_project(app, &path) {
        return Err(error(format!(
            "Choose a folder outside your projects and worktrees. {} is open in wtm.",
            place.display()
        )));
    }
    let mut made = Made {
        folder: path.clone(),
        ..Made::default()
    };
    match std::fs::read_dir(&path) {
        Ok(mut entries) => {
            if entries.any(|e| !e.is_ok_and(|e| e.file_name() == FINDER_METADATA)) {
                return Err(error(
                    "Choose a new or empty folder. Existing files will not be overwritten.",
                ));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            made.created = path
                .ancestors()
                .take_while(|dir| !dir.exists())
                .last()
                .map(Path::to_path_buf);
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotADirectory => {
            return Err(error("Choose a folder. The path you chose is a file."));
        }
        Err(e) => return Err(error(format!("Could not read {}: {e}", path.display()))),
    }
    let job = make_workspace(app, &mut made).and_then(|root| {
        app.plugin_jobs.insert(Job {
            id,
            provider: provider.into(),
            brief: brief.into(),
            folder: root.to_string_lossy().into_owned(),
            session: None,
            conversation: None,
            submitted: false,
            problem: None,
            validation_error: None,
        })
    });
    // Taken back on any failure, so retrying the same request on the same folder finds it as
    // empty as it was rather than refusing a folder this request filled.
    if job.is_err() {
        made.undo(app);
    }
    job
}

/// Create the folder, initialise it as a Git workspace with the guide, and register it.
fn make_workspace(app: &App, made: &mut Made) -> Reply<PathBuf> {
    let path = made.folder.clone();
    std::fs::create_dir_all(&path)
        .map_err(|e| error(format!("Could not create {}: {e}", path.display())))?;
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
    made.git = true;
    let result = app
        .runner
        .run(&invocation, &CancelToken::new())
        .map_err(error)?;
    if !result.is_success() {
        return Err(error(result.stderr));
    }
    write_guide(&path.join(".git")).map_err(error)?;
    let root = app.register(&path).map_err(error)?;
    made.registered = Some(root.clone());
    Ok(root)
}

/// Where the bundled examples go, beside the guide, so its relative links resolve there.
pub const GUIDE_EXAMPLES: &str = "wtm-plugin-examples";

/// Where `docs/plugins.md` stops being about writing a plugin and starts being about working on
/// wtm's own source. Nothing from here on is in the bundled guide: its commands and paths exist
/// only in a wtm checkout, and an agent told to run them in a plugin workspace would fail or guess.
const REPO_ONLY: &str = "\n## Developing wtm itself";

/// Every file under `examples/plugins/`. `src-tauri/tests/plugin_guide.rs` fails when an example
/// gains a file this list lacks, because the guide links to the folders and a missing file would
/// be a dead link.
const EXAMPLES: &[(&str, &str)] = &[
    (
        "project-tools/panel.html",
        include_str!("../../examples/plugins/project-tools/panel.html"),
    ),
    (
        "project-tools/summary.html",
        include_str!("../../examples/plugins/project-tools/summary.html"),
    ),
    (
        "project-tools/wtm-plugin.toml",
        include_str!("../../examples/plugins/project-tools/wtm-plugin.toml"),
    ),
    (
        "session-demo/backend.py",
        include_str!("../../examples/plugins/session-demo/backend.py"),
    ),
    (
        "session-demo/workbench.html",
        include_str!("../../examples/plugins/session-demo/workbench.html"),
    ),
    (
        "session-demo/wtm-plugin.toml",
        include_str!("../../examples/plugins/session-demo/wtm-plugin.toml"),
    ),
    (
        "workbench-toolkit/README.md",
        include_str!("../../examples/plugins/workbench-toolkit/README.md"),
    ),
    (
        "workbench-toolkit/runtime.py",
        include_str!("../../examples/plugins/workbench-toolkit/runtime.py"),
    ),
    (
        "workbench-toolkit/wtm-plugin.toml",
        include_str!("../../examples/plugins/workbench-toolkit/wtm-plugin.toml"),
    ),
];

/// Write the API guide, the contracts and the example packages into `dir`, the workspace's `.git`.
///
/// Under `.git` because reviewed packages omit Git metadata, so none of it can end up in the
/// package the agent writes. Bundled into the app rather than fetched or read from a source
/// checkout, which a user's machine does not have.
fn write_guide(dir: &Path) -> std::io::Result<()> {
    std::fs::write(dir.join("wtm-plugin-guide.md"), authoring_guide())?;
    std::fs::write(
        dir.join("wtm-plugin-catalog.json"),
        wtm_plugins::catalog::SOURCE,
    )?;
    std::fs::write(
        dir.join("wtm-plugin-operations.json"),
        include_str!("../../crates/wtm-plugins/contracts/operations.json"),
    )?;
    for (name, text) in EXAMPLES {
        let file = dir.join(GUIDE_EXAMPLES).join(name);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(file, text)?;
    }
    Ok(())
}

/// `docs/plugins.md` as the authoring workspace reads it: without the part about wtm's own
/// source, and with its example links pointing at the copies [`write_guide`] puts beside it.
fn authoring_guide() -> String {
    let docs = include_str!("../../docs/plugins.md");
    let guide = docs.split(REPO_ONLY).next().unwrap_or(docs);
    guide.replace("](../examples/plugins/", &format!("]({GUIDE_EXAMPLES}/"))
}
fn prompt(job: &Job) -> String {
    format!(
        "Create a WTM plugin for this request:\n\n{}\n\nWork only in this plugin workspace: {}. \
         {BRIEF_RULES}",
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
            if let Some(live) = identities.iter().find(|s| belongs(&job, s)) {
                job.session = Some(live.session.clone());
            }
            let session = active
                .iter()
                .find(|s| s.session == job.session.as_deref().unwrap_or_default());
            let status = if let Some(s) = session {
                match s.status {
                    AgentStatus::Starting => "starting",
                    AgentStatus::Working => "building",
                    AgentStatus::NeedsYou => "waiting",
                    AgentStatus::Failed => "error",
                    AgentStatus::Idle => {
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

/// Return to a draft's conversation, starting or resuming it if it is not running, and send the
/// brief if it has never been sent, or a fix request when asked for one.
///
/// # Errors
///
/// An unknown draft, WTM Chat disabled while the conversation must be started, a session that
/// cannot open, a busy agent, or a send that failed.
pub fn open(window: &dyn Window, app: &Arc<App>, id: &str, fix: bool) -> Reply<Job> {
    let _operation = app.plugin_jobs.operations.lock();
    let job = app.plugin_jobs.get(id)?;
    let session = if let Some(live) = live_session(app, &job) {
        live.session
    } else {
        // Asked only when a conversation must be started: returning to one already on screen is
        // not opening a WTM Chat pane.
        if !app.plugins.enabled("wtm.chat") {
            return Err(error(ENABLE_CHAT));
        }
        let options = SessionOptions {
            resume: job.conversation.clone(),
            ..SessionOptions::default()
        };
        let session = crate::commands::open_agent_branch_process(
            window,
            app,
            &job.folder,
            &job.folder,
            &job.provider,
            options,
            Branch::default(),
        )?;
        // The brief is the user's own words, so the pane is one the user has written to even
        // before anything is sent. Home must not treat it as a pane of its own to close.
        app.note_user_turn(&session);
        app.plugin_jobs
            .edit(id, |j| j.session = Some(session.clone()))?;
        // A provider can announce its identity inside open(), before the job knows the process id.
        for event in app.agent_replay(&session) {
            app.plugin_jobs.observe(app, &session, &event.event)?;
        }
        session
    };
    let job = app
        .plugin_jobs
        .edit(id, |j| j.session = Some(session.clone()))?;
    let project = app.project(&job.folder).map_err(error)?;
    let spec = project.agent_spec(&job.provider);
    window.announce(&SpawnedSession {
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
    });
    if !job.submitted {
        submit(app, id, &session, &prompt(&job), true)?;
    } else if fix && let Some(problem) = &job.validation_error {
        submit(
            app,
            id,
            &session,
            &format!("{FIX_REQUEST}\n\n{problem}"),
            false,
        )?;
    }
    app.plugin_jobs.get(id)
}

/// Send `text` to the draft's conversation. `brief` says whether it is the draft's first request,
/// whose `submitted` flag this keeps, or a later one, which leaves the flag alone.
fn submit(app: &App, id: &str, session: &str, text: &str, brief: bool) -> Reply<()> {
    if busy(app, session, false) {
        return Err(error(BUSY));
    }
    // Persisted before sending: after a crash an uncertain prompt must never be replayed
    // automatically. The visible conversation is where the user can choose to retry it.
    app.plugin_jobs.edit(id, |j| {
        j.submitted |= brief;
        j.problem = None;
    })?;
    let Err(e) = app.send_agent_turn(session, text, &[]) else {
        app.note_user_turn(session);
        return Ok(());
    };
    // Only a failure that proves nothing reached the provider gives the brief back to the next
    // open. Anything less certain keeps it submitted, so it can never be sent twice.
    let unsent = never_sent(&e);
    app.plugin_jobs.edit(id, |j| {
        j.problem = Some(format!(
            "{} {e}",
            if unsent { SEND_FAILED } else { SEND_UNKNOWN }
        ));
        if brief && unsent {
            j.submitted = false;
        }
    })?;
    Err(error(e))
}

/// Whether a failed send certainly delivered nothing.
///
/// `NoSuchSession` is raised before any byte is written. `Spawn` is how the pipe host reports a
/// write that failed, and a pipe write fails only when nobody is reading the other end, so no
/// complete frame can have been taken up. A timeout, or any error added later, is not known to be
/// either, and is treated as "may have arrived".
fn never_sent(error: &ExecError) -> bool {
    matches!(error, ExecError::NoSuchSession(_) | ExecError::Spawn { .. })
}

/// Validate a draft with the normal package manager, refusing while its agent is still writing.
///
/// # Errors
///
/// An unknown draft, a busy agent, or the package manager's own validation error, which is also
/// recorded on the draft so **Ask agent to fix** can send it.
pub fn review(app: &App, id: &str) -> Reply<wtm_plugins::Review> {
    let _operation = app.plugin_jobs.operations.lock();
    let job = app.plugin_jobs.get(id)?;
    if live_session(app, &job).is_some_and(|s| busy(app, &s.session, true)) {
        return Err(error(REVIEW_BUSY));
    }
    let result = app.plugins.prepare(&job.folder, "local", "").map_err(error);
    app.plugin_jobs.edit(id, |j| {
        j.validation_error = result.as_ref().err().map(|e| e.message.clone());
    })?;
    result
}

/// Forget a draft. Its folder, its Git history and its registration in the sidebar all stay:
/// they are the user's files, and the draft record was only ever a pointer to them.
///
/// # Errors
///
/// An unknown draft, a draft whose conversation is still running, or a failed save.
pub fn delete(app: &App, id: &str) -> Reply<()> {
    let _operation = app.plugin_jobs.operations.lock();
    let job = app.plugin_jobs.get(id)?;
    // A running conversation would otherwise lose the record that names it, and the next event it
    // emitted could match nothing, so its pane would be the only trace that a draft existed.
    if live_session(app, &job).is_some() {
        return Err(error(DELETE_LIVE));
    }
    app.plugin_jobs.remove(id)?;
    Ok(())
}

#[tauri::command]
pub async fn plugin_drafts(app: AppState<'_>) -> Reply<Vec<JobView>> {
    let app = Arc::clone(&app);
    blocking(move || list(&app)).await
}
/// What loading the drafts had to do that the user should hear about: `null` almost always.
#[tauri::command]
pub async fn plugin_drafts_notice(app: AppState<'_>) -> Reply<Option<String>> {
    Ok(app.plugin_jobs.notice())
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
                .edit(&job.id, |j| j.problem = Some(e.message.clone()))?;
        }
        handle.changed(DRAFTS_EVENT);
        // A failed launch is still a durable draft with an actionable error, not a lost task.
        app.plugin_jobs.get(&job.id)
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
        handle.changed(DRAFTS_EVENT);
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
        let result = review(&app, &id);
        handle.changed(DRAFTS_EVENT);
        result
    })
    .await
}
#[tauri::command]
pub async fn plugin_delete_draft(
    handle: tauri::AppHandle,
    app: AppState<'_>,
    id: String,
) -> Reply<()> {
    let app = Arc::clone(&app);
    blocking(move || {
        delete(&app, &id)?;
        handle.changed(DRAFTS_EVENT);
        Ok(())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft(app: &App) -> Job {
        app.plugin_jobs
            .insert(Job {
                id: uuid::Uuid::new_v4().to_string(),
                provider: "codex".into(),
                brief: "Build a panel".into(),
                folder: "/nowhere".into(),
                session: None,
                conversation: None,
                submitted: false,
                problem: None,
                validation_error: None,
            })
            .unwrap()
    }

    #[test]
    fn a_brief_that_never_reached_the_provider_is_sent_again_by_the_next_open() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::with_paths(wtm_config::AppPaths::rooted(dir.path())).unwrap();
        let job = draft(&app);

        // No such process: the send fails before a byte is written.
        assert!(submit(&app, &job.id, "gone", "brief", true).is_err());

        let saved = app.plugin_jobs.get(&job.id).unwrap();
        assert!(
            !saved.submitted,
            "a definite failure must give the brief back"
        );
        assert!(saved.problem.unwrap().starts_with(SEND_FAILED));
    }

    #[test]
    fn a_failed_fix_request_never_marks_the_brief_as_unsent() {
        let dir = tempfile::tempdir().unwrap();
        let app = App::with_paths(wtm_config::AppPaths::rooted(dir.path())).unwrap();
        let job = draft(&app);
        app.plugin_jobs
            .edit(&job.id, |j| j.submitted = true)
            .unwrap();

        assert!(submit(&app, &job.id, "gone", "fix it", false).is_err());

        // Resetting here would make the next open send the original brief a second time.
        assert!(app.plugin_jobs.get(&job.id).unwrap().submitted);
    }
}
