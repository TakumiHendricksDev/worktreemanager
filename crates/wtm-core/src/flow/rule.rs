//! Review rules: your private, per-project review knowledge.
//!
//! # Who can make a rule apply
//!
//! A rule you write applies as soon as you save it. A rule an agent proposes applies only once you
//! approve it, because otherwise a reviewer could teach every later reviewer whatever it liked. A
//! finding you dismissed never becomes a rule: dismissing it said the finding was wrong, and a rule
//! built from it would keep raising it.
//!
//! # Why revisions are kept
//!
//! A finding cites the rule revision it was measured against, so editing a rule must not change
//! what an old finding cited. Revisions are appended, numbered from 1, and never rewritten.

use serde::{Deserialize, Serialize};

use super::UnixMs;
use super::review::Finding;
use crate::model::ProjectId;

id! {
    /// One rule, as the store numbers it.
    RuleId
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleScope {
    pub areas: Vec<String>,
    /// Globs, relative to the repository root.
    pub paths: Vec<String>,
}

/// A rule's content, before it is saved or as the next revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleDraft {
    pub kind: RuleKind,
    pub text: String,
    pub rationale: String,
    pub scope: RuleScope,
    pub examples: Vec<String>,
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleRevision {
    pub number: u32,
    pub draft: RuleDraft,
    pub at: UnixMs,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RuleError {
    #[error("a dismissed finding cannot become a rule")]
    DismissedFinding,
    #[error("only a pending proposal can be approved or rejected; this one is {approval:?}")]
    NotPending { approval: RuleApproval },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rule {
    pub id: RuleId,
    pub project: ProjectId,
    pub origin: RuleOrigin,
    pub status: RuleStatus,
    pub approval: RuleApproval,
    /// Never empty: a rule is created with its first revision.
    pub revisions: Vec<RuleRevision>,
}

impl Rule {
    #[must_use]
    pub fn new(
        id: RuleId,
        project: ProjectId,
        origin: RuleOrigin,
        draft: RuleDraft,
        at: UnixMs,
    ) -> Self {
        let approval = match origin {
            RuleOrigin::Human | RuleOrigin::Imported => RuleApproval::Approved,
            RuleOrigin::Agent => RuleApproval::Pending,
        };
        Self {
            id,
            project,
            origin,
            status: RuleStatus::Active,
            approval,
            revisions: vec![RuleRevision {
                number: 1,
                draft,
                at,
            }],
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

    /// Your edit, appended as the next revision.
    #[must_use]
    pub fn revise(&self, draft: RuleDraft, at: UnixMs) -> Self {
        let number = self.current().map_or(1, |revision| revision.number + 1);
        let mut revised = self.clone();
        revised.revisions.push(RuleRevision { number, draft, at });
        revised
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
    use crate::flow::review::{Dismissal, FindingClass, FindingId, Location, Severity};
    use crate::flow::state::FlowId;

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

    fn rule(origin: RuleOrigin) -> Rule {
        Rule::new(
            RuleId(14),
            ProjectId::from_root(std::path::Path::new("/work/webapp")),
            origin,
            draft("Use the shared dialog for confirm and cancel flows"),
            100,
        )
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
    fn a_rejected_proposal_cannot_be_approved_afterwards() {
        let rejected = rule(RuleOrigin::Agent).reject().unwrap();
        assert!(!rejected.applies());
        assert_eq!(
            rejected.approve(),
            Err(RuleError::NotPending {
                approval: RuleApproval::Rejected
            })
        );
    }

    #[test]
    fn a_disabled_rule_does_not_apply() {
        let disabled = Rule {
            status: RuleStatus::Disabled,
            ..rule(RuleOrigin::Human)
        };
        assert!(!disabled.applies());
    }

    #[test]
    fn editing_a_rule_appends_a_revision_and_keeps_the_one_findings_cited() {
        let original = rule(RuleOrigin::Human);
        let revised = original.revise(draft("Use the shared dialog for every modal"), 200);

        assert_eq!(revised.current().unwrap().number, 2);
        assert_eq!(
            revised.revision(1).unwrap().draft.text,
            "Use the shared dialog for confirm and cancel flows"
        );
    }

    #[test]
    fn a_dismissed_finding_cannot_become_a_rule() {
        let dismissed = finding(Some(Dismissal {
            reason: "The modal is a deliberate exception.".to_owned(),
            at: 50,
        }));
        assert_eq!(
            RuleDraft::from_finding(&dismissed),
            Err(RuleError::DismissedFinding)
        );
    }

    #[test]
    fn a_finding_saved_as_a_rule_is_scoped_to_its_file() {
        let draft = RuleDraft::from_finding(&finding(None)).unwrap();
        assert_eq!(draft.scope.paths, ["src/ExportModal.vue"]);
        assert_eq!(draft.text, "Use the shared dialog");
    }
}
