//! Which of your rules a review role is given, what it is told about them, and the record of what
//! it was given.
//!
//! # Deterministic, with no vector store
//!
//! Retrieval is a filter and a sort over the project's rules. The same rules and the same change
//! give the same selection every time, and each selected rule carries the reason it was chosen, so
//! "why did the reviewer get this rule" has an answer and a test. A similarity search would find
//! rules that read like the change, which is not the same as rules that are about it, and its
//! answer would move whenever the index did.
//!
//! # The steps, in order
//!
//! 1. **Repository.** Only the project's own rules, and only the ones that apply now: active, and
//!    yours or approved by you. A pending proposal never reaches a role.
//! 2. **Path globs against the files.** A rule naming globs qualifies when one of them covers a
//!    file the change touches, or, for an implementation, a file the plan says it will.
//! 3. **Area tags.** A rule tagged with an area the role asks for qualifies too. A rule naming
//!    neither globs nor areas is about the whole project and always qualifies; one that names
//!    either and matches neither is left out.
//! 4. **Required first, capped per role.** Required rules sort ahead of guidance, then path
//!    matches ahead of area matches ahead of project-wide rules, then oldest first. The first
//!    [`RuleQuery`] cap are given; the rest are returned as capped rather than dropped, so the
//!    role can be told how many it did not see.
//!
//! A path match wins over an area match because it is evidence about this change; an area is
//! about the role's subject. That is also why the cap falls on area and project-wide matches
//! first.
//!
//! # What a review records
//!
//! Each invocation records the rule revisions it was given as `rule_applications`, keyed by
//! [`ReviewInvocation`]. A finding cites a rule id and revision, and the record is how anyone can later
//! check that the revision it cites is the one the role actually read.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use super::UnixMs;
use super::glob;
use super::instructions::InstructionSet;
use super::recipe::{RoleInput, RoleSpec};
use super::rule::{ExampleStance, Rule, RuleId, RuleKind, RuleRevision};
use super::state::FlowId;
use crate::model::ProjectId;

/// How many rules a role gets when its `knowledge.max_rules` does not say.
///
/// Each rule is a few lines of a role's brief, beside the diff and the instruction sources. Twenty
/// keeps them a small part of it; a project with more rules than that for one change is better
/// served by narrower scopes than by a bigger cap.
pub const DEFAULT_MAX_RULES: u32 = 20;

/// What a role is reviewing, for choosing its rules.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleQuery {
    files: Vec<String>,
    areas: Vec<String>,
    max_rules: u32,
}

impl RuleQuery {
    /// Files the change touches or plans to, and the areas asked for. Both are sorted and
    /// de-duplicated, and areas lower-cased the way rules store theirs.
    pub fn new(
        files: impl IntoIterator<Item = String>,
        areas: impl IntoIterator<Item = String>,
    ) -> Self {
        let files: BTreeSet<String> = files.into_iter().collect();
        let areas: BTreeSet<String> = areas
            .into_iter()
            .map(|area| area.trim().to_lowercase())
            .filter(|area| !area.is_empty())
            .collect();
        Self {
            files: files.into_iter().collect(),
            areas: areas.into_iter().collect(),
            max_rules: DEFAULT_MAX_RULES,
        }
    }

    /// The query for `role` reviewing `files`, or `None` when the role is not given rules at all.
    ///
    /// A role that declares `paths` reviews only the changed files those cover, so it is given the
    /// rules for those files and not for the rest of the change.
    #[must_use]
    pub fn for_role(role: &RoleSpec, files: &[String]) -> Option<Self> {
        if !role.inputs.contains(&RoleInput::Rules) {
            return None;
        }
        let files = files
            .iter()
            .filter(|file| {
                role.paths.is_empty() || role.paths.iter().any(|glob| glob::matches(glob, file))
            })
            .cloned();
        let query = Self::new(files, role.knowledge.areas.iter().cloned());
        Some(match role.knowledge.max_rules {
            Some(max) => query.capped(max),
            None => query,
        })
    }

    #[must_use]
    pub fn capped(self, max_rules: u32) -> Self {
        Self { max_rules, ..self }
    }

    /// Add areas, such as the ones a plan says it expects to touch.
    #[must_use]
    pub fn with_areas(self, areas: impl IntoIterator<Item = String>) -> Self {
        let max_rules = self.max_rules;
        Self::new(self.files, self.areas.into_iter().chain(areas)).capped(max_rules)
    }
}

/// Why a rule was given to a role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MatchReason {
    /// `glob`, one of the rule's, covers `file`, the first such file in path order.
    Path { glob: String, file: String },
    /// The rule is tagged `area` and the role asked for it.
    Area { area: String },
    /// The rule names no globs and no areas, so it is about the whole project.
    Project,
}

impl MatchReason {
    fn rank(&self) -> u8 {
        match self {
            Self::Path { .. } => 0,
            Self::Area { .. } => 1,
            Self::Project => 2,
        }
    }
}

/// One rule revision chosen for a role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectedRule {
    pub rule: RuleId,
    pub revision: u32,
    pub kind: RuleKind,
    pub reason: MatchReason,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleSelection {
    /// Given to the role, in the order it reads them.
    pub applied: Vec<SelectedRule>,
    /// Matched, and left out by the cap.
    pub capped: Vec<SelectedRule>,
}

/// Choose `project`'s rules for `query`. See the module docs for the steps.
#[must_use]
pub fn select(rules: &[Rule], project: &ProjectId, query: &RuleQuery) -> RuleSelection {
    let mut matched: Vec<SelectedRule> = rules
        .iter()
        .filter(|rule| &rule.project == project && rule.applies())
        .filter_map(|rule| {
            let revision = rule.current()?;
            Some(SelectedRule {
                rule: rule.id,
                revision: revision.number,
                kind: revision.draft.kind,
                reason: reason(revision, query)?,
            })
        })
        .collect();
    matched.sort_by_key(|selected| (selected.kind, selected.reason.rank(), selected.rule));

    let cap = usize::try_from(query.max_rules).unwrap_or(usize::MAX);
    let capped = if matched.len() > cap {
        matched.split_off(cap)
    } else {
        Vec::new()
    };
    RuleSelection {
        applied: matched,
        capped,
    }
}

fn reason(revision: &RuleRevision, query: &RuleQuery) -> Option<MatchReason> {
    let scope = &revision.draft.scope;
    for glob in &scope.paths {
        if let Some(file) = query.files.iter().find(|file| glob::matches(glob, file)) {
            return Some(MatchReason::Path {
                glob: glob.clone(),
                file: file.clone(),
            });
        }
    }
    if let Some(area) = scope
        .areas
        .iter()
        .find(|area| query.areas.contains(&area.to_lowercase()))
    {
        return Some(MatchReason::Area { area: area.clone() });
    }
    (scope.paths.is_empty() && scope.areas.is_empty()).then_some(MatchReason::Project)
}

/// One role's session in a flow: what `rule_applications` are recorded against.
///
/// A role can run more than once in a flow, after a failure or for Rerun affected, and each run
/// can be given different revisions, so the attempt is part of the key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewInvocation {
    pub flow: FlowId,
    /// The role's id in the recipe.
    pub role: String,
    /// From 1.
    pub attempt: u32,
}

/// A rule revision an invocation was given, as the store recorded it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordedApplication {
    pub invocation: ReviewInvocation,
    pub rule: RuleId,
    pub revision: u32,
    pub reason: MatchReason,
    pub at: UnixMs,
}

/// How much a rule has been used, for the Knowledge list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleUsage {
    pub rule: RuleId,
    /// Distinct flows that were given the rule, at any revision.
    pub reviews: u32,
    pub last_flow: FlowId,
    pub last_at: UnixMs,
}

/// The part of a role's brief that carries the project's instructions and your rules.
///
/// `instructions` is given to a role whose inputs include `instructions` and `rules` to one whose
/// inputs include `rules`; the engine decides, and passes `None` for the other. The instructions
/// come first and are called authoritative, and the rules are called private, because that is
/// the precedence a conflict is judged by. The role is told to report a disagreement between them
/// and not to settle it, since only you can.
#[must_use]
pub fn brief(
    instructions: Option<&InstructionSet>,
    rules: Option<(&RuleSelection, &[Rule])>,
) -> String {
    let mut out = String::new();
    if let Some(set) = instructions {
        out.push_str(
            "## Project instructions (authoritative)\n\n\
             These files belong to the repository's team. Follow them. Where one of the rules \
             below disagrees with them, report a conflict that names both and do not choose \
             between them.\n\n",
        );
        if set.documents.is_empty() {
            out.push_str("No instruction files were found.\n\n");
        }
        for document in &set.documents {
            let _ = writeln!(out, "<instructions path=\"{}\">", document.path);
            out.push_str(document.text.trim_end());
            out.push('\n');
            if document.truncated {
                out.push_str("[The rest of this file was left out for length.]\n");
            }
            out.push_str("</instructions>\n\n");
        }
        for skipped in &set.skipped {
            let _ = writeln!(out, "Not read: {} ({}).", skipped.path, skipped.reason);
        }
        if !set.skipped.is_empty() {
            out.push('\n');
        }
    }
    if let Some((selection, all)) = rules {
        out.push_str(
            "## Your reviewer's rules (private)\n\n\
             A change that breaks a required rule is a rule violation: cite the rule's id and \
             revision, as in R-14 v2. Guidance informs your judgement and is not a violation on \
             its own.\n\n",
        );
        if selection.applied.is_empty() {
            out.push_str("No rules apply to this change.\n\n");
        }
        for selected in &selection.applied {
            let Some(revision) = all
                .iter()
                .find(|rule| rule.id == selected.rule)
                .and_then(|rule| rule.revision(selected.revision))
            else {
                continue;
            };
            write_rule(&mut out, selected, revision);
        }
        if !selection.capped.is_empty() {
            let _ = writeln!(
                out,
                "{} more matching rules were left out to keep this brief short.\n",
                selection.capped.len()
            );
        }
    }
    out
}

fn write_rule(out: &mut String, selected: &SelectedRule, revision: &RuleRevision) {
    let draft = &revision.draft;
    let kind = match draft.kind {
        RuleKind::Required => "required",
        RuleKind::Guidance => "guidance",
    };
    let _ = writeln!(
        out,
        "<rule id=\"R-{}\" revision=\"{}\" kind=\"{kind}\">",
        selected.rule, selected.revision
    );
    let _ = writeln!(out, "{}", draft.text);
    if !draft.rationale.is_empty() {
        let _ = writeln!(out, "Why: {}", draft.rationale);
    }
    for example in &draft.examples {
        let stance = match example.stance {
            ExampleStance::Prefer => "Prefer",
            ExampleStance::Avoid => "Avoid",
        };
        let _ = writeln!(out, "{stance}: {}", example.text);
    }
    if !draft.references.is_empty() {
        let _ = writeln!(out, "See: {}", draft.references.join(", "));
    }
    out.push_str("</rule>\n\n");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::instructions::{InstructionDocument, SkippedDocument};
    use crate::flow::rule::{NewRule, RuleChange, RuleDraft, RuleExample, RuleScope};

    fn project() -> ProjectId {
        ProjectId::from_root(std::path::Path::new("/work/webapp"))
    }

    fn draft(kind: RuleKind, text: &str, areas: &[&str], paths: &[&str]) -> RuleDraft {
        RuleDraft {
            kind,
            text: text.to_owned(),
            rationale: String::new(),
            scope: RuleScope {
                areas: areas.iter().map(|area| (*area).to_owned()).collect(),
                paths: paths.iter().map(|path| (*path).to_owned()).collect(),
            },
            examples: Vec::new(),
            references: Vec::new(),
        }
    }

    fn rule(id: i64, draft: RuleDraft) -> Rule {
        Rule::create(
            RuleId(id),
            &NewRule::written(project(), draft).unwrap(),
            100,
        )
    }

    fn changed(files: &[&str]) -> RuleQuery {
        RuleQuery::new(files.iter().map(|file| (*file).to_owned()), [])
    }

    fn ids(rules: &[SelectedRule]) -> Vec<i64> {
        rules.iter().map(|selected| selected.rule.0).collect()
    }

    fn dialog_rule() -> Rule {
        rule(
            14,
            draft(
                RuleKind::Required,
                "Use AppDialog for confirm and cancel flows",
                &["frontend"],
                &["src/**/*.vue"],
            ),
        )
    }

    #[test]
    fn a_rule_scoped_to_components_is_given_when_a_component_changes() {
        let selection = select(
            &[dialog_rule()],
            &project(),
            &changed(&["src/exports/ExportModal.vue", "api/export.py"]),
        );
        assert_eq!(
            selection.applied,
            [SelectedRule {
                rule: RuleId(14),
                revision: 1,
                kind: RuleKind::Required,
                reason: MatchReason::Path {
                    glob: "src/**/*.vue".to_owned(),
                    file: "src/exports/ExportModal.vue".to_owned(),
                },
            }]
        );
    }

    #[test]
    fn a_rule_scoped_to_components_is_not_given_when_only_the_backend_changes() {
        let selection = select(&[dialog_rule()], &project(), &changed(&["api/export.py"]));
        assert!(selection.applied.is_empty());
        assert!(selection.capped.is_empty());
    }

    #[test]
    fn an_area_the_role_asks_for_brings_in_rules_its_files_did_not() {
        let query = changed(&["api/export.py"]).with_areas(["Frontend".to_owned()]);
        let selection = select(&[dialog_rule()], &project(), &query);
        assert_eq!(
            selection.applied[0].reason,
            MatchReason::Area {
                area: "frontend".to_owned()
            }
        );
    }

    #[test]
    fn a_rule_with_no_scope_is_given_for_any_change_in_its_project() {
        let anywhere = rule(
            22,
            draft(
                RuleKind::Guidance,
                "No new dependencies without a ticket note",
                &[],
                &[],
            ),
        );
        let selection = select(&[anywhere], &project(), &changed(&["README.md"]));
        assert_eq!(selection.applied[0].reason, MatchReason::Project);
    }

    #[test]
    fn another_projects_rules_are_never_given() {
        let mut elsewhere = dialog_rule();
        elsewhere.project = ProjectId::from_root(std::path::Path::new("/work/other"));
        let selection = select(&[elsewhere], &project(), &changed(&["src/App.vue"]));
        assert!(selection.applied.is_empty());
    }

    #[test]
    fn a_pending_proposal_or_a_disabled_rule_is_never_given() {
        let proposal = Rule::create(
            RuleId(30),
            &NewRule::proposal(
                project(),
                FlowId(1),
                "correctness",
                draft(RuleKind::Required, "Prefer select_related", &[], &[]),
                None,
            )
            .unwrap(),
            100,
        );
        let disabled = rule(31, draft(RuleKind::Required, "Old habit", &[], &[]))
            .change(&RuleChange::Disable, 200)
            .unwrap();

        let selection = select(&[proposal, disabled], &project(), &changed(&["a.py"]));
        assert!(selection.applied.is_empty());
    }

    #[test]
    fn the_latest_revision_is_the_one_given() {
        let revised = dialog_rule()
            .change(
                &RuleChange::Revise {
                    draft: draft(
                        RuleKind::Guidance,
                        "Prefer AppDialog",
                        &[],
                        &["src/**/*.vue"],
                    ),
                },
                200,
            )
            .unwrap();
        let selection = select(&[revised], &project(), &changed(&["src/App.vue"]));
        assert_eq!(selection.applied[0].revision, 2);
        assert_eq!(selection.applied[0].kind, RuleKind::Guidance);
    }

    #[test]
    fn required_rules_come_first_and_the_cap_falls_on_the_weakest_matches() {
        let rules = [
            rule(1, draft(RuleKind::Guidance, "anywhere", &[], &[])),
            rule(2, draft(RuleKind::Guidance, "by path", &[], &["src/**"])),
            rule(3, draft(RuleKind::Required, "anywhere, required", &[], &[])),
            rule(4, draft(RuleKind::Required, "by area", &["ui"], &[])),
            rule(5, draft(RuleKind::Required, "by path", &[], &["src/**"])),
        ];
        let query = changed(&["src/App.vue"])
            .with_areas(["ui".to_owned()])
            .capped(3);

        let selection = select(&rules, &project(), &query);

        assert_eq!(ids(&selection.applied), [5, 4, 3]);
        assert_eq!(ids(&selection.capped), [2, 1]);
    }

    #[test]
    fn the_same_rules_and_change_give_the_same_selection_whatever_order_they_arrive_in() {
        let mut rules = vec![
            rule(7, draft(RuleKind::Guidance, "b", &[], &["src/**"])),
            rule(3, draft(RuleKind::Guidance, "a", &[], &["src/**"])),
            dialog_rule(),
        ];
        let forwards = select(&rules, &project(), &changed(&["src/b.vue", "src/a.vue"]));
        rules.reverse();
        let backwards = select(&rules, &project(), &changed(&["src/a.vue", "src/b.vue"]));
        assert_eq!(forwards, backwards);
        assert_eq!(ids(&forwards.applied), [14, 3, 7]);
        assert!(matches!(
            &forwards.applied[1].reason,
            MatchReason::Path { file, .. } if file == "src/a.vue"
        ));
    }

    fn role(inputs: &str, extra: &str) -> RoleSpec {
        serde_json::from_str(&format!(
            r#"{{ "objective": "Review", "provider": "claude", "inputs": [{inputs}] {extra} }}"#
        ))
        .unwrap()
    }

    #[test]
    fn a_role_that_is_not_given_rules_has_no_query() {
        assert!(RuleQuery::for_role(&role(r#""diff""#, ""), &[]).is_none());
    }

    #[test]
    fn a_role_with_paths_gets_the_rules_for_its_own_files_only() {
        let ui = role(
            r#""diff", "rules""#,
            r#", "paths": ["src/**/*.vue"], "knowledge": { "areas": ["UI"], "max_rules": 5 }"#,
        );
        let query = RuleQuery::for_role(&ui, &["api/export.py".to_owned(), "src/A.vue".to_owned()])
            .unwrap();
        assert_eq!(
            query,
            RuleQuery::new(["src/A.vue".to_owned()], ["ui".to_owned()]).capped(5)
        );
    }

    #[test]
    fn the_brief_puts_the_teams_instructions_first_and_cites_each_rule_by_revision() {
        let mut with_example = dialog_rule();
        with_example.revisions[0].draft.examples = vec![RuleExample {
            stance: ExampleStance::Avoid,
            text: "src/components/BaseModal.vue".to_owned(),
        }];
        let rules = [with_example];
        let selection = select(&rules, &project(), &changed(&["src/App.vue"]));
        let instructions = InstructionSet {
            sources: Vec::new(),
            documents: vec![InstructionDocument {
                path: "REVIEW.md".to_owned(),
                pattern: "REVIEW.md".to_owned(),
                text: "Severity: blocking means unsafe to ship.\n".to_owned(),
                truncated: false,
            }],
            skipped: vec![SkippedDocument {
                path: "docs/review/huge.md".to_owned(),
                reason: "over the size limit".to_owned(),
            }],
        };

        let text = brief(Some(&instructions), Some((&selection, &rules)));

        let team = text.find("<instructions path=\"REVIEW.md\">").unwrap();
        let private = text
            .find("<rule id=\"R-14\" revision=\"1\" kind=\"required\">")
            .unwrap();
        assert!(team < private, "{text}");
        assert!(text.contains("report a conflict"), "{text}");
        assert!(text.contains("Not read: docs/review/huge.md"), "{text}");
        assert!(
            text.contains("Avoid: src/components/BaseModal.vue"),
            "{text}"
        );
    }

    #[test]
    fn a_role_given_only_instructions_hears_nothing_about_rules() {
        let text = brief(Some(&InstructionSet::default()), None);
        assert!(!text.contains("rules (private)"), "{text}");
        assert!(text.contains("No instruction files were found."), "{text}");
    }
}
