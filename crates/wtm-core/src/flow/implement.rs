//! Implementing an approved plan: the scope a writer is held to, the bounded loop of review and fix
//! that follows its work, and which checks a fix round runs again.
//!
//! # The scope is the plan's, and only a plan change widens it
//!
//! The plan you approved names the places the work expects to touch (`areas`, globs) and, in its
//! prose, the files it means to change: "add a test in `tests/export_test.py`". Both are the scope
//! ([`ImplementScope`]). A writer's commit is checked against it before the app makes it, and a file
//! outside it is refused and becomes a question. The answer is never "let it in, this once": the
//! approval was of a plan, so the only way to widen what it covers is a plan revision that covers
//! more, and that revision needs your approval like the first. A scope that grew quietly would make
//! the approval mean less every round.
//!
//! # The loop is bounded twice, and stops rather than loops
//!
//! After the writer hands in, the app runs the checks and the review recipe, and fixes what they
//! found within the scope, automatically, through the same writer. Two bounds keep that from
//! becoming a model arguing with itself on your subscription: a number of fix rounds per review
//! ([`FixBounds::rounds`]) and a number of attempts per finding ([`FixBounds::attempts`]). When either
//! is spent the loop stops and hands over with what is left listed and why ([`Remaining`]). Nothing
//! here can produce another round once a bound says no: [`next_round`] is the only way into one.
//!
//! # A finding is "the same problem" by the reviewer's word first
//!
//! A finding found again after a fix is a new row: consolidation gives every new set of reports a
//! new id. So the attempts a problem has had are counted by identity, not by id: a report whose
//! `repeats` names a report of a finding a round tried is the same problem, because the reviewer said
//! so; failing that, the key consolidation already uses to carry a dismissal across reruns
//! ([`same_finding`](super::consolidate::same_finding)) decides.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::UnixMs;
use super::consolidate::{FindingRecord, same_finding};
use super::glob;
use super::question::QuestionId;
use super::recipe::CheckSpec;
use super::review::{FindingClass, FindingId, Location};
use super::run::Field;
use super::state::FlowId;
use super::submission::{PlanReport, ReportedFinding};
use crate::model::ProjectId;

// ═══════════════════════════════ the scope ═══════════════════════════════

/// The files an implementation may change: what the approved plan covers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImplementScope {
    /// The plan's areas, as globs in [`glob`]'s dialect.
    pub areas: Vec<String>,
    /// The files the plan names in its prose, which may lie outside its areas: a test file, a
    /// fixture, a migration.
    pub files: BTreeSet<String>,
}

impl ImplementScope {
    /// The scope of implementing `plan`.
    #[must_use]
    pub fn of(plan: &PlanReport) -> Self {
        let areas: Vec<String> = plan
            .areas
            .iter()
            .map(|area| area.trim().trim_start_matches("./").to_owned())
            .filter(|area| !area.is_empty())
            .collect();
        // A file an area already names is covered; listing it twice says nothing more.
        let files = named_files(plan)
            .into_iter()
            .filter(|file| !areas.contains(file))
            .collect();
        Self { areas, files }
    }

    /// Whether the plan covers `path`, relative to the repository root.
    #[must_use]
    pub fn covers(&self, path: &str) -> bool {
        self.files.contains(path) || self.areas.iter().any(|area| glob::matches(area, path))
    }

    /// The paths in `changed` the plan does not cover, in the order given, each once.
    #[must_use]
    pub fn outliers(&self, changed: &[String]) -> Vec<String> {
        let mut seen = BTreeSet::new();
        changed
            .iter()
            .filter(|path| !self.covers(path) && seen.insert(path.as_str()))
            .cloned()
            .collect()
    }
}

/// The repository files `plan` names in its words: its summary, steps, criteria, verification and
/// risks.
///
/// A plan says "add `tests/export_test.py`" in a step without listing it as an area, and holding
/// the writer to the areas alone would refuse the very file the plan asked for. So a token that
/// reads as a relative file path — a name with an extension, optionally under directories, in
/// backticks or not, with a `:line` suffix dropped — counts as named. Deliberately narrow: no URL,
/// no absolute path, no `..`, no version number, and no two-letter abbreviation such as "e.g.", so
/// a plan's prose cannot widen the scope by accident. A file the plan meant but did not name is a
/// question, which is the safe way to be wrong.
#[must_use]
pub fn named_files(plan: &PlanReport) -> BTreeSet<String> {
    let texts = std::iter::once(&plan.summary)
        .chain(&plan.steps)
        .chain(&plan.acceptance_criteria)
        .chain(&plan.verification)
        .chain(&plan.risks);
    let mut out = BTreeSet::new();
    for text in texts {
        for token in text.split(|c: char| c.is_whitespace() || c == ',' || c == ';') {
            if let Some(path) = file_token(token) {
                out.insert(path);
            }
        }
    }
    out
}

/// `token` as a relative file path, when it reads as one.
fn file_token(token: &str) -> Option<String> {
    let trimmed = token.trim_matches(|c: char| {
        matches!(
            c,
            '`' | '"' | '\'' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | '*'
        )
    });
    // A sentence's full stop, a colon before a list, then a `:12` or `:12-20` line suffix.
    let trimmed = trimmed.trim_end_matches(['.', ':', '!', '?']);
    let trimmed = match trimmed.rsplit_once(':') {
        Some((path, lines))
            if !lines.is_empty() && lines.chars().all(|c| c.is_ascii_digit() || c == '-') =>
        {
            path
        }
        _ => trimmed,
    };
    let trimmed = trimmed.trim_start_matches("./");
    if trimmed.is_empty()
        || trimmed.contains("://")
        || trimmed.starts_with('/')
        || trimmed.starts_with('~')
        || trimmed.contains('*')
        || trimmed
            .split('/')
            .any(|segment| segment.is_empty() || segment == "..")
        || !trimmed
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '@' | '+'))
    {
        return None;
    }
    let name = trimmed.rsplit('/').next()?;
    let (stem, extension) = name.rsplit_once('.')?;
    let extension_reads = !extension.is_empty()
        && extension.len() <= 10
        && extension.chars().all(char::is_alphanumeric)
        && extension.chars().any(char::is_alphabetic);
    // A bare `e.g` or `v1.2` is prose; a stem of two characters or more with a letter in it, or a
    // dotfile such as `.env.example`, is a name.
    let stem_reads = stem.chars().any(char::is_alphabetic)
        && (stem.chars().count() >= 2 || trimmed.contains('/') || stem.starts_with('.'));
    // Without a directory, `csv.writer` and `exporter.units` read exactly like `README.md`: a plan
    // about code is full of dotted names. The first live plan named six such "files". So a name at
    // the top level counts only with an extension files are given, which a module's attribute is
    // not; under a directory the path already says it is one.
    let top_level_reads = trimmed.contains('/')
        || stem.starts_with('.')
        || FILE_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str());
    (extension_reads && stem_reads && top_level_reads).then(|| trimmed.to_owned())
}

/// Extensions a file named at the top level of a plan's prose may have: documents, config and the
/// usual source files. Not exhaustive on purpose; a file the plan meant and this missed is a
/// question, which is the safe way to be wrong.
const FILE_EXTENSIONS: &[&str] = &[
    "md",
    "txt",
    "rst",
    "adoc",
    "json",
    "toml",
    "yaml",
    "yml",
    "lock",
    "ini",
    "cfg",
    "conf",
    "env",
    "example",
    "sql",
    "csv",
    "html",
    "css",
    "scss",
    "svg",
    "py",
    "rs",
    "ts",
    "tsx",
    "js",
    "jsx",
    "mjs",
    "cjs",
    "svelte",
    "vue",
    "go",
    "rb",
    "java",
    "kt",
    "swift",
    "c",
    "h",
    "cpp",
    "hpp",
    "cs",
    "php",
    "sh",
    "bash",
    "zsh",
    "xml",
    "gradle",
    "properties",
    "mod",
    "sum",
    "gitignore",
];

// ═══════════════════════════════ the bounds ═══════════════════════════════

/// Fix rounds after one review when your config does not say.
pub const DEFAULT_FIX_ROUNDS: u32 = 2;
/// Attempts at one finding when your config does not say.
pub const DEFAULT_FIX_ATTEMPTS: u32 = 1;
/// The most fix rounds a config may ask for. Each is a writer turn and a review, against your
/// usage window, with nobody watching.
pub const MAX_FIX_ROUNDS: u32 = 5;
/// The most attempts a config may give one finding. A third attempt at the same problem is the
/// writer guessing, and a person is cheaper.
pub const MAX_FIX_ATTEMPTS: u32 = 3;

/// How far the loop of fixing what a review found may go, as your config said when the
/// implementation began.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FixBounds {
    /// Fix rounds per plan revision. Zero reviews the work and fixes nothing.
    pub rounds: u32,
    /// Rounds that may try one problem.
    pub attempts: u32,
}

impl Default for FixBounds {
    fn default() -> Self {
        Self {
            rounds: DEFAULT_FIX_ROUNDS,
            attempts: DEFAULT_FIX_ATTEMPTS,
        }
    }
}

// ═══════════════════════════════ the record ═══════════════════════════════

/// Where an implementation got to. One still going has none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImplementOutcome {
    /// Handed over to you to test. Not final: a plan change you agree to afterwards, about a
    /// finding left outside the plan, opens it again.
    Ready,
    /// You stopped it. Final.
    Cancelled,
}

/// One Implement run's implementation: who writes, what holds it, and where its review is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Implementation {
    pub flow: FlowId,
    pub project: ProjectId,
    /// The worktree's id, which is its path.
    pub worktree: String,
    pub key: String,
    /// The writer role.
    pub role: String,
    /// The review recipe the work gets.
    pub recipe: String,
    /// The worktree's `HEAD` when implementing began: what its commits are measured from.
    pub base_sha: String,
    /// The plan revision being implemented now. A plan change you approve moves it on.
    pub plan_revision: u32,
    pub scope: ImplementScope,
    pub bounds: FixBounds,
    /// The review run that reads its work, once there is one: a review of the issue's own work
    /// item, run again after each round.
    pub review_flow: Option<FlowId>,
    pub outcome: Option<ImplementOutcome>,
    /// What it stopped with, when it handed over: the findings left, and why each was left.
    pub remaining: Vec<Remaining>,
    /// Why the loop stopped, once it has.
    pub stopped: Option<StopReason>,
    pub started_at: UnixMs,
    pub updated_at: UnixMs,
}

/// An implementation to record, the first time a run implements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewImplementation {
    pub flow: FlowId,
    pub project: ProjectId,
    pub worktree: String,
    pub key: String,
    pub role: String,
    pub recipe: String,
    pub base_sha: String,
    pub plan_revision: u32,
    pub scope: ImplementScope,
    pub bounds: FixBounds,
}

/// A change to an implementation's record.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImplementChange {
    /// A plan revision you approved, and its scope.
    pub plan: Option<(u32, ImplementScope)>,
    pub review_flow: Option<FlowId>,
    /// Clearing it opens a ready implementation again; a cancelled one stays cancelled.
    pub outcome: Field<ImplementOutcome>,
    pub remaining: Option<Vec<Remaining>>,
    /// Cleared when the loop runs again: it has not stopped then.
    pub stopped: Field<StopReason>,
}

/// What a round set out to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoundKind {
    /// Implementing a plan revision.
    Implement,
    /// Fixing what a review or the checks found.
    Fix,
}

/// A finding a fix round tried, as it was when the round began: enough to know the same problem
/// when a later review reports it under a new id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttemptedFinding {
    pub finding: FindingId,
    pub title: String,
    pub location: Option<Location>,
    /// Its reports, as a rerun's brief names them (`E12.0`): what `repeats` refers to.
    pub labels: Vec<String>,
    /// Its leading report, for [`same_finding`].
    pub lead: Option<ReportedFinding>,
}

impl AttemptedFinding {
    /// `record` as a round tries it.
    #[must_use]
    pub fn of(record: &FindingRecord) -> Self {
        Self {
            finding: record.finding.id,
            title: record.finding.title.clone(),
            location: record.finding.location.clone(),
            labels: record
                .evidence
                .iter()
                .map(|e| e.source.key().label())
                .collect(),
            lead: record.evidence.first().map(|e| e.reported.clone()),
        }
    }

    /// Whether `record` reports the problem this was: a report of it says it `repeats` one of
    /// this one's, or its leading report is about the same place for the same reason.
    #[must_use]
    pub fn is(&self, record: &FindingRecord) -> bool {
        if record.finding.id == self.finding {
            return true;
        }
        let repeated = record.evidence.iter().any(|e| {
            e.reported
                .repeats
                .as_ref()
                .is_some_and(|label| self.labels.contains(label))
        });
        repeated
            || match (&self.lead, record.evidence.first()) {
                (Some(lead), Some(now)) => same_finding(lead, &now.reported),
                _ => false,
            }
    }
}

/// One round of writing: the plan, or a fix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[allow(clippy::struct_field_names)]
pub struct Round {
    pub flow: FlowId,
    /// 1 for the first, one more for each after it, across plan revisions.
    pub round: u32,
    pub kind: RoundKind,
    pub plan_revision: u32,
    /// For a fix round: the findings it was asked to fix.
    pub findings: Vec<AttemptedFinding>,
    /// For a fix round: the checks it was asked to make pass.
    pub checks: Vec<String>,
    /// The worktree's `HEAD` once the round's work was committed.
    pub head_sha: Option<String>,
    /// The files the round changed, from its first commit's parent to its last.
    pub changed: Vec<String>,
    pub started_at: UnixMs,
    pub ended_at: Option<UnixMs>,
}

/// A round to record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewRound {
    pub flow: FlowId,
    pub kind: RoundKind,
    pub plan_revision: u32,
    pub findings: Vec<AttemptedFinding>,
    pub checks: Vec<String>,
}

/// Who asked for a commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckpointBy {
    /// The writer, through `flow_checkpoint`.
    Writer,
    /// The app, committing what the writer left within the scope when it handed in.
    App,
}

/// A commit the app made for an implementation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImplementCheckpoint {
    pub flow: FlowId,
    pub round: u32,
    pub commit: String,
    pub message: String,
    pub paths: Vec<String>,
    pub by: CheckpointBy,
    pub made_at: UnixMs,
}

/// What a question an implementation asked is about, so its answer can do what it says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImplementAsk {
    /// The writer changed files the plan does not cover.
    Outliers,
    /// A review found problems in files the plan does not cover.
    Findings,
    /// The writer asked, with `flow_ask`.
    Writer,
}

/// A question an implementation asked, and what it is about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImplementQuestion {
    pub question: QuestionId,
    pub flow: FlowId,
    pub kind: ImplementAsk,
    /// The files a plan change would add.
    pub paths: Vec<String>,
    /// For [`ImplementAsk::Findings`]: the findings outside the plan.
    pub findings: Vec<FindingId>,
    /// The plan revision it was asked under.
    pub plan_revision: u32,
}

/// The option a scope question offers first: the plan grows to cover the files, and the revision
/// that does goes to you for approval.
pub const CHANGE_THE_PLAN: usize = 0;

/// What a scope question about `count` files offers, in order: [`CHANGE_THE_PLAN`] first.
#[must_use]
pub fn scope_options(kind: ImplementAsk, count: usize) -> Vec<String> {
    let them = if count == 1 { "it" } else { "them" };
    match kind {
        ImplementAsk::Findings => vec![
            format!("Change the plan to include {them}"),
            format!("Leave {them} for later"),
        ],
        _ => vec![
            format!("Change the plan to include {them}"),
            "Keep to the plan".to_owned(),
        ],
    }
}

/// The plan revision a "change the plan" answer records: `plan`, covering `paths` as well, with the
/// reason in its risks so the revision says why it is wider.
#[must_use]
pub fn widened(plan: &PlanReport, paths: &[String], why: &str) -> PlanReport {
    let mut next = plan.clone();
    for path in paths {
        if !next.areas.iter().any(|area| area == path) {
            next.areas.push(path.clone());
        }
    }
    next.risks.push(format!(
        "Widened while implementing, by your answer: {} added to the areas. {}",
        paths
            .iter()
            .map(|p| format!("`{p}`"))
            .collect::<Vec<_>>()
            .join(", "),
        why.trim()
    ));
    next
}

// ═══════════════════════════════ the loop ═══════════════════════════════

/// Why a finding was left for you rather than fixed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeftBecause {
    /// It is about files the plan does not cover; fixing it needs a plan change.
    OutsidePlan { paths: Vec<String> },
    /// It survived as many rounds as one problem is given.
    AttemptsSpent { attempts: u32 },
    /// The loop had used its rounds.
    RoundsSpent,
    /// Two sources of guidance disagree, which is a person's to settle.
    Conflict,
    /// A suggestion: optional by definition, so never changed on the loop's own say-so.
    Suggestion,
}

/// A finding left standing when the loop stopped, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Remaining {
    pub finding: FindingId,
    pub title: String,
    pub location: Option<Location>,
    pub because: LeftBecause,
}

/// A check left failing when the loop stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FailingCheck {
    pub check: String,
    pub because: LeftBecause,
}

/// Why the loop stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StopReason {
    /// Nothing the loop may fix is left.
    Clean,
    /// Something is left, and none of it is the loop's to fix: outside the plan, a conflict, a
    /// suggestion, or a problem already tried as often as allowed.
    NothingFixable,
    /// The rounds were spent with fixable work left.
    Bounded { rounds: u32 },
}

/// What the loop knows after a review.
#[derive(Debug, Clone, Copy)]
pub struct LoopFacts<'a> {
    /// The review's findings as saved, dismissed ones included.
    pub findings: &'a [FindingRecord],
    /// Checks whose newest record ran and did not pass. A blocked check did not run, so it is not
    /// here: making it pass is starting its services, not changing code.
    pub failing: &'a [String],
    pub scope: &'a ImplementScope,
    /// Every round of this implementation so far.
    pub rounds: &'a [Round],
    /// The plan revision being implemented: only its fix rounds count against the bounds.
    pub plan_revision: u32,
    pub bounds: FixBounds,
}

/// What happens after a review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NextRound {
    /// Fix these, through the writer, then verify and review again.
    Fix {
        findings: Vec<FindingId>,
        checks: Vec<String>,
        /// What this round leaves alone, and why.
        left: Vec<Remaining>,
    },
    /// Hand over to you.
    Stop {
        why: StopReason,
        remaining: Vec<Remaining>,
        failing: Vec<FailingCheck>,
    },
}

impl NextRound {
    /// The findings outside the plan, by the files they are in: what a scope question asks about.
    #[must_use]
    pub fn outside_plan(&self) -> Vec<(FindingId, Vec<String>)> {
        let listed = match self {
            Self::Fix { left, .. } => left,
            Self::Stop { remaining, .. } => remaining,
        };
        listed
            .iter()
            .filter_map(|r| match &r.because {
                LeftBecause::OutsidePlan { paths } => Some((r.finding, paths.clone())),
                _ => None,
            })
            .collect()
    }
}

/// The fix rounds of `plan_revision` so far.
fn fix_rounds(rounds: &[Round], plan_revision: u32) -> impl Iterator<Item = &Round> {
    rounds
        .iter()
        .filter(move |r| r.kind == RoundKind::Fix && r.plan_revision == plan_revision)
}

/// How many fix rounds of the current plan tried the problem `record` reports.
#[must_use]
pub fn attempts_at(record: &FindingRecord, rounds: &[Round], plan_revision: u32) -> u32 {
    let tried = fix_rounds(rounds, plan_revision)
        .filter(|round| round.findings.iter().any(|tried| tried.is(record)))
        .count();
    u32::try_from(tried).unwrap_or(u32::MAX)
}

/// Decide what follows a review: another fix round, or handing over.
///
/// A confirmed bug or a rule violation inside the plan, not yet tried as often as allowed, is the
/// loop's to fix; so is a check that ran and failed. Everything else is left with its reason. With
/// work to fix and rounds left, that is the next round; otherwise the loop stops, and says whether
/// it stopped clean, with nothing it may fix, or at its bound.
#[must_use]
pub fn next_round(facts: &LoopFacts<'_>) -> NextRound {
    let mut fixable = Vec::new();
    let mut left = Vec::new();
    for record in facts.findings {
        let finding = &record.finding;
        if finding.dismissed.is_some() {
            continue;
        }
        let leave = |because| Remaining {
            finding: finding.id,
            title: finding.title.clone(),
            location: finding.location.clone(),
            because,
        };
        match &finding.class {
            FindingClass::Suggestion => {
                left.push(leave(LeftBecause::Suggestion));
                continue;
            }
            FindingClass::Conflict { .. } => {
                left.push(leave(LeftBecause::Conflict));
                continue;
            }
            FindingClass::ConfirmedBug { .. } | FindingClass::RuleViolation { .. } => {}
        }
        if let Some(location) = &finding.location
            && !facts.scope.covers(&location.path)
        {
            left.push(leave(LeftBecause::OutsidePlan {
                paths: vec![location.path.clone()],
            }));
            continue;
        }
        let attempts = attempts_at(record, facts.rounds, facts.plan_revision);
        if attempts >= facts.bounds.attempts {
            left.push(leave(LeftBecause::AttemptsSpent { attempts }));
            continue;
        }
        fixable.push(record);
    }

    let mut checks = Vec::new();
    let mut failing = Vec::new();
    for check in facts.failing {
        let attempts = fix_rounds(facts.rounds, facts.plan_revision)
            .filter(|round| round.checks.contains(check))
            .count();
        let attempts = u32::try_from(attempts).unwrap_or(u32::MAX);
        if attempts >= facts.bounds.attempts {
            failing.push(FailingCheck {
                check: check.clone(),
                because: LeftBecause::AttemptsSpent { attempts },
            });
        } else {
            checks.push(check.clone());
        }
    }

    if fixable.is_empty() && checks.is_empty() {
        let why = if left.is_empty() && failing.is_empty() {
            StopReason::Clean
        } else {
            StopReason::NothingFixable
        };
        return NextRound::Stop {
            why,
            remaining: left,
            failing,
        };
    }
    let done =
        u32::try_from(fix_rounds(facts.rounds, facts.plan_revision).count()).unwrap_or(u32::MAX);
    if done >= facts.bounds.rounds {
        left.extend(fixable.iter().map(|record| Remaining {
            finding: record.finding.id,
            title: record.finding.title.clone(),
            location: record.finding.location.clone(),
            because: LeftBecause::RoundsSpent,
        }));
        failing.extend(checks.into_iter().map(|check| FailingCheck {
            check,
            because: LeftBecause::RoundsSpent,
        }));
        return NextRound::Stop {
            why: StopReason::Bounded { rounds: done },
            remaining: left,
            failing,
        };
    }
    NextRound::Fix {
        findings: fixable.iter().map(|record| record.finding.id).collect(),
        checks,
        left,
    }
}

/// The checks a round's changes make worth running again: those with no `paths`, which a change
/// anywhere may affect, and those whose `paths` cover a changed file. Nothing changed runs nothing.
#[must_use]
pub fn affected_checks<'a>(
    checks: impl IntoIterator<Item = (&'a str, &'a CheckSpec)>,
    changed: &[String],
) -> Vec<String> {
    if changed.is_empty() {
        return Vec::new();
    }
    checks
        .into_iter()
        .filter(|(_, spec)| {
            spec.paths.is_empty()
                || changed
                    .iter()
                    .any(|path| spec.paths.iter().any(|glob| glob::matches(glob, path)))
        })
        .map(|(id, _)| id.to_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::consolidate::{Evidence, Source};
    use crate::flow::recipe::{RoleProvider, Workspace};
    use crate::flow::review::{Dismissal, Finding, Severity};
    use crate::flow::run::{InvocationId, StepId};
    use crate::flow::submission::ReportedClass;

    fn plan(areas: &[&str], steps: &[&str]) -> PlanReport {
        PlanReport {
            summary: "Export units as CSV".to_owned(),
            steps: steps.iter().map(|s| (*s).to_owned()).collect(),
            acceptance_criteria: vec!["The export lists every unit".to_owned()],
            verification: vec!["`just test` passes".to_owned()],
            areas: areas.iter().map(|s| (*s).to_owned()).collect(),
            design_needed: false,
            design_reason: None,
            risks: Vec::new(),
        }
    }

    #[test]
    fn a_plans_areas_and_the_files_its_steps_name_are_its_scope() {
        let scope = ImplementScope::of(&plan(
            &["src/export/**"],
            &["Add a test in `tests/export_test.py:12` and update README.md."],
        ));
        assert!(scope.covers("src/export/csv.rs"));
        assert!(scope.covers("tests/export_test.py"));
        assert!(scope.covers("README.md"));
        assert!(!scope.covers("src/main.rs"));
        assert_eq!(
            scope.outliers(&["src/export/csv.rs".to_owned(), "src/main.rs".to_owned()]),
            vec!["src/main.rs".to_owned()]
        );
    }

    #[test]
    fn prose_that_only_looks_like_a_path_names_no_file() {
        let named = named_files(&plan(
            &[],
            &[
                "Use v1.2 of the API, e.g. the /v2 endpoint, as https://example.com/a.html says",
                "Touch ../outside.rs and ~/notes.md and src/*.rs, i.e. nothing",
            ],
        ));
        assert!(named.is_empty(), "{named:?}");
    }

    #[test]
    fn a_dotted_name_in_code_is_not_a_file_and_a_named_area_is_not_listed_twice() {
        // What a real planner wrote while the click-through ran: Python names, not files.
        let scope = ImplementScope::of(&plan(
            &["exporter/csv_export.py"],
            &[
                "Use csv.writer over io.StringIO and read Unit.occupied from exporter.units.",
                "Run sys.executable -m exporter.csv_export and compare with exporter/csv_export.py.",
                "Mention it in README.md and package.json.",
            ],
        ));
        assert_eq!(
            scope.files,
            BTreeSet::from(["README.md".to_owned(), "package.json".to_owned()]),
            "{scope:?}"
        );
    }

    #[test]
    fn a_dotfile_named_by_the_plan_is_a_file() {
        let named = named_files(&plan(&[], &["Copy `.env.example` to configure it"]));
        assert_eq!(named, BTreeSet::from([".env.example".to_owned()]));
    }

    fn record(id: i64, class: FindingClass, path: Option<&str>, invocation: i64) -> FindingRecord {
        let reported = ReportedFinding {
            title: format!("finding {id}"),
            severity: Severity::Notable,
            class: match &class {
                FindingClass::ConfirmedBug { .. } => ReportedClass::ConfirmedBug,
                FindingClass::RuleViolation { .. } => ReportedClass::RuleViolation,
                FindingClass::Conflict { .. } => ReportedClass::Conflict,
                FindingClass::Suggestion => ReportedClass::Suggestion,
            },
            body: "why".to_owned(),
            path: path.map(str::to_owned),
            line: Some(10),
            end_line: Some(12),
            verification: Some("ran it".to_owned()),
            rule: None,
            between: Vec::new(),
            requirement: None,
            evidence: Vec::new(),
            repeats: None,
        };
        FindingRecord {
            finding: Finding {
                id: FindingId(id),
                flow: FlowId(2),
                class,
                severity: Severity::Notable,
                location: path.map(|p| Location {
                    path: p.to_owned(),
                    start_line: 10,
                    end_line: 12,
                }),
                title: format!("finding {id}"),
                rationale: "why".to_owned(),
                requirement: None,
                tree_sha: "t".to_owned(),
                dismissed: None,
            },
            lines: true,
            evidence: vec![Evidence {
                source: Source {
                    step: StepId(1),
                    invocation: InvocationId(invocation),
                    ordinal: 0,
                    role: "correctness".to_owned(),
                    provider: RoleProvider::Claude,
                    model: None,
                    workspace: Workspace::Snapshot,
                    tree_sha: "t".to_owned(),
                },
                reported,
            }],
        }
    }

    fn bug(id: i64, path: &str, invocation: i64) -> FindingRecord {
        record(
            id,
            FindingClass::ConfirmedBug {
                verification: "ran it".to_owned(),
            },
            Some(path),
            invocation,
        )
    }

    fn fix_round(round: u32, tried: &[&FindingRecord], checks: &[&str]) -> Round {
        Round {
            flow: FlowId(1),
            round,
            kind: RoundKind::Fix,
            plan_revision: 1,
            findings: tried.iter().map(|r| AttemptedFinding::of(r)).collect(),
            checks: checks.iter().map(|c| (*c).to_owned()).collect(),
            head_sha: None,
            changed: Vec::new(),
            started_at: 0,
            ended_at: None,
        }
    }

    fn scope() -> ImplementScope {
        ImplementScope::of(&plan(&["src/**"], &[]))
    }

    #[test]
    fn a_bug_inside_the_plan_is_fixed_and_one_outside_it_is_left_for_a_plan_change() {
        let inside = bug(1, "src/export.rs", 10);
        let outside = bug(2, "docs/guide.md", 10);
        let findings = [inside, outside];
        let next = next_round(&LoopFacts {
            findings: &findings,
            failing: &[],
            scope: &scope(),
            rounds: &[],
            plan_revision: 1,
            bounds: FixBounds::default(),
        });
        let NextRound::Fix {
            findings: fixing,
            left,
            ..
        } = &next
        else {
            panic!("expected a fix round, got {next:?}");
        };
        assert_eq!(fixing, &vec![FindingId(1)]);
        assert_eq!(
            left[0].because,
            LeftBecause::OutsidePlan {
                paths: vec!["docs/guide.md".to_owned()]
            }
        );
        assert_eq!(
            next.outside_plan(),
            vec![(FindingId(2), vec!["docs/guide.md".to_owned()])]
        );
    }

    #[test]
    fn a_problem_reported_again_after_its_attempt_is_left_however_its_id_changed() {
        let first = bug(1, "src/export.rs", 10);
        let rounds = [fix_round(2, &[&first], &[])];
        // Found again after the fix, as a new finding: the reviewer said it repeats E10.0.
        let mut again = bug(7, "src/other.rs", 20);
        again.evidence[0].reported.repeats = Some("E10.0".to_owned());
        let findings = [again];
        let next = next_round(&LoopFacts {
            findings: &findings,
            failing: &[],
            scope: &scope(),
            rounds: &rounds,
            plan_revision: 1,
            bounds: FixBounds::default(),
        });
        assert_eq!(
            next,
            NextRound::Stop {
                why: StopReason::NothingFixable,
                remaining: vec![Remaining {
                    finding: FindingId(7),
                    title: "finding 7".to_owned(),
                    location: findings[0].finding.location.clone(),
                    because: LeftBecause::AttemptsSpent { attempts: 1 },
                }],
                failing: Vec::new(),
            }
        );
    }

    #[test]
    fn the_same_place_for_the_same_reason_is_the_same_problem_when_nobody_said_repeats() {
        let first = bug(1, "src/export.rs", 10);
        let rounds = [fix_round(2, &[&first], &[])];
        let again = bug(9, "src/export.rs", 30);
        assert_eq!(attempts_at(&again, &rounds, 1), 1);
        assert_eq!(attempts_at(&again, &rounds, 2), 0);
    }

    #[test]
    fn the_loop_stops_at_its_round_bound_with_what_it_could_still_fix_listed() {
        let first = bug(1, "src/a.rs", 10);
        let second = bug(2, "src/b.rs", 11);
        let rounds = [fix_round(2, &[&first], &[]), fix_round(3, &[&second], &[])];
        let fresh = bug(3, "src/c.rs", 12);
        let findings = [fresh];
        let next = next_round(&LoopFacts {
            findings: &findings,
            failing: &["unit".to_owned()],
            scope: &scope(),
            rounds: &rounds,
            plan_revision: 1,
            bounds: FixBounds {
                rounds: 2,
                attempts: 1,
            },
        });
        let NextRound::Stop {
            why,
            remaining,
            failing,
        } = next
        else {
            panic!("the bound must stop the loop");
        };
        assert_eq!(why, StopReason::Bounded { rounds: 2 });
        assert_eq!(remaining[0].because, LeftBecause::RoundsSpent);
        assert_eq!(failing[0].check, "unit");
    }

    #[test]
    fn zero_rounds_reviews_the_work_and_fixes_nothing() {
        let findings = [bug(1, "src/a.rs", 10)];
        let next = next_round(&LoopFacts {
            findings: &findings,
            failing: &[],
            scope: &scope(),
            rounds: &[],
            plan_revision: 1,
            bounds: FixBounds {
                rounds: 0,
                attempts: 1,
            },
        });
        assert!(matches!(
            next,
            NextRound::Stop {
                why: StopReason::Bounded { rounds: 0 },
                ..
            }
        ));
    }

    #[test]
    fn suggestions_conflicts_and_dismissed_findings_are_never_the_loops_to_fix() {
        let mut dismissed = bug(1, "src/a.rs", 10);
        dismissed.finding.dismissed = Some(Dismissal {
            reason: "intended".to_owned(),
            at: 0,
        });
        let findings = [
            dismissed,
            record(2, FindingClass::Suggestion, Some("src/a.rs"), 10),
            record(
                3,
                FindingClass::Conflict {
                    between: vec!["R-1".to_owned(), "REVIEW.md".to_owned()],
                },
                None,
                10,
            ),
        ];
        let next = next_round(&LoopFacts {
            findings: &findings,
            failing: &[],
            scope: &scope(),
            rounds: &[],
            plan_revision: 1,
            bounds: FixBounds::default(),
        });
        let NextRound::Stop { why, remaining, .. } = next else {
            panic!("nothing here is fixable");
        };
        assert_eq!(why, StopReason::NothingFixable);
        let reasons: Vec<&LeftBecause> = remaining.iter().map(|r| &r.because).collect();
        assert_eq!(reasons, [&LeftBecause::Suggestion, &LeftBecause::Conflict]);
    }

    #[test]
    fn a_clean_review_with_passing_checks_stops_clean() {
        let next = next_round(&LoopFacts {
            findings: &[],
            failing: &[],
            scope: &scope(),
            rounds: &[],
            plan_revision: 1,
            bounds: FixBounds::default(),
        });
        assert_eq!(
            next,
            NextRound::Stop {
                why: StopReason::Clean,
                remaining: Vec::new(),
                failing: Vec::new(),
            }
        );
    }

    #[test]
    fn a_failing_check_gets_a_round_and_then_is_left_once_tried() {
        let failing = ["unit".to_owned()];
        let first = next_round(&LoopFacts {
            findings: &[],
            failing: &failing,
            scope: &scope(),
            rounds: &[],
            plan_revision: 1,
            bounds: FixBounds::default(),
        });
        assert!(matches!(&first, NextRound::Fix { checks, .. } if checks == &["unit".to_owned()]));
        let rounds = [fix_round(2, &[], &["unit"])];
        let after = next_round(&LoopFacts {
            findings: &[],
            failing: &failing,
            scope: &scope(),
            rounds: &rounds,
            plan_revision: 1,
            bounds: FixBounds::default(),
        });
        assert!(matches!(
            after,
            NextRound::Stop {
                why: StopReason::NothingFixable,
                ..
            }
        ));
    }

    #[test]
    fn only_checks_whose_paths_a_change_touched_run_again() {
        let spec = |paths: &[&str]| CheckSpec {
            label: None,
            run: vec!["true".to_owned()],
            timeout_ms: None,
            needs: Vec::new(),
            start_services: None,
            paths: paths.iter().map(|p| (*p).to_owned()).collect(),
        };
        let lint = spec(&[]);
        let unit = spec(&["src/**"]);
        let docs = spec(&["docs/**"]);
        let checks = [("lint", &lint), ("unit", &unit), ("docs", &docs)];
        assert_eq!(
            affected_checks(checks, &["src/export.rs".to_owned()]),
            vec!["lint".to_owned(), "unit".to_owned()]
        );
        assert!(affected_checks(checks, &[]).is_empty());
    }

    #[test]
    fn widening_a_plan_adds_the_files_to_its_areas_and_says_why() {
        let next = widened(
            &plan(&["src/**"], &[]),
            &["docs/guide.md".to_owned()],
            "The export needs a line in the guide.",
        );
        assert_eq!(next.areas, ["src/**", "docs/guide.md"]);
        assert!(next.risks[0].contains("`docs/guide.md`"));
        assert!(ImplementScope::of(&next).covers("docs/guide.md"));
    }
}
