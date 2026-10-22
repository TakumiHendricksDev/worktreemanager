//! Recipes and roles: what a review or an implementation run does, declared as configuration.
//!
//! A recipe is a list of stages, and the roles in one stage run in parallel. A role is one headless
//! session: an objective, a provider with its model and effort, the skills it may invoke, what it
//! reads and what it hands back.
//!
//! # Where they are declared, and where they are refused
//!
//! In your own config: `[defaults.workflow]` in `~/.config/wtm/config.toml` for every project, or
//! `[projects.<path>.config.workflow]` for one. `wtm-config` refuses `workflow.role` and
//! `workflow.recipe` in a repository's `wtm.toml`, because a role decides which of your MCP servers
//! and plugins a run session may load and which provider account it spends. A file anyone can
//! commit to a branch must not be able to choose those.
//!
//! # Keyed tables, not arrays
//!
//! `[workflow.role.<id>]` and `[workflow.recipe.<id>]`, for the reason `AgentSpec` gives: tables
//! merge per key across config layers and arrays replace. Your defaults can define a set of roles
//! and one project can change one role's model without restating the others.
//!
//! # Two checks, at two different times
//!
//! [`WorkflowSpec::validate`] runs when the config loads and catches everything the file alone
//! decides: a stage naming a role that does not exist, a fallback loop, two writers in one stage.
//!
//! [`WorkflowSpec::check_skills`] runs before a recipe starts, because whether a skill exists is a
//! question only the provider can answer, and the two CLIs answer it differently: Claude reports its
//! commands in the `initialize` reply and Codex in `skills/list`, from different directories, with
//! plugin skills only one of them sees. A role's skills are therefore validated against *that
//! role's* provider, and a skill one provider lacks fails with that provider named, rather than
//! passing because the other one has it.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::ForgeKind;
use super::capacity::MAX_FALLBACKS;
use super::glob;
use super::instructions::DEFAULT_INSTRUCTION_SOURCES;
use super::start::StartSpec;
use super::tracker::{self, QueueSpec, StatusMap, TrackerKind};
use crate::model::AgentSkill;

/// How long a role may run when its config does not say. A review with a skill that fans out to
/// its own subagents takes minutes; half an hour separates slow from stuck.
pub const DEFAULT_TIMEOUT_MS: u64 = 30 * 60 * 1000;

/// The most retries a role may ask for. Each one is a whole provider turn against the
/// subscription's usage window, so a typo like `retries = 30` would spend a day's capacity on one
/// stuck role before anybody saw it.
pub const MAX_RETRIES: u32 = 3;

/// The `[workflow]` table.
///
/// Recipes, roles, the forge, the integration branches, the repository's instruction sources, and
/// the tracker with its status mapping, mapped fields and queue. A key this build does not know is
/// an error rather than something silently ignored.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowSpec {
    /// Where issues are read from. Absent, nothing is. Your own config and the local layer only,
    /// for the reason `forge` is: naming it lets the Work view use your tracker CLI's sign-in for
    /// this project (ARCHITECTURE §6a).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracker: Option<TrackerKind>,
    /// The project's status names, by the role each plays. The board's columns.
    #[serde(default, skip_serializing_if = "StatusMap::is_empty")]
    pub statuses: StatusMap,
    /// The project's names for tracker fields, such as `owner = "customfield_12345"`, shown on an
    /// issue. [`SPRINT_FIELD`](super::SPRINT_FIELD) names the one holding its sprints.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub fields: BTreeMap<String, String>,
    /// What fills the queue. A repository may declare this, the statuses and the fields: they are
    /// only read once you have named a tracker, and none of them can write anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue: Option<QueueSpec>,
    /// What Start Work does beyond starting the run: whether it moves the issue, and which form
    /// field takes the key. A repository may declare it too; moving needs a tracker you named.
    #[serde(default, skip_serializing_if = "StartSpec::is_default")]
    pub start: StartSpec,
    /// Where pull requests are read from. Absent, nothing is: see [`ForgeKind`] for why a workflow
    /// does not assume one. Accepted from your own config and the local layer only, because naming
    /// it is what lets the Work view call a forge on your behalf (ARCHITECTURE §6a), and a
    /// committed file is written by whoever can push to the branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub forge: Option<ForgeKind>,
    /// Branches work merges into, such as `main` or `release/*`, in [`glob`]'s dialect, so
    /// `release/*` also covers `release/4.2/hotfix`.
    ///
    /// Read for display so far: the Work view shows whether a pull request's base is one, and
    /// whether the forge protects it. A repository may declare these, since naming a branch grants
    /// nothing; the item that refuses pushes to them should treat a repository's list as additions
    /// to yours, never as a replacement.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub integration_branches: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub role: BTreeMap<String, RoleSpec>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub recipe: BTreeMap<String, RecipeSpec>,
    /// The role that investigates a started issue and plans it: a reviewer handing in a plan.
    /// Absent, an issue's run waits at "Ready to plan" and nothing is spent on it.
    ///
    /// Your own config and the local layer only, like the roles it names: choosing the planner
    /// chooses which account a started issue spends, by itself, once its worktree is ready.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planner: Option<String>,
    /// Commands the app runs when a role asks, through `flow_run_check`.
    ///
    /// Your own config and the local layer only, like roles: a check is a command wtm runs outside
    /// every sandbox, on a model's request, and a file anyone can commit must not be able to name
    /// one. The plan's repository-level `[[check]]`, behind the trust prompt, is a later item.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub check: BTreeMap<String, CheckSpec>,
    /// Whether a check that needs services wtm is not running may start them itself: the project's
    /// default for its checks, which a check's own `start_services` overrides. `None` is
    /// [`StartServices::Ask`].
    ///
    /// Your own config and the local layer only, like checks: `auto` lets a model's request start
    /// processes on this machine, and that is not a repository's to decide.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_services: Option<StartServices>,
    /// Globs naming the repository's own instruction files. `None` means
    /// [`DEFAULT_INSTRUCTION_SOURCES`]; an empty list means none at all, which is a choice a
    /// project can make and so is not the same as leaving the key out.
    ///
    /// Allowed in a repository's `wtm.toml`, unlike roles: it only names files in the repository
    /// for a review to read, which anyone who can commit could already have written into them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instruction_sources: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecipeSpec {
    /// What Settings calls it. Falls back to the table key.
    #[serde(default)]
    pub label: Option<String>,
    /// Run in order. The next stage starts when every role in this one has finished.
    #[serde(rename = "stage")]
    pub stages: Vec<StageSpec>,
    /// At most this many of a stage's roles run at once; the rest wait for a place. `None` runs a
    /// whole stage together.
    ///
    /// A cap rather than a fixed degree of parallelism, because what it protects is the account,
    /// not the machine: four roles on one subscription spend its window four times as fast, and a
    /// stage of six reviewers can reach a limit before any of them finishes.
    #[serde(default)]
    pub concurrency: Option<u32>,
}

/// Roles that run at the same time.
///
/// A table rather than a bare list of names so a stage can gain settings of its own without
/// changing the shape of every recipe already written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StageSpec {
    pub roles: Vec<String>,
}

/// A command a role may ask the app to run: tests, a linter, a build.
///
/// # Run by the app, so its result is a fact
///
/// A role that says "the tests pass" is reporting; the app running them is evidence. A reviewer's
/// sandbox cannot run most test suites anyway (Codex's read-only one refuses the temporary files
/// they write), so a role asks with `flow_run_check`, the app runs exactly this argv in the
/// directory the role reads, outside the sandbox and with the run's credential scrub, and keeps the
/// exit code and output as the check's record. The role names the check; it never supplies a
/// command or an argument.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckSpec {
    /// What the Reviews tab calls it. Falls back to the table key.
    #[serde(default)]
    pub label: Option<String>,
    /// The program and its arguments. Never a shell string, for the reason `Invocation::argv`
    /// gives; write `["sh", "-c", "…"]` to ask for a shell.
    pub run: Vec<String>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// Services the check needs running, such as `web`: ids of `[[environment.service]]`, which
    /// config loading holds it to. The check runs only once every one of them answers its health
    /// check; until then it is recorded as blocked, with the missing services named, and never as a
    /// pass or a quiet skip. A browser test against nothing would fail for a reason that says
    /// nothing about the change, and passing it would be worse.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub needs: Vec<String>,
    /// What happens when the services in `needs` are not running. `None` takes the workflow's
    /// [`WorkflowSpec::start_services`], and that defaults to asking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_services: Option<StartServices>,
}

/// What a check, or a runtime role, does about services it needs that are not running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartServices {
    /// Record it blocked and ask you, once per run, whether to start them and run what they block.
    /// The default, because a start runs processes on your machine at a model's request.
    #[default]
    Ask,
    /// Start them, wait for their health checks, and run. Only for checks your own config says may.
    Auto,
}

/// How long a check may run when its config does not say.
pub const DEFAULT_CHECK_TIMEOUT_MS: u64 = 10 * 60 * 1000;

impl CheckSpec {
    #[must_use]
    pub fn timeout_ms(&self) -> u64 {
        self.timeout_ms.unwrap_or(DEFAULT_CHECK_TIMEOUT_MS)
    }
}

/// The CLIs a role can run on.
///
/// Cursor is absent on purpose: Cursor roles are deferred, because its CLI cannot run under the
/// authority profiles a run session needs. Naming it is a config error rather than a role that
/// fails at start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleProvider {
    Claude,
    Codex,
}

impl RoleProvider {
    /// The catalogue id wtm uses everywhere else for this provider.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

impl std::fmt::Display for RoleProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Claude => "Claude",
            Self::Codex => "Codex",
        })
    }
}

/// How much authority a role runs with. The engine builds the actual profile; this only says
/// which of the two it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleProfile {
    /// Reads and reports. Writes nothing, anywhere.
    #[default]
    Reviewer,
    /// Edits the worktree. Commits and pushes still belong to the app.
    Writer,
}

impl RoleProfile {
    /// The config spelling, which is also what a run session's bridge is told it is.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Reviewer => "reviewer",
            Self::Writer => "writer",
        }
    }
}

/// What a role examines: the code, or the app the code runs as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleKind {
    /// Reads the change: a snapshot of the code, or the worktree.
    #[default]
    Static,
    /// Exercises the running app the worktree serves, after the static roles, and reports what it
    /// saw happen. See `wtm_core::flow::runtime`.
    Runtime,
}

impl RoleKind {
    // By reference, because serde's `skip_serializing_if` hands it one.
    #[allow(clippy::trivially_copy_pass_by_ref)]
    fn is_static(&self) -> bool {
        *self == Self::Static
    }
}

/// How a runtime role exercises the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeDriver {
    /// Through the project's own Playwright suite: the role's checks, which the app runs for it.
    Playwright,
    /// By driving a browser pane wtm opens on its service's verified preview, in a store of its
    /// own that is thrown away with the pane.
    Browser,
}

/// Which files a role reads while it works.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Workspace {
    /// A detached checkout of the snapshot. You can keep editing the worktree meanwhile, and what
    /// the role reports describes exactly the tree it read.
    Snapshot,
    /// The worktree itself, with its environment, for a role that has to run the code. Its results
    /// go stale if the worktree changes while it runs, which the end-of-run tree check detects.
    Worktree,
}

/// What a role is handed when it starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleInput {
    Issue,
    Plan,
    Design,
    /// The snapshot's patch against the base.
    Diff,
    /// Your rules, narrowed by the role's [`KnowledgeScope`].
    Rules,
    /// The repository's own instruction sources, such as its review guide.
    Instructions,
    /// Findings from earlier runs of this work.
    Findings,
    Answers,
}

/// What a role hands back, and therefore which schema its submission is checked against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputKind {
    #[default]
    Findings,
    Plan,
    Handoff,
}

/// What happens when a role fails after its retries, or its provider runs out of capacity.
///
/// Written as `"ask"`, the id of one role to run instead, or a list of up to
/// [`MAX_FALLBACKS`](super::capacity::MAX_FALLBACKS) role ids tried in order. That makes `ask` a
/// reserved role id, which validation enforces.
///
/// One id is followed through that role's own `fallback`, which is how a chain was written before
/// lists existed. A list is the whole chain: the roles in it are tried in its order, and their own
/// fallbacks are not consulted, so what a role falls back to is read in one place.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "FallbackSpelling", into = "FallbackSpelling")]
pub enum Fallback {
    /// Put it in front of you. The default, because switching provider changes what a review is
    /// worth and is not something to do on a role's behalf unless you said so.
    #[default]
    Ask,
    /// Run this role instead, from a summary of where the first one got to.
    Role(String),
    /// Run these roles instead, the first that can, each from a summary of where the run got to.
    Chain(Vec<String>),
}

/// How `fallback` is written in TOML: one string, or a list of them.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum FallbackSpelling {
    One(String),
    Many(Vec<String>),
}

impl From<FallbackSpelling> for Fallback {
    fn from(value: FallbackSpelling) -> Self {
        match value {
            FallbackSpelling::One(value) if value == ASK => Self::Ask,
            FallbackSpelling::One(value) => Self::Role(value),
            FallbackSpelling::Many(roles) => Self::Chain(roles),
        }
    }
}

impl From<Fallback> for FallbackSpelling {
    fn from(value: Fallback) -> Self {
        match value {
            Fallback::Ask => Self::One(ASK.to_owned()),
            Fallback::Role(role) => Self::One(role),
            Fallback::Chain(roles) => Self::Many(roles),
        }
    }
}

const ASK: &str = "ask";

/// Which of your rules a role receives. Rules scoped to the paths a change touches always qualify;
/// this adds area tags and caps the count.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeScope {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub areas: Vec<String>,
    /// At most this many, required rules first. `None` leaves the engine's own cap.
    #[serde(default)]
    pub max_rules: Option<u32>,
}

impl KnowledgeScope {
    fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

/// One role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleSpec {
    /// One line: what this role is for. The first thing its brief says.
    pub objective: String,
    /// Everything else the role should be told.
    #[serde(default)]
    pub instructions: Option<String>,
    pub provider: RoleProvider,
    /// In the provider's own spelling, and never checked here, for the reason `AgentSpec::model`
    /// gives: a config outlives the CLI version it was written against.
    #[serde(default)]
    pub model: Option<String>,
    /// Also the provider's own spelling. The ladder is per model, so no enum could hold it.
    #[serde(default)]
    pub effort: Option<String>,
    /// Skills the role may invoke, with or without the leading `/`.
    ///
    /// Named here rather than inherited from wherever the provider would find them, because a run
    /// session does not see your usual setup. Codex runs get a home of their own so that your MCP
    /// servers and plugins stay out, and that also leaves your personal skills behind. The engine
    /// supplies exactly these.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
    /// MCP servers the session may load, by name. Empty means none beyond the run's own bridge.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp: Vec<String>,
    /// Plugins the session may load, by name. Empty means none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plugins: Vec<String>,
    #[serde(default, skip_serializing_if = "KnowledgeScope::is_default")]
    pub knowledge: KnowledgeScope,
    /// Globs, relative to the repository root, that this role is about. Empty means the whole
    /// change. A role whose globs match none of the changed files is skipped for that run.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    /// Checks from `[workflow.check]` this role may ask the app to run. Empty means none: a check
    /// costs real time on your machine, so a role is given the ones its objective needs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<String>,
    pub inputs: Vec<RoleInput>,
    #[serde(default)]
    pub output: OutputKind,
    #[serde(default)]
    pub profile: RoleProfile,
    /// Defaults to the snapshot for a reviewer and the worktree for a writer. See
    /// [`Self::workspace`].
    #[serde(default)]
    pub workspace: Option<Workspace>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    /// Further attempts after a failure, at most [`MAX_RETRIES`].
    #[serde(default)]
    pub retries: u32,
    #[serde(default)]
    pub fallback: Fallback,
    /// Whether this role may run a model that bills usage credits rather than your plan's included
    /// limits (Fable, under Claude Code's headless mode; see
    /// [`bills_credits`](super::capacity::bills_credits)). Off unless you say so, for this role
    /// alone: a run session has nobody watching it to see a credits prompt, and headless Claude
    /// Code does not show one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_credit_billing: bool,
    /// Static, the default, or runtime: a role that exercises the running app.
    #[serde(default, skip_serializing_if = "RoleKind::is_static")]
    pub kind: RoleKind,
    /// For a runtime role: the `[[environment.service]]` whose verified preview its browser pane
    /// opens on. Config loading holds it to a declared service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    /// For a runtime role: how it exercises the app. `None` uses the project's Playwright when the
    /// worktree's `package.json` declares `@playwright/test` and the role has checks, and a browser
    /// pane otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver: Option<RuntimeDriver>,
}

impl RoleSpec {
    /// Where the role reads. A runtime role always reads the worktree, because the app it
    /// exercises runs from there.
    #[must_use]
    pub fn workspace(&self) -> Workspace {
        if self.kind == RoleKind::Runtime {
            return Workspace::Worktree;
        }
        self.workspace.unwrap_or(match self.profile {
            RoleProfile::Reviewer => Workspace::Snapshot,
            RoleProfile::Writer => Workspace::Worktree,
        })
    }

    #[must_use]
    pub fn timeout_ms(&self) -> u64 {
        self.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS)
    }

    /// The skills as the providers spell them in their lists: without the leading `/`.
    pub fn skill_names(&self) -> impl Iterator<Item = &str> {
        self.skills.iter().map(|skill| skill_name(skill))
    }
}

/// A config mistake in `[workflow]`, with the key it is about.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize)]
#[error("workflow.{key}: {message}")]
pub struct WorkflowError {
    /// Relative to `workflow`, such as `role.conventions.fallback`.
    pub key: String,
    pub message: String,
}

impl WorkflowError {
    pub(crate) fn new(key: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            message: message.into(),
        }
    }
}

impl WorkflowSpec {
    /// Everything the file alone can get wrong, first problem only.
    ///
    /// Fail-fast like the rest of config validation: one misspelt role id makes every stage and
    /// fallback that names it look wrong too, and a list would mostly be that one mistake repeated.
    pub fn validate(&self) -> Result<(), WorkflowError> {
        for branch in &self.integration_branches {
            // `refs/heads/main` or `origin/main` would never equal a pull request's base, which
            // the forge names bare, so the entry would silently match nothing.
            if branch.is_empty()
                || branch.starts_with('/')
                || branch.starts_with("refs/")
                || branch.chars().any(char::is_whitespace)
            {
                return Err(WorkflowError::new(
                    "integration_branches",
                    format!(
                        "`{branch}` is not a branch name or pattern; write it as the forge \
                         names it, such as `main` or `release/*`"
                    ),
                ));
            }
        }
        for (id, role) in &self.role {
            check_id(&format!("role.{id}"), id)?;
            if id == ASK {
                return Err(WorkflowError::new(
                    format!("role.{id}"),
                    "`ask` is reserved: `fallback = \"ask\"` means put the failure in front of you",
                ));
            }
            self.check_role(id, role)?;
        }
        for (id, recipe) in &self.recipe {
            check_id(&format!("recipe.{id}"), id)?;
            self.check_recipe(id, recipe)?;
        }
        for (id, check) in &self.check {
            check_id(&format!("check.{id}"), id)?;
            if check
                .run
                .first()
                .is_none_or(|program| program.trim().is_empty())
            {
                return Err(WorkflowError::new(
                    format!("check.{id}.run"),
                    "a check needs a program to run, as `run = [\"just\", \"test\"]`",
                ));
            }
            if check.timeout_ms == Some(0) {
                return Err(WorkflowError::new(
                    format!("check.{id}.timeout_ms"),
                    "a check needs some time to run",
                ));
            }
            if check.needs.iter().any(|service| service.trim().is_empty()) {
                return Err(WorkflowError::new(
                    format!("check.{id}.needs"),
                    "a service name cannot be empty",
                ));
            }
        }
        for pattern in self.instruction_sources.iter().flatten() {
            glob::check(pattern).map_err(|message| {
                WorkflowError::new("instruction_sources", format!("`{pattern}`: {message}"))
            })?;
        }
        if let Some(planner) = &self.planner {
            self.check_planner(planner)?;
        }
        self.statuses.validate()?;
        tracker::validate_fields(&self.fields)?;
        if let Some(queue) = &self.queue {
            tracker::validate_queue(queue)?;
        }
        Ok(())
    }

    /// The instruction sources a review reads: the configured list, or the default one.
    #[must_use]
    pub fn instruction_sources(&self) -> Vec<String> {
        self.instruction_sources.clone().unwrap_or_else(|| {
            DEFAULT_INSTRUCTION_SOURCES
                .iter()
                .map(|source| (*source).to_owned())
                .collect()
        })
    }

    /// What `check` does about services it needs that are not running: its own setting, then the
    /// workflow's, then asking.
    #[must_use]
    pub fn start_policy(&self, check: &str) -> StartServices {
        self.check
            .get(check)
            .and_then(|spec| spec.start_services)
            .or(self.start_services)
            .unwrap_or_default()
    }

    fn check_role(&self, id: &str, role: &RoleSpec) -> Result<(), WorkflowError> {
        let key = |field: &str| format!("role.{id}.{field}");

        if role.objective.trim().is_empty() {
            return Err(WorkflowError::new(
                key("objective"),
                "a role needs an objective: it is the first line of the role's brief",
            ));
        }
        for skill in &role.skills {
            let name = skill_name(skill);
            if name.is_empty() || name.chars().any(char::is_whitespace) {
                return Err(WorkflowError::new(
                    key("skills"),
                    format!("`{skill}` is not a skill name"),
                ));
            }
        }
        for (field, names) in [("mcp", &role.mcp), ("plugins", &role.plugins)] {
            if names.iter().any(|name| name.trim().is_empty()) {
                return Err(WorkflowError::new(key(field), "a name cannot be empty"));
            }
        }
        for pattern in &role.paths {
            glob::check(pattern)
                .map_err(|why| WorkflowError::new(key("paths"), format!("`{pattern}`: {why}")))?;
        }
        if let Some(missing) = role
            .checks
            .iter()
            .find(|name| !self.check.contains_key(*name))
        {
            return Err(WorkflowError::new(
                key("checks"),
                format!(
                    "there is no check `{missing}`; declare it as `[workflow.check.{missing}]`"
                ),
            ));
        }
        if role.inputs.is_empty() {
            return Err(WorkflowError::new(
                key("inputs"),
                "a role with no inputs has nothing to work from; list some of issue, plan, design, \
                 diff, rules, instructions, findings, answers",
            ));
        }
        if !role.knowledge.is_default() && !role.inputs.contains(&RoleInput::Rules) {
            return Err(WorkflowError::new(
                key("knowledge"),
                "a knowledge scope narrows the rules a role receives, and this role does not \
                 receive rules; add `rules` to its inputs",
            ));
        }
        if role
            .knowledge
            .areas
            .iter()
            .any(|area| area.trim().is_empty())
        {
            return Err(WorkflowError::new(
                key("knowledge.areas"),
                "an area cannot be empty",
            ));
        }
        if role.knowledge.max_rules == Some(0) {
            return Err(WorkflowError::new(
                key("knowledge.max_rules"),
                "a cap of zero gives the role no rules; remove `rules` from its inputs instead",
            ));
        }
        if role.timeout_ms == Some(0) {
            return Err(WorkflowError::new(
                key("timeout_ms"),
                "a role needs some time to run",
            ));
        }
        if role.retries > MAX_RETRIES {
            return Err(WorkflowError::new(
                key("retries"),
                format!(
                    "at most {MAX_RETRIES}: each retry is a whole turn against your usage window"
                ),
            ));
        }
        if role.profile == RoleProfile::Writer && role.workspace() == Workspace::Snapshot {
            return Err(WorkflowError::new(
                key("workspace"),
                "a writer's edits have to land in the worktree; the snapshot checkout is removed \
                 when the stage ends",
            ));
        }
        Self::check_runtime(id, role)?;
        self.check_fallback(id, role)
    }

    fn check_runtime(id: &str, role: &RoleSpec) -> Result<(), WorkflowError> {
        let key = |field: &str| format!("role.{id}.{field}");
        if role.kind == RoleKind::Static {
            for (field, set) in [
                ("service", role.service.is_some()),
                ("driver", role.driver.is_some()),
            ] {
                if set {
                    return Err(WorkflowError::new(
                        key(field),
                        "only a runtime role exercises a service; add `kind = \"runtime\"`",
                    ));
                }
            }
            return Ok(());
        }
        if role.profile == RoleProfile::Writer {
            return Err(WorkflowError::new(
                key("profile"),
                "a runtime role reads and reports what the app does; a writer is a separate role",
            ));
        }
        if role.workspace == Some(Workspace::Snapshot) {
            return Err(WorkflowError::new(
                key("workspace"),
                "a runtime role exercises the app the worktree serves, so it reads the worktree; \
                 leave `workspace` out",
            ));
        }
        if role
            .service
            .as_ref()
            .is_some_and(|service| service.trim().is_empty())
        {
            return Err(WorkflowError::new(
                key("service"),
                "a service name cannot be empty",
            ));
        }
        match role.driver {
            Some(RuntimeDriver::Playwright) if role.checks.is_empty() => Err(WorkflowError::new(
                key("checks"),
                "a Playwright runtime role runs its checks; name the `[workflow.check]` that runs \
                 your end-to-end suite",
            )),
            // Without Playwright the role drives a browser pane, which opens on a service's preview.
            Some(RuntimeDriver::Browser) | None if role.service.is_none() => {
                Err(WorkflowError::new(
                    key("service"),
                    "a runtime role that drives a browser pane needs the service whose preview it \
                     opens, such as `service = \"web\"`",
                ))
            }
            _ => Ok(()),
        }
    }

    /// The planner is a reviewer that hands in a plan, and it reads the issue's worktree.
    ///
    /// Read-only because planning comes before any approval, and the plan's policy (§4.2) lets
    /// nothing write until a plan is approved. The worktree rather than a snapshot because its
    /// conversation is resumed in the same directory, possibly hours later, when a question is
    /// answered, and a snapshot checkout is removed when its stage ends.
    fn check_planner(&self, planner: &str) -> Result<(), WorkflowError> {
        let Some(role) = self.role.get(planner) else {
            return Err(WorkflowError::new(
                "planner",
                format!("there is no role `{planner}`; declare it as `[workflow.role.{planner}]`"),
            ));
        };
        if role.output != OutputKind::Plan {
            return Err(WorkflowError::new(
                "planner",
                format!(
                    "`{planner}` hands back findings or a handoff; a planner's `output` is `plan`"
                ),
            ));
        }
        if role.profile != RoleProfile::Reviewer {
            return Err(WorkflowError::new(
                "planner",
                format!(
                    "`{planner}` is a writer; a planner reads and plans, and nothing writes before \
                     a plan is approved"
                ),
            ));
        }
        if role.workspace == Some(Workspace::Snapshot) {
            return Err(WorkflowError::new(
                format!("role.{planner}.workspace"),
                "a planner reads the issue's worktree: its conversation is resumed there when you \
                 answer a question, and a snapshot checkout is gone by then",
            ));
        }
        Ok(())
    }

    fn check_fallback(&self, id: &str, role: &RoleSpec) -> Result<(), WorkflowError> {
        let key = format!("role.{id}.fallback");
        let named: Vec<&String> = match &role.fallback {
            Fallback::Ask => return Ok(()),
            Fallback::Role(alternative) => vec![alternative],
            Fallback::Chain(roles) => {
                if roles.is_empty() {
                    return Err(WorkflowError::new(
                        key,
                        "an empty list falls back to nothing; write `fallback = \"ask\"`",
                    ));
                }
                if roles.len() > MAX_FALLBACKS {
                    return Err(WorkflowError::new(
                        key,
                        format!(
                            "at most {MAX_FALLBACKS} alternatives: each is a whole role on another \
                             account, usually with nobody watching"
                        ),
                    ));
                }
                if roles.iter().any(|r| r == ASK) {
                    return Err(WorkflowError::new(
                        key,
                        "`ask` is what happens after the last alternative; leave it out of the list",
                    ));
                }
                let mut once = BTreeSet::new();
                if let Some(twice) = roles.iter().find(|r| !once.insert(r.as_str())) {
                    return Err(WorkflowError::new(
                        key,
                        format!("`{twice}` is listed twice; it would only ever run once"),
                    ));
                }
                roles.iter().collect()
            }
        };
        for alternative in named {
            self.check_alternative(&key, id, role, alternative)?;
        }
        if let Fallback::Role(alternative) = &role.fallback {
            let mut seen = BTreeSet::from([id]);
            let mut next = alternative.as_str();
            loop {
                if !seen.insert(next) {
                    return Err(WorkflowError::new(
                        key,
                        format!(
                            "the fallbacks from `{id}` lead back to `{next}` and would never end"
                        ),
                    ));
                }
                match self.role.get(next).map(|role| &role.fallback) {
                    Some(Fallback::Role(after)) => next = after,
                    _ => break,
                }
            }
        }
        if self.chain_from(id).len() > MAX_FALLBACKS {
            return Err(WorkflowError::new(
                key,
                format!(
                    "the fallbacks from `{id}` are more than {MAX_FALLBACKS} roles long; list the \
                     ones you want, at most {MAX_FALLBACKS}, as `fallback = [...]`"
                ),
            ));
        }
        Ok(())
    }

    /// One alternative a role names: a different role that exists and stands in for it exactly.
    fn check_alternative(
        &self,
        key: &str,
        id: &str,
        role: &RoleSpec,
        alternative: &str,
    ) -> Result<(), WorkflowError> {
        if alternative == id {
            return Err(WorkflowError::new(
                key,
                "a role cannot be its own fallback; use `retries` to try it again",
            ));
        }
        let Some(target) = self.role.get(alternative) else {
            return Err(WorkflowError::new(
                key,
                format!("there is no role `{alternative}`"),
            ));
        };
        // A fallback runs because the first role could not, usually with nobody watching. Letting
        // it carry more authority would make "the reviewer ran out of capacity" a way to get a
        // writer, and a different output kind would hand consolidation something it cannot read.
        if target.profile != role.profile {
            return Err(WorkflowError::new(
                key,
                format!("`{alternative}` runs with a different profile, and a fallback may not"),
            ));
        }
        if target.output != role.output {
            return Err(WorkflowError::new(
                key,
                format!("`{alternative}` hands back a different kind of output"),
            ));
        }
        if target.kind != role.kind {
            return Err(WorkflowError::new(
                key,
                format!(
                    "`{alternative}` is a different kind of role: a fallback reads what the role \
                     it stands in for reads"
                ),
            ));
        }
        Ok(())
    }

    fn check_recipe(&self, id: &str, recipe: &RecipeSpec) -> Result<(), WorkflowError> {
        if recipe.concurrency == Some(0) {
            return Err(WorkflowError::new(
                format!("recipe.{id}.concurrency"),
                "a cap of zero would run nothing; leave it out to run each stage whole",
            ));
        }
        let key = format!("recipe.{id}.stage");
        if recipe.stages.is_empty() {
            return Err(WorkflowError::new(key, "a recipe needs at least one stage"));
        }

        // A runtime role exercises what the static roles have finished reading, so no static role
        // may run beside it or after it. The last static stage is where runtime stages may start.
        let last_static = recipe.stages.iter().rposition(|stage| {
            stage.roles.iter().any(|name| {
                self.role
                    .get(name)
                    .is_some_and(|r| r.kind == RoleKind::Static)
            })
        });
        let mut used = BTreeSet::new();
        for (index, stage) in recipe.stages.iter().enumerate() {
            if last_static.is_some_and(|last| index <= last)
                && let Some(runtime) = stage.roles.iter().find(|name| {
                    self.role
                        .get(*name)
                        .is_some_and(|r| r.kind == RoleKind::Runtime)
                })
            {
                return Err(WorkflowError::new(
                    format!("{key}[{index}].roles"),
                    format!(
                        "`{runtime}` is a runtime role, and runtime roles run after the static \
                         ones; move it to a stage after the last static role"
                    ),
                ));
            }
            let key = format!("{key}[{index}].roles");
            if stage.roles.is_empty() {
                return Err(WorkflowError::new(key, "a stage needs at least one role"));
            }

            let mut roles = Vec::with_capacity(stage.roles.len());
            for name in &stage.roles {
                let Some(role) = self.role.get(name) else {
                    return Err(WorkflowError::new(
                        key,
                        format!("there is no role `{name}`"),
                    ));
                };
                // A role's findings, its reruns and its fallback are all keyed by its id within a
                // run, so the same role twice would be two results with one name.
                if !used.insert(name.as_str()) {
                    return Err(WorkflowError::new(
                        key,
                        format!("`{name}` appears twice in this recipe; define a second role"),
                    ));
                }
                roles.push(role);
            }

            let writers = roles
                .iter()
                .filter(|role| role.profile == RoleProfile::Writer)
                .count();
            if writers > 1 {
                return Err(WorkflowError::new(
                    key,
                    "two writers in one stage would edit the same worktree at once",
                ));
            }
            let live_readers = roles
                .iter()
                .filter(|role| {
                    role.profile == RoleProfile::Reviewer && role.workspace() == Workspace::Worktree
                })
                .count();
            if writers == 1 && live_readers > 0 {
                return Err(WorkflowError::new(
                    key,
                    "a role reading the worktree beside a writer would always finish stale; move \
                     it to the next stage or let it read the snapshot",
                ));
            }
        }
        Ok(())
    }

    /// The roles a recipe can end up running: its stages' roles and every fallback they lead to.
    pub(crate) fn reachable_roles(&self, recipe: &RecipeSpec) -> Vec<(&str, &RoleSpec)> {
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for name in recipe.stages.iter().flat_map(|stage| &stage.roles) {
            let chain = self.fallback_chain(name);
            for id in std::iter::once(name).chain(&chain) {
                let Some((id, role)) = self.role.get_key_value(id.as_str()) else {
                    continue;
                };
                if seen.insert(id.as_str()) {
                    out.push((id.as_str(), role));
                }
            }
        }
        out
    }

    /// The fallbacks a role leads to, in the order they would be tried, at most
    /// [`MAX_FALLBACKS`]. `ask` is implied after the last one.
    ///
    /// The chain is always the role's own: when its first alternative also runs out, the engine
    /// takes the next from this list, not from the alternative's config.
    #[must_use]
    pub fn fallback_chain(&self, role: &str) -> Vec<String> {
        let mut chain = self.chain_from(role);
        chain.truncate(MAX_FALLBACKS);
        chain
    }

    /// [`Self::fallback_chain`] without the cap, so validation can say a chain is too long.
    fn chain_from(&self, role: &str) -> Vec<String> {
        let mut seen = BTreeSet::from([role.to_owned()]);
        let mut chain = Vec::new();
        let mut push = |id: &str, chain: &mut Vec<String>| {
            if seen.insert(id.to_owned()) {
                chain.push(id.to_owned());
                true
            } else {
                false
            }
        };
        let mut next = self.role.get(role).map(|role| &role.fallback);
        while let Some(fallback) = next {
            match fallback {
                Fallback::Ask => break,
                Fallback::Chain(roles) => {
                    for id in roles {
                        push(id, &mut chain);
                    }
                    break;
                }
                // One id: followed through that role's own fallback, as chains were first written.
                Fallback::Role(id) => {
                    if !push(id, &mut chain) {
                        break;
                    }
                    next = self.role.get(id).map(|role| &role.fallback);
                }
            }
        }
        chain
    }

    /// Whether every skill a recipe's roles name is one their own provider lists.
    ///
    /// Every problem is reported, not just the first: unlike a config typo, a missing skill in one
    /// role says nothing about the next, and the screen that shows this needs the whole list to
    /// say which roles cannot start.
    pub fn check_skills(&self, recipe: &str, catalog: &SkillCatalog) -> Result<(), SkillError> {
        let Some(spec) = self.recipe.get(recipe) else {
            return Err(SkillError::UnknownRecipe(recipe.to_owned()));
        };

        let mut problems = Vec::new();
        for (id, role) in self.reachable_roles(spec) {
            if role.skills.is_empty() {
                continue;
            }
            let Some(listed) = catalog.listed.get(&role.provider) else {
                problems.push(SkillProblem::Unlisted {
                    role: id.to_owned(),
                    provider: role.provider,
                });
                continue;
            };
            for skill in role.skill_names() {
                if !listed.contains(skill) {
                    problems.push(SkillProblem::Missing {
                        role: id.to_owned(),
                        provider: role.provider,
                        skill: skill.to_owned(),
                    });
                }
            }
        }

        if problems.is_empty() {
            Ok(())
        } else {
            Err(SkillError::Problems(problems))
        }
    }
}

/// The skills each provider reported for the worktree a recipe is about to run in.
///
/// # Only what a provider said, never wtm's guess
///
/// Fill this from the provider's own answer: Claude's `initialize` reply or Codex's `skills/list`,
/// asked by a session started the way the role will run. Not from `wtm_agent::skills`, which reads
/// skill directories to seed the composer before a session speaks. That seed is acceptable because
/// the provider corrects it within a turn; it would stop being acceptable the moment a run trusted
/// it, because a skill it lists and the provider does not would pass this check and fail mid-run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkillCatalog {
    listed: BTreeMap<RoleProvider, BTreeSet<String>>,
}

impl SkillCatalog {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record what `provider` reported, replacing anything it reported before.
    #[must_use]
    pub fn with(mut self, provider: RoleProvider, skills: &[AgentSkill]) -> Self {
        self.listed.insert(
            provider,
            skills
                .iter()
                .map(|skill| skill_name(&skill.name).to_owned())
                .collect(),
        );
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SkillError {
    #[error("there is no recipe `{0}`")]
    UnknownRecipe(String),
    #[error("{}", describe(.0))]
    Problems(Vec<SkillProblem>),
}

/// One role that cannot start for want of a skill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SkillProblem {
    /// The provider answered, and the skill was not in its answer.
    Missing {
        role: String,
        provider: RoleProvider,
        skill: String,
    },
    /// The provider has not answered, so the role's skills cannot be checked. Refused rather than
    /// waved through: an unchecked skill is the failure this whole check exists to move earlier.
    Unlisted {
        role: String,
        provider: RoleProvider,
    },
}

impl std::fmt::Display for SkillProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing {
                role,
                provider,
                skill,
            } => write!(
                f,
                "{provider} does not list the skill `{skill}` that role `{role}` uses"
            ),
            Self::Unlisted { role, provider } => write!(
                f,
                "{provider} has not reported its skills, so role `{role}` cannot be checked"
            ),
        }
    }
}

fn describe(problems: &[SkillProblem]) -> String {
    problems
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

/// A skill as both CLIs list it. People write `/team-review` because that is how they type it.
fn skill_name(raw: &str) -> &str {
    raw.trim().trim_start_matches('/')
}

impl WorkflowSpec {
    /// Whether `branch` is one of the integration branches, by name or by pattern.
    #[must_use]
    pub fn is_integration_branch(&self, branch: &str) -> bool {
        self.integration_branches
            .iter()
            .any(|pattern| glob::matches(pattern, branch))
    }

    /// The integration branches named outright. A pattern names no branch a forge can be asked
    /// about, so its protection is read only once it matches a pull request's base.
    pub fn named_integration_branches(&self) -> impl Iterator<Item = &str> {
        self.integration_branches
            .iter()
            .map(String::as_str)
            .filter(|branch| !branch.contains(['*', '?']))
    }
}

/// Same rule as database ids: the id ends up in keys, logs and directory names.
fn check_id(key: &str, id: &str) -> Result<(), WorkflowError> {
    if id.is_empty()
        || !id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
    {
        return Err(WorkflowError::new(
            key,
            "an id may contain only letters, digits, underscores and hyphens",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// Two reviewers on different providers in one stage, then a consolidator. The shape the
    /// review slice is built around.
    fn review() -> serde_json::Value {
        json!({
            "role": {
                "conventions": {
                    "objective": "Check the change against the instruction sources and rules",
                    "provider": "claude",
                    "model": "opus",
                    "inputs": ["diff", "instructions", "rules"],
                    "knowledge": { "areas": ["ui"], "max_rules": 20 },
                    "fallback": "conventions-codex"
                },
                "conventions-codex": {
                    "objective": "The same check, on Codex",
                    "provider": "codex",
                    "inputs": ["diff", "instructions", "rules"]
                },
                "correctness": {
                    "objective": "Find bugs the change introduces",
                    "provider": "codex",
                    "effort": "high",
                    "skills": ["/team-review"],
                    "inputs": ["diff"]
                },
                "consolidate": {
                    "objective": "Merge the findings",
                    "provider": "claude",
                    "inputs": ["findings"]
                }
            },
            "recipe": {
                "review": {
                    "stage": [
                        { "roles": ["conventions", "correctness"] },
                        { "roles": ["consolidate"] }
                    ]
                }
            }
        })
    }

    fn workflow(value: serde_json::Value) -> WorkflowSpec {
        serde_json::from_value(value).unwrap()
    }

    /// `review()` with `patch` merged into it, one level deep per role.
    fn review_with(patch: &serde_json::Value) -> WorkflowSpec {
        let mut base = review();
        for (section, entries) in patch.as_object().unwrap() {
            for (id, fields) in entries.as_object().unwrap() {
                let slot = &mut base[section][id];
                if slot.is_null() {
                    *slot = fields.clone();
                } else {
                    for (field, value) in fields.as_object().unwrap() {
                        slot[field] = value.clone();
                    }
                }
            }
        }
        workflow(base)
    }

    fn refusal(spec: &WorkflowSpec) -> WorkflowError {
        spec.validate().unwrap_err()
    }

    fn with_planner(fields: &serde_json::Value) -> WorkflowSpec {
        let mut spec = review_with(&json!({ "role": { "planner": fields } }));
        spec.planner = Some("planner".to_owned());
        spec
    }

    #[test]
    fn a_planner_is_a_reviewer_that_hands_in_a_plan_from_the_worktree() {
        let planner = json!({
            "objective": "Investigate the issue and plan it",
            "provider": "claude",
            "inputs": ["issue", "instructions", "rules"],
            "output": "plan",
        });
        with_planner(&planner).validate().unwrap();

        let mut findings = planner.clone();
        findings["output"] = json!("findings");
        assert_eq!(refusal(&with_planner(&findings)).key, "planner");

        let mut writer = planner.clone();
        writer["profile"] = json!("writer");
        writer["workspace"] = json!("worktree");
        assert!(refusal(&with_planner(&writer)).message.contains("writer"));

        let mut snapshot = planner;
        snapshot["workspace"] = json!("snapshot");
        assert_eq!(
            refusal(&with_planner(&snapshot)).key,
            "role.planner.workspace"
        );

        let mut missing = review_with(&json!({}));
        missing.planner = Some("nobody".to_owned());
        assert!(refusal(&missing).message.contains("no role `nobody`"));
    }

    fn skills(names: &[&str]) -> Vec<AgentSkill> {
        names
            .iter()
            .map(|name| AgentSkill {
                name: (*name).to_owned(),
                description: None,
                scope: None,
            })
            .collect()
    }

    #[test]
    fn a_review_recipe_with_parallel_roles_on_two_providers_is_valid() {
        workflow(review()).validate().unwrap();
    }

    #[test]
    fn a_role_may_name_only_checks_the_workflow_declares() {
        let spec = review_with(&json!({
            "check": { "unit": { "run": ["just", "test"] } },
            "role": { "correctness": { "checks": ["unit", "e2e"] } },
        }));
        let error = refusal(&spec);
        assert_eq!(error.key, "role.correctness.checks");
        assert!(error.message.contains("`e2e`"), "{error}");

        review_with(&json!({
            "check": { "unit": { "run": ["just", "test"] } },
            "role": { "correctness": { "checks": ["unit"] } },
        }))
        .validate()
        .unwrap();
    }

    #[test]
    fn a_check_with_nothing_to_run_is_refused() {
        let spec = review_with(&json!({ "check": { "unit": { "run": [] } } }));
        assert_eq!(refusal(&spec).key, "check.unit.run");
        let spec = review_with(&json!({ "check": { "unit": { "run": ["  "] } } }));
        assert_eq!(refusal(&spec).key, "check.unit.run");
    }

    #[test]
    fn a_check_names_the_services_it_needs_and_defaults_its_timeout() {
        let spec = review_with(&json!({
            "check": { "e2e": { "run": ["just", "e2e"], "needs": ["web"] } },
        }));
        spec.validate().unwrap();
        assert_eq!(spec.check["e2e"].needs, ["web"]);
        assert_eq!(spec.check["e2e"].timeout_ms(), DEFAULT_CHECK_TIMEOUT_MS);
    }

    #[test]
    fn a_check_asks_before_starting_services_unless_its_config_or_the_projects_says_auto() {
        let spec = review_with(&json!({
            "check": {
                "e2e": { "run": ["just", "e2e"], "needs": ["web"] },
                "smoke": { "run": ["just", "smoke"], "needs": ["web"], "start_services": "auto" },
            },
        }));
        assert_eq!(spec.start_policy("e2e"), StartServices::Ask);
        assert_eq!(spec.start_policy("smoke"), StartServices::Auto);

        let mut project_auto = spec.clone();
        project_auto.start_services = Some(StartServices::Auto);
        assert_eq!(project_auto.start_policy("e2e"), StartServices::Auto);
        project_auto.check.get_mut("smoke").unwrap().start_services = Some(StartServices::Ask);
        assert_eq!(project_auto.start_policy("smoke"), StartServices::Ask);
    }

    /// `review()` with a runtime role after its two stages.
    fn with_runtime(runtime: &serde_json::Value, stages: &serde_json::Value) -> WorkflowSpec {
        let mut base = review();
        base["role"]["clickthrough"] = runtime.clone();
        base["recipe"]["review"]["stage"] = stages.clone();
        workflow(base)
    }

    fn clickthrough() -> serde_json::Value {
        json!({
            "objective": "Click through what the change touches",
            "provider": "claude",
            "kind": "runtime",
            "service": "web",
            "inputs": ["diff"],
        })
    }

    #[test]
    fn a_runtime_role_runs_after_every_static_role_and_reads_the_worktree() {
        let after = with_runtime(
            &clickthrough(),
            &json!([
                { "roles": ["conventions", "correctness"] },
                { "roles": ["consolidate"] },
                { "roles": ["clickthrough"] },
            ]),
        );
        after.validate().unwrap();
        assert_eq!(after.role["clickthrough"].workspace(), Workspace::Worktree);

        let beside = with_runtime(
            &clickthrough(),
            &json!([
                { "roles": ["conventions", "correctness", "clickthrough"] },
                { "roles": ["consolidate"] },
            ]),
        );
        let error = refusal(&beside);
        assert_eq!(error.key, "recipe.review.stage[0].roles");
        assert!(
            error.message.contains("after the last static role"),
            "{error}"
        );
    }

    #[test]
    fn a_runtime_role_that_may_drive_a_pane_names_its_service_and_never_reads_a_snapshot() {
        let stages = json!([
            { "roles": ["conventions", "correctness"] },
            { "roles": ["clickthrough"] },
        ]);
        let mut no_service = clickthrough();
        no_service.as_object_mut().unwrap().remove("service");
        assert_eq!(
            refusal(&with_runtime(&no_service, &stages)).key,
            "role.clickthrough.service"
        );

        let mut snapshot = clickthrough();
        snapshot["workspace"] = json!("snapshot");
        assert_eq!(
            refusal(&with_runtime(&snapshot, &stages)).key,
            "role.clickthrough.workspace"
        );

        let mut playwright = no_service.clone();
        playwright["driver"] = json!("playwright");
        assert_eq!(
            refusal(&with_runtime(&playwright, &stages)).key,
            "role.clickthrough.checks"
        );

        let mut static_with_service = review();
        static_with_service["role"]["correctness"]["service"] = json!("web");
        assert_eq!(
            refusal(&workflow(static_with_service)).key,
            "role.correctness.service"
        );
    }

    #[test]
    fn a_stage_naming_a_role_that_does_not_exist_is_refused_with_its_key() {
        let spec = review_with(&json!({ "recipe": { "review": {
            "stage": [{ "roles": ["conventions", "corectness"] }]
        }}}));
        let error = refusal(&spec);
        assert_eq!(error.key, "recipe.review.stage[0].roles");
        assert!(error.message.contains("corectness"), "{error}");
    }

    #[test]
    fn a_role_cannot_appear_twice_in_one_recipe() {
        let spec = review_with(&json!({ "recipe": { "review": {
            "stage": [{ "roles": ["correctness"] }, { "roles": ["correctness"] }]
        }}}));
        assert!(refusal(&spec).message.contains("twice"));
    }

    #[test]
    fn an_empty_recipe_or_stage_is_refused() {
        let empty_recipe = review_with(&json!({ "recipe": { "review": { "stage": [] } } }));
        assert!(
            refusal(&empty_recipe)
                .message
                .contains("at least one stage")
        );

        let empty_stage =
            review_with(&json!({ "recipe": { "review": { "stage": [{ "roles": [] }] } } }));
        assert!(refusal(&empty_stage).message.contains("at least one role"));
    }

    #[test]
    fn a_concurrency_cap_of_zero_is_refused_and_one_is_kept_for_the_plan() {
        let zero = review_with(&json!({ "recipe": { "review": { "concurrency": 0 } } }));
        assert_eq!(refusal(&zero).key, "recipe.review.concurrency");

        let one = review_with(&json!({ "recipe": { "review": { "concurrency": 1 } } }));
        let planned = crate::flow::plan(&one, "review", None).unwrap();
        assert_eq!(planned.concurrency, Some(1));
    }

    #[test]
    fn ask_is_reserved_so_a_fallback_can_never_mean_two_things() {
        let spec = review_with(&json!({ "role": { "ask": {
            "objective": "x", "provider": "claude", "inputs": ["diff"]
        }}}));
        assert_eq!(refusal(&spec).key, "role.ask");
    }

    #[test]
    fn a_fallback_defaults_to_asking_and_round_trips_as_a_plain_string() {
        let spec = workflow(review());
        assert_eq!(spec.role["correctness"].fallback, Fallback::Ask);
        assert_eq!(
            spec.role["conventions"].fallback,
            Fallback::Role("conventions-codex".to_owned())
        );
        let written = serde_json::to_value(&spec.role["conventions"]).unwrap();
        assert_eq!(written["fallback"], "conventions-codex");
        let asked = serde_json::to_value(&spec.role["correctness"]).unwrap();
        assert_eq!(asked["fallback"], "ask");
    }

    #[test]
    fn a_fallback_loop_is_refused() {
        let spec = review_with(&json!({ "role": {
            "conventions-codex": { "fallback": "conventions" }
        }}));
        let error = refusal(&spec);
        assert!(error.key.ends_with(".fallback"), "{error}");
        assert!(error.message.contains("never end"), "{error}");
    }

    /// Three Claude alternatives and one Codex one beside the review's roles, for chain tests.
    fn with_alternatives(fallback: &serde_json::Value) -> WorkflowSpec {
        let alternative = |provider: &str| {
            json!({
                "objective": "Find bugs the change introduces",
                "provider": provider,
                "inputs": ["diff"]
            })
        };
        review_with(&json!({ "role": {
            "correctness": { "fallback": fallback },
            "on-claude": alternative("claude"),
            "on-haiku": alternative("claude"),
            "on-codex-mini": alternative("codex"),
            "fourth": alternative("claude"),
        }}))
    }

    #[test]
    fn a_fallback_list_is_the_whole_chain_in_the_order_written() {
        let spec = with_alternatives(&json!(["on-codex-mini", "on-claude", "on-haiku"]));
        spec.validate().unwrap();
        assert_eq!(
            spec.fallback_chain("correctness"),
            ["on-codex-mini", "on-claude", "on-haiku"]
        );
        // A listed role's own fallback is not followed: the list is read in one place.
        let mut followed = spec.clone();
        followed.role.get_mut("on-haiku").unwrap().fallback = Fallback::Role("fourth".to_owned());
        assert_eq!(
            followed.fallback_chain("correctness"),
            ["on-codex-mini", "on-claude", "on-haiku"]
        );
        let written = serde_json::to_value(&spec.role["correctness"]).unwrap();
        assert_eq!(
            written["fallback"],
            json!(["on-codex-mini", "on-claude", "on-haiku"])
        );
    }

    #[test]
    fn a_fallback_chain_may_name_at_most_three_alternatives_written_either_way() {
        let four = with_alternatives(&json!(["on-codex-mini", "on-claude", "on-haiku", "fourth"]));
        let error = refusal(&four);
        assert_eq!(error.key, "role.correctness.fallback");
        assert!(error.message.contains("at most 3"), "{error}");

        // One name followed through each role's own fallback counts the same way.
        let walked = review_with(&json!({ "role": {
            "correctness": { "fallback": "a" },
            "a": { "objective": "x", "provider": "claude", "inputs": ["diff"], "fallback": "b" },
            "b": { "objective": "x", "provider": "codex", "inputs": ["diff"], "fallback": "c" },
            "c": { "objective": "x", "provider": "claude", "inputs": ["diff"], "fallback": "d" },
            "d": { "objective": "x", "provider": "codex", "inputs": ["diff"] },
        }}));
        let error = refusal(&walked);
        assert_eq!(error.key, "role.correctness.fallback");
        assert!(error.message.contains("more than 3"), "{error}");
    }

    #[test]
    fn a_fallback_list_refuses_ask_a_repeat_an_empty_list_and_a_role_that_could_not_stand_in() {
        let ask = with_alternatives(&json!(["on-claude", "ask"]));
        assert!(refusal(&ask).message.contains("leave it out"));
        let twice = with_alternatives(&json!(["on-claude", "on-claude"]));
        assert!(refusal(&twice).message.contains("listed twice"));
        let empty = with_alternatives(&json!([]));
        assert!(refusal(&empty).message.contains("empty list"));
        let mut writer = with_alternatives(&json!(["on-claude"]));
        writer.role.get_mut("on-claude").unwrap().profile = RoleProfile::Writer;
        writer.role.get_mut("on-claude").unwrap().workspace = Some(Workspace::Worktree);
        assert!(refusal(&writer).message.contains("different profile"));
    }

    #[test]
    fn a_recipe_naming_a_credit_billing_model_is_refused_unless_that_role_opted_in() {
        let spec = review_with(&json!({ "role": {
            "conventions-codex": { "provider": "claude", "model": "fable" }
        }}));
        spec.validate().unwrap();
        let problems = spec.credit_problems("review");
        assert_eq!(problems.len(), 1, "the fallback is reachable, so it counts");
        assert_eq!(problems[0].role, "conventions-codex");
        assert!(
            problems[0]
                .to_string()
                .contains("allow_credit_billing = true"),
            "{}",
            problems[0]
        );

        let opted = review_with(&json!({ "role": {
            "conventions-codex": { "provider": "claude", "model": "fable", "allow_credit_billing": true }
        }}));
        assert!(opted.credit_problems("review").is_empty());
    }

    #[test]
    fn a_role_cannot_fall_back_to_itself_or_to_nothing() {
        let own = review_with(&json!({ "role": { "correctness": { "fallback": "correctness" } } }));
        assert!(refusal(&own).message.contains("its own fallback"));

        let dangling = review_with(&json!({ "role": { "correctness": { "fallback": "nobody" } } }));
        assert!(refusal(&dangling).message.contains("no role `nobody`"));
    }

    #[test]
    fn a_fallback_may_not_carry_more_authority_than_the_role_it_replaces() {
        let spec = review_with(&json!({ "role": {
            "conventions-codex": { "profile": "writer", "output": "findings" }
        }}));
        assert!(refusal(&spec).message.contains("different profile"));
    }

    #[test]
    fn a_fallback_must_hand_back_the_same_kind_of_output() {
        let spec = review_with(&json!({ "role": { "conventions-codex": { "output": "plan" } } }));
        assert!(refusal(&spec).message.contains("different kind of output"));
    }

    #[test]
    fn two_writers_in_one_stage_are_refused() {
        let spec = review_with(&json!({
            "role": {
                "fix-a": { "objective": "Fix", "provider": "codex", "profile": "writer",
                           "inputs": ["findings"], "output": "handoff" },
                "fix-b": { "objective": "Fix", "provider": "claude", "profile": "writer",
                           "inputs": ["findings"], "output": "handoff" }
            },
            "recipe": { "fix": { "stage": [{ "roles": ["fix-a", "fix-b"] }] } }
        }));
        assert!(refusal(&spec).message.contains("two writers"));
    }

    #[test]
    fn a_writer_shares_a_stage_with_snapshot_readers_but_not_with_worktree_readers() {
        let base = json!({
            "role": {
                "fix": { "objective": "Fix", "provider": "codex", "profile": "writer",
                         "inputs": ["findings"], "output": "handoff" },
                "e2e": { "objective": "Run the app", "provider": "claude",
                         "workspace": "worktree", "inputs": ["diff"] }
            },
            "recipe": { "fix": { "stage": [{ "roles": ["fix", "correctness"] }] } }
        });
        review_with(&base).validate().unwrap();

        let mut beside_live = base;
        beside_live["recipe"]["fix"]["stage"][0]["roles"] = json!(["fix", "e2e"]);
        assert!(
            refusal(&review_with(&beside_live))
                .message
                .contains("always finish stale")
        );
    }

    #[test]
    fn a_writer_cannot_be_pointed_at_the_snapshot() {
        let spec = review_with(&json!({ "role": { "fix": {
            "objective": "Fix", "provider": "codex", "profile": "writer",
            "workspace": "snapshot", "inputs": ["findings"], "output": "handoff"
        }}}));
        assert_eq!(refusal(&spec).key, "role.fix.workspace");
    }

    #[test]
    fn a_reviewer_reads_the_snapshot_and_a_writer_the_worktree_unless_told_otherwise() {
        let spec = review_with(&json!({ "role": { "fix": {
            "objective": "Fix", "provider": "codex", "profile": "writer",
            "inputs": ["findings"], "output": "handoff"
        }}}));
        assert_eq!(spec.role["correctness"].workspace(), Workspace::Snapshot);
        assert_eq!(spec.role["fix"].workspace(), Workspace::Worktree);
    }

    #[test]
    fn retries_above_the_cap_are_refused() {
        let spec =
            review_with(&json!({ "role": { "correctness": { "retries": MAX_RETRIES + 1 } } }));
        assert_eq!(refusal(&spec).key, "role.correctness.retries");
    }

    #[test]
    fn a_knowledge_scope_on_a_role_that_receives_no_rules_is_refused() {
        let spec = review_with(&json!({ "role": { "correctness": {
            "knowledge": { "areas": ["data"] }
        }}}));
        assert_eq!(refusal(&spec).key, "role.correctness.knowledge");
    }

    #[test]
    fn a_workflow_that_names_no_instruction_sources_reads_the_default_ones() {
        assert_eq!(
            workflow(review()).instruction_sources(),
            DEFAULT_INSTRUCTION_SOURCES
        );
    }

    #[test]
    fn an_empty_instruction_source_list_means_none_rather_than_the_default() {
        let none = WorkflowSpec {
            instruction_sources: Some(Vec::new()),
            ..workflow(review())
        };
        assert!(none.instruction_sources().is_empty());
    }

    #[test]
    fn an_instruction_source_outside_the_repository_is_refused() {
        let outside = WorkflowSpec {
            instruction_sources: Some(vec!["REVIEW.md".to_owned(), "/etc/passwd".to_owned()]),
            ..workflow(review())
        };
        let error = refusal(&outside);
        assert_eq!(error.key, "instruction_sources");
        assert!(error.message.contains("/etc/passwd"), "{error}");
    }

    #[test]
    fn a_role_with_no_inputs_is_refused() {
        let spec = review_with(&json!({ "role": { "correctness": { "inputs": [] } } }));
        assert_eq!(refusal(&spec).key, "role.correctness.inputs");
    }

    #[test]
    fn a_path_glob_that_leaves_the_repository_is_refused() {
        let spec = review_with(&json!({ "role": { "correctness": { "paths": ["../other/**"] } } }));
        assert_eq!(refusal(&spec).key, "role.correctness.paths");
    }

    #[test]
    fn cursor_is_not_a_role_provider_yet() {
        let mut value = review();
        value["role"]["correctness"]["provider"] = json!("cursor");
        assert!(serde_json::from_value::<WorkflowSpec>(value).is_err());
    }

    #[test]
    fn a_skill_the_provider_lacks_fails_and_names_the_provider() {
        let spec = workflow(review());
        let catalog = SkillCatalog::new()
            .with(RoleProvider::Claude, &skills(&["team-review"]))
            .with(RoleProvider::Codex, &skills(&["plan"]));

        let error = spec.check_skills("review", &catalog).unwrap_err();
        assert_eq!(
            error,
            SkillError::Problems(vec![SkillProblem::Missing {
                role: "correctness".to_owned(),
                provider: RoleProvider::Codex,
                skill: "team-review".to_owned(),
            }])
        );
        // Claude listing it is no help: the role runs on Codex, and the message says so.
        assert_eq!(
            error.to_string(),
            "Codex does not list the skill `team-review` that role `correctness` uses"
        );
    }

    #[test]
    fn a_skill_the_provider_lists_passes_whichever_way_the_slash_is_written() {
        let spec = workflow(review());
        let catalog = SkillCatalog::new()
            .with(RoleProvider::Claude, &[])
            .with(RoleProvider::Codex, &skills(&["/team-review"]));
        spec.check_skills("review", &catalog).unwrap();
    }

    #[test]
    fn a_provider_that_has_not_reported_its_skills_is_refused_rather_than_trusted() {
        let spec = workflow(review());
        let catalog = SkillCatalog::new().with(RoleProvider::Claude, &[]);
        let error = spec.check_skills("review", &catalog).unwrap_err();
        assert!(
            matches!(&error, SkillError::Problems(problems) if problems == &[SkillProblem::Unlisted {
                role: "correctness".to_owned(),
                provider: RoleProvider::Codex,
            }]),
            "{error:?}"
        );
    }

    #[test]
    fn a_fallback_roles_skills_are_checked_because_it_may_run() {
        let spec = review_with(&json!({ "role": {
            "conventions-codex": { "skills": ["conventions"] }
        }}));
        let catalog = SkillCatalog::new()
            .with(RoleProvider::Claude, &skills(&["conventions"]))
            .with(RoleProvider::Codex, &skills(&["team-review"]));
        let error = spec.check_skills("review", &catalog).unwrap_err();
        assert_eq!(
            error.to_string(),
            "Codex does not list the skill `conventions` that role `conventions-codex` uses"
        );
    }

    #[test]
    fn checking_a_recipe_that_does_not_exist_says_so() {
        let error = workflow(review())
            .check_skills("revew", &SkillCatalog::new())
            .unwrap_err();
        assert_eq!(error, SkillError::UnknownRecipe("revew".to_owned()));
    }

    #[test]
    fn a_workflow_names_its_forge_and_says_which_branches_are_integration_branches() {
        let spec = workflow(json!({
            "forge": "github",
            "integration_branches": ["main", "release/*"]
        }));
        spec.validate().unwrap();
        assert_eq!(spec.forge, Some(ForgeKind::Github));
        assert!(spec.is_integration_branch("main"));
        assert!(spec.is_integration_branch("release/4.2"));
        // The path dialect: a pattern that covers a "directory" covers what is beneath it.
        assert!(spec.is_integration_branch("release/4.2/hotfix"));
        assert!(!spec.is_integration_branch("feature/main"));
        assert_eq!(
            spec.named_integration_branches().collect::<Vec<_>>(),
            ["main"]
        );
    }

    #[test]
    fn a_forge_this_build_cannot_read_is_a_config_error_not_a_silent_no_forge() {
        let error = serde_json::from_value::<WorkflowSpec>(json!({ "forge": "gitlab" }));
        assert!(error.is_err());
    }

    #[test]
    fn an_integration_branch_written_as_a_ref_or_with_a_space_is_refused() {
        for branch in ["refs/heads/main", "/main", "", "release candidate"] {
            let error = workflow(json!({ "integration_branches": [branch] }))
                .validate()
                .unwrap_err();
            assert_eq!(error.key, "integration_branches", "{branch}");
        }
    }
}
