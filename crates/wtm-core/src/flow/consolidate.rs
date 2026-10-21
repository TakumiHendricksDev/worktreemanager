//! Consolidation: what several roles reported, as one list of findings.
//!
//! # The same problem, reported twice, is one finding
//!
//! Two reviewers that read the same change often see the same thing: a correctness role runs the
//! test and confirms the off-by-one, and a conventions role, reading the same lines, suspects it.
//! Listing both makes the review look twice as worried as it is, and dismissing one leaves the
//! other. So reports are merged when they are about the same place for the same reason, and the
//! finding keeps every report as its evidence: who said it, on which snapshot, and what they ran.
//!
//! # The key, and why it is so plain
//!
//! Two reports are the same finding when they are in the same file, their lines overlap, and they
//! cite the same rule (or neither cites one). That is all. Comparing titles or bodies would make the
//! merge depend on how two models happened to phrase a sentence, which is neither deterministic nor
//! explainable; a person reading "merged because both are about `export.rs` lines 40–48" can check
//! it. Overlap is transitive here — if A overlaps B and B overlaps C, all three are one finding —
//! because the alternative depends on which pair is compared first, and the result must not depend
//! on the order roles finished in.
//!
//! Two refinements, each because the plain key would merge things a reader would not:
//!
//! - **A conflict merges only with conflicts.** It is about two sources of guidance disagreeing,
//!   not about the code at those lines, and a bug report absorbing it would hide it from the one
//!   section where you resolve it. A conflict about no file is the same finding as another that names
//!   the same sources, whatever the order they are named in.
//! - **A report about no file** merges only with another that cites the same rule. Without a place
//!   or a rule there is nothing to say two of them are the same.
//!
//! A report that names a file and no line is about the whole file, and overlaps every line of it.
//!
//! # Who speaks for a merged finding
//!
//! One report leads: the strongest class (a confirmed bug over a rule violation over a conflict
//! over a suggestion), then the most severe, then the earliest role in name order. Its title, body
//! and class are the finding's; its severity is raised to the highest any report gave. The others
//! stay as evidence, in full.
//!
//! # Keeping a finding's id across a rerun
//!
//! A finding is saved, so it has an id that a dismissal, a rule's provenance and the window all
//! refer to. [`reconcile`] decides, after a role is run again, which saved findings still stand
//! as they were, which are replaced, and which dismissals carry over. A dismissal follows the
//! problem rather than the row: if the rerun role reports the same thing again, it stays dismissed.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::glob;
use super::recipe::{RoleProvider, Workspace};
use super::review::{Dismissal, Finding, FindingClass, FindingId, Location, Severity};
use super::run::{InvocationId, StepId};
use super::snapshot::TreeCheck;
use super::submission::{ReportedClass, ReportedFinding};

/// Where one reported finding came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Source {
    pub step: StepId,
    /// The attempt whose submission it was in.
    pub invocation: InvocationId,
    /// Its position in that submission, from 0. With the invocation, what makes it this report.
    pub ordinal: u32,
    pub role: String,
    pub provider: RoleProvider,
    pub model: Option<String>,
    /// What the role read. A role that read the worktree may have run code that depends on any
    /// file, so its reports go stale on any change; see [`TreeCheck::stale_findings`].
    pub workspace: Workspace,
    /// The snapshot tree the role read.
    pub tree_sha: String,
}

impl Source {
    /// What makes this report itself: its attempt and its place in that attempt's submission.
    #[must_use]
    pub fn key(&self) -> EvidenceKey {
        EvidenceKey {
            invocation: self.invocation,
            ordinal: self.ordinal,
        }
    }
}

/// One report, by its attempt and position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvidenceKey {
    pub invocation: InvocationId,
    pub ordinal: u32,
}

/// One reported finding and where it came from: what consolidation works on, and what a finding
/// keeps as its evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Evidence {
    pub source: Source,
    /// Exactly what the role submitted.
    pub reported: ReportedFinding,
}

impl Evidence {
    /// Whether this report no longer describes the code, given how the worktree compares with the
    /// tree the role read.
    #[must_use]
    pub fn is_stale(&self, check: &TreeCheck) -> bool {
        match check {
            TreeCheck::Unchanged => false,
            TreeCheck::Changed {
                paths, complete, ..
            } => {
                self.source.workspace == Workspace::Worktree
                    || !complete
                    || self
                        .reported
                        .path
                        .as_ref()
                        .map_or(!paths.is_empty(), |path| paths.contains(path))
            }
        }
    }
}

/// A finding made of one or more reports about the same thing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Merged {
    /// The reports, as indices into what [`consolidate`] was given, in ascending order.
    pub members: Vec<usize>,
    /// The member whose title, body and class are the finding's.
    pub lead: usize,
    pub class: FindingClass,
    pub severity: Severity,
    pub location: Option<Location>,
    /// Whether any report named lines. A finding about a whole file has a location and no lines.
    pub lines: bool,
    pub title: String,
    pub rationale: String,
    pub requirement: Option<String>,
    /// The tree the lead report was found on.
    pub tree_sha: String,
}

/// How strongly a class claims a problem. A finding takes the strongest class among its reports.
fn class_rank(class: ReportedClass) -> u8 {
    match class {
        ReportedClass::ConfirmedBug => 3,
        ReportedClass::RuleViolation => 2,
        ReportedClass::Conflict => 1,
        ReportedClass::Suggestion => 0,
    }
}

/// The lines a report covers. `None` for a report naming a file and no line: the whole file.
fn lines_of(reported: &ReportedFinding) -> Option<(u32, u32)> {
    reported
        .line
        .map(|start| (start, reported.end_line.unwrap_or(start).max(start)))
}

/// The sources a conflict names, compared without regard to order or case.
fn between_of(reported: &ReportedFinding) -> BTreeSet<String> {
    reported
        .between
        .iter()
        .map(|source| source.trim().to_lowercase())
        .collect()
}

/// Whether two reports are the same finding. See the module docs.
#[must_use]
pub fn same_finding(a: &ReportedFinding, b: &ReportedFinding) -> bool {
    let conflict_a = a.class == ReportedClass::Conflict;
    let conflict_b = b.class == ReportedClass::Conflict;
    if conflict_a != conflict_b {
        return false;
    }
    let rule = |r: &ReportedFinding| r.rule.as_ref().map(|cited| cited.id);
    if rule(a) != rule(b) {
        return false;
    }
    match (&a.path, &b.path) {
        (Some(path_a), Some(path_b)) => {
            path_a == path_b
                && match (lines_of(a), lines_of(b)) {
                    (Some((start_a, end_a)), Some((start_b, end_b))) => {
                        start_a <= end_b && start_b <= end_a
                    }
                    // A whole file overlaps every line of it.
                    _ => true,
                }
        }
        (None, None) => {
            if conflict_a {
                let sources = between_of(a);
                !sources.is_empty() && sources == between_of(b)
            } else {
                rule(a).is_some()
            }
        }
        _ => false,
    }
}

/// Merge reports that describe the same finding. Deterministic: the same reports in any order give
/// the same findings, in the same order.
#[must_use]
pub fn consolidate(evidence: &[Evidence]) -> Vec<Merged> {
    // Union-find over every pair: few enough reports that the square is nothing, and a pairwise
    // test is the only way overlap stays transitive whatever order the reports came in.
    let mut parent: Vec<usize> = (0..evidence.len()).collect();
    for a in 0..evidence.len() {
        for b in (a + 1)..evidence.len() {
            if same_finding(&evidence[a].reported, &evidence[b].reported) {
                let (root_a, root_b) = (root(&mut parent, a), root(&mut parent, b));
                if root_a != root_b {
                    parent[root_a.max(root_b)] = root_a.min(root_b);
                }
            }
        }
    }
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for index in 0..evidence.len() {
        let group = root(&mut parent, index);
        groups.entry(group).or_default().push(index);
    }

    let mut merged: Vec<Merged> = groups
        .into_values()
        .map(|members| merge(evidence, members))
        .collect();
    merged.sort_by(|a, b| order_key(evidence, a).cmp(&order_key(evidence, b)));
    merged
}

/// The root of `at`'s set, halving the path on the way.
fn root(parent: &mut [usize], mut at: usize) -> usize {
    while parent[at] != at {
        parent[at] = parent[parent[at]];
        at = parent[at];
    }
    at
}

/// Most severe first, then by place, then by the report that leads.
type OrderKey<'a> = (
    std::cmp::Reverse<Severity>,
    Option<(&'a str, u32)>,
    (&'a str, InvocationId, u32),
);

/// The order findings are listed in. Built from content alone, never from the order reports
/// arrived in.
fn order_key<'a>(evidence: &'a [Evidence], merged: &'a Merged) -> OrderKey<'a> {
    let lead = &evidence[merged.lead].source;
    (
        std::cmp::Reverse(merged.severity),
        merged
            .location
            .as_ref()
            .map(|location| (location.path.as_str(), location.start_line)),
        (lead.role.as_str(), lead.invocation, lead.ordinal),
    )
}

fn merge(evidence: &[Evidence], members: Vec<usize>) -> Merged {
    let lead = *members
        .iter()
        .max_by(|&&a, &&b| {
            let rank = |i: usize| {
                let reported = &evidence[i].reported;
                (class_rank(reported.class), reported.severity)
            };
            let source = |i: usize| {
                let source = &evidence[i].source;
                (source.role.as_str(), source.invocation, source.ordinal)
            };
            // The earliest source wins a tie, so the comparison is reversed for it.
            rank(a)
                .cmp(&rank(b))
                .then_with(|| source(b).cmp(&source(a)))
        })
        .unwrap_or(&members[0]);
    let reports = || members.iter().map(|&i| &evidence[i].reported);

    let location = evidence[lead].reported.path.as_ref().map(|path| {
        let spans: Vec<(u32, u32)> = reports().filter_map(lines_of).collect();
        let start = spans.iter().map(|(start, _)| *start).min().unwrap_or(1);
        let end = spans.iter().map(|(_, end)| *end).max().unwrap_or(start);
        Location {
            path: path.clone(),
            start_line: start,
            end_line: end,
        }
    });
    let lead_report = &evidence[lead].reported;
    Merged {
        lines: reports().any(|reported| reported.line.is_some()),
        class: lead_report.finding_class(),
        severity: reports()
            .map(|reported| reported.severity)
            .max()
            .unwrap_or(lead_report.severity),
        location,
        title: lead_report.title.clone(),
        rationale: lead_report.body.clone(),
        requirement: lead_report.requirement.clone().or_else(|| {
            reports()
                .find_map(|reported| reported.requirement.clone())
                .filter(|requirement| !requirement.trim().is_empty())
        }),
        tree_sha: evidence[lead].source.tree_sha.clone(),
        lead,
        members,
    }
}

/// A finding as the store keeps it: the finding, whether it names lines, and every report it was
/// made of.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FindingRecord {
    #[serde(flatten)]
    pub finding: Finding,
    pub lines: bool,
    pub evidence: Vec<Evidence>,
}

impl FindingRecord {
    fn keys(&self) -> BTreeSet<EvidenceKey> {
        self.evidence.iter().map(|e| e.source.key()).collect()
    }
}

/// A finding to save, not yet numbered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewFinding {
    pub class: FindingClass,
    pub severity: Severity,
    pub location: Option<Location>,
    pub lines: bool,
    pub title: String,
    pub rationale: String,
    pub requirement: Option<String>,
    pub tree_sha: String,
    /// Carried over from a finding this one replaces. See [`reconcile`].
    pub dismissed: Option<Dismissal>,
    pub evidence: Vec<Evidence>,
}

/// What saving a consolidation changes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reconciled {
    /// Saved findings that stand exactly as they were.
    pub keep: Vec<FindingId>,
    /// Saved findings no longer reported as they were. Kept in the record, and no longer listed.
    pub supersede: Vec<FindingId>,
    pub create: Vec<NewFinding>,
}

/// Decide what a fresh consolidation changes about the findings already saved.
///
/// A saved finding made of exactly the same reports is kept as it is, id and dismissal included:
/// nothing about it changed. Every other merged finding is new, and every saved one not kept is
/// superseded. A new finding inherits a dismissal from a saved one it shares a report with, or,
/// failing that, from one about the same problem — the same key [`same_finding`] uses, compared
/// between their leading reports — because a role run again after an edit that reports the same
/// thing is not new information about it.
#[must_use]
pub fn reconcile(saved: &[FindingRecord], evidence: &[Evidence], merged: &[Merged]) -> Reconciled {
    let mut out = Reconciled::default();
    let mut kept: BTreeSet<FindingId> = BTreeSet::new();
    for group in merged {
        let keys: BTreeSet<EvidenceKey> = group
            .members
            .iter()
            .map(|&i| evidence[i].source.key())
            .collect();
        if let Some(same) = saved.iter().find(|record| record.keys() == keys) {
            out.keep.push(same.finding.id);
            kept.insert(same.finding.id);
            continue;
        }
        let lead = &evidence[group.lead].reported;
        let dismissed = saved
            .iter()
            .filter(|record| record.finding.dismissed.is_some())
            .find(|record| !record.keys().is_disjoint(&keys))
            .or_else(|| {
                saved
                    .iter()
                    .filter(|record| record.finding.dismissed.is_some())
                    .find(|record| {
                        record
                            .evidence
                            .iter()
                            .any(|old| same_finding(&old.reported, lead))
                    })
            })
            .and_then(|record| record.finding.dismissed.clone());
        out.create.push(NewFinding {
            class: group.class.clone(),
            severity: group.severity,
            location: group.location.clone(),
            lines: group.lines,
            title: group.title.clone(),
            rationale: group.rationale.clone(),
            requirement: group.requirement.clone(),
            tree_sha: group.tree_sha.clone(),
            dismissed,
            evidence: group.members.iter().map(|&i| evidence[i].clone()).collect(),
        });
    }
    out.supersede = saved
        .iter()
        .map(|record| record.finding.id)
        .filter(|id| !kept.contains(id))
        .collect();
    out
}

/// Whether a finding no longer describes the code: every report it was made of has gone stale.
///
/// All rather than any, because a finding made of a report from before an edit and one from a role
/// run again after it is still being reported about the code as it is now. `check` gives, for each
/// tree a report was found on, how the worktree compares with it.
#[must_use]
pub fn finding_is_stale(
    record: &FindingRecord,
    mut check: impl FnMut(&str) -> Option<TreeCheck>,
) -> bool {
    !record.evidence.is_empty()
        && record.evidence.iter().all(|evidence| {
            check(&evidence.source.tree_sha).is_none_or(|compared| evidence.is_stale(&compared))
        })
}

/// Whether a role has to run again for its result to describe the worktree as it is now.
///
/// `paths` are the role's globs and `read` what it reads; `check` compares the worktree with the
/// tree it last read. A role scoped to paths none of the changes touch has nothing new to read. A
/// role about the whole change, or one that read the worktree itself, is affected by any change,
/// and an incomplete list of changes affects every role, because the change it misses may be the
/// one in a role's paths.
#[must_use]
pub fn role_is_affected(paths: &[String], read: Workspace, check: &TreeCheck) -> bool {
    match check {
        TreeCheck::Unchanged => false,
        TreeCheck::Changed {
            paths: changed,
            complete,
            ..
        } => {
            !complete
                || read == Workspace::Worktree
                || paths.is_empty()
                || changed
                    .iter()
                    .any(|file| paths.iter().any(|pattern| glob::matches(pattern, file)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::submission::RuleCitation;

    fn report(path: Option<&str>, lines: Option<(u32, u32)>) -> ReportedFinding {
        ReportedFinding {
            title: "A query per row".to_owned(),
            severity: Severity::Notable,
            class: ReportedClass::Suggestion,
            body: "Each row loads its parent.".to_owned(),
            path: path.map(str::to_owned),
            line: lines.map(|(start, _)| start),
            end_line: lines.map(|(_, end)| end),
            verification: None,
            rule: None,
            between: Vec::new(),
            requirement: None,
            evidence: Vec::new(),
        }
    }

    fn from(role: &str, invocation: i64, ordinal: u32, reported: ReportedFinding) -> Evidence {
        Evidence {
            source: Source {
                step: StepId(invocation),
                invocation: InvocationId(invocation),
                ordinal,
                role: role.to_owned(),
                provider: RoleProvider::Claude,
                model: None,
                workspace: Workspace::Snapshot,
                tree_sha: "t1".to_owned(),
            },
            reported,
        }
    }

    fn keys(evidence: &[Evidence], merged: &Merged) -> Vec<(String, u32)> {
        merged
            .members
            .iter()
            .map(|&i| (evidence[i].source.role.clone(), evidence[i].source.ordinal))
            .collect()
    }

    #[test]
    fn two_roles_reporting_overlapping_lines_of_one_file_make_one_finding_with_both_reports() {
        let evidence = [
            from(
                "conventions",
                1,
                0,
                report(Some("src/export.rs"), Some((40, 48))),
            ),
            from(
                "correctness",
                2,
                0,
                report(Some("src/export.rs"), Some((45, 52))),
            ),
        ];
        let merged = consolidate(&evidence);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].members, [0, 1]);
        assert_eq!(
            merged[0].location,
            Some(Location {
                path: "src/export.rs".to_owned(),
                start_line: 40,
                end_line: 52
            }),
            "the finding covers every report's lines"
        );
    }

    #[test]
    fn reports_on_lines_that_do_not_overlap_stay_separate_findings() {
        let evidence = [
            from(
                "conventions",
                1,
                0,
                report(Some("src/export.rs"), Some((3, 3))),
            ),
            from(
                "correctness",
                2,
                0,
                report(Some("src/export.rs"), Some((9, 12))),
            ),
            from(
                "correctness",
                2,
                1,
                report(Some("src/other.rs"), Some((3, 3))),
            ),
        ];
        assert_eq!(consolidate(&evidence).len(), 3);
    }

    #[test]
    fn reports_citing_different_rules_at_the_same_place_stay_separate() {
        let cite = |id: i64| {
            let mut reported = report(Some("ui/Modal.svelte"), Some((12, 12)));
            reported.class = ReportedClass::RuleViolation;
            reported.rule = Some(RuleCitation { id, revision: 1 });
            reported
        };
        let evidence = [
            from("conventions", 1, 0, cite(14)),
            from("ui", 2, 0, cite(14)),
            from("ui", 2, 1, cite(21)),
        ];
        let merged = consolidate(&evidence);
        assert_eq!(merged.len(), 2);
        assert!(
            merged
                .iter()
                .any(|m| keys(&evidence, m) == [("conventions".into(), 0), ("ui".into(), 0)])
        );
    }

    #[test]
    fn overlap_is_transitive_so_a_chain_of_reports_is_one_finding() {
        let evidence = [
            from("a", 1, 0, report(Some("x.rs"), Some((1, 5)))),
            from("c", 3, 0, report(Some("x.rs"), Some((9, 12)))),
            from("b", 2, 0, report(Some("x.rs"), Some((4, 10)))),
        ];
        let merged = consolidate(&evidence);
        assert_eq!(merged.len(), 1, "1–5 and 9–12 meet through 4–10");
        assert_eq!(merged[0].members, [0, 1, 2]);
    }

    #[test]
    fn a_report_about_a_whole_file_overlaps_every_line_of_it() {
        let evidence = [
            from("a", 1, 0, report(Some("x.rs"), None)),
            from("b", 2, 0, report(Some("x.rs"), Some((300, 310)))),
        ];
        let merged = consolidate(&evidence);
        assert_eq!(merged.len(), 1);
        assert!(merged[0].lines, "one report named lines");

        let alone = consolidate(&evidence[..1]);
        assert!(!alone[0].lines, "a whole-file finding names no lines");
    }

    #[test]
    fn a_conflict_never_absorbs_or_is_absorbed_by_a_finding_about_the_code() {
        let mut conflict = report(Some("x.rs"), Some((1, 5)));
        conflict.class = ReportedClass::Conflict;
        conflict.between = vec!["R-21".to_owned(), "REVIEW.md".to_owned()];
        let evidence = [
            from("a", 1, 0, conflict),
            from("b", 2, 0, report(Some("x.rs"), Some((1, 5)))),
        ];
        assert_eq!(consolidate(&evidence).len(), 2);
    }

    #[test]
    fn two_conflicts_about_no_file_are_one_when_they_name_the_same_sources_in_any_order() {
        let conflict = |between: [&str; 2]| {
            let mut reported = report(None, None);
            reported.class = ReportedClass::Conflict;
            reported.between = between.iter().map(|s| (*s).to_owned()).collect();
            reported
        };
        let evidence = [
            from("a", 1, 0, conflict(["R-21", "REVIEW.md"])),
            from("b", 2, 0, conflict(["review.md", "R-21"])),
            from("b", 2, 1, conflict(["R-3", "AGENTS.md"])),
        ];
        assert_eq!(consolidate(&evidence).len(), 2);
    }

    #[test]
    fn reports_about_no_file_and_no_rule_are_never_merged() {
        let evidence = [
            from("a", 1, 0, report(None, None)),
            from("b", 2, 0, report(None, None)),
        ];
        assert_eq!(consolidate(&evidence).len(), 2);
    }

    #[test]
    fn a_confirmed_bug_leads_over_a_suggestion_and_the_finding_takes_the_highest_severity() {
        let mut suggestion = report(Some("x.rs"), Some((3, 3)));
        suggestion.severity = Severity::Blocking;
        suggestion.title = "Might be off by one".to_owned();
        let mut bug = report(Some("x.rs"), Some((3, 4)));
        bug.class = ReportedClass::ConfirmedBug;
        bug.verification = Some("ran the range test; it skipped the last row".to_owned());
        bug.title = "Off by one in the export loop".to_owned();
        let evidence = [
            from("conventions", 1, 0, suggestion),
            from("correctness", 2, 0, bug),
        ];
        let merged = &consolidate(&evidence)[0];
        assert_eq!(merged.lead, 1);
        assert_eq!(merged.title, "Off by one in the export loop");
        assert_eq!(
            merged.class,
            FindingClass::ConfirmedBug {
                verification: "ran the range test; it skipped the last row".to_owned()
            }
        );
        assert_eq!(merged.severity, Severity::Blocking);
    }

    #[test]
    fn consolidation_does_not_depend_on_the_order_the_reports_arrived_in() {
        let mut bug = report(Some("x.rs"), Some((3, 4)));
        bug.class = ReportedClass::ConfirmedBug;
        bug.verification = Some("ran it".to_owned());
        let evidence = vec![
            from("conventions", 1, 0, report(Some("x.rs"), Some((3, 3)))),
            from("correctness", 2, 0, bug),
            from("correctness", 2, 1, report(Some("y.rs"), Some((1, 1)))),
            from("ui", 3, 0, report(None, None)),
        ];
        let described = |evidence: &[Evidence]| {
            consolidate(evidence)
                .iter()
                .map(|m| {
                    let mut members = keys(evidence, m);
                    members.sort();
                    (m.title.clone(), m.severity, m.location.clone(), members)
                })
                .collect::<Vec<_>>()
        };
        let forward = described(&evidence);
        let mut reversed = evidence.clone();
        reversed.reverse();
        assert_eq!(described(&reversed), forward);
        let mut rotated = evidence;
        rotated.rotate_left(2);
        assert_eq!(described(&rotated), forward);
    }

    fn saved(id: i64, evidence: Vec<Evidence>, dismissed: bool) -> FindingRecord {
        let lead = &evidence[0];
        FindingRecord {
            finding: Finding {
                id: FindingId(id),
                flow: crate::flow::FlowId(1),
                class: lead.reported.finding_class(),
                severity: lead.reported.severity,
                location: lead.reported.location(),
                title: lead.reported.title.clone(),
                rationale: lead.reported.body.clone(),
                requirement: None,
                tree_sha: lead.source.tree_sha.clone(),
                dismissed: dismissed.then(|| Dismissal {
                    reason: "intended".to_owned(),
                    at: 5,
                }),
            },
            lines: true,
            evidence,
        }
    }

    #[test]
    fn a_finding_made_of_the_same_reports_keeps_its_id_and_everything_else_is_replaced() {
        let first = from("conventions", 1, 0, report(Some("x.rs"), Some((3, 3))));
        let second = from("correctness", 2, 0, report(Some("y.rs"), Some((9, 9))));
        let saved_findings = [
            saved(10, vec![first.clone()], false),
            saved(11, vec![second], false),
        ];
        // `correctness` ran again and now reports something else.
        let rerun = from("correctness", 5, 0, report(Some("z.rs"), Some((1, 1))));
        let evidence = [first, rerun];
        let merged = consolidate(&evidence);
        let plan = reconcile(&saved_findings, &evidence, &merged);
        assert_eq!(plan.keep, [FindingId(10)]);
        assert_eq!(plan.supersede, [FindingId(11)]);
        assert_eq!(plan.create.len(), 1);
        assert_eq!(
            plan.create[0].evidence[0].source.invocation,
            InvocationId(5)
        );
    }

    #[test]
    fn a_dismissal_survives_a_rerun_that_reports_the_same_problem_again() {
        let before = from("correctness", 2, 0, report(Some("y.rs"), Some((9, 9))));
        let saved_findings = [saved(11, vec![before], true)];
        let again = from("correctness", 7, 0, report(Some("y.rs"), Some((8, 10))));
        let evidence = [again];
        let plan = reconcile(&saved_findings, &evidence, &consolidate(&evidence));
        assert_eq!(plan.supersede, [FindingId(11)]);
        assert_eq!(
            plan.create[0].dismissed.as_ref().map(|d| d.reason.as_str()),
            Some("intended")
        );
    }

    #[test]
    fn a_dismissal_is_not_handed_to_a_different_problem() {
        let before = from("correctness", 2, 0, report(Some("y.rs"), Some((9, 9))));
        let saved_findings = [saved(11, vec![before], true)];
        let elsewhere = from("correctness", 7, 0, report(Some("y.rs"), Some((40, 41))));
        let evidence = [elsewhere];
        let plan = reconcile(&saved_findings, &evidence, &consolidate(&evidence));
        assert_eq!(plan.create[0].dismissed, None);
    }

    fn changed(paths: &[&str]) -> TreeCheck {
        TreeCheck::Changed {
            tree_sha: "t2".to_owned(),
            paths: paths.iter().map(|path| (*path).to_owned()).collect(),
            complete: true,
        }
    }

    #[test]
    fn a_finding_goes_stale_when_every_report_of_it_is_on_a_file_that_changed_since() {
        let record = saved(
            1,
            vec![from("a", 1, 0, report(Some("x.rs"), Some((3, 3))))],
            false,
        );
        assert!(finding_is_stale(&record, |_| Some(changed(&["x.rs"]))));
        assert!(!finding_is_stale(&record, |_| Some(changed(&["y.rs"]))));
        assert!(!finding_is_stale(&record, |_| Some(TreeCheck::Unchanged)));
    }

    #[test]
    fn a_finding_a_rerun_role_reported_again_on_the_new_tree_is_current() {
        let mut fresh = from("b", 9, 0, report(Some("x.rs"), Some((3, 3))));
        fresh.source.tree_sha = "t2".to_owned();
        let record = saved(
            1,
            vec![from("a", 1, 0, report(Some("x.rs"), Some((3, 3)))), fresh],
            false,
        );
        let check = |tree: &str| {
            Some(if tree == "t2" {
                TreeCheck::Unchanged
            } else {
                changed(&["x.rs"])
            })
        };
        assert!(!finding_is_stale(&record, check));
    }

    #[test]
    fn a_role_scoped_to_paths_no_change_touches_is_not_rerun() {
        let ui = ["src/**/*.svelte".to_owned()];
        assert!(!role_is_affected(
            &ui,
            Workspace::Snapshot,
            &changed(&["api/export.py"])
        ));
        assert!(role_is_affected(
            &ui,
            Workspace::Snapshot,
            &changed(&["src/lib/Modal.svelte"])
        ));
    }

    #[test]
    fn a_role_about_the_whole_change_or_one_that_read_the_worktree_is_rerun_after_any_change() {
        let ui = ["src/**/*.svelte".to_owned()];
        assert!(role_is_affected(
            &[],
            Workspace::Snapshot,
            &changed(&["README.md"])
        ));
        assert!(role_is_affected(
            &ui,
            Workspace::Worktree,
            &changed(&["README.md"])
        ));
        assert!(!role_is_affected(
            &[],
            Workspace::Snapshot,
            &TreeCheck::Unchanged
        ));
    }

    #[test]
    fn an_incomplete_list_of_changes_reruns_every_role() {
        let ui = ["src/**/*.svelte".to_owned()];
        let check = TreeCheck::Changed {
            tree_sha: "t2".to_owned(),
            paths: ["README.md".to_owned()].into(),
            complete: false,
        };
        assert!(role_is_affected(&ui, Workspace::Snapshot, &check));
    }
}
