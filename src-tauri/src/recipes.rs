//! Recipes: storing them for Settings, and running one in a worktree a step at a time.
//!
//! The shape and its rules are in [`wtm_core::model::recipe`]; this is the half that touches the
//! world. A run opens each step as an ordinary agent pane — tiled in the worktree, titled after the
//! step, with its transcript, approvals and model pill — sends the step's text, and waits for that
//! turn to end before the next step starts. Nothing about a step's pane is special once it is open:
//! someone can type into it, answer its approvals, or close it.
//!
//! # Why steps open like a hand-opened pane and not like a delegation
//!
//! [`crate::handoff::open_pane`] exists for a *model* asking for a session, and caps the mode it may
//! hand out at the asking session's own. A recipe step's mode was chosen by the person in Settings,
//! exactly as a pane's picker is, so it opens through the same request a pane does
//! ([`crate::commands::session_request_for`]) and keeps the repository's own refusals — an agent the
//! `wtm.toml` does not offer is still refused — without the ceiling meant for models.
//!
//! # One run per worktree
//!
//! Two recipes running at once in one checkout would be two writers in the same files, which is the
//! conflict every other orchestration path here avoids too. A second start in a worktree whose run
//! is still going is refused, and says which run is in the way.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use wtm_core::model::recipe::{
    self, Conversation, Recipe, RecipeStep, StepFailure, StepRun, compose,
};
use wtm_core::model::{AgentSkill, Project, Worktree};

use crate::app::{App, SessionScope};
use crate::commands::{AppState, Reply, SessionOptions, blocking};
use crate::view::ErrorView;

/// Emitted with a [`RunView`] whenever a run or one of its steps changes.
pub const RUN_EVENT: &str = "recipe:run";

/// What every recipe run id starts with, so the window can tell a step's pane from a delegation's.
pub const RUN_PREFIX: &str = "recipe-";

/// How long one step may take before the run gives up on it, in milliseconds.
///
/// Three hours, far past [`crate::turns::TURN_TIMEOUT_MS`]'s ten minutes. That one bounds a model
/// waiting on another model's answer; this bounds an implementation step that may build, run a test
/// suite and stop for an approval nobody is looking at. Stop is how a person ends a run early; this
/// only keeps a session that died without saying so from holding a run open forever.
pub const STEP_TIMEOUT_MS: u64 = 3 * 60 * 60 * 1000;

/// Variables the window may supply. Everything else is the backend's to fill, so a caller cannot
/// tell a step it is in a different worktree from the one it runs in.
const SUPPLIED: [&str; 2] = ["form.", "action."];

// ─────────────────────────────── views ───────────────────────────────

/// Whether a step runs a skill or a prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    Skill,
    Prompt,
}

/// One step as the window edits it: both texts kept, whichever `kind` is chosen.
///
/// Both, because switching "A skill" to "A prompt" and back in the editor should not throw away
/// what was typed in the other. Only the chosen one is stored.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepView {
    pub name: String,
    pub agent: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub mode: Option<String>,
    pub kind: StepKind,
    #[serde(default)]
    pub skill: String,
    #[serde(default)]
    pub args: String,
    #[serde(default)]
    pub prompt: String,
    pub conversation: Conversation,
    pub previous_reply: bool,
    pub on_failure: StepFailure,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecipeView {
    /// Empty for a recipe not saved yet; the save assigns one from its name.
    pub id: String,
    pub name: String,
    pub description: String,
    pub repositories: Vec<String>,
    pub steps: Vec<StepView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProblemView {
    pub id: String,
    pub message: String,
}

/// Everything Settings shows: the recipes, the ones that could not be read, and every binding.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecipesView {
    pub recipes: Vec<RecipeView>,
    pub problems: Vec<ProblemView>,
    pub offers: BTreeMap<String, Vec<String>>,
}

/// A save's answer: the id it was stored under, and the whole list as it now stands.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedRecipe {
    pub id: String,
    pub all: RecipesView,
}

/// One step's text as it would be sent now, for the question's "what it will send".
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewStep {
    pub text: String,
    /// Whether the previous step's reply is appended below this text when it runs.
    pub gets_previous: bool,
    pub missing: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    Done,
    Failed,
    Stopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    Running,
    Done,
    Failed,
    Skipped,
    Stopped,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunStepView {
    pub name: String,
    pub agent: String,
    pub model: Option<String>,
    pub mode: Option<String>,
    pub status: StepStatus,
    /// The step's live session, once it has one. A continuing step shares its predecessor's.
    pub session: Option<String>,
    pub error: Option<String>,
    pub started_ms: Option<u64>,
    pub ended_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunView {
    pub id: String,
    pub recipe_id: String,
    pub recipe_name: String,
    pub project_id: String,
    pub worktree_id: String,
    /// The action that offered it, or `None` for a run started from a menu.
    pub action: Option<String>,
    pub status: RunStatus,
    pub started_ms: u64,
    pub ended_ms: Option<u64>,
    /// Variables a step used that this run had no value for. See [`recipe::Filled::missing`].
    pub missing: Vec<String>,
    pub steps: Vec<RunStepView>,
}

impl From<&RecipeStep> for StepView {
    fn from(step: &RecipeStep) -> Self {
        let (kind, skill, args, prompt) = match &step.run {
            StepRun::Skill { skill, args } => {
                (StepKind::Skill, skill.clone(), args.clone(), String::new())
            }
            StepRun::Prompt { prompt } => (
                StepKind::Prompt,
                String::new(),
                String::new(),
                prompt.clone(),
            ),
        };
        Self {
            name: step.name.clone(),
            agent: step.agent.clone(),
            model: step.model.clone(),
            effort: step.effort.clone(),
            mode: step.mode.clone(),
            kind,
            skill,
            args,
            prompt,
            conversation: step.conversation,
            previous_reply: step.previous_reply,
            on_failure: step.on_failure,
        }
    }
}

impl From<StepView> for RecipeStep {
    fn from(view: StepView) -> Self {
        let blank = |value: Option<String>| value.filter(|v| !v.trim().is_empty());
        let run = match view.kind {
            StepKind::Skill => StepRun::Skill {
                skill: recipe::skill_name(&view.skill).to_owned(),
                args: view.args.trim().to_owned(),
            },
            StepKind::Prompt => StepRun::Prompt {
                prompt: view.prompt.trim().to_owned(),
            },
        };
        Self {
            name: view.name.trim().to_owned(),
            agent: view.agent.trim().to_owned(),
            model: blank(view.model),
            effort: blank(view.effort),
            mode: blank(view.mode),
            run,
            conversation: view.conversation,
            previous_reply: view.previous_reply,
            on_failure: view.on_failure,
        }
    }
}

impl From<&Recipe> for RecipeView {
    fn from(recipe: &Recipe) -> Self {
        Self {
            id: recipe.id.clone(),
            name: recipe.name.clone(),
            description: recipe.description.clone(),
            repositories: recipe.repositories.clone(),
            steps: recipe.steps.iter().map(StepView::from).collect(),
        }
    }
}

impl From<RecipeView> for Recipe {
    fn from(view: RecipeView) -> Self {
        Self {
            id: view.id.trim().to_owned(),
            name: view.name.trim().to_owned(),
            description: view.description.trim().to_owned(),
            repositories: view.repositories,
            steps: view.steps.into_iter().map(RecipeStep::from).collect(),
        }
    }
}

// ─────────────────────────────── the registry ───────────────────────────────

struct Run {
    view: RunView,
    stop: Arc<AtomicBool>,
}

/// Every run since launch that has not been dismissed. Owned by [`App`].
///
/// Not persisted. A run is a thread waiting on live sessions, and after a relaunch there is nothing
/// left to wait on — the panes come back through session restore, as panes.
#[derive(Default)]
pub struct Registry {
    runs: parking_lot::Mutex<Vec<Run>>,
    next: AtomicU64,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry")
            .field("runs", &self.runs.lock().len())
            .finish_non_exhaustive()
    }
}

impl Registry {
    fn next_id(&self, now_ms: u64) -> String {
        let n = self.next.fetch_add(1, Ordering::Relaxed);
        format!("{RUN_PREFIX}{now_ms}-{n}")
    }

    /// Record a new run, unless one is still going in the same worktree.
    fn admit(&self, view: RunView) -> Result<Arc<AtomicBool>, String> {
        let mut runs = self.runs.lock();
        if let Some(busy) = runs.iter().find(|run| {
            run.view.worktree_id == view.worktree_id && run.view.status == RunStatus::Running
        }) {
            return Err(format!(
                "{} is already running in this worktree. Stop it before starting another recipe.",
                busy.view.recipe_name
            ));
        }
        // A finished run in the same worktree is replaced: the strip shows one run, and the
        // panes the old one opened are still there to read.
        runs.retain(|run| run.view.worktree_id != view.worktree_id);
        let stop = Arc::new(AtomicBool::new(false));
        runs.push(Run {
            view,
            stop: Arc::clone(&stop),
        });
        Ok(stop)
    }

    fn update(&self, id: &str, change: impl FnOnce(&mut RunView)) -> Option<RunView> {
        let mut runs = self.runs.lock();
        let run = runs.iter_mut().find(|run| run.view.id == id)?;
        change(&mut run.view);
        Some(run.view.clone())
    }

    #[must_use]
    pub fn list(&self) -> Vec<RunView> {
        self.runs
            .lock()
            .iter()
            .map(|run| run.view.clone())
            .collect()
    }

    /// Ask a run to stop, returning the session its current step is waiting on, if it has one.
    fn stop(&self, id: &str) -> Option<String> {
        let runs = self.runs.lock();
        let run = runs.iter().find(|run| run.view.id == id)?;
        run.stop.store(true, Ordering::Relaxed);
        run.view
            .steps
            .iter()
            .find(|step| step.status == StepStatus::Running)
            .and_then(|step| step.session.clone())
    }

    /// Forget a run that has finished. A running one stays: its thread still writes to it.
    fn dismiss(&self, id: &str) -> bool {
        let mut runs = self.runs.lock();
        let before = runs.len();
        runs.retain(|run| run.view.id != id || run.view.status == RunStatus::Running);
        runs.len() != before
    }
}

fn emit(handle: &AppHandle, view: Option<RunView>) {
    if let Some(view) = view
        && let Err(err) = handle.emit(RUN_EVENT, &view)
    {
        // No window: the run carries on and `recipe_runs` reports it when one comes back.
        tracing::debug!(error = %err, "could not announce a recipe run");
    }
}

// ─────────────────────────────── storage commands ───────────────────────────────

fn all(app: &App) -> Reply<RecipesView> {
    let (recipes, problems) = app.config.recipes()?;
    Ok(RecipesView {
        recipes: recipes.iter().map(RecipeView::from).collect(),
        problems: problems
            .into_iter()
            .map(|p| ProblemView {
                id: p.id,
                message: p.message,
            })
            .collect(),
        offers: app.config.recipe_offers()?,
    })
}

/// Every recipe, every one that could not be read, and what each action offers.
#[tauri::command]
pub async fn list_recipes(app: AppState<'_>) -> Reply<RecipesView> {
    let app = Arc::clone(&app);
    blocking(move || all(&app)).await
}

/// Every reason `recipe` cannot be stored, including agents this build cannot drive.
fn problems_with(recipe: &Recipe) -> Vec<String> {
    let mut problems = recipe.problems();
    for (index, step) in recipe.steps.iter().enumerate() {
        if !step.agent.is_empty() && wtm_agent::entry(&step.agent).is_none() {
            problems.push(format!(
                "Step {}'s agent `{}` is not one this build of wtm can drive.",
                index + 1,
                step.agent
            ));
        }
    }
    problems
}

/// Validate and store a recipe, assigning an id from its name if it has none.
///
/// Refused whole rather than stored with its problems, which is the reason Settings saves recipes
/// with a button instead of on every keystroke: a recipe that exists is one an action can offer, so a
/// half-written one must not exist.
#[tauri::command]
pub async fn save_recipe(app: AppState<'_>, recipe: RecipeView) -> Reply<SavedRecipe> {
    let app = Arc::clone(&app);
    blocking(move || {
        let mut recipe = Recipe::from(recipe);
        let problems = problems_with(&recipe);
        if !problems.is_empty() {
            return Err(ErrorView::new("config", problems.join(" ")));
        }
        if recipe.id.is_empty() {
            let (existing, broken) = app.config.recipes()?;
            let taken: Vec<&str> = existing
                .iter()
                .map(|r| r.id.as_str())
                .chain(broken.iter().map(|p| p.id.as_str()))
                .collect();
            recipe.id = recipe::new_id(&recipe.name, &taken);
        }
        app.config.save_recipe(&recipe)?;
        Ok(SavedRecipe {
            id: recipe.id,
            all: all(&app)?,
        })
    })
    .await
}

#[tauri::command]
pub async fn delete_recipe(app: AppState<'_>, id: String) -> Reply<RecipesView> {
    let app = Arc::clone(&app);
    blocking(move || {
        app.config.delete_recipe(&id)?;
        all(&app)
    })
    .await
}

/// Replace which recipes `action` offers, in order. An empty list turns the action's question off.
#[tauri::command]
pub async fn set_recipe_offers(
    app: AppState<'_>,
    action: String,
    recipes: Vec<String>,
) -> Reply<RecipesView> {
    let app = Arc::clone(&app);
    blocking(move || {
        app.config.set_recipe_offers(&action, &recipes)?;
        all(&app)
    })
    .await
}

/// How long the step editor waits for an agent's CLI to list its own commands, in milliseconds.
///
/// Claude answers `initialize` in a second or two and Codex once its thread is open; twenty seconds
/// covers a cold start with MCP servers to launch. Past it the editor keeps the list it read from
/// disk, which is never worse than not asking.
const PROBE_TIMEOUT_MS: u64 = 20_000;

/// Where the step editor asks about skills: the worktree in view, its repository, or home.
///
/// The worktree first, because that is the answer that matches what a step will see when it runs
/// there. Claude enables plugins per directory — a plugin installed for one checkout through its
/// `.claude/settings.local.json` is absent from the repository's main checkout — so asking from the
/// root would leave out exactly the skills someone installed for the work they are doing.
fn skills_dir(app: &App, project_id: Option<&str>, worktree_id: Option<&str>) -> PathBuf {
    if let Some(project) = project_id.and_then(|id| app.project(id).ok()) {
        if let Some(worktree) = worktree_id.and_then(|id| app.worktree(&project, id).ok()) {
            return worktree.path;
        }
        return project.root;
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// The skills an agent would find on disk, for the step editor's first paint.
///
/// The provider's own seed — the list a fresh pane's `/` menu starts from. It cannot see a plugin's
/// skills, which is what [`probe_agent_skills`] is for; this is what shows while that answer is on
/// its way, and what stays if it never comes.
#[tauri::command]
pub async fn agent_skills(
    app: AppState<'_>,
    agent: String,
    project_id: Option<String>,
    worktree_id: Option<String>,
) -> Reply<Vec<AgentSkill>> {
    let app = Arc::clone(&app);
    blocking(move || {
        let Some(entry) = wtm_agent::entry(&agent) else {
            return Ok(Vec::new());
        };
        let cwd = skills_dir(&app, project_id.as_deref(), worktree_id.as_deref());
        let req = wtm_agent::SessionRequest {
            cwd: cwd.to_string_lossy().into_owned(),
            ..wtm_agent::SessionRequest::default()
        };
        Ok(entry.provider.seed_skills(&req))
    })
    .await
}

/// Collects the skill lists a probe session reports, and notices when its process goes.
struct Listing {
    tx: parking_lot::Mutex<Option<std::sync::mpsc::Sender<Vec<AgentSkill>>>>,
}

impl wtm_agent::session::AgentSink for Listing {
    fn on_event(&self, _session: &wtm_core::model::SessionId, event: &wtm_core::model::AgentEvent) {
        if let wtm_core::model::AgentEvent::SkillsListed { skills } = event
            && let Some(tx) = self.tx.lock().as_ref()
        {
            let _ = tx.send(skills.clone());
        }
    }

    fn on_exit(
        &self,
        _session: &wtm_core::model::SessionId,
        _outcome: &wtm_core::model::ExitOutcome,
    ) {
        // Dropping the sender ends the wait at once, rather than at the deadline.
        self.tx.lock().take();
    }

    fn on_ready(&self, _session: &wtm_core::model::SessionId) {}
}

/// Every command an agent's own CLI reports for a directory — its plugins' skills included — or
/// `None` if it did not answer in time.
///
/// # Asked, not read
///
/// A plugin's skills are in no directory wtm reads, and which plugins are on for which directory is
/// layered across three settings files and a per-checkout install record. The CLI has already worked
/// that out, and answers without a turn and without a model call: Claude to `initialize`, Codex
/// with `skills/list` once its thread is open, Cursor with `available_commands_update`. So this opens
/// a session the way a pane does — the same handshake a pane's `/` menu learns its list from — waits
/// for that answer, and closes it. No prompt is ever sent.
///
/// The session is wtm-agent's own, never registered with the app: no pane, no resume entry, no
/// handoff token and no MCP servers, because none of them has anything to do with a list.
#[tauri::command]
pub async fn probe_agent_skills(
    app: AppState<'_>,
    agent: String,
    project_id: Option<String>,
    worktree_id: Option<String>,
) -> Reply<Option<Vec<AgentSkill>>> {
    let app = Arc::clone(&app);
    blocking(move || {
        let cwd = skills_dir(&app, project_id.as_deref(), worktree_id.as_deref());
        probe_skills(&app, &agent, &cwd).map_err(|message| ErrorView::new("exec", message))
    })
    .await
}

/// [`probe_agent_skills`] for a directory: open, wait for the CLI's own list, close.
///
/// `Ok(None)` for an agent this machine cannot run, or a CLI that did not answer in time.
///
/// # Errors
///
/// When the CLI could not be started at all.
pub fn probe_skills(
    app: &App,
    agent: &str,
    cwd: &std::path::Path,
) -> Result<Option<Vec<AgentSkill>>, String> {
    let Some(entry) = wtm_agent::entry(agent) else {
        return Ok(None);
    };
    let Some(executable) = app.agent_executable(entry) else {
        return Ok(None);
    };
    let req = wtm_agent::SessionRequest {
        cwd: cwd.to_string_lossy().into_owned(),
        executable: Some(executable.to_string_lossy().into_owned()),
        ..wtm_agent::SessionRequest::default()
    };
    // The session reports the disk seed first, as it opens, and the CLI's own answer after it.
    let seeded = !entry.provider.seed_skills(&req).is_empty();

    let (tx, rx) = std::sync::mpsc::channel();
    let sink: Arc<dyn wtm_agent::session::AgentSink> = Arc::new(Listing {
        tx: parking_lot::Mutex::new(Some(tx)),
    });
    let session = wtm_agent::AgentSession::open(
        entry.provider,
        &req,
        Arc::clone(&app.pipe) as Arc<dyn wtm_core::ports::pipe::PipeHost>,
        &sink,
        crate::commands::SHELL_TIMEOUT_MS,
        None,
    )
    .map_err(|e| e.to_string())?;

    let answer = listed(app.clock.as_ref(), &rx, usize::from(seeded));
    if let Err(error) = session.close() {
        tracing::debug!(%error, agent, "could not close a skills probe");
    }
    Ok(answer)
}

/// The list after the first `skip`, or `None` if the session ended or the deadline passed first.
///
/// Polled against the [`Clock`](wtm_core::ports::clock::Clock) port for `turns::wait_until`'s
/// reason: `recv_timeout`'s own deadline would bring the system clock back in.
fn listed(
    clock: &dyn wtm_core::ports::clock::Clock,
    rx: &std::sync::mpsc::Receiver<Vec<AgentSkill>>,
    skip: usize,
) -> Option<Vec<AgentSkill>> {
    let deadline = clock.monotonic_ms() + PROBE_TIMEOUT_MS;
    let mut seen = 0;
    loop {
        match rx.recv_timeout(std::time::Duration::from_millis(100)) {
            Ok(skills) => {
                seen += 1;
                if seen > skip {
                    return Some(skills);
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if clock.monotonic_ms() >= deadline {
                    return None;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return None,
        }
    }
}

// ─────────────────────────────── running ───────────────────────────────

/// What the backend knows about where a run happens, as variables.
fn place_vars(project: &Project, worktree: &Worktree) -> BTreeMap<String, String> {
    let mut vars = worktree_vars(worktree);
    vars.insert("project.name".to_owned(), project.display_name().to_owned());
    vars.insert(
        "project.path".to_owned(),
        project.root.to_string_lossy().into_owned(),
    );
    vars
}

/// The `worktree.*` variables: its directory name, branch, path, and the issue it is for.
fn worktree_vars(worktree: &Worktree) -> BTreeMap<String, String> {
    let mut vars = BTreeMap::new();
    vars.insert("worktree.name".to_owned(), worktree.dirname().to_owned());
    vars.insert(
        "worktree.branch".to_owned(),
        worktree
            .checkout
            .branch()
            .map(|b| b.without_prefix().to_owned())
            .unwrap_or_default(),
    );
    vars.insert(
        "worktree.path".to_owned(),
        worktree.path.to_string_lossy().into_owned(),
    );
    // The issue a worktree is for, read off its branch or directory the way the sidebar reads it —
    // so a recipe run from a worktree's menu, long after the New Worktree form that named the issue
    // has gone, still knows it. Absent rather than empty when there is none, so the run names it as
    // missing instead of sending a prompt about nothing.
    if let Some(issue) = crate::view::extract_issue_key(worktree) {
        vars.insert("worktree.issue".to_owned(), issue);
    }
    vars
}

/// The window's variables, minus any outside `form.` and `action.`, over the place's own.
fn run_vars(
    project: &Project,
    worktree: &Worktree,
    supplied: BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    with_supplied(place_vars(project, worktree), supplied)
}

fn with_supplied(
    mut vars: BTreeMap<String, String>,
    supplied: BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    for (key, value) in supplied {
        if SUPPLIED.iter().any(|prefix| key.starts_with(prefix)) {
            vars.insert(key, value);
        }
    }
    vars
}

/// The recipe, the project and the worktree a run would use, or why it cannot.
fn resolve(
    app: &App,
    recipe_id: &str,
    project_id: &str,
    worktree_id: &str,
) -> Reply<(Recipe, Project, Worktree)> {
    let (recipes, _) = app.config.recipes()?;
    let recipe = recipes
        .into_iter()
        .find(|r| r.id == recipe_id)
        .ok_or_else(|| ErrorView::new("config", "that recipe no longer exists"))?;
    let project = app.project(project_id)?;
    if !recipe.applies_to(project.id.as_str()) {
        return Err(ErrorView::new(
            "config",
            format!("{} is limited to other repositories.", recipe.name),
        ));
    }
    let worktree = app.worktree(&project, worktree_id)?;
    Ok((recipe, project, worktree))
}

/// Each step's text as it would be sent if the recipe started now.
///
/// The first step's previous reply does not exist yet, so `{{previous.reply}}` is not reported
/// missing here; the question marks where a reply will be appended instead.
#[tauri::command]
pub async fn preview_recipe(
    app: AppState<'_>,
    recipe_id: String,
    project_id: String,
    worktree_id: String,
    vars: BTreeMap<String, String>,
) -> Reply<Vec<PreviewStep>> {
    let app = Arc::clone(&app);
    blocking(move || {
        let (recipe, project, worktree) = resolve(&app, &recipe_id, &project_id, &worktree_id)?;
        let vars = run_vars(&project, &worktree, vars);
        Ok(recipe
            .steps
            .iter()
            .enumerate()
            .map(|(index, step)| {
                let mut filled = compose(step, &vars, None);
                filled.missing.retain(|m| m != recipe::PREVIOUS_REPLY);
                PreviewStep {
                    text: filled.text,
                    gets_previous: index > 0
                        && step.previous_reply
                        && step.conversation == Conversation::New,
                    missing: filled.missing,
                }
            })
            .collect())
    })
    .await
}

/// Start a recipe in a worktree, and return the run as it begins.
///
/// Returns once the run is recorded, not when it ends: the steps run on their own thread, and the
/// window follows them through [`RUN_EVENT`].
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn start_recipe(
    handle: AppHandle,
    app: AppState<'_>,
    recipe_id: String,
    project_id: String,
    worktree_id: String,
    action: Option<String>,
    vars: BTreeMap<String, String>,
) -> Reply<RunView> {
    let app = Arc::clone(&app);
    blocking(move || {
        let (recipe, project, worktree) = resolve(&app, &recipe_id, &project_id, &worktree_id)?;
        // Checked again here, not only at save: the file is hand-editable between the two.
        let problems = problems_with(&recipe);
        if !problems.is_empty() {
            return Err(ErrorView::new("config", problems.join(" ")));
        }
        let vars = run_vars(&project, &worktree, vars);
        let now = app.clock.now_unix_ms();
        let view = begin(
            &recipe,
            app.recipes.next_id(now),
            project.id.as_str(),
            worktree.id.as_str(),
            action,
            now,
        );
        let stop = app
            .recipes
            .admit(view.clone())
            .map_err(|message| ErrorView::new("exec", message))?;
        emit(&handle, Some(view.clone()));

        let drive = Drive {
            host: Live {
                handle,
                app: Arc::clone(&app),
                project,
                worktree,
            },
            run: view.id.clone(),
            recipe,
            vars,
            stop,
        };
        std::thread::Builder::new()
            .name(format!("wtm-recipe-{}", view.id))
            .spawn(move || drive.run())
            .map_err(|e| ErrorView::new("exec", format!("could not start the recipe: {e}")))?;
        Ok(view)
    })
    .await
}

/// A run's record as it starts: every step pending.
fn begin(
    recipe: &Recipe,
    id: String,
    project_id: &str,
    worktree_id: &str,
    action: Option<String>,
    now: u64,
) -> RunView {
    RunView {
        id,
        recipe_id: recipe.id.clone(),
        recipe_name: recipe.name.clone(),
        project_id: project_id.to_owned(),
        worktree_id: worktree_id.to_owned(),
        action,
        status: RunStatus::Running,
        started_ms: now,
        ended_ms: None,
        missing: Vec::new(),
        steps: recipe
            .steps
            .iter()
            .map(|step| RunStepView {
                name: step.name.clone(),
                agent: step.agent.clone(),
                model: step.model.clone(),
                mode: step.mode.clone(),
                status: StepStatus::Pending,
                session: None,
                error: None,
                started_ms: None,
                ended_ms: None,
            })
            .collect(),
    }
}

/// Stop a run: the current step's turn is interrupted and the steps after it are skipped.
///
/// The panes stay. What a stopped step had done so far is in its transcript, and closing panes is
/// the person's call, as it is for any pane.
#[tauri::command]
pub async fn stop_recipe(app: AppState<'_>, run: String) -> Reply<()> {
    let app = Arc::clone(&app);
    blocking(move || {
        if let Some(session) = app.recipes.stop(&run) {
            // The run's own thread notices the flag too; interrupting here is what makes the turn
            // end now rather than after whatever tool call it is in the middle of.
            if let Err(error) = app.with_agent(&session, wtm_agent::AgentSession::interrupt) {
                tracing::debug!(%error, "could not interrupt a stopped recipe step");
            }
        }
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn recipe_runs(app: AppState<'_>) -> Reply<Vec<RunView>> {
    Ok(app.recipes.list())
}

/// Forget a finished run, which takes its strip off the worktree.
#[tauri::command]
pub async fn dismiss_recipe_run(app: AppState<'_>, run: String) -> Reply<()> {
    app.recipes.dismiss(&run);
    Ok(())
}

/// What a run needs from the world: a clock, somewhere to record its progress, and sessions.
///
/// A trait so the order a run does things in — which step opens a session, which continues one,
/// what each is sent, what a failure or a Stop does to the rest — can be tested with a scripted
/// stand-in, without a window or an agent CLI. [`Live`] is the real one.
trait Host {
    fn now(&self) -> u64;
    /// Apply a change to the run's record and announce it.
    fn record(&self, run: &str, change: &mut dyn FnMut(&mut RunView));
    /// Open a step's own session, titled as given. Its id, or why it could not open.
    fn open(&self, step: &RecipeStep, title: &str, run: &str) -> Result<String, String>;
    /// Whether a session is still there to continue.
    fn alive(&self, session: &str) -> bool;
    /// Send `text` and wait for the turn it starts to end, or for `stop`.
    fn turn(&self, session: &str, text: &str, stop: &AtomicBool) -> Ended;
}

/// One run's thread: the recipe as it was when it started, and where it runs.
///
/// The recipe is a copy taken at start, so an edit saved in Settings while a run is going changes the
/// next run and not this one — the steps already sent were sent from this copy, and the rest should
/// agree with them.
struct Drive<H> {
    host: H,
    run: String,
    recipe: Recipe,
    vars: BTreeMap<String, String>,
    stop: Arc<AtomicBool>,
}

/// How one step ended.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Ended {
    Done(String),
    Failed(String),
    Stopped,
}

impl<H: Host> Drive<H> {
    fn update(&self, mut change: impl FnMut(&mut RunView)) {
        self.host.record(&self.run, &mut change);
    }

    fn step(&self, index: usize, mut change: impl FnMut(&mut RunStepView)) {
        self.update(|run| {
            if let Some(step) = run.steps.get_mut(index) {
                change(step);
            }
        });
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    fn run(self) {
        let mut previous: Option<String> = None;
        let mut sessions: Vec<Option<String>> = Vec::new();
        let mut halted = false;

        for (index, step) in self.recipe.steps.iter().enumerate() {
            if halted || self.stopped() {
                self.step(index, |s| s.status = StepStatus::Skipped);
                sessions.push(None);
                continue;
            }

            let started = self.host.now();
            let filled = compose(step, &self.vars, previous.as_deref());
            self.update(|run| {
                for missing in &filled.missing {
                    if !run.missing.contains(missing) {
                        run.missing.push(missing.clone());
                    }
                }
                if let Some(s) = run.steps.get_mut(index) {
                    s.status = StepStatus::Running;
                    s.started_ms = Some(started);
                }
            });

            let session = match step.conversation {
                // The previous step's own session, still open. Never a fresh one in its place: the
                // step was written to carry on a conversation, and a new session would answer it
                // without any of what came before.
                Conversation::Continue => index
                    .checked_sub(1)
                    .and_then(|i| sessions.get(i).cloned().flatten())
                    .filter(|s| self.host.alive(s))
                    .ok_or_else(|| {
                        format!(
                            "step {index}'s conversation is closed, so there is nothing to continue"
                        )
                    }),
                Conversation::New => {
                    let title = format!("{} · {}", step.name, self.recipe.name);
                    self.host.open(step, &title, &self.run)
                }
            };
            sessions.push(session.as_ref().ok().cloned());
            if let Ok(id) = &session {
                self.step(index, |s| s.session = Some(id.clone()));
            }

            let ended = match session {
                Err(error) => Ended::Failed(error),
                Ok(session) => self.host.turn(&session, &filled.text, &self.stop),
            };
            let finished = self.host.now();
            match ended {
                Ended::Done(reply) => {
                    previous = Some(reply);
                    self.step(index, |s| {
                        s.status = StepStatus::Done;
                        s.ended_ms = Some(finished);
                    });
                }
                Ended::Stopped => {
                    self.step(index, |s| {
                        s.status = StepStatus::Stopped;
                        s.ended_ms = Some(finished);
                    });
                }
                Ended::Failed(error) => {
                    tracing::info!(run = %self.run, step = index + 1, %error, "a recipe step failed");
                    // A step that carries on after a failure gets no reply to build on, rather than
                    // the reply from two steps back presented as the last one.
                    previous = None;
                    halted = step.on_failure == StepFailure::Stop;
                    self.step(index, |s| {
                        s.status = StepStatus::Failed;
                        s.error = Some(error.clone());
                        s.ended_ms = Some(finished);
                    });
                }
            }
        }

        let ended = self.host.now();
        let stopped = self.stopped();
        self.update(|run| {
            run.ended_ms = Some(ended);
            run.status = if stopped {
                RunStatus::Stopped
            } else if run.steps.iter().any(|s| s.status == StepStatus::Failed) {
                RunStatus::Failed
            } else {
                RunStatus::Done
            };
        });
    }
}

/// The real [`Host`]: the app's sessions, opened in the run's worktree, announced to the window.
struct Live {
    handle: AppHandle,
    app: Arc<App>,
    project: Project,
    worktree: Worktree,
}

impl Host for Live {
    fn now(&self) -> u64 {
        self.app.clock.now_unix_ms()
    }

    fn record(&self, run: &str, change: &mut dyn FnMut(&mut RunView)) {
        emit(&self.handle, self.app.recipes.update(run, change));
    }

    fn alive(&self, session: &str) -> bool {
        self.app.overview_of(session).is_some()
    }

    /// Open a step's own session as a tiled pane, the way a pane's picker opens one.
    fn open(&self, step: &RecipeStep, title: &str, run: &str) -> Result<String, String> {
        let (app, project, worktree) = (&self.app, &self.project, &self.worktree);
        if !app.files.exists(&worktree.path) {
            return Err(format!(
                "`{}` no longer exists on disk — the worktree may need pruning",
                worktree.path.display()
            ));
        }
        let entry = wtm_agent::entry(&step.agent).ok_or_else(|| {
            format!(
                "`{}` is not an agent this build of wtm knows how to drive",
                step.agent
            )
        })?;
        // The repository's refusal, kept for the reason `open_agent_process` keeps it: a refusal only
        // the launcher enforces is not a refusal.
        if !project.offers_agent(&step.agent) {
            return Err(format!(
                "this repository's `wtm.toml` does not offer `{}`",
                step.agent
            ));
        }
        let spec = project.agent_spec(&step.agent);
        let req = crate::commands::session_request_for(
            app,
            project,
            &spec,
            entry,
            worktree,
            Some(SessionOptions {
                model: step.model.clone(),
                effort: step.effort.clone(),
                mode: step.mode.clone(),
                ..SessionOptions::default()
            }),
            None,
        )
        .map_err(|e| e.message)?;

        let sink: Arc<dyn wtm_agent::session::AgentSink> =
            crate::agent_bridge::AgentEventSink::new(self.handle.clone());
        let scope = SessionScope::worktree(project.id.as_str(), worktree.id.as_str());
        let session = app.open_agent(entry, &req, scope, &sink).map_err(|error| {
            if let Some(token) = req
                .mcp
                .get(crate::handoff::SERVER_NAME)
                .and_then(|server| server.env.get(crate::handoff::TOKEN_ENV))
            {
                app.handoff.forget_unbound(token);
            }
            format!("could not start a {} session: {error}", entry.label)
        })?;

        // Named before the first turn, so the resume list says the step rather than its prompt.
        app.title_live_session(session.as_str(), title);
        crate::agent_bridge::announce_spawn(
            &self.handle,
            &crate::agent_bridge::SpawnedSession {
                session: session.as_str().to_owned(),
                project: project.id.as_str().to_owned(),
                worktree: worktree.id.as_str().to_owned(),
                provider: step.agent.clone(),
                model: req.model.clone(),
                effort: req.effort.clone(),
                mode: req.mode.clone(),
                parent_session: None,
                run: Some(run.to_owned()),
                title: Some(title.to_owned()),
                opened_by: None,
            },
        );
        Ok(session.as_str().to_owned())
    }

    /// Send a step's text and wait for the turn it starts to end.
    ///
    /// Through the turn watch directly rather than `turns::send`, which records an exchange in the
    /// message log — that log draws Home's wires between sessions, and a recipe step has no sending
    /// session to draw one from.
    fn turn(&self, session: &str, text: &str, stop: &AtomicBool) -> Ended {
        use crate::turns::{Outcome, Wait};

        let waiter = match self.app.turns.watch(session, None) {
            Ok(waiter) => waiter,
            Err(error) => return Ended::Failed(error),
        };
        if let Err(error) = self.app.send_agent_turn(session, text, &[]) {
            self.app.turns.cancel(&waiter);
            return Ended::Failed(format!("the session would not take the step: {error}"));
        }
        match crate::turns::wait_until(self.app.clock.as_ref(), &waiter, STEP_TIMEOUT_MS, || {
            stop.load(Ordering::Relaxed)
        }) {
            Wait::Ended(Outcome::Finished, reply) => Ended::Done(reply),
            Wait::Ended(Outcome::Failed(message), _) => Ended::Failed(message),
            Wait::Ended(Outcome::Gone(summary), _) => {
                Ended::Failed(crate::turns::ended_before_answering(&summary))
            }
            Wait::Stopped => {
                if let Err(error) = self
                    .app
                    .with_agent(session, wtm_agent::AgentSession::interrupt)
                {
                    tracing::debug!(%error, "could not interrupt a stopped recipe step");
                }
                Ended::Stopped
            }
            Wait::TimedOut => Ended::Failed(
                "the step did not finish in three hours; its pane is still open".to_owned(),
            ),
            Wait::Lost => Ended::Failed("the session went away without answering".to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    //! The registry is what stands between a second start and two writers in one checkout, and the
    //! variable filter is what keeps the window from telling a step it is somewhere else.

    use super::*;

    fn view(id: &str, worktree: &str, status: RunStatus) -> RunView {
        RunView {
            id: id.to_owned(),
            recipe_id: "r".to_owned(),
            recipe_name: "Implementation".to_owned(),
            project_id: "/repo".to_owned(),
            worktree_id: worktree.to_owned(),
            action: None,
            status,
            started_ms: 0,
            ended_ms: None,
            missing: Vec::new(),
            steps: Vec::new(),
        }
    }

    /// A [`Host`] that opens numbered sessions and answers turns from a script.
    #[derive(Default)]
    struct Script {
        registry: Registry,
        opened: parking_lot::Mutex<Vec<String>>,
        sent: parking_lot::Mutex<Vec<(String, String)>>,
        answers: parking_lot::Mutex<std::collections::VecDeque<Ended>>,
        closed: Vec<String>,
    }

    impl Host for &Script {
        fn now(&self) -> u64 {
            0
        }
        fn record(&self, run: &str, change: &mut dyn FnMut(&mut RunView)) {
            self.registry.update(run, change);
        }
        fn open(&self, step: &RecipeStep, title: &str, _run: &str) -> Result<String, String> {
            let mut opened = self.opened.lock();
            opened.push(format!("{} {title}", step.agent));
            Ok(format!("s{}", opened.len()))
        }
        fn alive(&self, session: &str) -> bool {
            !self.closed.iter().any(|c| c == session)
        }
        fn turn(&self, session: &str, text: &str, stop: &AtomicBool) -> Ended {
            self.sent.lock().push((session.to_owned(), text.to_owned()));
            let answer = self
                .answers
                .lock()
                .pop_front()
                .unwrap_or(Ended::Done(String::new()));
            if answer == Ended::Stopped {
                stop.store(true, Ordering::Relaxed);
            }
            answer
        }
    }

    fn prompt_step(agent: &str, name: &str, text: &str) -> RecipeStep {
        RecipeStep {
            name: name.to_owned(),
            agent: agent.to_owned(),
            run: StepRun::Prompt {
                prompt: text.to_owned(),
            },
            ..RecipeStep::default()
        }
    }

    /// Run `steps` against `script` and return the run's final record.
    fn drive(script: &Script, steps: Vec<RecipeStep>, answers: Vec<Ended>) -> RunView {
        let recipe = Recipe {
            id: "r".to_owned(),
            name: "Ship".to_owned(),
            steps,
            ..Recipe::default()
        };
        *script.answers.lock() = answers.into();
        let stop = script
            .registry
            .admit(begin(&recipe, "run".to_owned(), "/repo", "/wt", None, 0))
            .unwrap();
        Drive {
            host: script,
            run: "run".to_owned(),
            recipe,
            vars: BTreeMap::from([("form.ticket".to_owned(), "ABC-1".to_owned())]),
            stop,
        }
        .run();
        script.registry.list().remove(0)
    }

    fn statuses(run: &RunView) -> Vec<StepStatus> {
        run.steps.iter().map(|s| s.status).collect()
    }

    #[test]
    fn steps_run_in_order_and_a_continuing_step_keeps_its_conversation_while_a_new_one_gets_the_reply()
     {
        let script = Script::default();
        let mut build = prompt_step("claude", "Build", "Now build it.");
        build.conversation = Conversation::Continue;
        let mut review = prompt_step("codex", "Review", "Review {{form.ticket}}.");
        review.previous_reply = true;
        let run = drive(
            &script,
            vec![
                prompt_step("claude", "Plan", "Plan {{form.ticket}}."),
                build,
                review,
            ],
            vec![
                Ended::Done("the plan".to_owned()),
                Ended::Done("built it".to_owned()),
                Ended::Done("looks fine".to_owned()),
            ],
        );

        assert_eq!(
            *script.opened.lock(),
            vec![
                "claude Plan · Ship".to_owned(),
                "codex Review · Ship".to_owned()
            ],
            "the continuing step opens nothing of its own"
        );
        let sent = script.sent.lock().clone();
        assert_eq!(sent[0], ("s1".to_owned(), "Plan ABC-1.".to_owned()));
        assert_eq!(sent[1], ("s1".to_owned(), "Now build it.".to_owned()));
        assert_eq!(sent[2].0, "s2");
        assert!(sent[2].1.starts_with("Review ABC-1."));
        assert!(sent[2].1.ends_with("built it"), "{}", sent[2].1);
        assert_eq!(run.status, RunStatus::Done);
        assert_eq!(statuses(&run), vec![StepStatus::Done; 3]);
        assert_eq!(run.steps[1].session.as_deref(), Some("s1"));
    }

    #[test]
    fn a_failed_step_skips_the_rest_unless_it_says_to_carry_on() {
        let script = Script::default();
        let run = drive(
            &script,
            vec![
                prompt_step("claude", "A", "a"),
                prompt_step("claude", "B", "b"),
            ],
            vec![Ended::Failed("limit reached".to_owned())],
        );
        assert_eq!(
            statuses(&run),
            vec![StepStatus::Failed, StepStatus::Skipped]
        );
        assert_eq!(run.steps[0].error.as_deref(), Some("limit reached"));
        assert_eq!(run.status, RunStatus::Failed);
        assert_eq!(script.sent.lock().len(), 1);

        let script = Script::default();
        let mut tolerant = prompt_step("claude", "A", "a");
        tolerant.on_failure = StepFailure::Continue;
        let mut next = prompt_step("codex", "B", "b");
        next.previous_reply = true;
        let run = drive(
            &script,
            vec![tolerant, next],
            vec![Ended::Failed("no".to_owned()), Ended::Done("ok".to_owned())],
        );
        assert_eq!(statuses(&run), vec![StepStatus::Failed, StepStatus::Done]);
        assert_eq!(
            script.sent.lock()[1].1,
            "b",
            "a failed step leaves no reply for the next to be given"
        );
        assert_eq!(run.status, RunStatus::Failed);
    }

    #[test]
    fn stopping_ends_the_running_step_as_stopped_and_skips_the_rest() {
        let script = Script::default();
        let run = drive(
            &script,
            vec![
                prompt_step("claude", "A", "a"),
                prompt_step("claude", "B", "b"),
                prompt_step("claude", "C", "c"),
            ],
            vec![Ended::Done("ok".to_owned()), Ended::Stopped],
        );
        assert_eq!(
            statuses(&run),
            vec![StepStatus::Done, StepStatus::Stopped, StepStatus::Skipped]
        );
        assert_eq!(run.status, RunStatus::Stopped);
        assert_eq!(script.sent.lock().len(), 2);
    }

    #[test]
    fn a_step_cannot_continue_a_conversation_whose_pane_was_closed() {
        let script = Script {
            closed: vec!["s1".to_owned()],
            ..Script::default()
        };
        let mut continuing = prompt_step("claude", "B", "b");
        continuing.conversation = Conversation::Continue;
        let run = drive(
            &script,
            vec![prompt_step("claude", "A", "a"), continuing],
            vec![Ended::Done("ok".to_owned())],
        );
        assert_eq!(statuses(&run), vec![StepStatus::Done, StepStatus::Failed]);
        assert!(run.steps[1].error.as_deref().unwrap().contains("closed"));
        assert_eq!(
            script.opened.lock().len(),
            1,
            "no new session stands in for the closed one"
        );
    }

    #[test]
    fn a_variable_the_run_has_no_value_for_is_named_on_the_run() {
        let script = Script::default();
        let run = drive(
            &script,
            vec![prompt_step("claude", "A", "Fix {{action.exit_code}}")],
            vec![],
        );
        assert_eq!(run.missing, vec!["action.exit_code".to_owned()]);
        assert_eq!(script.sent.lock()[0].1, "Fix");
    }

    #[test]
    fn a_second_run_in_a_busy_worktree_is_refused_and_one_elsewhere_is_not() {
        let registry = Registry::default();
        registry
            .admit(view("a", "/wt", RunStatus::Running))
            .expect("first run");
        let refused = registry
            .admit(view("b", "/wt", RunStatus::Running))
            .expect_err("busy");
        assert!(refused.contains("Implementation is already running"));
        registry
            .admit(view("c", "/other", RunStatus::Running))
            .expect("another worktree");
    }

    #[test]
    fn a_finished_run_is_replaced_by_the_next_and_only_a_finished_run_can_be_dismissed() {
        let registry = Registry::default();
        registry
            .admit(view("a", "/wt", RunStatus::Running))
            .unwrap();
        assert!(!registry.dismiss("a"), "a running run keeps its record");
        registry.update("a", |run| run.status = RunStatus::Done);
        registry
            .admit(view("b", "/wt", RunStatus::Running))
            .unwrap();
        let ids: Vec<String> = registry.list().into_iter().map(|run| run.id).collect();
        assert_eq!(ids, vec!["b".to_owned()]);
    }

    #[test]
    fn stopping_a_run_names_the_session_its_running_step_waits_on() {
        let registry = Registry::default();
        let mut running = view("a", "/wt", RunStatus::Running);
        running.steps.push(RunStepView {
            name: "Plan".to_owned(),
            agent: "claude".to_owned(),
            model: None,
            mode: None,
            status: StepStatus::Running,
            session: Some("s1".to_owned()),
            error: None,
            started_ms: None,
            ended_ms: None,
        });
        let stop = registry.admit(running).unwrap();
        assert_eq!(registry.stop("a"), Some("s1".to_owned()));
        assert!(stop.load(Ordering::Relaxed));
        assert_eq!(registry.stop("missing"), None);
    }

    #[test]
    fn a_step_view_keeps_only_the_text_its_kind_uses_and_drops_the_slash() {
        let step = RecipeStep::from(StepView {
            name: " Review ".to_owned(),
            agent: "codex".to_owned(),
            model: Some(String::new()),
            effort: None,
            mode: None,
            kind: StepKind::Skill,
            skill: "/review".to_owned(),
            args: String::new(),
            prompt: "left over from the prompt tab".to_owned(),
            conversation: Conversation::New,
            previous_reply: false,
            on_failure: StepFailure::Stop,
        });
        assert_eq!(step.name, "Review");
        assert_eq!(
            step.model, None,
            "an empty picker means the provider default"
        );
        assert_eq!(
            step.run,
            StepRun::Skill {
                skill: "review".to_owned(),
                args: String::new()
            }
        );
    }

    #[test]
    fn the_window_can_supply_form_and_action_values_but_not_say_where_the_run_is() {
        let place = BTreeMap::from([("worktree.path".to_owned(), "/real".to_owned())]);
        let supplied = BTreeMap::from([
            ("worktree.path".to_owned(), "/elsewhere".to_owned()),
            ("form.ticket".to_owned(), "ABC-1".to_owned()),
            ("action.exit_code".to_owned(), "2".to_owned()),
            ("previous.reply".to_owned(), "forged".to_owned()),
        ]);
        let vars = with_supplied(place, supplied);
        assert_eq!(vars["worktree.path"], "/real");
        assert_eq!(vars["form.ticket"], "ABC-1");
        assert_eq!(vars["action.exit_code"], "2");
        assert!(!vars.contains_key("previous.reply"));
    }

    #[test]
    fn a_worktree_named_for_an_issue_gives_every_run_that_issue() {
        use wtm_core::model::{BranchRef, Checkout, WorktreeId};
        let worktree = |branch: &str, dir: &str| {
            let path = std::path::PathBuf::from(dir);
            Worktree {
                id: WorktreeId::from_path(&path),
                path,
                head: None,
                checkout: Checkout::Branch {
                    branch: BranchRef::new(branch),
                },
                is_main: false,
                is_bare: false,
                locked: None,
                prunable: None,
            }
        };
        let vars = worktree_vars(&worktree("task/ACME-8599-export", "/wt/ACME-8599-export"));
        assert_eq!(
            vars.get("worktree.issue").map(String::as_str),
            Some("ACME-8599")
        );
        assert!(
            !worktree_vars(&worktree("spike", "/wt/spike")).contains_key("worktree.issue"),
            "no issue is reported missing, not sent as an empty string"
        );
    }

    #[test]
    fn a_recipe_naming_an_agent_this_build_cannot_drive_is_refused() {
        let recipe = Recipe {
            name: "R".to_owned(),
            steps: vec![RecipeStep {
                name: "S".to_owned(),
                agent: "gemini".to_owned(),
                run: StepRun::Prompt {
                    prompt: "Go.".to_owned(),
                },
                ..RecipeStep::default()
            }],
            ..Recipe::default()
        };
        assert!(problems_with(&recipe).iter().any(|p| p.contains("gemini")));
    }
}
