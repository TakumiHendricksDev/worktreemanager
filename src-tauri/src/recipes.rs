//! Settings › Recipes over IPC: the recipes and roles a project's runs use, edited in the app and
//! written back to the file that declares them.
//!
//! # What is here, and what is not
//!
//! Writing is `wtm-config`'s (`FileConfigStore::save_workflow`): which file, which table, only the
//! keys that changed, validation of the whole project as loading would see it, and the local layer's
//! trust approval following an edit made here. Whether a run already under way sees an edit is the
//! engine's, and it does not (`Engine::run_workflow`). This module is the commands, the views they
//! answer with, and the one thing that is its own: asking the providers which skills they list.
//!
//! # A skill check asks the providers, the way a run would
//!
//! Whether a role's skill exists is a question only its provider can answer, and the two answer it
//! differently (`wtm_core::flow::recipe` says how). So Check recipe opens a session for each
//! provider a recipe's skilled roles use, prepared exactly as a run's session is — the run profile,
//! the plugin wtm writes a Claude role's skills into, Codex's home of its own — in the project's own
//! checkout, takes the list the provider sends when the session starts, and closes it. No turn is
//! sent, so nothing is spent. A provider that cannot be asked, because its CLI is missing or its
//! sign-in would bill an API key, is said to be unasked rather than passed: an unchecked skill is
//! exactly what the check exists to catch before a run.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::{self, Sender};
use std::time::Duration;

use parking_lot::Mutex;
use serde::Serialize;
use wtm_config::{WorkflowDraft, WorkflowFile, WorkflowHome, WorkflowSaveError};
use wtm_core::flow::{
    FlowDir, FlowId, ImplementSpec, RecipeSpec, RoleProvider, RoleSpec, SkillCatalog, SkillError,
    SkillProblem, WorkflowSpec,
};
use wtm_core::model::{AgentEvent, AgentSkill, ExitOutcome, Project, SessionId};

use crate::app::{App, FlowRole};
use crate::commands::{AppState, Reply, blocking};
use crate::flow_session::{self, RoleSession};
use crate::view::ErrorView;

/// How long a provider has to list its skills once its session starts. Claude answers its
/// `initialize` in a second or two and Codex its `skills/list` about as fast; a minute separates a
/// slow start from one that will never answer.
const LIST_WAIT: Duration = Duration::from_secs(60);

/// The role name a skill check's session runs under. Never a recipe's: it names the directories
/// the check writes, which are removed when it ends.
const CHECK_ROLE: &str = "skill-check";

/// Settings › Recipes, whole: what the project's runs would use, and where it is written.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecipeSettingsView {
    pub file: WorkflowFile,
    /// Each place recipes can be kept, with its table and file, so the editor can say where an edit
    /// goes and, when nothing declares a workflow yet, offer the choice.
    pub places: Vec<PlaceView>,
    pub workflow: WorkflowView,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaceView {
    pub home: WorkflowHome,
    /// The table, such as `[defaults.workflow]`.
    pub header: String,
    pub path: String,
}

/// The merged `[workflow]`'s recipes and roles, and what refers to roles from outside a recipe.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowView {
    pub role: BTreeMap<String, RoleSpec>,
    pub recipe: BTreeMap<String, RecipeSpec>,
    pub planner: Option<String>,
    pub designer: Option<String>,
    pub implement: Option<ImplementSpec>,
    /// `[workflow.check]`, which a role may be given.
    pub checks: Vec<CheckChoice>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckChoice {
    pub id: String,
    pub label: String,
}

/// What Check recipe found.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillCheckView {
    /// The roles checked: the recipe's, and every fallback they lead to. A Claude list depends on
    /// which skills were copied in to ask it, so a mark on any other role would be a guess.
    pub roles: Vec<String>,
    /// Each provider asked, and what it said.
    pub providers: Vec<ProviderSkills>,
    /// The recipe's roles that could not start for want of a skill, as a run would report them.
    pub problems: Vec<SkillProblem>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSkills {
    pub provider: RoleProvider,
    /// The skills it listed, without the leading `/` and without the prefix of the plugin a Claude
    /// run's skills are copied into. `None` when it could not be asked.
    pub listed: Option<Vec<String>>,
    /// Why it could not be asked.
    pub problem: Option<String>,
}

/// A refusal, with its detail the editor reads to put the message beside its field.
fn refused(error: &WorkflowSaveError) -> ErrorView {
    let view = ErrorView::new("recipes", error.to_string());
    let detail = match error {
        WorkflowSaveError::Edit(edit) => serde_json::to_value(edit),
        other => serde_json::to_value(other),
    };
    match detail {
        Ok(detail) => view.with_detail(detail),
        Err(_) => view,
    }
}

fn root(project: &str) -> &Path {
    Path::new(project)
}

fn settings(app: &App, project: &str) -> Result<RecipeSettingsView, ErrorView> {
    // Loaded first, so a project that does not load says why here rather than as an empty editor.
    let loaded = app.project(project)?;
    let file = app.config.workflow_file(root(project))?;
    let places = WorkflowHome::ALL
        .into_iter()
        .map(|home| PlaceView {
            home,
            header: home.header(&file.root_key),
            path: file.path_of(home).to_string_lossy().into_owned(),
        })
        .collect();
    let workflow = loaded.workflow.unwrap_or_default();
    Ok(RecipeSettingsView {
        file,
        places,
        workflow: WorkflowView {
            checks: workflow
                .check
                .iter()
                .map(|(id, spec)| CheckChoice {
                    id: id.clone(),
                    label: spec.label.clone().unwrap_or_else(|| id.clone()),
                })
                .collect(),
            role: workflow.role,
            recipe: workflow.recipe,
            planner: workflow.planner,
            designer: workflow.designer,
            implement: workflow.implement,
        },
    })
}

/// The project's recipes and roles, and where they are written.
#[tauri::command]
pub async fn recipe_settings(app: AppState<'_>, project: String) -> Reply<RecipeSettingsView> {
    let app = Arc::clone(&app);
    blocking(move || settings(&app, &project)).await
}

/// Whether `draft` would load if saved: validation of the whole project, nothing written.
#[tauri::command]
pub async fn check_recipes(
    app: AppState<'_>,
    project: String,
    draft: WorkflowDraft,
    home: Option<WorkflowHome>,
) -> Reply<()> {
    let app = Arc::clone(&app);
    blocking(move || {
        app.config
            .check_workflow(root(&project), &draft, home)
            .map_err(|e| refused(&e))
    })
    .await
}

/// Write `draft` to the file that declares the project's workflow, or to `home` when none does.
#[tauri::command]
pub async fn save_recipes(
    app: AppState<'_>,
    project: String,
    draft: WorkflowDraft,
    revision: String,
    home: Option<WorkflowHome>,
) -> Reply<RecipeSettingsView> {
    let app = Arc::clone(&app);
    blocking(move || {
        app.config
            .save_workflow(root(&project), &draft, &revision, home)
            .map_err(|e| refused(&e))?;
        settings(&app, &project)
    })
    .await
}

/// Ask each provider `recipe`'s skilled roles use for its skills, and check the roles against them.
#[tauri::command]
pub async fn check_recipe_skills(
    app: AppState<'_>,
    project: String,
    draft: WorkflowDraft,
    recipe: String,
) -> Reply<SkillCheckView> {
    let app = Arc::clone(&app);
    blocking(move || {
        let loaded = app.project(&project)?;
        let workflow = WorkflowSpec {
            role: draft.role,
            recipe: draft.recipe,
            ..WorkflowSpec::default()
        };
        let wanted = skills_by_provider(&workflow, &recipe);
        let answers: Vec<(RoleProvider, Result<Vec<String>, String>)> =
            std::thread::scope(|scope| {
                let asking: Vec<_> = wanted
                    .iter()
                    .map(|(provider, skills)| {
                        let app = &app;
                        let loaded = &loaded;
                        scope.spawn(move || (*provider, ask_skills(app, loaded, *provider, skills)))
                    })
                    .collect();
                asking
                    .into_iter()
                    .filter_map(|handle| handle.join().ok())
                    .collect()
            });
        check(&workflow, &recipe, answers).map_err(|e| ErrorView::new("recipes", e))
    })
    .await
}

/// Every role `recipe` can run, fallbacks included, since a fallback starts with nobody watching.
fn reached(workflow: &WorkflowSpec, recipe: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let roles = workflow
        .recipe
        .get(recipe)
        .into_iter()
        .flat_map(|spec| &spec.stages)
        .flat_map(|stage| &stage.roles);
    for id in roles {
        for role in std::iter::once(id.clone()).chain(workflow.fallback_chain(id)) {
            if workflow.role.contains_key(&role) && !out.contains(&role) {
                out.push(role);
            }
        }
    }
    out
}

/// The skills each provider is asked for: those of every role `recipe` can run.
fn skills_by_provider(
    workflow: &WorkflowSpec,
    recipe: &str,
) -> BTreeMap<RoleProvider, Vec<String>> {
    let mut out: BTreeMap<RoleProvider, BTreeSet<String>> = BTreeMap::new();
    for id in reached(workflow, recipe) {
        let Some(role) = workflow.role.get(&id) else {
            continue;
        };
        out.entry(role.provider)
            .or_default()
            .extend(role.skill_names().map(str::to_owned));
    }
    out.into_iter()
        .filter(|(_, skills)| !skills.is_empty())
        .map(|(provider, skills)| (provider, skills.into_iter().collect()))
        .collect()
}

/// The check, from what the providers said: the run's own rule, applied to the run's own answer.
fn check(
    workflow: &WorkflowSpec,
    recipe: &str,
    answers: Vec<(RoleProvider, Result<Vec<String>, String>)>,
) -> Result<SkillCheckView, String> {
    let mut catalog = SkillCatalog::new();
    let mut providers = Vec::with_capacity(answers.len());
    for (provider, answer) in answers {
        match answer {
            Ok(listed) => {
                let skills: Vec<AgentSkill> = listed
                    .iter()
                    .map(|name| AgentSkill {
                        name: name.clone(),
                        description: None,
                        scope: None,
                    })
                    .collect();
                catalog = catalog.with(provider, &skills);
                providers.push(ProviderSkills {
                    provider,
                    listed: Some(listed),
                    problem: None,
                });
            }
            Err(problem) => providers.push(ProviderSkills {
                provider,
                listed: None,
                problem: Some(problem),
            }),
        }
    }
    let problems = match workflow.check_skills(recipe, &catalog) {
        Ok(()) => Vec::new(),
        Err(SkillError::Problems(problems)) => problems,
        Err(unknown @ SkillError::UnknownRecipe(_)) => return Err(unknown.to_string()),
    };
    Ok(SkillCheckView {
        roles: reached(workflow, recipe),
        providers,
        problems,
    })
}

/// A Claude run lists a role's skills under the plugin wtm copies them into; the role names them
/// without it. Taken off here, as the engine does, so the two compare.
fn as_named(provider: RoleProvider, listed: &str) -> String {
    let name = listed.trim_start_matches('/');
    let prefix = format!("{}:", wtm_agent::authority::RUN_SKILL_PLUGIN);
    match provider {
        RoleProvider::Claude => name.strip_prefix(prefix.as_str()).unwrap_or(name),
        RoleProvider::Codex => name,
    }
    .to_owned()
}

/// Hands the first skill list a session sends to whoever is waiting for it.
struct Listed {
    tx: Mutex<Option<Sender<Vec<AgentSkill>>>>,
}

impl wtm_agent::session::AgentSink for Listed {
    fn on_event(&self, _session: &SessionId, event: &AgentEvent) {
        if let AgentEvent::SkillsListed { skills } = event
            && let Some(tx) = self.tx.lock().take()
        {
            let _ = tx.send(skills.clone());
        }
    }
    fn on_exit(&self, _session: &SessionId, _outcome: &ExitOutcome) {
        // A session that ended before listing anything: drop the sender, so the wait ends now.
        self.tx.lock().take();
    }
    fn on_ready(&self, _session: &SessionId) {}
}

/// What `provider` lists in `project`'s checkout, asked by a session prepared as a run's would be,
/// with `skills` copied in where Claude needs them.
fn ask_skills(
    app: &Arc<App>,
    project: &Project,
    provider: RoleProvider,
    skills: &[String],
) -> Result<Vec<String>, String> {
    // F3's check first: a provider that would bill an API key is not started, even to list.
    crate::run_profile::preflight(app, &[provider.id()])?;
    let spec = RoleSession {
        project: project.root.to_string_lossy().into_owned(),
        worktree: project.root.to_string_lossy().into_owned(),
        role: FlowRole {
            // No run: flow 0, which no run is ever numbered (the store's ids start at 1), so the
            // directories this writes are the check's alone.
            flow: FlowId(0),
            role: CHECK_ROLE.to_owned(),
            authority: wtm_agent::authority::Authority::Reviewer,
            browser: None,
            design: false,
        },
        provider: provider.id().to_owned(),
        model: None,
        effort: None,
        brief: String::new(),
        resume: None,
        cwd: None,
        skills: skills.to_vec(),
    };
    let prepared = flow_session::prepare(app, &spec)?;
    let cleanup = |app: &App| {
        app.handoff.forget_unbound(&prepared.token);
        let _ = std::fs::remove_dir_all(&prepared.places.session_dir);
        let _ = std::fs::remove_dir_all(
            FlowDir::new(&app.config.paths().flow_dir, FlowId(0))
                .path()
                .join("skills")
                .join(CHECK_ROLE),
        );
    };
    let Some(executable) = app.agent_executable(prepared.entry) else {
        cleanup(app);
        return Err(format!(
            "wtm cannot find {}'s CLI on your PATH",
            prepared.entry.label
        ));
    };
    let mut request = prepared.request.clone();
    request.executable = Some(executable.to_string_lossy().into_owned());

    let (tx, rx) = mpsc::channel();
    let sink: Arc<dyn wtm_agent::session::AgentSink> = Arc::new(Listed {
        tx: Mutex::new(Some(tx)),
    });
    let session = match wtm_agent::AgentSession::open(
        prepared.entry.provider,
        &request,
        Arc::clone(&app.pipe) as Arc<dyn wtm_core::ports::pipe::PipeHost>,
        &sink,
        LIST_WAIT.as_millis().try_into().unwrap_or(u64::MAX),
        None,
    ) {
        Ok(session) => session,
        Err(error) => {
            cleanup(app);
            return Err(format!(
                "{} did not start to list its skills: {error}",
                prepared.entry.label
            ));
        }
    };
    let listed = rx.recv_timeout(LIST_WAIT);
    let _ = session.close();
    cleanup(app);
    match listed {
        Ok(skills) => Ok(skills
            .iter()
            .map(|skill| as_named(provider, &skill.name))
            .collect()),
        Err(_) => Err(format!(
            "{} started but did not list its skills",
            prepared.entry.label
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workflow(text: &str) -> WorkflowSpec {
        toml::from_str(text).unwrap()
    }

    const TWO_PROVIDERS: &str = r#"
[role.conventions]
objective = "Conventions"
provider = "claude"
skills = ["/team-review"]
inputs = ["diff"]
fallback = "conventions-codex"

[role.conventions-codex]
objective = "Conventions, on Codex"
provider = "codex"
skills = ["team-review", "lint-notes"]
inputs = ["diff"]

[role.plain]
objective = "No skills"
provider = "codex"
inputs = ["diff"]

[[recipe.standard.stage]]
roles = ["conventions", "plain"]
"#;

    /// `recipes.ts` reads these spellings: the config's own for a role and a recipe, camelCase for
    /// the views around them, and one `kind` on a refusal's detail, where the editor finds its key.
    #[test]
    fn the_views_and_refusals_are_spelled_as_the_window_reads_them() {
        let workflow = workflow(TWO_PROVIDERS);
        let view = WorkflowView {
            role: workflow.role.clone(),
            recipe: workflow.recipe.clone(),
            planner: None,
            designer: None,
            implement: None,
            checks: vec![CheckChoice {
                id: "unit".to_owned(),
                label: "Unit tests".to_owned(),
            }],
        };
        let json = serde_json::to_value(&view).unwrap();
        assert_eq!(
            json["recipe"]["standard"]["stage"][0]["roles"],
            serde_json::json!(["conventions", "plain"])
        );
        assert_eq!(json["role"]["conventions"]["fallback"], "conventions-codex");
        assert_eq!(json["role"]["plain"]["fallback"], "ask");
        assert!(json["role"]["plain"]["timeout_ms"].is_null());
        assert_eq!(json["checks"][0]["label"], "Unit tests");

        let check = check(
            &workflow,
            "standard",
            vec![(RoleProvider::Codex, Ok(vec!["team-review".to_owned()]))],
        )
        .unwrap();
        let json = serde_json::to_value(&check).unwrap();
        assert_eq!(
            json["roles"],
            serde_json::json!(["conventions", "conventions-codex", "plain"])
        );
        assert_eq!(
            json["providers"][0]["listed"],
            serde_json::json!(["team-review"])
        );
        assert_eq!(json["problems"][0]["kind"], "unlisted");

        let invalid = refused(&WorkflowSaveError::Invalid {
            key: Some("role.plain.retries".to_owned()),
            message: "at most 3".to_owned(),
        });
        assert_eq!(invalid.kind, "recipes");
        assert_eq!(
            invalid.detail,
            Some(serde_json::json!({
                "kind": "invalid",
                "key": "role.plain.retries",
                "message": "at most 3"
            }))
        );
        let unremovable = refused(&WorkflowSaveError::Edit(
            wtm_config::EditError::Unremovable {
                key: "role.spare".to_owned(),
                from: "[defaults.workflow]".to_owned(),
            },
        ));
        assert_eq!(
            unremovable.detail,
            Some(serde_json::json!({
                "kind": "unremovable",
                "key": "role.spare",
                "from": "[defaults.workflow]"
            }))
        );
        assert_eq!(
            serde_json::to_value(refused(&WorkflowSaveError::NoHome)).unwrap()["detail"],
            serde_json::json!({ "kind": "no_home" })
        );
    }

    #[test]
    fn a_fallbacks_skills_are_asked_for_on_its_own_provider_and_a_role_without_skills_asks_nothing()
    {
        let asked = skills_by_provider(&workflow(TWO_PROVIDERS), "standard");
        assert_eq!(
            asked,
            BTreeMap::from([
                (RoleProvider::Claude, vec!["team-review".to_owned()]),
                (
                    RoleProvider::Codex,
                    vec!["lint-notes".to_owned(), "team-review".to_owned()]
                ),
            ])
        );
    }

    #[test]
    fn a_skill_one_provider_lists_and_the_other_does_not_fails_with_the_one_that_does_not_named() {
        let view = check(
            &workflow(TWO_PROVIDERS),
            "standard",
            vec![
                (
                    RoleProvider::Claude,
                    Ok(vec![as_named(RoleProvider::Claude, "wtm:team-review")]),
                ),
                (RoleProvider::Codex, Ok(vec!["team-review".to_owned()])),
            ],
        )
        .unwrap();
        assert_eq!(
            view.problems,
            vec![SkillProblem::Missing {
                role: "conventions-codex".to_owned(),
                provider: RoleProvider::Codex,
                skill: "lint-notes".to_owned(),
            }]
        );
    }

    #[test]
    fn a_provider_that_could_not_be_asked_leaves_its_roles_unchecked_rather_than_passed() {
        let view = check(
            &workflow(TWO_PROVIDERS),
            "standard",
            vec![
                (
                    RoleProvider::Claude,
                    Err("Claude would bill an API key".to_owned()),
                ),
                (
                    RoleProvider::Codex,
                    Ok(vec!["team-review".to_owned(), "lint-notes".to_owned()]),
                ),
            ],
        )
        .unwrap();
        assert_eq!(
            view.problems,
            vec![SkillProblem::Unlisted {
                role: "conventions".to_owned(),
                provider: RoleProvider::Claude,
            }]
        );
        assert_eq!(
            view.providers[0].problem.as_deref(),
            Some("Claude would bill an API key")
        );
    }
}
