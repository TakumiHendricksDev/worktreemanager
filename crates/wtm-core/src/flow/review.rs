//! What a review read, and what it found.
//!
//! # Why a snapshot keeps a patch file
//!
//! The tree a review read is a git object nothing refers to, so `git gc` may delete it. The durable
//! evidence is a `git diff --binary` patch written to a file, with its hash stored beside the
//! hashes git reported. The tree hash is still kept, because comparing it is how a later review
//! knows whether anything changed.
//!
//! # Why a finding's class carries its evidence
//!
//! A confirmed bug without the verification that confirmed it is an opinion, and a rule violation
//! without the rule revision it broke cannot be checked against what the rule said at the time. So
//! those are fields of the class rather than optional fields on every finding: a finding that
//! lacks them cannot be built, rather than being built and then caught by a review.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::UnixMs;
use super::rule::RuleId;
use super::state::FlowId;

id! {
    /// One finding, as the store numbers it.
    FindingId
}

/// A file a flow keeps under its own data directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlobRef {
    /// Relative to the flow's directory, so moving the data directory moves its blobs with it.
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
}

/// What an invocation read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub head_sha: String,
    /// The tree including uncommitted changes, which is what a review of unsubmitted work reads.
    pub tree_sha: String,
    pub base_sha: String,
    pub patch: BlobRef,
}

/// How serious a finding is. Ordered, so the most serious sorts last.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Minor,
    Notable,
    /// Should not be published as it is.
    Blocking,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FindingClass {
    /// A bug a role demonstrated, not just suspected.
    ConfirmedBug {
        /// What was run or read to confirm it.
        verification: String,
    },
    /// Code that breaks one of your rules, as the cited revision stated it.
    RuleViolation {
        rule: RuleId,
        revision: u32,
    },
    /// Two sources of guidance that disagree. A role reports this rather than picking a side.
    Conflict {
        /// The sources, such as a rule and a section of the repository's review guide.
        between: Vec<String>,
    },
    Suggestion,
}

/// Lines in a file, inclusive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Location {
    /// Relative to the repository root, as git reports changed paths.
    pub path: String,
    pub start_line: u32,
    pub end_line: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Dismissal {
    pub reason: String,
    pub at: UnixMs,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Finding {
    pub id: FindingId,
    pub flow: FlowId,
    pub class: FindingClass,
    pub severity: Severity,
    /// `None` for a finding about the change as a whole.
    pub location: Option<Location>,
    pub title: String,
    pub rationale: String,
    /// The requirement or acceptance criterion it is measured against, when that is not a rule.
    pub requirement: Option<String>,
    /// The snapshot tree it was found on.
    pub tree_sha: String,
    pub dismissed: Option<Dismissal>,
}

impl Finding {
    /// Whether this finding no longer describes the code, given the paths that changed since the
    /// snapshot it was found on.
    ///
    /// A finding about one file is stale when that file changed. A finding about the change as a
    /// whole is stale when anything did, because nothing narrower says what it depended on.
    #[must_use]
    pub fn is_stale(&self, changed: &BTreeSet<String>) -> bool {
        match &self.location {
            Some(location) => changed.contains(&location.path),
            None => !changed.is_empty(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(location: Option<Location>) -> Finding {
        Finding {
            id: FindingId(1),
            flow: FlowId(1),
            class: FindingClass::ConfirmedBug {
                verification: "a query-count test went from passing to failing".to_owned(),
            },
            severity: Severity::Notable,
            location,
            title: "A query per row in the export loop".to_owned(),
            rationale: "Each unit loads its parent separately.".to_owned(),
            requirement: None,
            tree_sha: "9f00".to_owned(),
            dismissed: None,
        }
    }

    fn at(path: &str) -> Location {
        Location {
            path: path.to_owned(),
            start_line: 88,
            end_line: 90,
        }
    }

    fn changed(paths: &[&str]) -> BTreeSet<String> {
        paths.iter().map(|path| (*path).to_owned()).collect()
    }

    #[test]
    fn a_finding_goes_stale_when_its_file_changes() {
        let found = finding(Some(at("app/export.py")));
        assert!(found.is_stale(&changed(&["app/export.py", "README.md"])));
    }

    #[test]
    fn a_finding_in_a_file_that_did_not_change_stays_current() {
        let found = finding(Some(at("app/export.py")));
        assert!(!found.is_stale(&changed(&["app/models.py"])));
    }

    #[test]
    fn a_finding_about_the_whole_change_goes_stale_when_anything_changes() {
        let found = finding(None);
        assert!(found.is_stale(&changed(&["app/models.py"])));
        assert!(!found.is_stale(&changed(&[])));
    }

    #[test]
    fn the_most_serious_finding_sorts_last() {
        let mut severities = [Severity::Blocking, Severity::Minor, Severity::Notable];
        severities.sort();
        assert_eq!(
            severities,
            [Severity::Minor, Severity::Notable, Severity::Blocking]
        );
    }
}
