//! What a role hands back through `flow_submit`, the schema it is checked against, and the check.
//!
//! # The only route a result takes
//!
//! A role's result is what it submits, never what it says. A review's last message is prose about
//! the findings, written for a reader; parsing it would make the result whatever the model happened
//! to phrase, and a run that "finished" with an unparseable summary would look the same as one that
//! found nothing. So the engine reads a role's result from one place: a `flow_submit` call whose
//! payload passed [`read`]. A turn that ends without one has not produced a result, however
//! confident its last message sounds.
//!
//! # One schema, advertised and enforced
//!
//! [`schema`] is what a role is shown, in its context, and the same value is what [`read`] checks the
//! payload against before anything typed is built from it. Two copies, one for the model and one in
//! Rust, would drift, and the model would be told a shape the app then refuses. The typed structs
//! below are what the engine works with afterwards; the tests feed payloads through both halves,
//! and a payload the schema passes that the structs then refuse would come back as a problem
//! rather than a panic, so a disagreement between the two shows up as a refused submission.
//!
//! # Checked in two passes
//!
//! The structural pass ([`super::schema::check`]) catches a wrong type, a missing field, a field the
//! schema does not name. The second pass is what a JSON Schema cannot say without `if`/`then`, which
//! the checker deliberately does not support: a confirmed bug needs the verification that confirmed
//! it, a rule violation names its rule, a line needs a file. Both passes report every problem with a
//! JSON Pointer, and the problems go back to the model as the tool's error, for it to fix and submit
//! again in the same turn.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::recipe::OutputKind;
use super::review::{FindingClass, Location, Severity};
use super::rule::RuleId;
use super::schema::{self, SchemaProblem};

/// A plan, from a planning role: what approving it would let a writer do.
///
/// Every field but the risks is required, because each is something the approval rests on. The
/// acceptance criteria are what the work will be checked against, the verification is how, and the
/// areas are the paths a writer may touch without asking — the scope a later checkpoint is held to.
/// A plan that leaves one out would be approved for less than it looks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PlanReport {
    pub summary: String,
    pub steps: Vec<String>,
    /// What "done" means, one criterion each, as the issue's acceptance criteria say it or as the
    /// planner read them from the issue.
    pub acceptance_criteria: Vec<String>,
    /// How each criterion will be shown to hold: the tests, checks or manual steps.
    pub verification: Vec<String>,
    /// The files or directories the work expects to touch, relative to the repository root, as
    /// paths or globs in [`super::glob`]'s dialect.
    pub areas: Vec<String>,
    /// Whether the work needs a design pass before implementing: substantial UI, or a change a
    /// person should see before it is built.
    pub design_needed: bool,
    /// Why, or why not, in one sentence.
    #[serde(default)]
    pub design_reason: Option<String>,
    #[serde(default)]
    pub risks: Vec<String>,
}

/// An implementation's handoff, from a writer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Handoff {
    pub summary: String,
    #[serde(default)]
    pub changed: Vec<String>,
    #[serde(default)]
    pub checks: Vec<String>,
    #[serde(default)]
    pub open_questions: Vec<String>,
    /// What the writer says about each acceptance criterion of the plan. Its claim, shown as its
    /// claim: the hand-over sets it beside the checks and the review, which are the evidence.
    #[serde(default)]
    pub acceptance: Vec<CriterionClaim>,
}

/// A writer's word on one acceptance criterion, by its number in the plan's list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CriterionClaim {
    /// 1 for the plan's first criterion.
    pub criterion: u32,
    pub met: bool,
    /// Where it can be seen: a test, a file and line, a check.
    pub evidence: String,
}

/// A review's findings, from a reviewer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FindingsReport {
    #[serde(default)]
    pub summary: Option<String>,
    /// Empty is a result: the role looked and found nothing.
    pub findings: Vec<ReportedFinding>,
}

/// Which class a finding claims to be, before the second pass has checked it carries what that
/// class needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportedClass {
    ConfirmedBug,
    RuleViolation,
    Conflict,
    Suggestion,
}

/// The rule revision a violation breaks, as the role's context listed it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleCitation {
    pub id: i64,
    pub revision: u32,
}

/// One finding as a role reported it.
///
/// Flat rather than shaped like [`FindingClass`], because the schema a model is shown has to be one
/// object it can fill in: a tagged union is a `oneOf`, and models fill those in badly. The second
/// pass of [`read`] is what makes the flat shape as strict as the union would be.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ReportedFinding {
    pub title: String,
    pub severity: Severity,
    pub class: ReportedClass,
    /// Why it matters: the rationale a reader needs to agree or not.
    pub body: String,
    /// Relative to the repository root.
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub line: Option<u32>,
    #[serde(default)]
    pub end_line: Option<u32>,
    /// For a confirmed bug: what was run or read that confirmed it.
    #[serde(default)]
    pub verification: Option<String>,
    /// For a rule violation.
    #[serde(default)]
    pub rule: Option<RuleCitation>,
    /// For a conflict: the sources that disagree.
    #[serde(default)]
    pub between: Vec<String>,
    /// The requirement or acceptance criterion it is measured against, when that is not a rule.
    #[serde(default)]
    pub requirement: Option<String>,
    #[serde(default)]
    pub evidence: Vec<String>,
    /// When a role run again reports a finding from the earlier list its brief gave it, that
    /// finding's id there (`E12.0`): the role's own word that it still holds, which is how a
    /// re-review says what was addressed without comparing places that the new commits moved.
    #[serde(default)]
    pub repeats: Option<String>,
}

impl ReportedFinding {
    /// The domain's class, with the evidence [`read`] has already required.
    #[must_use]
    pub fn finding_class(&self) -> FindingClass {
        match self.class {
            ReportedClass::ConfirmedBug => FindingClass::ConfirmedBug {
                verification: self.verification.clone().unwrap_or_default(),
            },
            ReportedClass::RuleViolation => {
                let rule = self
                    .rule
                    .clone()
                    .unwrap_or(RuleCitation { id: 0, revision: 0 });
                FindingClass::RuleViolation {
                    rule: RuleId(rule.id),
                    revision: rule.revision,
                }
            }
            ReportedClass::Conflict => FindingClass::Conflict {
                between: self.between.clone(),
            },
            ReportedClass::Suggestion => FindingClass::Suggestion,
        }
    }

    /// Where it is, when it is about one place. A path with no line covers the whole file.
    #[must_use]
    pub fn location(&self) -> Option<Location> {
        let path = self.path.clone()?;
        let start = self.line.unwrap_or(1);
        Some(Location {
            path,
            start_line: start,
            end_line: self.end_line.unwrap_or(start),
        })
    }
}

/// A submission that passed [`read`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "payload", rename_all = "snake_case")]
pub enum Submission {
    Plan(PlanReport),
    Findings(FindingsReport),
    Handoff(Handoff),
}

impl Submission {
    #[must_use]
    pub fn kind(&self) -> OutputKind {
        match self {
            Self::Plan(_) => OutputKind::Plan,
            Self::Findings(_) => OutputKind::Findings,
            Self::Handoff(_) => OutputKind::Handoff,
        }
    }

    /// A few words for a receipt or a status line: "3 findings", "a plan of 4 steps".
    #[must_use]
    pub fn summary(&self) -> String {
        match self {
            Self::Plan(plan) => format!("a plan of {} steps", plan.steps.len()),
            Self::Findings(report) => match report.findings.len() {
                1 => "1 finding".to_owned(),
                n => format!("{n} findings"),
            },
            Self::Handoff(_) => "a handoff".to_owned(),
        }
    }
}

/// The spelling of `kind` on the `flow_submit` call.
#[must_use]
pub fn kind_name(kind: OutputKind) -> &'static str {
    match kind {
        OutputKind::Findings => "findings",
        OutputKind::Plan => "plan",
        OutputKind::Handoff => "handoff",
    }
}

/// `flow_submit`'s `kind`, read back.
#[must_use]
pub fn kind_from(name: &str) -> Option<OutputKind> {
    match name {
        "findings" => Some(OutputKind::Findings),
        "plan" => Some(OutputKind::Plan),
        "handoff" => Some(OutputKind::Handoff),
        _ => None,
    }
}

fn text() -> Value {
    json!({ "type": "string", "minLength": 1 })
}

fn texts() -> Value {
    json!({ "type": "array", "items": { "type": "string", "minLength": 1 } })
}

/// The JSON Schema a `kind` payload is checked against, and shown to the role that submits it.
#[must_use]
pub fn schema(kind: OutputKind) -> Value {
    match kind {
        OutputKind::Plan => json!({
            "type": "object",
            "properties": {
                "summary": { "type": "string", "minLength": 1, "description": "What the work is, in two or three sentences." },
                "steps": { "type": "array", "items": text(), "minItems": 1, "description": "The implementation, in order." },
                "acceptanceCriteria": { "type": "array", "items": text(), "minItems": 1, "description": "What done means, one criterion each." },
                "verification": { "type": "array", "items": text(), "minItems": 1, "description": "How each criterion will be shown to hold: tests, checks, manual steps." },
                "areas": { "type": "array", "items": text(), "minItems": 1, "description": "Files or directories the work expects to touch, relative to the repository root; globs allowed, such as src/export/**." },
                "designNeeded": { "type": "boolean", "description": "Whether a design pass should come before implementing: substantial UI, or something a person should see first." },
                "designReason": { "type": "string", "minLength": 1, "description": "Why, or why not, in one sentence." },
                "risks": texts(),
            },
            "required": ["summary", "steps", "acceptanceCriteria", "verification", "areas", "designNeeded"],
            "additionalProperties": false,
        }),
        OutputKind::Handoff => json!({
            "type": "object",
            "properties": {
                "summary": text(),
                "changed": texts(),
                "checks": texts(),
                "openQuestions": texts(),
                "acceptance": {
                    "type": "array",
                    "description": "One entry per acceptance criterion of the plan you were \
                                    given, by its number in that list: whether it is met, and \
                                    where that can be seen. Leave it out when you were given no \
                                    plan.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "criterion": { "type": "integer", "minimum": 1 },
                            "met": { "type": "boolean" },
                            "evidence": text(),
                        },
                        "required": ["criterion", "met", "evidence"],
                        "additionalProperties": false,
                    },
                },
            },
            "required": ["summary"],
            "additionalProperties": false,
        }),
        OutputKind::Findings => json!({
            "type": "object",
            "properties": {
                "summary": { "type": "string", "description": "One or two sentences on the change as a whole." },
                "findings": {
                    "type": "array",
                    "description": "Every finding. An empty list means you looked and found nothing.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "title": text(),
                            "severity": { "type": "string", "enum": ["minor", "notable", "blocking"] },
                            "class": {
                                "type": "string",
                                "enum": ["confirmed_bug", "rule_violation", "conflict", "suggestion"],
                                "description": "confirmed_bug needs `verification`; rule_violation needs `rule`; conflict needs `between` (two or more sources).",
                            },
                            "body": { "type": "string", "minLength": 1, "description": "Why it matters." },
                            "path": { "type": "string", "minLength": 1, "description": "Relative to the repository root." },
                            "line": { "type": "integer", "minimum": 1 },
                            "endLine": { "type": "integer", "minimum": 1 },
                            "verification": { "type": "string", "minLength": 1, "description": "What you ran or read that confirmed the bug." },
                            "rule": {
                                "type": "object",
                                "properties": {
                                    "id": { "type": "integer", "minimum": 1 },
                                    "revision": { "type": "integer", "minimum": 1 },
                                },
                                "required": ["id", "revision"],
                                "additionalProperties": false,
                            },
                            "between": texts(),
                            "requirement": text(),
                            "evidence": texts(),
                            "repeats": { "type": "string", "minLength": 1, "description": "Only when this reports again a finding from the earlier list in your brief: that finding's id, such as E12.0." },
                        },
                        "required": ["title", "severity", "class", "body"],
                        "additionalProperties": false,
                    },
                },
            },
            "required": ["findings"],
            "additionalProperties": false,
        }),
    }
}

/// Check a `kind` payload and build the submission, or list every problem with it.
///
/// # Errors
///
/// Every problem, each with the JSON Pointer of the value it is about.
pub fn read(kind: OutputKind, payload: &Value) -> Result<Submission, Vec<SchemaProblem>> {
    let problems = schema::check(&schema(kind), payload);
    if !problems.is_empty() {
        return Err(problems);
    }
    let typed = |e: serde_json::Error| vec![SchemaProblem::new("", e.to_string())];
    let submission = match kind {
        OutputKind::Plan => {
            let plan: PlanReport = serde_json::from_value(payload.clone()).map_err(typed)?;
            let problems = check_plan(&plan);
            if !problems.is_empty() {
                return Err(problems);
            }
            Submission::Plan(plan)
        }
        OutputKind::Handoff => {
            Submission::Handoff(serde_json::from_value(payload.clone()).map_err(typed)?)
        }
        OutputKind::Findings => {
            let report: FindingsReport = serde_json::from_value(payload.clone()).map_err(typed)?;
            let problems = check_findings(&report);
            if !problems.is_empty() {
                return Err(problems);
            }
            Submission::Findings(report)
        }
    };
    Ok(submission)
}

/// What a plan's schema cannot say: each area is a place in the repository, written so a writer's
/// scope can be checked against it later.
fn check_plan(plan: &PlanReport) -> Vec<SchemaProblem> {
    let mut problems = Vec::new();
    for (index, area) in plan.areas.iter().enumerate() {
        if let Err(why) = super::glob::check(area.trim()) {
            problems.push(SchemaProblem::new(format!("/areas/{index}"), why));
        }
    }
    problems
}

/// What the flat finding shape cannot say in the schema itself.
fn check_findings(report: &FindingsReport) -> Vec<SchemaProblem> {
    let mut problems = Vec::new();
    for (index, finding) in report.findings.iter().enumerate() {
        let at = |field: &str| format!("/findings/{index}/{field}");
        let blank = |text: &Option<String>| text.as_deref().is_none_or(|t| t.trim().is_empty());
        match finding.class {
            ReportedClass::ConfirmedBug if blank(&finding.verification) => {
                problems.push(SchemaProblem::new(
                    at("verification"),
                    "a confirmed bug needs the verification that confirmed it; if you did not \
                     confirm it, report it as a suggestion",
                ));
            }
            ReportedClass::RuleViolation if finding.rule.is_none() => {
                problems.push(SchemaProblem::new(
                    at("rule"),
                    "a rule violation names the rule and revision it breaks, as your context lists \
                     them; without one, report it as a suggestion",
                ));
            }
            ReportedClass::Conflict if finding.between.len() < 2 => {
                problems.push(SchemaProblem::new(
                    at("between"),
                    "a conflict names at least two sources that disagree",
                ));
            }
            _ => {}
        }
        if let Some(path) = &finding.path
            && (path.starts_with('/') || path.split('/').any(|segment| segment == ".."))
        {
            problems.push(SchemaProblem::new(
                at("path"),
                "must be relative to the repository root, with no `..`",
            ));
        }
        if finding.line.is_some() && finding.path.is_none() {
            problems.push(SchemaProblem::new(at("line"), "a line needs a `path`"));
        }
        match (finding.line, finding.end_line) {
            (None, Some(_)) => {
                problems.push(SchemaProblem::new(
                    at("endLine"),
                    "an end line needs a `line`",
                ));
            }
            (Some(start), Some(end)) if end < start => {
                problems.push(SchemaProblem::new(
                    at("endLine"),
                    format!("ends at {end}, before it starts at {start}"),
                ));
            }
            _ => {}
        }
    }
    problems
}

/// The refusal a model reads when a submission does not pass: every problem, then what to do.
#[must_use]
pub fn refusal(kind: OutputKind, problems: &[SchemaProblem]) -> String {
    let list: Vec<String> = problems.iter().map(|p| format!("- {p}")).collect();
    format!(
        "The `{}` payload was not accepted:\n{}\nFix these and call `flow_submit` again with the \
         whole payload. Nothing from this attempt was kept.",
        kind_name(kind),
        list.join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(extra: &Value) -> Value {
        let mut base = json!({
            "title": "Unbounded retry",
            "severity": "blocking",
            "class": "suggestion",
            "body": "The loop never gives up.",
        });
        for (key, value) in extra.as_object().unwrap() {
            base[key] = value.clone();
        }
        base
    }

    fn findings(items: &[Value]) -> Value {
        json!({ "findings": items })
    }

    fn problems(kind: OutputKind, payload: &Value) -> Vec<String> {
        let mut at: Vec<String> = read(kind, payload)
            .unwrap_err()
            .into_iter()
            .map(|p| p.at)
            .collect();
        at.sort();
        at
    }

    #[test]
    fn every_schema_uses_only_keywords_the_checker_enforces() {
        for kind in [OutputKind::Findings, OutputKind::Plan, OutputKind::Handoff] {
            schema::supported(&schema(kind)).unwrap();
        }
    }

    #[test]
    fn a_well_formed_findings_payload_becomes_findings_with_their_classes_and_places() {
        let payload = findings(&[
            finding(&json!({
                "class": "confirmed_bug",
                "verification": "ran the sync test; it never returns",
                "path": "src/sync.rs",
                "line": 42,
                "endLine": 48,
            })),
            finding(&json!({ "class": "rule_violation", "rule": { "id": 14, "revision": 2 } })),
        ]);
        let Ok(Submission::Findings(report)) = read(OutputKind::Findings, &payload) else {
            panic!("a well-formed payload is accepted");
        };
        assert_eq!(
            report.findings[0].finding_class(),
            FindingClass::ConfirmedBug {
                verification: "ran the sync test; it never returns".to_owned()
            }
        );
        assert_eq!(
            report.findings[0].location(),
            Some(Location {
                path: "src/sync.rs".to_owned(),
                start_line: 42,
                end_line: 48
            })
        );
        assert_eq!(
            report.findings[1].finding_class(),
            FindingClass::RuleViolation {
                rule: RuleId(14),
                revision: 2
            }
        );
    }

    #[test]
    fn looking_and_finding_nothing_is_a_result() {
        assert_eq!(
            read(OutputKind::Findings, &findings(&[])),
            Ok(Submission::Findings(FindingsReport {
                summary: None,
                findings: vec![]
            }))
        );
    }

    #[test]
    fn a_confirmed_bug_without_its_verification_is_refused_and_says_what_to_do_instead() {
        let refused = read(
            OutputKind::Findings,
            &findings(&[finding(&json!({ "class": "confirmed_bug" }))]),
        )
        .unwrap_err();
        assert_eq!(refused[0].at, "/findings/0/verification");
        assert!(refused[0].message.contains("suggestion"));
    }

    #[test]
    fn every_problem_in_a_submission_comes_back_at_once() {
        let payload = findings(&[
            finding(&json!({ "severity": "catastrophic" })),
            finding(&json!({ "class": "conflict", "between": ["REVIEW.md"] })),
            finding(&json!({ "line": 3 })),
            finding(&json!({ "path": "../outside.rs" })),
        ]);
        // The structural pass first: the severity is wrong, so the semantic pass never runs.
        assert_eq!(
            problems(OutputKind::Findings, &payload),
            ["/findings/0/severity"]
        );
        let mut fixed = payload;
        fixed["findings"][0]["severity"] = json!("minor");
        assert_eq!(
            problems(OutputKind::Findings, &fixed),
            [
                "/findings/1/between",
                "/findings/2/line",
                "/findings/3/path"
            ]
        );
    }

    #[test]
    fn a_line_range_that_ends_before_it_starts_is_refused() {
        let payload = findings(&[finding(&json!({ "path": "a.rs", "line": 9, "endLine": 3 }))]);
        assert_eq!(
            problems(OutputKind::Findings, &payload),
            ["/findings/0/endLine"]
        );
    }

    fn plan(extra: &Value) -> Value {
        let mut base = json!({
            "summary": "Export units as CSV from the list view.",
            "steps": ["Add the button", "Write the rows"],
            "acceptanceCriteria": ["A download starts", "Rows match the list"],
            "verification": ["A test that reads the file back"],
            "areas": ["src/export/**", "src/views/UnitList.svelte"],
            "designNeeded": false,
        });
        for (key, value) in extra.as_object().unwrap() {
            base[key] = value.clone();
        }
        base
    }

    #[test]
    fn a_field_the_schema_does_not_name_is_refused_rather_than_dropped() {
        // An agent adding `approved: true` to a plan must not get it silently ignored, or a later
        // reader would wonder where the approval went.
        let payload = plan(&json!({ "approved": true }));
        assert_eq!(problems(OutputKind::Plan, &payload), ["/approved"]);
    }

    #[test]
    fn a_plan_carries_its_criteria_its_verification_its_areas_and_whether_it_needs_design() {
        let Ok(Submission::Plan(read)) = read(OutputKind::Plan, &plan(&json!({}))) else {
            panic!("a whole plan is accepted");
        };
        assert_eq!(read.acceptance_criteria.len(), 2);
        assert_eq!(read.areas, ["src/export/**", "src/views/UnitList.svelte"]);
        assert!(!read.design_needed);

        let mut thin = plan(&json!({}));
        for field in [
            "acceptanceCriteria",
            "verification",
            "areas",
            "designNeeded",
        ] {
            thin.as_object_mut().unwrap().remove(field);
        }
        assert!(read_err_mentions(&thin, "acceptanceCriteria"));
        assert!(read_err_mentions(&thin, "designNeeded"));
    }

    fn read_err_mentions(payload: &Value, field: &str) -> bool {
        read(OutputKind::Plan, payload)
            .unwrap_err()
            .iter()
            .any(|p| p.to_string().contains(field))
    }

    #[test]
    fn a_plan_area_outside_the_repository_is_refused_where_it_was_written() {
        let payload = plan(&json!({ "areas": ["src/ok", "../elsewhere", "/etc"] }));
        assert_eq!(
            problems(OutputKind::Plan, &payload),
            ["/areas/1", "/areas/2"]
        );
    }

    #[test]
    fn a_submission_round_trips_through_the_json_the_store_keeps() {
        let submission = read(
            OutputKind::Handoff,
            &json!({ "summary": "done", "openQuestions": ["why?"] }),
        )
        .unwrap();
        let stored = serde_json::to_string(&submission).unwrap();
        assert_eq!(
            serde_json::from_str::<Submission>(&stored).unwrap(),
            submission
        );
        assert!(stored.contains(r#""kind":"handoff""#), "{stored}");
    }

    #[test]
    fn the_refusal_lists_each_problem_and_asks_for_the_whole_payload_again() {
        let words = refusal(
            OutputKind::Findings,
            &[SchemaProblem::new(
                "/findings/0/body",
                "is required and missing",
            )],
        );
        assert!(words.contains("- /findings/0/body: is required and missing"));
        assert!(words.contains("call `flow_submit` again"));
    }
}
