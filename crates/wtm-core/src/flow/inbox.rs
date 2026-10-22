//! What runs need you for: one durable list, and the only thing about a run that tells you.
//!
//! # One owner
//!
//! A run's sessions are headless, behind its rail, so nobody is looking at their panes and nothing
//! about a pane may notify. What a run needs — a question, an approval, a role that failed, a usage
//! pause, an interruption, a review that is ready to read — is an item here instead, and an item is
//! filed once, kept across restarts, and announced once. Home's Needs you, the Work view's inbox and
//! the dock badge all read the same items, so none of them can say something the others do not.
//!
//! # Kind and target
//!
//! An item's kind says what it asks of you. Its target says where the thing it is about lives —
//! the run, one of its roles, a finding, a question, an approval — which is where a click on it
//! lands, from the inbox or from a notification. The same kind about the same target is one item
//! while it is open: a role that fails twice, or a limit two routes report, is one item and one
//! notification, not two.
//!
//! # Open, resolved, dismissed
//!
//! An item is open until something closes it, and there are two ways to close one because they
//! mean different things afterwards. **Resolved** is the thing being settled: the question
//! answered, the paused role resumed, the failed role retried, the ready review read. **Dismissed**
//! is a person putting it away without settling it. Nothing an operating system does is either:
//! dismissing a notification banner closes the banner, and the item it was about stays open.

use serde::{Deserialize, Serialize};

use super::UnixMs;
use super::approval::ApprovalId;
use super::question::{Question, QuestionId};
use super::review::{Finding, FindingClass, FindingId, Severity};
use super::run::StepId;
use super::state::FlowId;
use crate::model::ProjectId;

id! {
    /// Something a run needs you to know or decide.
    InboxItemId
}

/// What an item asks of you.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InboxKind {
    /// A role asked something only you can answer.
    Question,
    /// Something waits on your yes or no. A provider's own permission prompt in a run session is
    /// one: under a run's profile it should never need to ask, so the prompt is worth knowing about
    /// whether or not anyone answers it.
    Approval,
    /// A role failed for good — retries spent, no fallback left — or the run could not run at all.
    Failure,
    /// A provider's usage limit paused a role, or switched it to its fallback.
    Capacity,
    /// The app went away mid-run, and the run waits for you to resume it.
    Interrupted,
    /// A review finished and its findings are ready to read.
    ReviewReady,
}

/// Where the thing an item is about lives, and so where a click on it lands.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InboxTarget {
    /// The run as a whole: its item's own flow.
    Flow,
    /// One role of the run, named so a reader need not look the step up.
    Step {
        step: StepId,
        role: String,
    },
    Finding {
        finding: FindingId,
    },
    Question {
        question: QuestionId,
    },
    Approval {
        approval: ApprovalId,
    },
}

impl InboxTarget {
    /// A short name for the target, as an item's subject: the role, or "run".
    #[must_use]
    pub fn subject(&self) -> String {
        match self {
            Self::Flow => "run".to_owned(),
            Self::Step { role, .. } => role.clone(),
            Self::Finding { finding } => format!("finding {finding}"),
            Self::Question { question } => format!("question {question}"),
            Self::Approval { approval } => format!("approval {approval}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InboxState {
    Open,
    /// The thing it was about is settled.
    Resolved,
    /// A person put it away without settling it.
    Dismissed,
}

/// Something a run needs you to know or decide, as the inbox lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InboxItem {
    pub id: InboxItemId,
    pub flow: FlowId,
    pub kind: InboxKind,
    pub target: InboxTarget,
    pub title: String,
    pub body: String,
    pub state: InboxState,
    pub created_at: UnixMs,
    /// When it was resolved or dismissed.
    pub closed_at: Option<UnixMs>,
    /// The project and worktree the run is of, so a click can arrive there without reading the run
    /// first: a review's, or the worktree an issue's start is giving it. `None` for any other flow.
    pub project: Option<ProjectId>,
    pub worktree: Option<String>,
    /// The issue's key, when the run is an issue's own (Start Work's): a click on an item about
    /// it lands on the issue rather than on a worktree's Reviews tab.
    #[serde(default)]
    pub issue: Option<String>,
    /// The question itself, for an item about one, so its card is drawn from one read.
    pub question: Option<Question>,
}

impl InboxItem {
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.state == InboxState::Open
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewInboxItem {
    pub flow: FlowId,
    pub kind: InboxKind,
    pub target: InboxTarget,
    pub title: String,
    pub body: String,
}

/// How many findings a review-ready item names before it says how many more there are.
const READY_NAMED: usize = 3;

/// The title and body a finished review is filed with: how much it found and the worst of it, so
/// the one notification a review sends says whether it is worth stopping for.
///
/// `findings` are the ones still standing, dismissed ones left out; `failed` are the roles that
/// failed for good, because a review missing one is incomplete and should say so where you read
/// that it is ready.
#[must_use]
pub fn review_ready(findings: &[&Finding], failed: &[&str]) -> (String, String) {
    let mut ranked: Vec<&Finding> = findings.to_vec();
    // Worst first: the class a reviewer acts on soonest, then severity, then the order they came.
    ranked.sort_by_key(|f| (class_rank(&f.class), std::cmp::Reverse(f.severity)));
    let count = ranked.len();
    let title = match (count, failed.len()) {
        (0, 0) => "Review ready: nothing found".to_owned(),
        (0, _) => "Review ready, incomplete: nothing found by the roles that ran".to_owned(),
        (n, 0) => format!("Review ready: {n} finding{}", plural(n)),
        (n, _) => format!("Review ready, incomplete: {n} finding{}", plural(n)),
    };
    let mut lines: Vec<String> = ranked
        .iter()
        .take(READY_NAMED)
        .map(|f| {
            let place = f.location.as_ref().map_or_else(String::new, |at| {
                format!(" ({}:{})", at.path, at.start_line)
            });
            format!(
                "{}, {}: {}{place}",
                class_words(&f.class),
                severity_words(f.severity),
                f.title
            )
        })
        .collect();
    if count > READY_NAMED {
        lines.push(format!("and {} more", count - READY_NAMED));
    }
    if !failed.is_empty() {
        let names: Vec<String> = failed.iter().map(|role| format!("`{role}`")).collect();
        lines.push(format!(
            "{} failed, so this review is missing what {} would have found.",
            names.join(", "),
            if failed.len() == 1 { "it" } else { "they" }
        ));
    }
    if lines.is_empty() {
        lines.push("Every role ran and none reported anything.".to_owned());
    }
    (title, lines.join("\n"))
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn class_rank(class: &FindingClass) -> u8 {
    match class {
        FindingClass::ConfirmedBug { .. } => 0,
        FindingClass::RuleViolation { .. } => 1,
        FindingClass::Conflict { .. } => 2,
        FindingClass::Suggestion => 3,
    }
}

fn class_words(class: &FindingClass) -> &'static str {
    match class {
        FindingClass::ConfirmedBug { .. } => "Confirmed bug",
        FindingClass::RuleViolation { .. } => "Rule violation",
        FindingClass::Conflict { .. } => "Conflict",
        FindingClass::Suggestion => "Suggestion",
    }
}

fn severity_words(severity: Severity) -> &'static str {
    match severity {
        Severity::Minor => "minor",
        Severity::Notable => "notable",
        Severity::Blocking => "blocking",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::review::Location;

    fn finding(id: i64, class: FindingClass, severity: Severity, title: &str) -> Finding {
        Finding {
            id: FindingId(id),
            flow: FlowId(1),
            class,
            severity,
            location: Some(Location {
                path: "src/totals.py".to_owned(),
                start_line: u32::try_from(id).unwrap(),
                end_line: u32::try_from(id).unwrap(),
            }),
            title: title.to_owned(),
            rationale: String::new(),
            requirement: None,
            tree_sha: "t".to_owned(),
            dismissed: None,
        }
    }

    #[test]
    fn a_ready_review_leads_with_its_worst_finding_whatever_order_the_roles_reported_in() {
        let suggestion = finding(1, FindingClass::Suggestion, Severity::Blocking, "Rename it");
        let bug = finding(
            2,
            FindingClass::ConfirmedBug {
                verification: "ran it".to_owned(),
            },
            Severity::Notable,
            "Drops the last row",
        );
        let minor_bug = finding(
            3,
            FindingClass::ConfirmedBug {
                verification: "ran it".to_owned(),
            },
            Severity::Minor,
            "Off by one in a log line",
        );

        let (title, body) = review_ready(&[&suggestion, &minor_bug, &bug], &[]);

        assert_eq!(title, "Review ready: 3 findings");
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(
            lines,
            [
                "Confirmed bug, notable: Drops the last row (src/totals.py:2)",
                "Confirmed bug, minor: Off by one in a log line (src/totals.py:3)",
                "Suggestion, blocking: Rename it (src/totals.py:1)",
            ]
        );
    }

    #[test]
    fn a_clean_review_says_it_found_nothing_and_a_long_one_counts_the_rest() {
        assert_eq!(
            review_ready(&[], &[]),
            (
                "Review ready: nothing found".to_owned(),
                "Every role ran and none reported anything.".to_owned()
            )
        );
        let many: Vec<Finding> = (1..=5)
            .map(|id| finding(id, FindingClass::Suggestion, Severity::Minor, "Tidy"))
            .collect();
        let (_, body) = review_ready(&many.iter().collect::<Vec<_>>(), &[]);
        assert_eq!(body.lines().last(), Some("and 2 more"));
    }

    #[test]
    fn a_ready_review_missing_a_role_says_it_is_incomplete() {
        let (title, body) = review_ready(&[], &["ui"]);
        assert_eq!(
            title,
            "Review ready, incomplete: nothing found by the roles that ran"
        );
        assert_eq!(
            body,
            "`ui` failed, so this review is missing what it would have found."
        );
    }

    #[test]
    fn a_target_says_which_role_it_is_about_in_its_subject() {
        let step = InboxTarget::Step {
            step: StepId(7),
            role: "correctness".to_owned(),
        };
        assert_eq!(step.subject(), "correctness");
        assert_eq!(InboxTarget::Flow.subject(), "run");
    }

    #[test]
    fn a_target_travels_as_its_kind_and_its_ids() {
        // The window routes a click by these exact keys, and `wtm-appdb` stores the same split, so
        // a rename has to fail here rather than land a click nowhere.
        let finding = InboxTarget::Finding {
            finding: FindingId(12),
        };
        assert_eq!(
            serde_json::to_value(&finding).unwrap(),
            serde_json::json!({ "kind": "finding", "finding": 12 })
        );
        let step = InboxTarget::Step {
            step: StepId(3),
            role: "ui".to_owned(),
        };
        assert_eq!(
            serde_json::to_value(&step).unwrap(),
            serde_json::json!({ "kind": "step", "step": 3, "role": "ui" })
        );
        assert_eq!(
            serde_json::to_value(InboxKind::ReviewReady).unwrap(),
            serde_json::json!("review_ready")
        );
    }
}
