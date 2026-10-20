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

use super::glob;
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
/// Only recipes and roles so far. The tracker, forge and status mapping the plan puts here arrive
/// with the items that first read them, and until then a key this build does not know is an error
/// rather than something silently ignored.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowSpec {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub role: BTreeMap<String, RoleSpec>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub recipe: BTreeMap<String, RecipeSpec>,
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
/// Written as a plain string: `"ask"`, or the id of the role to run instead. That makes `ask` a
/// reserved role id, which validation enforces.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub enum Fallback {
    /// Put it in front of you. The default, because switching provider changes what a review is
    /// worth and is not something to do on a role's behalf unless you said so.
    #[default]
    Ask,
    /// Run this role instead, from a summary of where the first one got to.
    Role(String),
}

impl From<String> for Fallback {
    fn from(value: String) -> Self {
        if value == ASK {
            Self::Ask
        } else {
            Self::Role(value)
        }
    }
}

impl From<Fallback> for String {
    fn from(value: Fallback) -> Self {
        match value {
            Fallback::Ask => ASK.to_owned(),
            Fallback::Role(role) => role,
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
}

impl RoleSpec {
    #[must_use]
    pub fn workspace(&self) -> Workspace {
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
    fn new(key: impl Into<String>, message: impl Into<String>) -> Self {
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
        Ok(())
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
        self.check_fallback(id, role)
    }

    fn check_fallback(&self, id: &str, role: &RoleSpec) -> Result<(), WorkflowError> {
        let key = format!("role.{id}.fallback");
        let Fallback::Role(alternative) = &role.fallback else {
            return Ok(());
        };
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
        let mut seen = BTreeSet::from([id]);
        let mut next = alternative.as_str();
        while seen.insert(next) {
            match self.role.get(next).map(|role| &role.fallback) {
                Some(Fallback::Role(after)) => next = after,
                _ => return Ok(()),
            }
        }
        Err(WorkflowError::new(
            key,
            format!("the fallbacks from `{id}` lead back to `{next}` and would never end"),
        ))
    }

    fn check_recipe(&self, id: &str, recipe: &RecipeSpec) -> Result<(), WorkflowError> {
        let key = format!("recipe.{id}.stage");
        if recipe.stages.is_empty() {
            return Err(WorkflowError::new(key, "a recipe needs at least one stage"));
        }

        let mut used = BTreeSet::new();
        for (index, stage) in recipe.stages.iter().enumerate() {
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
            let mut next = Some(name.as_str());
            while let Some(id) = next {
                let Some((id, role)) = self.role.get_key_value(id) else {
                    break;
                };
                if !seen.insert(id.as_str()) {
                    break;
                }
                out.push((id.as_str(), role));
                next = match &role.fallback {
                    Fallback::Role(after) => Some(after.as_str()),
                    Fallback::Ask => None,
                };
            }
        }
        out
    }

    /// The fallbacks a role leads to, in the order they would be tried. `ask` is implied after the
    /// last one.
    #[must_use]
    pub fn fallback_chain(&self, role: &str) -> Vec<String> {
        let mut seen = BTreeSet::from([role]);
        let mut chain = Vec::new();
        let mut next = self.role.get(role).map(|role| &role.fallback);
        while let Some(Fallback::Role(id)) = next {
            if !seen.insert(id) {
                break;
            }
            chain.push(id.clone());
            next = self.role.get(id).map(|role| &role.fallback);
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
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
}
