//! Turning a recipe into the stages a run will actually execute.
//!
//! # Pure, so a run can be shown before it starts
//!
//! Which roles run, in which order, beside which others, and which are skipped because the change
//! does not touch their paths: all of that follows from the config and the list of changed files.
//! Deciding it here, with no I/O, is what lets Settings and the run sheet show the plan before any
//! session opens, and lets the engine execute exactly what was shown.
//!
//! # Skipping is decided per run, not per recipe
//!
//! A role scoped to `src/**/*.svelte` has nothing to say about a change to the database layer.
//! Opening a session to have it report that would cost a turn and add a result nobody reads, so the
//! plan records it as skipped, with the reason, rather than leaving it out silently. When the
//! changed files are not known, nothing is skipped: running a role needlessly is a cost, skipping
//! one wrongly is a missed review.

use std::collections::BTreeSet;

use serde::Serialize;

use super::glob;
use super::recipe::{RoleProfile, RoleProvider, RoleSpec, WorkflowError, WorkflowSpec, Workspace};

/// A recipe, resolved against one change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecipePlan {
    pub recipe: String,
    pub stages: Vec<PlannedStage>,
}

/// Roles that start together. The next stage waits for all of them.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedStage {
    pub roles: Vec<PlannedRole>,
    pub skipped: Vec<SkippedRole>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedRole {
    pub id: String,
    pub provider: RoleProvider,
    pub profile: RoleProfile,
    pub workspace: Workspace,
    pub timeout_ms: u64,
    pub retries: u32,
    /// Roles to try, in order, if this one cannot finish. Asking you comes after the last.
    pub fallbacks: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedRole {
    pub id: String,
    pub reason: SkipReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// The role names paths, and the change touches none of them.
    NoMatchingChanges,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlanError {
    #[error("there is no recipe `{0}`")]
    UnknownRecipe(String),
    #[error(transparent)]
    Invalid(#[from] WorkflowError),
}

impl PlannedStage {
    /// Whether a role in this stage reads the snapshot checkout, so it has to exist before the
    /// stage starts and can be removed once it ends.
    #[must_use]
    pub fn reads_snapshot(&self) -> bool {
        self.roles
            .iter()
            .any(|role| role.workspace == Workspace::Snapshot)
    }

    /// Whether this stage has nothing left to run.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.roles.is_empty()
    }
}

impl RecipePlan {
    /// Whether the run needs a snapshot checkout at all.
    #[must_use]
    pub fn reads_snapshot(&self) -> bool {
        self.stages.iter().any(PlannedStage::reads_snapshot)
    }

    /// Every provider a planned role or one of its fallbacks runs on, which is the set whose
    /// sign-in the preflight has to check and whose skills have to be listed.
    #[must_use]
    pub fn providers(&self, workflow: &WorkflowSpec) -> BTreeSet<RoleProvider> {
        self.stages
            .iter()
            .flat_map(|stage| &stage.roles)
            .flat_map(|role| std::iter::once(&role.id).chain(&role.fallbacks))
            .filter_map(|id| workflow.role.get(id))
            .map(|role| role.provider)
            .collect()
    }
}

/// Plan `recipe` for a change touching `changed`, or for an unknown change when `None`.
///
/// Validates first, so a plan never exists for a recipe the config check would have refused.
pub fn plan(
    workflow: &WorkflowSpec,
    recipe: &str,
    changed: Option<&BTreeSet<String>>,
) -> Result<RecipePlan, PlanError> {
    workflow.validate()?;
    let spec = workflow
        .recipe
        .get(recipe)
        .ok_or_else(|| PlanError::UnknownRecipe(recipe.to_owned()))?;

    let stages = spec
        .stages
        .iter()
        .map(|stage| {
            let mut planned = PlannedStage::default();
            for id in &stage.roles {
                // Validation guarantees every stage role exists.
                let Some(role) = workflow.role.get(id) else {
                    continue;
                };
                if touches(role, changed) {
                    planned.roles.push(PlannedRole {
                        id: id.clone(),
                        provider: role.provider,
                        profile: role.profile,
                        workspace: role.workspace(),
                        timeout_ms: role.timeout_ms(),
                        retries: role.retries,
                        fallbacks: workflow.fallback_chain(id),
                    });
                } else {
                    planned.skipped.push(SkippedRole {
                        id: id.clone(),
                        reason: SkipReason::NoMatchingChanges,
                    });
                }
            }
            planned
        })
        .collect();

    Ok(RecipePlan {
        recipe: recipe.to_owned(),
        stages,
    })
}

/// Whether a role has anything to look at in this change.
fn touches(role: &RoleSpec, changed: Option<&BTreeSet<String>>) -> bool {
    let Some(changed) = changed else {
        return true;
    };
    role.paths.is_empty()
        || changed.iter().any(|path| {
            role.paths
                .iter()
                .any(|pattern| glob::matches(pattern, path))
        })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn workflow() -> WorkflowSpec {
        serde_json::from_value(json!({
            "role": {
                "ui": {
                    "objective": "Components and styles",
                    "provider": "claude",
                    "paths": ["src/**/*.svelte", "src/styles"],
                    "inputs": ["diff", "rules"],
                    "fallback": "ui-codex"
                },
                "ui-codex": {
                    "objective": "Components and styles, on Codex",
                    "provider": "codex",
                    "inputs": ["diff", "rules"]
                },
                "correctness": {
                    "objective": "Bugs",
                    "provider": "codex",
                    "inputs": ["diff"]
                },
                "e2e": {
                    "objective": "Click through the change",
                    "provider": "claude",
                    "workspace": "worktree",
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
                        { "roles": ["ui", "correctness"] },
                        { "roles": ["e2e"] },
                        { "roles": ["consolidate"] }
                    ]
                }
            }
        }))
        .unwrap()
    }

    fn changed(paths: &[&str]) -> BTreeSet<String> {
        paths.iter().map(|path| (*path).to_owned()).collect()
    }

    fn ids(stage: &PlannedStage) -> Vec<&str> {
        stage.roles.iter().map(|role| role.id.as_str()).collect()
    }

    #[test]
    fn roles_in_a_stage_run_together_and_stages_keep_their_order() {
        let plan = plan(&workflow(), "review", None).unwrap();
        let stages: Vec<Vec<&str>> = plan.stages.iter().map(ids).collect();
        assert_eq!(
            stages,
            vec![vec!["ui", "correctness"], vec!["e2e"], vec!["consolidate"]]
        );
    }

    #[test]
    fn a_role_whose_paths_match_no_changed_file_is_skipped_and_says_why() {
        let plan = plan(&workflow(), "review", Some(&changed(&["app/db.py"]))).unwrap();
        assert_eq!(ids(&plan.stages[0]), vec!["correctness"]);
        assert_eq!(
            plan.stages[0].skipped,
            vec![SkippedRole {
                id: "ui".to_owned(),
                reason: SkipReason::NoMatchingChanges,
            }]
        );
    }

    #[test]
    fn a_role_runs_when_any_changed_file_matches_any_of_its_paths() {
        let deep = plan(
            &workflow(),
            "review",
            Some(&changed(&["app/db.py", "src/lib/work/Queue.svelte"])),
        )
        .unwrap();
        assert_eq!(ids(&deep.stages[0]), vec!["ui", "correctness"]);

        let in_directory = plan(
            &workflow(),
            "review",
            Some(&changed(&["src/styles/components/_tab.scss"])),
        )
        .unwrap();
        assert_eq!(ids(&in_directory.stages[0]), vec!["ui", "correctness"]);
    }

    #[test]
    fn nothing_is_skipped_when_the_changed_files_are_unknown() {
        let plan = plan(&workflow(), "review", None).unwrap();
        assert!(plan.stages.iter().all(|stage| stage.skipped.is_empty()));
    }

    #[test]
    fn only_stages_with_a_snapshot_reader_need_the_snapshot_checkout() {
        let plan = plan(&workflow(), "review", None).unwrap();
        let reads: Vec<bool> = plan
            .stages
            .iter()
            .map(PlannedStage::reads_snapshot)
            .collect();
        assert_eq!(reads, vec![true, false, true]);
        assert!(plan.reads_snapshot());
    }

    #[test]
    fn a_planned_role_carries_its_fallbacks_in_order_and_its_resolved_defaults() {
        let plan = plan(&workflow(), "review", None).unwrap();
        let ui = &plan.stages[0].roles[0];
        assert_eq!(ui.fallbacks, vec!["ui-codex".to_owned()]);
        assert_eq!(ui.workspace, Workspace::Snapshot);
        assert_eq!(ui.timeout_ms, super::super::recipe::DEFAULT_TIMEOUT_MS);
        assert_eq!(plan.stages[1].roles[0].workspace, Workspace::Worktree);
    }

    #[test]
    fn the_providers_a_run_needs_include_those_its_fallbacks_would_use() {
        let mut spec = workflow();
        spec.recipe.get_mut("review").unwrap().stages.truncate(1);
        spec.role.get_mut("correctness").unwrap().provider = RoleProvider::Claude;
        let plan = plan(&spec, "review", None).unwrap();
        assert_eq!(
            plan.providers(&spec),
            BTreeSet::from([RoleProvider::Claude, RoleProvider::Codex])
        );
    }

    #[test]
    fn an_invalid_workflow_is_never_planned() {
        let mut spec = workflow();
        spec.role.get_mut("ui-codex").unwrap().fallback =
            super::super::recipe::Fallback::Role("ui".to_owned());
        assert!(matches!(
            plan(&spec, "review", None),
            Err(PlanError::Invalid(_))
        ));
        assert_eq!(
            plan(&workflow(), "nope", None),
            Err(PlanError::UnknownRecipe("nope".to_owned()))
        );
    }
}
