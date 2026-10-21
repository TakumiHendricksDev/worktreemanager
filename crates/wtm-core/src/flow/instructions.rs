//! A repository's own instruction sources: the files where a team keeps its project instructions
//! and review knowledge, such as `CLAUDE.md`, `AGENTS.md`, `REVIEW.md` and `docs/review/**`.
//!
//! # Authoritative, and never edited
//!
//! These belong to the team and are reviewed like code. A review reads them as the project's
//! instructions, ahead of your private [rules](super::rule), and a role that finds a rule
//! disagreeing with them reports a conflict instead of choosing. wtm only reads them: there is no
//! write path here or anywhere else, and proposing a change to one is a pull request someone
//! reviews, not something a run does.
//!
//! # Which files
//!
//! `[workflow] instruction_sources`, a list of [`glob`] patterns, or [`DEFAULT_INSTRUCTION_SOURCES`]
//! when the list is not set. They are matched against the worktree's file list, the one git
//! reports, so an ignored file is never an instruction source and nothing walks the disk to find
//! one. Reading the matched files is I/O and belongs to the caller; this module decides which
//! files, in which order, and what a role is told about them.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::glob;

/// The sources a project without `instruction_sources` uses: the files the agent CLIs already read
/// as project instructions, plus the conventional home of a team's review guide.
pub const DEFAULT_INSTRUCTION_SOURCES: &[&str] =
    &["CLAUDE.md", "AGENTS.md", "REVIEW.md", "docs/review/**"];

/// The most text one source file contributes. A review guide is a few pages; a file far past this
/// is more likely a generated document someone globbed by accident, and a role's whole brief has
/// to fit in its context window beside the diff.
pub const MAX_DOCUMENT_BYTES: usize = 64 * 1024;

/// The most text every source together contributes, for the same reason.
pub const MAX_TOTAL_BYTES: usize = 256 * 1024;

/// One configured pattern and the files it matched, in path order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceMatch {
    pub pattern: String,
    /// Relative to the repository root. A file an earlier pattern already matched is not repeated.
    pub paths: Vec<String>,
}

/// The files `patterns` name among `files`, pattern by pattern, in the order they were configured.
///
/// Configured order is kept because it is the order a team would read them in, which is usually
/// the general instructions before the review-specific ones. Within one pattern, paths are sorted
/// so the result does not depend on the order git listed them.
#[must_use]
pub fn resolve<S: AsRef<str>>(patterns: &[S], files: &[String]) -> Vec<SourceMatch> {
    let mut sorted: Vec<&str> = files.iter().map(String::as_str).collect();
    sorted.sort_unstable();
    sorted.dedup();
    let mut seen = BTreeSet::new();
    patterns
        .iter()
        .map(|pattern| {
            let pattern = pattern.as_ref();
            let paths = sorted
                .iter()
                .filter(|path| glob::matches(pattern, path) && seen.insert(**path))
                .map(|path| (*path).to_owned())
                .collect();
            SourceMatch {
                pattern: pattern.to_owned(),
                paths,
            }
        })
        .collect()
}

/// One source file's text, as a role receives it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstructionDocument {
    pub path: String,
    /// The configured pattern that matched it.
    pub pattern: String,
    pub text: String,
    /// The file was longer than a role is given, so `text` is its beginning.
    pub truncated: bool,
}

/// A matched file a role is not given, and why. Shown, never silently dropped, because a review
/// that quietly ignored the team's guide would look like one that had read it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedDocument {
    pub path: String,
    pub reason: String,
}

/// Everything a project's instruction sources gave, ready for a role's brief.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstructionSet {
    pub sources: Vec<SourceMatch>,
    pub documents: Vec<InstructionDocument>,
    pub skipped: Vec<SkippedDocument>,
}

/// Cut `text` to at most `limit` bytes without splitting a character. True when it was cut.
pub fn truncate(text: &mut String, limit: usize) -> bool {
    if text.len() <= limit {
        return false;
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|path| (*path).to_owned()).collect()
    }

    #[test]
    fn the_default_sources_find_the_agent_instructions_and_the_review_guide() {
        let listing = files(&[
            "src/main.ts",
            "docs/review/learnings.md",
            "REVIEW.md",
            "CLAUDE.md",
            "docs/review/checks/sql.md",
            "docs/guide.md",
        ]);
        let found = resolve(DEFAULT_INSTRUCTION_SOURCES, &listing);

        let paths: Vec<(&str, Vec<&str>)> = found
            .iter()
            .map(|source| {
                (
                    source.pattern.as_str(),
                    source.paths.iter().map(String::as_str).collect(),
                )
            })
            .collect();
        assert_eq!(
            paths,
            [
                ("CLAUDE.md", vec!["CLAUDE.md"]),
                ("AGENTS.md", vec![]),
                ("REVIEW.md", vec!["REVIEW.md"]),
                (
                    "docs/review/**",
                    vec!["docs/review/checks/sql.md", "docs/review/learnings.md"]
                ),
            ]
        );
    }

    #[test]
    fn a_file_two_patterns_match_is_read_once_under_the_first() {
        let found = resolve(
            &["docs/**", "docs/review/**"],
            &files(&["docs/review/focus.md"]),
        );
        assert_eq!(found[0].paths, ["docs/review/focus.md"]);
        assert!(found[1].paths.is_empty());
    }

    #[test]
    fn a_root_level_name_does_not_match_the_same_name_deeper_down() {
        // `CLAUDE.md` is the repository's own; a vendored package's is not this project's
        // instructions.
        let found = resolve(&["CLAUDE.md"], &files(&["vendor/lib/CLAUDE.md"]));
        assert!(found[0].paths.is_empty());
    }

    #[test]
    fn truncating_never_splits_a_character() {
        let mut text = "ab\u{e9}".to_owned(); // é is two bytes
        assert!(truncate(&mut text, 3));
        assert_eq!(text, "ab");

        let mut short = "fits".to_owned();
        assert!(!truncate(&mut short, 10));
        assert_eq!(short, "fits");
    }
}
