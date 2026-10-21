//! Review rules: your private, per-project review knowledge.
//!
//! # Who can make a rule apply
//!
//! A rule you write applies as soon as you save it. A rule an agent proposes applies only once you
//! approve it, because otherwise a reviewer could teach every later reviewer whatever it liked. A
//! finding you dismissed never becomes a rule: dismissing it said the finding was wrong, and a rule
//! built from it would keep raising it.
//!
//! Those three are enforced by construction. A rule reaches a store only as a [`NewRule`], and the
//! constructors are the only way to build one: [`NewRule::written`] and [`NewRule::from_finding`]
//! are yours, [`NewRule::proposal`] is an agent's and starts pending, and both finding routes
//! refuse a dismissed finding.
//!
//! # Why revisions are kept
//!
//! A finding cites the rule revision it was measured against, so editing a rule must not change
//! what an old finding cited. Revisions are appended, numbered from 1, and never rewritten. For the
//! same reason a rule is never deleted: disabling it stops reviews using it, and replacing it
//! points at the rule that took its place, while every revision anything cited stays readable.
//!
//! # Rules are private, and the repository's instructions outrank them
//!
//! A team keeps its own review knowledge in the repository, in files such as `REVIEW.md`. Rules
//! here are one person's layer on top, so a review reads both and reports a disagreement as a
//! conflict rather than letting a private rule quietly override team policy. See
//! [`instructions`](super::instructions).

use serde::{Deserialize, Serialize};

use super::UnixMs;
use super::glob;
use super::review::{Finding, FindingId, Location};
use super::state::FlowId;
use crate::model::ProjectId;

id! {
    /// One rule, as the store numbers it.
    RuleId
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleKind {
    /// Breaking it is a rule violation.
    Required,
    /// Advice a reviewer weighs.
    Guidance,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleOrigin {
    Human,
    Agent,
    /// Brought in by you from elsewhere, so it is treated as yours.
    Imported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleStatus {
    Active,
    Disabled,
    /// Replaced by another rule, named in [`Rule::superseded_by`]. Final.
    Superseded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleApproval {
    Pending,
    Approved,
    Rejected,
}

/// Where a rule applies. Empty lists mean anywhere in the project.
///
/// The project itself is the rule's [`Rule::project`] rather than a field here, because a rule
/// never moves between repositories: one that fits two projects is two rules, each free to change.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleScope {
    /// Tags such as `frontend` or `exports`, matched against the areas a role asks for.
    pub areas: Vec<String>,
    /// Globs, relative to the repository root, in the [`glob`] dialect.
    pub paths: Vec<String>,
}

/// Whether an example shows what to do or what not to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExampleStance {
    Prefer,
    Avoid,
}

/// A pointer at code that shows the rule, usually a path with an optional `:line`.
///
/// Free text rather than a parsed location, because the useful example is often a component name
/// or a pattern rather than one line, and a reviewer reads it either way.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleExample {
    pub stance: ExampleStance,
    pub text: String,
}

/// A rule's content, before it is saved or as the next revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleDraft {
    pub kind: RuleKind,
    pub text: String,
    pub rationale: String,
    pub scope: RuleScope,
    pub examples: Vec<RuleExample>,
    /// Links, documents or tickets that explain the rule further.
    pub references: Vec<String>,
}

impl RuleDraft {
    /// A draft prefilled from a finding, for "Save finding as rule".
    pub fn from_finding(finding: &Finding) -> Result<Self, RuleError> {
        if finding.dismissed.is_some() {
            return Err(RuleError::DismissedFinding);
        }
        Ok(Self {
            kind: RuleKind::Guidance,
            text: finding.title.clone(),
            rationale: finding.rationale.clone(),
            scope: RuleScope {
                areas: Vec::new(),
                paths: finding
                    .location
                    .iter()
                    .map(|location| location.path.clone())
                    .collect(),
            },
            examples: Vec::new(),
            references: Vec::new(),
        })
    }

    /// The draft as a store keeps it, or why it cannot be kept.
    ///
    /// Text is trimmed and blank entries dropped, so a trailing comma in the Areas field is not an
    /// area. Areas are lower-cased, because they are typed by hand in more than one place and
    /// `Frontend` in a rule must match `frontend` in a role. Globs must be ones the matcher
    /// accepts: a glob that can never match would make a rule that silently never applies.
    pub fn checked(self) -> Result<Self, RuleError> {
        let text = self.text.trim().to_owned();
        if text.is_empty() {
            return Err(RuleError::EmptyText);
        }
        let mut areas: Vec<String> = Vec::new();
        for area in self.scope.areas {
            let area = area.trim().to_lowercase();
            if !area.is_empty() && !areas.contains(&area) {
                areas.push(area);
            }
        }
        let mut paths: Vec<String> = Vec::new();
        for path in self.scope.paths {
            let path = path.trim().to_owned();
            if path.is_empty() || paths.contains(&path) {
                continue;
            }
            glob::check(&path).map_err(|message| RuleError::BadGlob {
                glob: path.clone(),
                message: message.to_owned(),
            })?;
            paths.push(path);
        }
        Ok(Self {
            kind: self.kind,
            text,
            rationale: self.rationale.trim().to_owned(),
            scope: RuleScope { areas, paths },
            examples: self
                .examples
                .into_iter()
                .filter_map(|example| {
                    let text = example.text.trim().to_owned();
                    (!text.is_empty()).then_some(RuleExample {
                        stance: example.stance,
                        text,
                    })
                })
                .collect(),
            references: self
                .references
                .into_iter()
                .map(|reference| reference.trim().to_owned())
                .filter(|reference| !reference.is_empty())
                .collect(),
        })
    }
}

/// Where a rule came from: the Knowledge list's Origin line, and the finding or run that taught it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuleSource {
    /// Written from scratch, with New rule.
    Written,
    /// Saved from a finding, with Save as rule.
    Finding {
        flow: FlowId,
        finding: FindingId,
        location: Option<Location>,
    },
    /// Proposed by a role during a run.
    Proposal {
        flow: FlowId,
        /// The role's id in the recipe.
        role: String,
        finding: Option<FindingId>,
    },
    /// Written to take the place of another rule, which is now superseded by this one.
    Replacement { replaces: RuleId },
}

impl RuleSource {
    /// The flow the rule came out of, if it came out of one.
    #[must_use]
    pub fn flow(&self) -> Option<FlowId> {
        match self {
            Self::Finding { flow, .. } | Self::Proposal { flow, .. } => Some(*flow),
            Self::Written | Self::Replacement { .. } => None,
        }
    }
}

/// A rule about to be saved. Built only by the constructors below; see the module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewRule {
    project: ProjectId,
    origin: RuleOrigin,
    source: RuleSource,
    draft: RuleDraft,
}

impl NewRule {
    /// Yours, written from scratch. Applies once saved.
    pub fn written(project: ProjectId, draft: RuleDraft) -> Result<Self, RuleError> {
        Ok(Self {
            project,
            origin: RuleOrigin::Human,
            source: RuleSource::Written,
            draft: draft.checked()?,
        })
    }

    /// Yours, saved from a finding with the draft as you edited it. Applies once saved.
    pub fn from_finding(
        project: ProjectId,
        finding: &Finding,
        draft: RuleDraft,
    ) -> Result<Self, RuleError> {
        if finding.dismissed.is_some() {
            return Err(RuleError::DismissedFinding);
        }
        Ok(Self {
            project,
            origin: RuleOrigin::Human,
            source: RuleSource::Finding {
                flow: finding.flow,
                finding: finding.id,
                location: finding.location.clone(),
            },
            draft: draft.checked()?,
        })
    }

    /// An agent's, proposed by `role` during `flow`. Pending until you approve it.
    pub fn proposal(
        project: ProjectId,
        flow: FlowId,
        role: &str,
        draft: RuleDraft,
        finding: Option<&Finding>,
    ) -> Result<Self, RuleError> {
        if finding.is_some_and(|finding| finding.dismissed.is_some()) {
            return Err(RuleError::DismissedFinding);
        }
        Ok(Self {
            project,
            origin: RuleOrigin::Agent,
            source: RuleSource::Proposal {
                flow,
                role: role.to_owned(),
                finding: finding.map(|finding| finding.id),
            },
            draft: draft.checked()?,
        })
    }

    /// Yours, replacing `replaces`. Only a store's replace builds this, because the old rule has to
    /// be superseded in the same write.
    pub fn replacement(
        project: ProjectId,
        replaces: RuleId,
        draft: RuleDraft,
    ) -> Result<Self, RuleError> {
        Ok(Self {
            project,
            origin: RuleOrigin::Human,
            source: RuleSource::Replacement { replaces },
            draft: draft.checked()?,
        })
    }

    #[must_use]
    pub fn project(&self) -> &ProjectId {
        &self.project
    }

    #[must_use]
    pub fn origin(&self) -> RuleOrigin {
        self.origin
    }

    #[must_use]
    pub fn source(&self) -> &RuleSource {
        &self.source
    }

    #[must_use]
    pub fn draft(&self) -> &RuleDraft {
        &self.draft
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleRevision {
    pub number: u32,
    pub draft: RuleDraft,
    pub at: UnixMs,
}

/// Something you do to a saved rule. Each is checked by [`Rule::change`] before a store writes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuleChange {
    /// Your edit, appended as the next revision.
    Revise {
        draft: RuleDraft,
    },
    Approve,
    /// Your edit of a proposal, approved with it in one step, so the proposal applies only as you
    /// wrote it and never for a moment as the agent did.
    ApproveRevised {
        draft: RuleDraft,
    },
    Reject,
    Disable,
    Enable,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuleError {
    #[error("a dismissed finding cannot become a rule")]
    DismissedFinding,
    #[error("only a pending proposal can be approved or rejected; this one is {approval:?}")]
    NotPending { approval: RuleApproval },
    #[error("only an approved rule can be turned on, off or replaced; this one is {approval:?}")]
    NotApproved { approval: RuleApproval },
    #[error("a rejected proposal cannot be edited")]
    Rejected,
    #[error("rule {by} replaced this one, so it can no longer change")]
    Superseded { by: RuleId },
    #[error("a rule needs some text")]
    EmptyText,
    #[error("`{glob}`: {message}")]
    BadGlob { glob: String, message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rule {
    pub id: RuleId,
    pub project: ProjectId,
    pub origin: RuleOrigin,
    pub source: RuleSource,
    pub status: RuleStatus,
    pub approval: RuleApproval,
    /// Set exactly when the status is [`RuleStatus::Superseded`].
    pub superseded_by: Option<RuleId>,
    /// Never empty: a rule is created with its first revision. The first is by the rule's origin;
    /// every later one is yours, because nothing but you edits a saved rule.
    pub revisions: Vec<RuleRevision>,
    /// When anything about the rule last changed.
    pub updated_at: UnixMs,
}

impl Rule {
    /// The rule a store saves, numbered `id`.
    #[must_use]
    pub fn create(id: RuleId, new: &NewRule, at: UnixMs) -> Self {
        let approval = match new.origin {
            RuleOrigin::Human | RuleOrigin::Imported => RuleApproval::Approved,
            RuleOrigin::Agent => RuleApproval::Pending,
        };
        Self {
            id,
            project: new.project.clone(),
            origin: new.origin,
            source: new.source.clone(),
            status: RuleStatus::Active,
            approval,
            superseded_by: None,
            revisions: vec![RuleRevision {
                number: 1,
                draft: new.draft.clone(),
                at,
            }],
            updated_at: at,
        }
    }

    /// Whether reviews use this rule now.
    #[must_use]
    pub fn applies(&self) -> bool {
        self.status == RuleStatus::Active && self.approval == RuleApproval::Approved
    }

    /// The revision reviews use, which is always the latest.
    #[must_use]
    pub fn current(&self) -> Option<&RuleRevision> {
        self.revisions.last()
    }

    /// The revision a finding cited.
    #[must_use]
    pub fn revision(&self, number: u32) -> Option<&RuleRevision> {
        self.revisions
            .iter()
            .find(|revision| revision.number == number)
    }

    pub fn approve(&self) -> Result<Self, RuleError> {
        self.decide(RuleApproval::Approved)
    }

    pub fn reject(&self) -> Result<Self, RuleError> {
        self.decide(RuleApproval::Rejected)
    }

    /// The rule with `change` made at `at`, or why the change is refused.
    ///
    /// Turning a rule off that is already off, or on that is already on, is not an error and
    /// changes nothing, so a second click on a button that has not repainted yet is harmless.
    pub fn change(&self, change: &RuleChange, at: UnixMs) -> Result<Self, RuleError> {
        let changed = match change {
            RuleChange::Revise { draft } => self.revise(draft.clone(), at)?,
            RuleChange::Approve => self.approve()?,
            RuleChange::Reject => self.reject()?,
            RuleChange::ApproveRevised { draft } => {
                if self.approval != RuleApproval::Pending {
                    return Err(RuleError::NotPending {
                        approval: self.approval,
                    });
                }
                self.revise(draft.clone(), at)?.approve()?
            }
            RuleChange::Disable => return self.turn(RuleStatus::Disabled, at),
            RuleChange::Enable => return self.turn(RuleStatus::Active, at),
        };
        Ok(Self {
            updated_at: at,
            ..changed
        })
    }

    /// This rule, superseded by `by`, which a store creates in the same write.
    pub fn supersede(&self, by: RuleId, at: UnixMs) -> Result<Self, RuleError> {
        self.ensure_open()?;
        if self.approval != RuleApproval::Approved {
            return Err(RuleError::NotApproved {
                approval: self.approval,
            });
        }
        Ok(Self {
            status: RuleStatus::Superseded,
            superseded_by: Some(by),
            updated_at: at,
            ..self.clone()
        })
    }

    fn revise(&self, draft: RuleDraft, at: UnixMs) -> Result<Self, RuleError> {
        self.ensure_open()?;
        if self.approval == RuleApproval::Rejected {
            return Err(RuleError::Rejected);
        }
        let draft = draft.checked()?;
        let number = self.current().map_or(1, |revision| revision.number + 1);
        let mut revised = self.clone();
        revised.revisions.push(RuleRevision { number, draft, at });
        Ok(revised)
    }

    fn turn(&self, status: RuleStatus, at: UnixMs) -> Result<Self, RuleError> {
        self.ensure_open()?;
        if self.approval != RuleApproval::Approved {
            return Err(RuleError::NotApproved {
                approval: self.approval,
            });
        }
        if self.status == status {
            return Ok(self.clone());
        }
        Ok(Self {
            status,
            updated_at: at,
            ..self.clone()
        })
    }

    fn ensure_open(&self) -> Result<(), RuleError> {
        match (self.status, self.superseded_by) {
            (RuleStatus::Superseded, Some(by)) => Err(RuleError::Superseded { by }),
            // Unreachable through these methods, which set both together; a store row that broke
            // the pairing is still not editable.
            (RuleStatus::Superseded, None) => Err(RuleError::Superseded { by: self.id }),
            _ => Ok(()),
        }
    }

    fn decide(&self, approval: RuleApproval) -> Result<Self, RuleError> {
        if self.approval != RuleApproval::Pending {
            return Err(RuleError::NotPending {
                approval: self.approval,
            });
        }
        Ok(Self {
            approval,
            ..self.clone()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::review::{Dismissal, FindingClass, Severity};

    fn draft(text: &str) -> RuleDraft {
        RuleDraft {
            kind: RuleKind::Required,
            text: text.to_owned(),
            rationale: "One dialog component keeps focus handling consistent.".to_owned(),
            scope: RuleScope {
                areas: vec!["frontend".to_owned()],
                paths: vec!["src/**/*.vue".to_owned()],
            },
            examples: Vec::new(),
            references: Vec::new(),
        }
    }

    fn project() -> ProjectId {
        ProjectId::from_root(std::path::Path::new("/work/webapp"))
    }

    fn rule(origin: RuleOrigin) -> Rule {
        let text = draft("Use the shared dialog for confirm and cancel flows");
        let new = match origin {
            RuleOrigin::Human => NewRule::written(project(), text),
            RuleOrigin::Agent => NewRule::proposal(project(), FlowId(1), "conventions", text, None),
            RuleOrigin::Imported => Ok(NewRule {
                origin: RuleOrigin::Imported,
                ..NewRule::written(project(), text).unwrap()
            }),
        };
        Rule::create(RuleId(14), &new.unwrap(), 100)
    }

    fn finding(dismissed: Option<Dismissal>) -> Finding {
        Finding {
            id: FindingId(3),
            flow: FlowId(1),
            class: FindingClass::Suggestion,
            severity: Severity::Minor,
            location: Some(Location {
                path: "src/ExportModal.vue".to_owned(),
                start_line: 12,
                end_line: 12,
            }),
            title: "Use the shared dialog".to_owned(),
            rationale: "A hand-rolled modal skips focus trapping.".to_owned(),
            requirement: None,
            tree_sha: "9f00".to_owned(),
            dismissed,
        }
    }

    fn dismissal() -> Dismissal {
        Dismissal {
            reason: "The modal is a deliberate exception.".to_owned(),
            at: 50,
        }
    }

    #[test]
    fn a_rule_you_write_applies_as_soon_as_it_is_saved() {
        assert!(rule(RuleOrigin::Human).applies());
        assert!(rule(RuleOrigin::Imported).applies());
    }

    #[test]
    fn an_agents_proposal_applies_only_once_you_approve_it() {
        let proposed = rule(RuleOrigin::Agent);
        assert!(!proposed.applies());
        assert!(proposed.approve().unwrap().applies());
    }

    #[test]
    fn a_rejected_proposal_cannot_be_approved_or_edited_afterwards() {
        let rejected = rule(RuleOrigin::Agent).reject().unwrap();
        assert!(!rejected.applies());
        assert_eq!(
            rejected.approve(),
            Err(RuleError::NotPending {
                approval: RuleApproval::Rejected
            })
        );
        let edit = RuleChange::Revise {
            draft: draft("Something else"),
        };
        assert_eq!(rejected.change(&edit, 200), Err(RuleError::Rejected));
    }

    #[test]
    fn a_disabled_rule_does_not_apply_until_you_turn_it_back_on() {
        let disabled = rule(RuleOrigin::Human)
            .change(&RuleChange::Disable, 200)
            .unwrap();
        assert_eq!(disabled.status, RuleStatus::Disabled);
        assert!(!disabled.applies());
        assert_eq!(disabled.updated_at, 200);

        let enabled = disabled.change(&RuleChange::Enable, 300).unwrap();
        assert!(enabled.applies());
    }

    #[test]
    fn turning_a_rule_off_twice_changes_nothing_the_second_time() {
        let disabled = rule(RuleOrigin::Human)
            .change(&RuleChange::Disable, 200)
            .unwrap();
        assert_eq!(
            disabled.change(&RuleChange::Disable, 300).unwrap(),
            disabled
        );
    }

    #[test]
    fn a_pending_proposal_cannot_be_turned_off_before_anyone_approved_it() {
        // Disabling would make it look like a rule that once applied.
        assert_eq!(
            rule(RuleOrigin::Agent).change(&RuleChange::Disable, 200),
            Err(RuleError::NotApproved {
                approval: RuleApproval::Pending
            })
        );
    }

    #[test]
    fn editing_a_rule_appends_a_revision_and_keeps_the_one_findings_cited() {
        let original = rule(RuleOrigin::Human);
        let edit = RuleChange::Revise {
            draft: draft("Use the shared dialog for every modal"),
        };
        let revised = original.change(&edit, 200).unwrap();

        assert_eq!(revised.current().unwrap().number, 2);
        assert_eq!(revised.current().unwrap().at, 200);
        assert_eq!(revised.revision(1).unwrap().at, 100);
        assert_eq!(
            revised.revision(1).unwrap().draft.text,
            "Use the shared dialog for confirm and cancel flows"
        );
    }

    #[test]
    fn a_proposal_you_edit_before_approving_applies_only_as_you_wrote_it() {
        let proposed = rule(RuleOrigin::Agent);
        let change = RuleChange::ApproveRevised {
            draft: draft("Use the shared dialog for confirmations only"),
        };
        let approved = proposed.change(&change, 200).unwrap();

        assert!(approved.applies());
        assert_eq!(approved.revisions.len(), 2);
        assert_eq!(
            approved.current().unwrap().draft.text,
            "Use the shared dialog for confirmations only"
        );
    }

    #[test]
    fn an_approved_rule_cannot_be_approved_again_with_an_edit() {
        let change = RuleChange::ApproveRevised {
            draft: draft("Quietly different"),
        };
        assert_eq!(
            rule(RuleOrigin::Human).change(&change, 200),
            Err(RuleError::NotPending {
                approval: RuleApproval::Approved
            })
        );
    }

    #[test]
    fn a_superseded_rule_points_at_its_replacement_and_cannot_change_again() {
        let replaced = rule(RuleOrigin::Human).supersede(RuleId(15), 200).unwrap();
        assert_eq!(replaced.status, RuleStatus::Superseded);
        assert_eq!(replaced.superseded_by, Some(RuleId(15)));
        assert!(!replaced.applies());

        for change in [
            RuleChange::Enable,
            RuleChange::Revise {
                draft: draft("Back from the dead"),
            },
        ] {
            assert_eq!(
                replaced.change(&change, 300),
                Err(RuleError::Superseded { by: RuleId(15) })
            );
        }
    }

    #[test]
    fn a_proposal_nobody_approved_cannot_be_replaced() {
        assert_eq!(
            rule(RuleOrigin::Agent).supersede(RuleId(15), 200),
            Err(RuleError::NotApproved {
                approval: RuleApproval::Pending
            })
        );
    }

    #[test]
    fn a_dismissed_finding_cannot_become_a_rule_by_any_route() {
        let dismissed = finding(Some(dismissal()));
        assert_eq!(
            RuleDraft::from_finding(&dismissed),
            Err(RuleError::DismissedFinding)
        );
        assert_eq!(
            NewRule::from_finding(project(), &dismissed, draft("Edited after all")),
            Err(RuleError::DismissedFinding)
        );
        assert_eq!(
            NewRule::proposal(
                project(),
                FlowId(1),
                "conventions",
                draft("An agent's lesson"),
                Some(&dismissed)
            ),
            Err(RuleError::DismissedFinding)
        );
    }

    #[test]
    fn a_finding_saved_as_a_rule_is_scoped_to_its_file_and_remembers_where_it_came_from() {
        let finding = finding(None);
        let draft = RuleDraft::from_finding(&finding).unwrap();
        assert_eq!(draft.scope.paths, ["src/ExportModal.vue"]);
        assert_eq!(draft.text, "Use the shared dialog");

        let new = NewRule::from_finding(project(), &finding, draft).unwrap();
        assert_eq!(new.origin(), RuleOrigin::Human);
        assert_eq!(new.source().flow(), Some(FlowId(1)));
        assert!(matches!(
            new.source(),
            RuleSource::Finding { finding: FindingId(3), location: Some(location), .. }
                if location.start_line == 12
        ));
    }

    #[test]
    fn a_draft_is_tidied_before_it_is_kept() {
        let messy = RuleDraft {
            text: "  Use the shared dialog \n".to_owned(),
            scope: RuleScope {
                areas: vec![" Frontend".to_owned(), "frontend".to_owned(), String::new()],
                paths: vec!["src/**/*.vue ".to_owned(), " ".to_owned()],
            },
            examples: vec![
                RuleExample {
                    stance: ExampleStance::Prefer,
                    text: " src/components/Dialog.vue ".to_owned(),
                },
                RuleExample {
                    stance: ExampleStance::Avoid,
                    text: "  ".to_owned(),
                },
            ],
            references: vec![String::new(), "docs/ui.md".to_owned()],
            ..draft("unused")
        };
        let tidy = messy.checked().unwrap();

        assert_eq!(tidy.text, "Use the shared dialog");
        assert_eq!(tidy.scope.areas, ["frontend"]);
        assert_eq!(tidy.scope.paths, ["src/**/*.vue"]);
        assert_eq!(tidy.examples.len(), 1);
        assert_eq!(tidy.references, ["docs/ui.md"]);
    }

    #[test]
    fn a_rule_with_no_text_or_a_glob_that_can_never_match_is_refused() {
        assert_eq!(draft("   ").checked(), Err(RuleError::EmptyText));

        let climbing = RuleDraft {
            scope: RuleScope {
                areas: Vec::new(),
                paths: vec!["../elsewhere/**".to_owned()],
            },
            ..draft("Fine text")
        };
        assert!(matches!(
            NewRule::written(project(), climbing),
            Err(RuleError::BadGlob { glob, .. }) if glob == "../elsewhere/**"
        ));
    }
}
