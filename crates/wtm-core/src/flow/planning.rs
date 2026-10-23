//! Planning an issue: what the planner read, the plans it handed in, what you decided about them,
//! and when an approval stops covering what it approved.
//!
//! # A plan is approved against an issue, not just a plan
//!
//! "Implement this plan" means implement it for the issue you were looking at. If someone edits
//! the issue's description afterwards — a changed acceptance criterion, a narrowed scope — the plan
//! may no longer answer it, and the approval should say so rather than carry on as if nothing
//! happened. So a plan approval pins two revisions: the plan's number, and the issue's
//! ([`IssueText::revision`]), and it is stale as soon as either differs from what is current.
//!
//! # The issue's revision is its text, not its timestamp
//!
//! A tracker's own "updated" time moves on every change: a status transition, a new assignee, a
//! comment, Start Work's own move. None of those changes what is to be built, and an approval that
//! went stale on each of them would teach people to approve again without looking. So the revision
//! is a hash of the parts that say what the work is — summary, type, description, the project's
//! mapped fields, parent and links — and nothing else ([`IssueText`]). A change to any of those is a
//! change to the work, and it is shown as a diff of exactly those parts ([`diff`]).

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use super::UnixMs;
use super::approval::{Approval, ApprovalId, Binding, PinChange, Pinned};
use super::knowledge::MatchReason;
use super::publish::hash_of;
use super::recipe::RoleProvider;
use super::rule::{RuleId, RuleKind};
use super::run::{InvocationId, StepId};
use super::state::FlowId;
use super::submission::PlanReport;
use super::tracker::{IssueDetail, TrackerKind};
use crate::model::ProjectId;

id! {
    /// One plan revision, as the store numbers it.
    PlanId
}

id! {
    /// One decision about a plan revision, as the store numbers it.
    PlanDecisionId
}

/// One of the project's mapped fields, as the issue had it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueTextField {
    pub name: String,
    pub value: Option<String>,
}

/// What an issue says the work is: the parts of it a plan answers.
///
/// Team state — status, assignee, priority, sprint, labels — is left out on purpose; see the module
/// docs. Links and the parent are in, because "blocked by" and "part of" change what a plan can
/// assume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueText {
    pub key: String,
    pub summary: String,
    pub kind: Option<String>,
    pub description: Option<String>,
    /// The project's mapped fields, in the order the tracker returned them.
    pub fields: Vec<IssueTextField>,
    /// The parent, as `KEY · summary`.
    pub parent: Option<String>,
    /// Each link, as `relation KEY · summary`.
    pub links: Vec<String>,
}

impl IssueText {
    /// The parts of a whole read of an issue that say what the work is.
    #[must_use]
    pub fn from_detail(detail: &IssueDetail) -> Self {
        let titled = |key: &str, summary: Option<&str>| match summary {
            Some(summary) if !summary.trim().is_empty() => format!("{key} · {}", summary.trim()),
            _ => key.to_owned(),
        };
        Self {
            key: detail.issue.key.clone(),
            summary: detail.issue.summary.clone(),
            kind: detail.issue.kind.clone(),
            description: detail.description.clone(),
            fields: detail
                .fields
                .iter()
                .map(|field| IssueTextField {
                    name: field.name.clone(),
                    value: field.value.clone(),
                })
                .collect(),
            parent: detail
                .parent
                .as_ref()
                .map(|parent| titled(&parent.key, parent.summary.as_deref())),
            links: detail
                .links
                .iter()
                .map(|link| {
                    format!(
                        "{} {}",
                        link.relation,
                        titled(&link.key, link.summary.as_deref())
                    )
                })
                .collect(),
        }
    }

    /// The issue's revision for an approval: the hash of this text, so an edit to any part of it
    /// changes the revision and nothing else does.
    #[must_use]
    pub fn revision(&self) -> String {
        hash_of(self)
    }
}

/// Whether a line was kept, added or removed between two versions of a text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffKind {
    Same,
    Added,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffLine {
    pub kind: DiffKind,
    pub text: String,
}

/// One part of an issue that differs between the version a plan was approved against and now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueChange {
    /// "Summary", "Description", a mapped field's name, "Parent" or "Links".
    pub part: String,
    pub before: Option<String>,
    pub after: Option<String>,
    /// The two versions line by line, for text long enough to need it.
    pub lines: Vec<DiffLine>,
}

/// Lines longer than this many on either side are not diffed line by line: the whole of both is
/// shown instead. An issue description is a few dozen lines; this only bounds a pathological one.
const MAX_DIFF_LINES: usize = 2_000;

/// Every part that differs between `before` and `after`, in the order an issue is read.
#[must_use]
pub fn diff(before: &IssueText, after: &IssueText) -> Vec<IssueChange> {
    let mut changes = Vec::new();
    let mut part = |name: &str, old: Option<String>, new: Option<String>| {
        if old != new {
            let multiline = [&old, &new]
                .iter()
                .any(|text| text.as_deref().is_some_and(|t| t.contains('\n')));
            let lines = if multiline {
                line_diff(old.as_deref().unwrap_or(""), new.as_deref().unwrap_or(""))
            } else {
                Vec::new()
            };
            changes.push(IssueChange {
                part: name.to_owned(),
                before: old,
                after: new,
                lines,
            });
        }
    };
    part(
        "Summary",
        Some(before.summary.clone()),
        Some(after.summary.clone()),
    );
    part("Type", before.kind.clone(), after.kind.clone());
    part(
        "Description",
        before.description.clone(),
        after.description.clone(),
    );
    let mut names: Vec<&str> = before.fields.iter().map(|f| f.name.as_str()).collect();
    for field in &after.fields {
        if !names.contains(&field.name.as_str()) {
            names.push(&field.name);
        }
    }
    for name in names {
        let value = |text: &IssueText| {
            text.fields
                .iter()
                .find(|f| f.name == name)
                .and_then(|f| f.value.clone())
        };
        part(name, value(before), value(after));
    }
    part("Parent", before.parent.clone(), after.parent.clone());
    let joined = |links: &[String]| (!links.is_empty()).then(|| links.join("\n"));
    part("Links", joined(&before.links), joined(&after.links));
    changes
}

/// A line diff by longest common subsequence: small, exact, and enough for an issue's text.
fn line_diff(before: &str, after: &str) -> Vec<DiffLine> {
    let old: Vec<&str> = before.lines().collect();
    let new: Vec<&str> = after.lines().collect();
    let line = |kind, text: &str| DiffLine {
        kind,
        text: text.to_owned(),
    };
    if old.len() > MAX_DIFF_LINES || new.len() > MAX_DIFF_LINES {
        return old
            .iter()
            .map(|text| line(DiffKind::Removed, text))
            .chain(new.iter().map(|text| line(DiffKind::Added, text)))
            .collect();
    }
    // `common[i][j]` is the longest common subsequence of `old[i..]` and `new[j..]`.
    let mut common = vec![vec![0usize; new.len() + 1]; old.len() + 1];
    for i in (0..old.len()).rev() {
        for j in (0..new.len()).rev() {
            common[i][j] = if old[i] == new[j] {
                common[i + 1][j + 1] + 1
            } else {
                common[i + 1][j].max(common[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::with_capacity(old.len().max(new.len()));
    while i < old.len() && j < new.len() {
        if old[i] == new[j] {
            out.push(line(DiffKind::Same, old[i]));
            i += 1;
            j += 1;
        } else if common[i + 1][j] >= common[i][j + 1] {
            out.push(line(DiffKind::Removed, old[i]));
            i += 1;
        } else {
            out.push(line(DiffKind::Added, new[j]));
            j += 1;
        }
    }
    out.extend(old[i..].iter().map(|text| line(DiffKind::Removed, text)));
    out.extend(new[j..].iter().map(|text| line(DiffKind::Added, text)));
    out
}

/// A linked issue, given to the planner as read-only context: what the work depends on or affects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanDependency {
    pub relation: String,
    pub key: String,
    pub summary: Option<String>,
    pub status: Option<String>,
}

/// One of your rules the planner was given, with why it applied.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanRule {
    pub rule: RuleId,
    pub revision: u32,
    pub kind: RuleKind,
    pub text: String,
    pub reason: MatchReason,
}

/// Everything the planner was given to work from: what the context drawer lists.
///
/// Recorded when the planner starts, from the reads it was given, so the drawer shows what the plan
/// was made from rather than what the tracker or the repository say now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanContext {
    pub issue: IssueText,
    /// The team status when it was read.
    pub status: String,
    pub sprint: Option<String>,
    pub sprint_goal: Option<String>,
    /// The parent and every link, as read-only context.
    pub dependencies: Vec<PlanDependency>,
    /// The repository's instruction files the brief carried, by path.
    pub instruction_sources: Vec<String>,
    /// Your active rules that applied, in the order the planner read them.
    pub rules: Vec<PlanRule>,
    /// The skills the planner's role names.
    pub skills: Vec<String>,
    pub role: String,
    pub provider: RoleProvider,
    pub model: Option<String>,
    pub read_at: UnixMs,
}

/// An Implement run's planning: the role it plans with, where, and what it was last given.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Planning {
    pub flow: FlowId,
    pub project: ProjectId,
    /// The worktree's id, which is its path.
    pub worktree: String,
    pub key: String,
    pub tracker: TrackerKind,
    pub role: String,
    pub context: Option<PlanContext>,
    pub created_at: UnixMs,
    pub updated_at: UnixMs,
}

/// A run's planning to record, the first time it plans.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewPlanning {
    pub flow: FlowId,
    pub project: ProjectId,
    pub worktree: String,
    pub key: String,
    pub tracker: TrackerKind,
    pub role: String,
}

/// One plan the planner handed in, numbered within its run, with the issue as it read it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanRecord {
    pub id: PlanId,
    pub flow: FlowId,
    /// 1 for the first, one more for each after it.
    pub revision: u32,
    pub step: Option<StepId>,
    pub invocation: Option<InvocationId>,
    pub plan: PlanReport,
    /// The issue as the planner was given it, which is what the plan answers.
    pub issue: IssueText,
    /// [`IssueText::revision`] of `issue`.
    pub issue_revision: String,
    pub submitted_at: UnixMs,
}

/// A plan to record, before the store numbers it and its revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewPlan {
    pub flow: FlowId,
    pub step: Option<StepId>,
    pub invocation: Option<InvocationId>,
    pub plan: PlanReport,
    pub issue: IssueText,
}

/// What you decided about one plan revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlanVerdict {
    /// Implement it. `skip_design` says to go straight to implementing even if the plan asked for
    /// a design pass.
    #[serde(rename_all = "camelCase")]
    Approved {
        approval: ApprovalId,
        skip_design: bool,
    },
    /// Not this one: what to change, which the planner is told in the same conversation.
    ChangesRequested { note: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanDecision {
    pub id: PlanDecisionId,
    pub flow: FlowId,
    pub plan: PlanId,
    pub revision: u32,
    pub verdict: PlanVerdict,
    pub at: UnixMs,
}

/// What an approval of a plan was of, kept with it: the revision, and the issue as you saw it, so
/// a later edit of the issue can be shown as a diff against exactly that.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanApproval {
    pub revision: u32,
    pub issue: IssueText,
    pub skip_design: bool,
    /// Whether the plan asked for a design pass, so "approved, design skipped" reads as a choice.
    pub design_needed: bool,
}

/// The pins a plan approval covers: the plan's revision and the issue's.
#[must_use]
pub fn plan_binding(revision: u32, issue_revision: &str) -> Binding {
    Binding::default()
        .with(Pinned::PlanRevision, revision.to_string())
        .with(Pinned::IssueRevision, issue_revision)
}

/// Where a run's planning stands, for the issue workspace and the queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum PlanStanding {
    /// Nothing handed in yet.
    None,
    /// The newest plan waits for your decision.
    Waiting { revision: u32 },
    /// You asked for changes to the newest plan; the planner is revising it.
    Revising { revision: u32, note: String },
    /// Approved, and the approval still covers the newest plan and the issue as it reads now.
    Approved {
        approval: ApprovalId,
        revision: u32,
        skip_design: bool,
    },
    /// Approved, but the issue or the plan changed since: the approval covers neither any more.
    Stale {
        approval: ApprovalId,
        revision: u32,
        changes: Vec<PinChange>,
        /// The issue as you approved against, against the issue now.
        diff: Vec<IssueChange>,
    },
}

/// Where planning stands, given the newest plan, the newest decision about any revision, the
/// approval that decision made with what it was of, and the issue as it reads now (when it has
/// been read since the plan).
#[must_use]
pub fn standing(
    latest: Option<&PlanRecord>,
    decision: Option<&PlanDecision>,
    approval: Option<(&Approval, &PlanApproval)>,
    issue_now: Option<&IssueText>,
) -> PlanStanding {
    let Some(latest) = latest else {
        return PlanStanding::None;
    };
    match decision.map(|d| &d.verdict) {
        Some(PlanVerdict::ChangesRequested { note })
            if decision.is_some_and(|d| d.revision == latest.revision) =>
        {
            PlanStanding::Revising {
                revision: latest.revision,
                note: note.clone(),
            }
        }
        Some(PlanVerdict::Approved { skip_design, .. }) => {
            let Some((approval, subject)) = approval else {
                return PlanStanding::Waiting {
                    revision: latest.revision,
                };
            };
            let current_issue = issue_now.unwrap_or(&subject.issue);
            let current = plan_binding(latest.revision, &current_issue.revision());
            match approval.check(&current) {
                Ok(()) => PlanStanding::Approved {
                    approval: approval.id,
                    revision: subject.revision,
                    skip_design: *skip_design,
                },
                Err(stale) => PlanStanding::Stale {
                    approval: approval.id,
                    revision: subject.revision,
                    diff: diff(&subject.issue, current_issue),
                    changes: stale.changes,
                },
            }
        }
        _ => PlanStanding::Waiting {
            revision: latest.revision,
        },
    }
}

/// The plan as text for a brief or a turn: what the planner handed in, as it reads back.
#[must_use]
pub fn plan_text(plan: &PlanReport) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "{}", plan.summary.trim());
    let list = |out: &mut String, title: &str, items: &[String]| {
        if !items.is_empty() {
            let _ = writeln!(out, "\n{title}:");
            for item in items {
                let _ = writeln!(out, "- {}", item.trim());
            }
        }
    };
    list(&mut out, "Steps", &plan.steps);
    list(&mut out, "Acceptance criteria", &plan.acceptance_criteria);
    list(&mut out, "Verification", &plan.verification);
    list(&mut out, "Areas", &plan.areas);
    let _ = writeln!(
        out,
        "\nDesign needed: {}{}",
        if plan.design_needed { "yes" } else { "no" },
        plan.design_reason
            .as_deref()
            .map_or_else(String::new, |why| format!(" — {}", why.trim()))
    );
    list(&mut out, "Risks", &plan.risks);
    out.trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::approval::ApprovalIntent;
    use crate::flow::tracker::{Issue, IssueField, IssueLink, IssueStatus, StatusCategory};

    fn detail(description: &str) -> IssueDetail {
        IssueDetail {
            issue: Issue {
                key: "ACME-7".to_owned(),
                summary: "Export the unit list".to_owned(),
                status: IssueStatus {
                    name: "In Progress".to_owned(),
                    category: StatusCategory::InProgress,
                },
                kind: Some("Story".to_owned()),
                priority: Some("High".to_owned()),
                assignee: Some("Sam".to_owned()),
                labels: vec!["export".to_owned()],
            },
            description: Some(description.to_owned()),
            parent: None,
            links: vec![IssueLink {
                relation: "is blocked by".to_owned(),
                key: "ACME-3".to_owned(),
                summary: Some("Unit model".to_owned()),
                status: None,
            }],
            updated: Some("2026-10-22T10:00:00.000+0000".to_owned()),
            fields: vec![IssueField {
                name: "Acceptance criteria".to_owned(),
                id: "customfield_1".to_owned(),
                value: Some("A spreadsheet downloads".to_owned()),
            }],
            sprints: Vec::new(),
        }
    }

    fn record(revision: u32, issue: &IssueText) -> PlanRecord {
        PlanRecord {
            id: PlanId(i64::from(revision)),
            flow: FlowId(1),
            revision,
            step: None,
            invocation: None,
            plan: PlanReport {
                summary: "Export as CSV".to_owned(),
                steps: vec!["Write rows".to_owned()],
                acceptance_criteria: vec!["A file downloads".to_owned()],
                verification: vec!["A test".to_owned()],
                areas: vec!["src/export/**".to_owned()],
                design_needed: true,
                design_reason: None,
                risks: Vec::new(),
            },
            issue: issue.clone(),
            issue_revision: issue.revision(),
            submitted_at: 10,
        }
    }

    fn approved(revision: u32, issue: &IssueText) -> (Approval, PlanApproval, PlanDecision) {
        let approval = Approval::grant(
            ApprovalId(5),
            FlowId(1),
            ApprovalIntent::Plan,
            plan_binding(revision, &issue.revision()),
            20,
        )
        .unwrap();
        let subject = PlanApproval {
            revision,
            issue: issue.clone(),
            skip_design: false,
            design_needed: true,
        };
        let decision = PlanDecision {
            id: PlanDecisionId(1),
            flow: FlowId(1),
            plan: PlanId(i64::from(revision)),
            revision,
            verdict: PlanVerdict::Approved {
                approval: ApprovalId(5),
                skip_design: false,
            },
            at: 20,
        };
        (approval, subject, decision)
    }

    #[test]
    fn moving_or_reassigning_an_issue_does_not_change_its_revision() {
        let before = detail("Export the units.");
        let mut moved = before.clone();
        moved.issue.status.name = "In Review".to_owned();
        moved.issue.assignee = Some("Alex".to_owned());
        moved.issue.priority = Some("Low".to_owned());
        moved.issue.labels.clear();
        moved.updated = Some("2026-10-23T09:00:00.000+0000".to_owned());
        assert_eq!(
            IssueText::from_detail(&before).revision(),
            IssueText::from_detail(&moved).revision()
        );
    }

    #[test]
    fn editing_what_the_issue_asks_for_changes_its_revision() {
        let before = IssueText::from_detail(&detail("Export the units."));
        let mut field = detail("Export the units.");
        field.fields[0].value = Some("A CSV downloads".to_owned());
        let edited = IssueText::from_detail(&detail("Export the units as XLSX."));
        assert_ne!(before.revision(), edited.revision());
        assert_ne!(before.revision(), IssueText::from_detail(&field).revision());
    }

    #[test]
    fn a_plan_approved_for_the_issue_as_it_reads_stays_approved() {
        let issue = IssueText::from_detail(&detail("Export the units."));
        let latest = record(2, &issue);
        let (approval, subject, decision) = approved(2, &issue);
        assert_eq!(
            standing(
                Some(&latest),
                Some(&decision),
                Some((&approval, &subject)),
                Some(&issue)
            ),
            PlanStanding::Approved {
                approval: ApprovalId(5),
                revision: 2,
                skip_design: false,
            }
        );
    }

    #[test]
    fn an_issue_edited_after_its_plan_was_approved_makes_the_approval_stale_with_a_diff() {
        let issue = IssueText::from_detail(&detail("Export the units.\nAs a spreadsheet."));
        let latest = record(1, &issue);
        let (approval, subject, decision) = approved(1, &issue);
        let edited = IssueText::from_detail(&detail("Export the units.\nAs XLSX, not CSV."));

        let PlanStanding::Stale { changes, diff, .. } = standing(
            Some(&latest),
            Some(&decision),
            Some((&approval, &subject)),
            Some(&edited),
        ) else {
            panic!("an edited issue makes the approval stale");
        };
        assert_eq!(
            changes.iter().map(|c| c.pin).collect::<Vec<_>>(),
            [Pinned::IssueRevision]
        );
        assert_eq!(diff.len(), 1);
        assert_eq!(diff[0].part, "Description");
        assert_eq!(
            diff[0]
                .lines
                .iter()
                .map(|l| (l.kind, l.text.as_str()))
                .collect::<Vec<_>>(),
            [
                (DiffKind::Same, "Export the units."),
                (DiffKind::Removed, "As a spreadsheet."),
                (DiffKind::Added, "As XLSX, not CSV."),
            ]
        );
    }

    #[test]
    fn a_newer_plan_than_the_one_approved_leaves_the_approval_stale_too() {
        let issue = IssueText::from_detail(&detail("Export the units."));
        let (approval, subject, decision) = approved(1, &issue);
        let newer = record(2, &issue);
        let PlanStanding::Stale { changes, diff, .. } = standing(
            Some(&newer),
            Some(&decision),
            Some((&approval, &subject)),
            Some(&issue),
        ) else {
            panic!("a newer plan is not the one approved");
        };
        assert_eq!(changes[0].pin, Pinned::PlanRevision);
        assert!(diff.is_empty());
    }

    #[test]
    fn asking_for_changes_to_the_newest_plan_reads_as_revising_it() {
        let issue = IssueText::from_detail(&detail("Export the units."));
        let latest = record(1, &issue);
        let decision = PlanDecision {
            id: PlanDecisionId(1),
            flow: FlowId(1),
            plan: latest.id,
            revision: 1,
            verdict: PlanVerdict::ChangesRequested {
                note: "Keep the column order".to_owned(),
            },
            at: 30,
        };
        assert!(matches!(
            standing(Some(&latest), Some(&decision), None, None),
            PlanStanding::Revising { revision: 1, .. }
        ));
        // Once the planner hands in revision 2, it waits for you again.
        assert_eq!(
            standing(Some(&record(2, &issue)), Some(&decision), None, None),
            PlanStanding::Waiting { revision: 2 }
        );
    }

    #[test]
    fn a_short_field_that_changed_is_shown_before_and_after_without_a_line_diff() {
        let before = IssueText::from_detail(&detail("x"));
        let mut renamed = detail("x");
        renamed.issue.summary = "Export the unit list as XLSX".to_owned();
        let after = IssueText::from_detail(&renamed);
        let changes = diff(&before, &after);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].part, "Summary");
        assert!(changes[0].lines.is_empty());
        assert_eq!(
            changes[0].after.as_deref(),
            Some("Export the unit list as XLSX")
        );
    }

    #[test]
    fn the_plan_reads_back_as_text_with_every_part_named() {
        let issue = IssueText::from_detail(&detail("x"));
        let text = plan_text(&record(1, &issue).plan);
        for part in [
            "Export as CSV",
            "Steps:",
            "Acceptance criteria:",
            "Verification:",
            "Areas:",
            "Design needed: yes",
        ] {
            assert!(text.contains(part), "{part} in {text}");
        }
    }
}
