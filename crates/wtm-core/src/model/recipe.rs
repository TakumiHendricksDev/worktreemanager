//! Recipes: agent work lined up once and run in a worktree whenever someone chooses to.
//!
//! A recipe is an ordered list of steps. Each step names an agent, the model, effort and mode it
//! runs on, and what it is asked: a skill, sent as `/name args` exactly as the composer sends it, or
//! a prompt. Step two starts when step one's turn is over. That is the whole idea, and it is
//! deliberately smaller than the Work view's review recipes, whose stages run roles in parallel
//! under authority profiles — this one is for "plan it, build it, have the other model look", typed
//! once instead of three times a day.
//!
//! # Where they come from, and where they cannot
//!
//! Only the user's own `config.toml`. A repository's `wtm.toml` cannot declare one, because a recipe
//! is a list of prompts that agents act on with the permissions the recipe asks for, and a checked-in
//! file deciding what your agents are told is the trust problem `[agent.<id>.mcp]` already is. The
//! loading lives in `wtm-config`; this file is only the shape, and the rules that do not need a disk.
//!
//! # Actions offer, people run
//!
//! An action — a worktree created, a setup failed — can be bound to recipes. Binding one never makes
//! it run: the moment *asks*, and with nothing bound it asks nothing. [`offered`] is the filter that
//! decides what a question lists; nothing here starts anything.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// One recipe, as stored and as edited.
///
/// The id is the table key in `config.toml`, so it is not a field of the table — a hand-edited file
/// whose key and an inner `id` disagreed would have two answers to "which recipe is this".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recipe {
    #[serde(skip)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Repository roots this recipe is offered and runnable in. Empty means every one.
    ///
    /// By root rather than by name, because a name is a display override and two checkouts of the
    /// same repository are different places to run agents.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repositories: Vec<String>,
    #[serde(default)]
    pub steps: Vec<RecipeStep>,
}

/// One step: who runs it, on what, and what they are asked.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecipeStep {
    #[serde(default)]
    pub name: String,
    /// A catalogue id: `claude`, `codex`, `cursor`.
    #[serde(default)]
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// The provider's own spelling of a permission mode, as the pane's picker sends it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(flatten)]
    pub run: StepRun,
    #[serde(default)]
    pub conversation: Conversation,
    /// Append the previous step's final reply under this step's text.
    ///
    /// Only meaningful for a step that starts a new conversation: one that continues the previous
    /// step's already has the reply in front of it, and pasting it in again would be the agent's own
    /// words read back to it as if someone else had said them.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub previous_reply: bool,
    #[serde(default)]
    pub on_failure: StepFailure,
}

/// What a step asks: a skill by name, or a prompt.
///
/// Stored flat in the step's own table — `kind = "skill"` beside `skill = "review"` — rather than a
/// nested table nobody would write by hand. Read through [`StepRunWire`] so `kind` may be left out:
/// a step that says `skill = "review"` and nothing else plainly means the skill.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "StepRunWire", into = "StepRunWire")]
pub enum StepRun {
    Skill { skill: String, args: String },
    Prompt { prompt: String },
}

/// [`StepRun`] as it sits in TOML and JSON: every field optional, `kind` inferred when absent.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct StepRunWire {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    skill: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    args: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prompt: Option<String>,
}

impl From<StepRunWire> for StepRun {
    fn from(wire: StepRunWire) -> Self {
        let skill = match wire.kind.as_deref() {
            Some("skill") => true,
            Some(_) => false,
            None => wire.skill.is_some() && wire.prompt.is_none(),
        };
        if skill {
            Self::Skill {
                skill: wire.skill.unwrap_or_default(),
                args: wire.args.unwrap_or_default(),
            }
        } else {
            Self::Prompt {
                prompt: wire.prompt.unwrap_or_default(),
            }
        }
    }
}

impl From<StepRun> for StepRunWire {
    fn from(run: StepRun) -> Self {
        match run {
            StepRun::Skill { skill, args } => Self {
                kind: Some("skill".to_owned()),
                skill: Some(skill),
                args: (!args.is_empty()).then_some(args),
                prompt: None,
            },
            StepRun::Prompt { prompt } => Self {
                kind: Some("prompt".to_owned()),
                prompt: Some(prompt),
                ..Self::default()
            },
        }
    }
}

impl Default for StepRun {
    fn default() -> Self {
        Self::Prompt {
            prompt: String::new(),
        }
    }
}

/// Whether a step opens its own session or carries on in the previous step's.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Conversation {
    #[default]
    New,
    Continue,
}

/// What a failed step does to the steps after it.
///
/// `Stop` by default, because the usual next step builds on this one, and running "implement the
/// plan" after the plan failed is work done against nothing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepFailure {
    #[default]
    Stop,
    Continue,
}

impl Recipe {
    /// Whether this recipe may be offered and run in the repository at `root`.
    #[must_use]
    pub fn applies_to(&self, root: &str) -> bool {
        self.repositories.is_empty() || self.repositories.iter().any(|r| r == root)
    }

    /// Every reason this recipe cannot be saved or run, worded for the person editing it.
    ///
    /// A list rather than the first problem, so an editor can mark each step that needs attention
    /// rather than have someone fix one, save, and meet the next.
    #[must_use]
    pub fn problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        if self.name.trim().is_empty() {
            problems.push("A recipe needs a name.".to_owned());
        }
        if self.steps.is_empty() {
            problems.push("A recipe needs at least one step.".to_owned());
        }
        for (index, step) in self.steps.iter().enumerate() {
            let number = index + 1;
            if step.name.trim().is_empty() {
                problems.push(format!("Step {number} needs a name."));
            }
            if step.agent.trim().is_empty() {
                problems.push(format!("Step {number} has no agent."));
            }
            match &step.run {
                StepRun::Skill { skill, .. } if skill_name(skill).is_empty() => {
                    problems.push(format!("Step {number} runs a skill but doesn't name one."));
                }
                StepRun::Skill { skill, .. } if skill_name(skill).contains(char::is_whitespace) => {
                    problems.push(format!(
                        "Step {number}'s skill name has a space in it; put arguments in Arguments."
                    ));
                }
                StepRun::Prompt { prompt } if prompt.trim().is_empty() => {
                    problems.push(format!("Step {number} has no prompt."));
                }
                _ => {}
            }
            if step.conversation == Conversation::Continue {
                match index.checked_sub(1).and_then(|i| self.steps.get(i)) {
                    None => problems.push(format!(
                        "Step {number} continues a conversation, but no step comes before it."
                    )),
                    // A conversation belongs to one CLI: there is no way to hand a Claude
                    // transcript to Codex and have it carry on, so this is refused rather than
                    // quietly opening a new session the editor said would not exist.
                    Some(previous) if previous.agent != step.agent => problems.push(format!(
                        "Step {number} continues step {index}'s conversation, which runs on a \
                         different agent."
                    )),
                    Some(_) => {}
                }
            }
        }
        problems
    }
}

/// A skill name without the slash someone typed in front of it.
///
/// Accepted with or without, because the composer shows `/review` and the TOML says `review`, and
/// either is a reasonable thing to paste.
#[must_use]
pub fn skill_name(skill: &str) -> &str {
    skill.trim().trim_start_matches('/')
}

/// The namespaces [`fill`] substitutes. Anything else in braces is left as written.
///
/// Fixed rather than "any dotted name", because a prompt about a Jinja or Handlebars template will
/// contain `{{ user.name }}` meaning exactly that, and blanking it would change what was asked.
pub const NAMESPACES: [&str; 5] = ["worktree", "project", "form", "action", "previous"];

/// The previous step's final reply, under its variable name.
pub const PREVIOUS_REPLY: &str = "previous.reply";

/// A step's text with its variables filled in, and the ones that had no value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Filled {
    pub text: String,
    /// Variables in a known namespace that the run had no value for. They are left empty in the
    /// text and named in the run, so a recipe bound to the wrong action says so instead of sending
    /// a prompt with a hole in it that nobody can see.
    pub missing: Vec<String>,
}

/// Replace every `{{ namespace.key }}` whose namespace is one of [`NAMESPACES`].
///
/// Hand-rolled rather than the project template engine, which is strict about undefined values and
/// has filters — both right for a branch name that must not be wrong, and both wrong for prose,
/// where a missing value should not make the whole prompt unsendable and a `|` is just a character.
#[must_use]
pub fn fill(text: &str, vars: &BTreeMap<String, String>) -> Filled {
    let mut out = String::with_capacity(text.len());
    let mut missing = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find("{{") {
        out.push_str(&rest[..open]);
        let after = &rest[open + 2..];
        let Some(close) = after.find("}}") else {
            out.push_str(&rest[open..]);
            rest = "";
            break;
        };
        let key = after[..close].trim();
        let known = key
            .split_once('.')
            .is_some_and(|(namespace, name)| NAMESPACES.contains(&namespace) && !name.is_empty());
        if known {
            if let Some(value) = vars.get(key) {
                out.push_str(value);
            } else if !missing.iter().any(|m| m == key) {
                missing.push(key.to_owned());
            }
        } else {
            out.push_str(&rest[open..open + 2 + close + 2]);
        }
        rest = &after[close + 2..];
    }
    out.push_str(rest);
    Filled { text: out, missing }
}

/// What a step sends: its skill line or prompt, filled, with the previous reply appended if asked.
///
/// `previous` is the last finished step's reply, or `None` for the first step or after a step that
/// said nothing. A step that places `{{previous.reply}}` itself is not given a second copy below.
#[must_use]
pub fn compose(
    step: &RecipeStep,
    vars: &BTreeMap<String, String>,
    previous: Option<&str>,
) -> Filled {
    let mut vars = vars.clone();
    if let Some(reply) = previous {
        vars.insert(PREVIOUS_REPLY.to_owned(), reply.to_owned());
    }
    let (source, prefix) = match &step.run {
        StepRun::Skill { skill, args } => (args.as_str(), format!("/{} ", skill_name(skill))),
        StepRun::Prompt { prompt } => (prompt.as_str(), String::new()),
    };
    let placed = source.contains("previous.reply");
    let filled = fill(source, &vars);
    let mut text = format!("{prefix}{}", filled.text).trim().to_owned();
    if step.previous_reply
        && step.conversation == Conversation::New
        && !placed
        && let Some(reply) = previous.filter(|r| !r.trim().is_empty())
    {
        text.push_str("\n\n---\nThe previous step's reply:\n\n");
        text.push_str(reply.trim());
    }
    Filled {
        text,
        missing: filled.missing,
    }
}

/// The recipes an action offers in the repository at `root`, in the order the binding lists them.
///
/// Ids that no longer name a recipe are skipped rather than reported: deleting a recipe should not
/// turn every question that offered it into an error, and the Actions settings show what is left.
#[must_use]
pub fn offered<'a>(recipes: &'a [Recipe], binding: &[String], root: &str) -> Vec<&'a Recipe> {
    binding
        .iter()
        .filter_map(|id| recipes.iter().find(|recipe| &recipe.id == id))
        .filter(|recipe| recipe.applies_to(root))
        .collect()
}

/// A table key for a new recipe called `name`, unique among `taken`.
///
/// Lowercase ASCII words joined by hyphens, so it is a bare TOML key someone can type. A name with
/// nothing usable in it — all punctuation, or another script entirely — still gets an id.
#[must_use]
pub fn new_id(name: &str, taken: &[&str]) -> String {
    let mut base = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            base.push(c.to_ascii_lowercase());
        } else if !base.is_empty() && !base.ends_with('-') {
            base.push('-');
        }
    }
    let base = base.trim_end_matches('-');
    let base = if base.is_empty() { "recipe" } else { base };
    if !taken.contains(&base) {
        return base.to_owned();
    }
    // Bounded by how many are taken: among `taken.len() + 1` suffixes at least one is free.
    (2..=taken.len() + 2)
        .map(|n| format!("{base}-{n}"))
        .find(|candidate| !taken.contains(&candidate.as_str()))
        .unwrap_or_else(|| base.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(agent: &str, run: StepRun) -> RecipeStep {
        RecipeStep {
            name: "Step".to_owned(),
            agent: agent.to_owned(),
            run,
            ..RecipeStep::default()
        }
    }

    fn prompt(text: &str) -> StepRun {
        StepRun::Prompt {
            prompt: text.to_owned(),
        }
    }

    fn vars(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn known_variables_are_filled_and_missing_ones_are_named_rather_than_kept() {
        let filled = fill(
            "Plan {{form.ticket}} on {{ worktree.branch }} for {{form.owner}}",
            &vars(&[("form.ticket", "ABC-1"), ("worktree.branch", "feat/abc-1")]),
        );
        assert_eq!(filled.text, "Plan ABC-1 on feat/abc-1 for ");
        assert_eq!(filled.missing, vec!["form.owner".to_owned()]);
    }

    #[test]
    fn braces_outside_the_known_namespaces_are_left_exactly_as_written() {
        // A prompt about a template engine means its braces literally.
        let text = "Fix {{ user.name }} and {{name}} in the template, and {{ unclosed";
        assert_eq!(fill(text, &BTreeMap::new()).text, text);
    }

    #[test]
    fn a_skill_step_sends_its_name_with_a_slash_and_its_filled_arguments() {
        let s = step(
            "codex",
            StepRun::Skill {
                skill: "/review".to_owned(),
                args: "focus on {{form.ticket}}".to_owned(),
            },
        );
        assert_eq!(
            compose(&s, &vars(&[("form.ticket", "ABC-1")]), None).text,
            "/review focus on ABC-1"
        );
    }

    #[test]
    fn the_previous_reply_is_appended_once_and_only_to_a_new_conversation() {
        let mut s = step("codex", prompt("Review this."));
        s.previous_reply = true;
        let text = compose(&s, &BTreeMap::new(), Some("I changed two files.")).text;
        assert!(text.starts_with("Review this."));
        assert!(text.ends_with("I changed two files."));

        // Continuing the same conversation already has it.
        s.conversation = Conversation::Continue;
        assert_eq!(
            compose(&s, &BTreeMap::new(), Some("I changed two files.")).text,
            "Review this."
        );

        // Placed by the variable, it is not added a second time below.
        let mut placed = step("codex", prompt("Check: {{previous.reply}}"));
        placed.previous_reply = true;
        assert_eq!(
            compose(&placed, &BTreeMap::new(), Some("done")).text,
            "Check: done"
        );
    }

    #[test]
    fn a_step_cannot_continue_a_conversation_held_by_another_agent_or_by_nobody() {
        let mut first = step("claude", prompt("Plan it."));
        first.conversation = Conversation::Continue;
        let mut second = step("codex", prompt("Build it."));
        second.conversation = Conversation::Continue;
        let recipe = Recipe {
            name: "R".to_owned(),
            steps: vec![first, second],
            ..Recipe::default()
        };
        let problems = recipe.problems();
        assert!(
            problems
                .iter()
                .any(|p| p.contains("no step comes before it"))
        );
        assert!(problems.iter().any(|p| p.contains("different agent")));
    }

    #[test]
    fn an_empty_skill_or_prompt_is_a_problem_and_a_complete_recipe_has_none() {
        let recipe = Recipe {
            name: "R".to_owned(),
            steps: vec![
                step(
                    "claude",
                    StepRun::Skill {
                        skill: "/".to_owned(),
                        args: String::new(),
                    },
                ),
                step("claude", prompt("  ")),
            ],
            ..Recipe::default()
        };
        assert_eq!(recipe.problems().len(), 2);

        let fine = Recipe {
            name: "R".to_owned(),
            steps: vec![step("claude", prompt("Go."))],
            ..Recipe::default()
        };
        assert!(fine.problems().is_empty());
    }

    #[test]
    fn an_action_offers_only_recipes_that_exist_and_apply_to_the_repository() {
        let everywhere = Recipe {
            id: "a".to_owned(),
            ..Recipe::default()
        };
        let elsewhere = Recipe {
            id: "b".to_owned(),
            repositories: vec!["/other".to_owned()],
            ..Recipe::default()
        };
        let recipes = [everywhere, elsewhere];
        let binding = ["b".to_owned(), "gone".to_owned(), "a".to_owned()];
        let ids: Vec<&str> = offered(&recipes, &binding, "/repo")
            .iter()
            .map(|r| r.id.as_str())
            .collect();
        assert_eq!(ids, vec!["a"]);
    }

    #[test]
    fn a_new_id_is_a_bare_key_and_never_collides() {
        assert_eq!(new_id("Fix setup!", &[]), "fix-setup");
        assert_eq!(new_id("Review", &["review", "review-2"]), "review-3");
        assert_eq!(new_id("***", &[]), "recipe");
    }
}
