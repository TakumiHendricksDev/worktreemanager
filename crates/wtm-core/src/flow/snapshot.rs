//! The parts of a snapshot that need no git: where its files go, and what a changed tree means for
//! the results read from it.
//!
//! The git side, taking the snapshot and checking the tree again, is
//! [`usecase::snapshot`](crate::usecase::snapshot), behind the [`Git`](crate::ports::Git) port.
//!
//! # Why the flow directory is laid out here
//!
//! A [`BlobRef`](super::BlobRef) stores its path relative to the flow's directory, so the
//! database keeps working if the data directory moves. That only holds if exactly one piece of code
//! decides what the relative paths are, and it has to be one that both the engine and its tests can
//! call without a filesystem.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::Serialize;

use super::recipe::Workspace;
use super::review::{Finding, FindingId};
use super::state::FlowId;

/// One flow's files: `<data>/flow/<id>/`.
///
/// Named after the flow's id, which the store never reuses, so a later flow can never find an
/// earlier one's patch or checkout in its directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlowDir {
    root: PathBuf,
}

impl FlowDir {
    /// `flows` is the directory holding every flow's directory, `AppPaths::flow_dir`.
    #[must_use]
    pub fn new(flows: &Path, flow: FlowId) -> Self {
        Self {
            root: flows.join(flow.to_string()),
        }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.root
    }

    /// Where a path relative to this directory, as a `BlobRef` holds it, is on disk.
    #[must_use]
    pub fn resolve(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }

    /// The patch from `base` to `tree`, relative to this directory.
    ///
    /// Named by both object ids because the patch is a function of exactly those two. Taking the
    /// same snapshot twice writes the same file to the same name, rather than a second copy.
    #[must_use]
    pub fn patch(base_sha: &str, tree_sha: &str) -> String {
        format!("snapshots/{base_sha}-{tree_sha}.patch")
    }

    /// A scratch index for one operation. Unique per label, so two roles finishing at once can
    /// each check the tree without fighting over one `index.lock`.
    #[must_use]
    pub fn scratch_index(&self, label: &ScratchLabel) -> PathBuf {
        self.root.join("scratch").join(format!("{}.index", label.0))
    }

    /// The detached checkout static roles read for the snapshot of `tree`.
    #[must_use]
    pub fn checkout(&self, tree_sha: &str) -> PathBuf {
        self.root.join("checkouts").join(tree_sha)
    }
}

/// Names one operation's scratch files, such as `take` or `check-correctness`.
///
/// Its own type so the characters are checked once, where it is made: it becomes a file name, and a
/// label with a `/` in it would put a scratch index somewhere else entirely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScratchLabel(String);

impl ScratchLabel {
    /// `None` unless `label` is 1 to 64 letters, digits, underscores and hyphens.
    #[must_use]
    pub fn new(label: &str) -> Option<Self> {
        let valid = !label.is_empty()
            && label.len() <= 64
            && label.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
            });
        valid.then(|| Self(label.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The worktree compared with a snapshot of it.
///
/// Used at the end of a role that read the worktree, and again whenever the Work view asks whether
/// earlier results still describe the code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TreeCheck {
    /// The worktree still holds exactly the snapshot's tree, untracked files included.
    Unchanged,
    /// It holds `tree_sha` now, which differs from the snapshot's in `paths`.
    Changed {
        tree_sha: String,
        paths: BTreeSet<String>,
        /// False when git listed more changed paths than could be captured. Every result is then
        /// treated as stale, because an unlisted path may be exactly the one a finding is in.
        complete: bool,
    },
}

impl TreeCheck {
    #[must_use]
    pub fn is_stale(&self) -> bool {
        matches!(self, Self::Changed { .. })
    }

    /// The paths that differ from the snapshot. Empty when nothing does.
    #[must_use]
    pub fn changed_paths(&self) -> Vec<&str> {
        match self {
            Self::Unchanged => Vec::new(),
            Self::Changed { paths, .. } => paths.iter().map(String::as_str).collect(),
        }
    }

    /// Which of a role's findings this makes stale, given what the role read.
    ///
    /// A role that read the snapshot checkout read a tree that never moved, so its findings still
    /// hold wherever the code has not changed since: only those in changed files go stale. A role
    /// that read the worktree while it was being edited is different. It may have run the tests or
    /// the app, which depend on far more files than the ones a finding cites, and some of what it
    /// read may be from before the edit and some from after. So any change during its run makes
    /// every one of its findings stale.
    #[must_use]
    pub fn stale_findings(&self, read: Workspace, findings: &[Finding]) -> Vec<FindingId> {
        let Self::Changed {
            paths, complete, ..
        } = self
        else {
            return Vec::new();
        };
        findings
            .iter()
            .filter(|finding| read == Workspace::Worktree || !complete || finding.is_stale(paths))
            .map(|finding| finding.id)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::review::{FindingClass, Location, Severity};

    fn finding(id: i64, path: Option<&str>) -> Finding {
        Finding {
            id: FindingId(id),
            flow: FlowId(7),
            class: FindingClass::Suggestion,
            severity: Severity::Minor,
            location: path.map(|path| Location {
                path: path.to_owned(),
                start_line: 1,
                end_line: 2,
            }),
            title: "t".to_owned(),
            rationale: "r".to_owned(),
            requirement: None,
            tree_sha: "aaaa".to_owned(),
            dismissed: None,
        }
    }

    fn changed(paths: &[&str]) -> TreeCheck {
        TreeCheck::Changed {
            tree_sha: "bbbb".to_owned(),
            paths: paths.iter().map(|path| (*path).to_owned()).collect(),
            complete: true,
        }
    }

    #[test]
    fn a_flows_files_live_in_a_directory_named_after_its_id() {
        let dir = FlowDir::new(Path::new("/data/wtm/flow"), FlowId(42));
        assert_eq!(dir.path(), Path::new("/data/wtm/flow/42"));
        assert_eq!(
            dir.resolve(&FlowDir::patch("b1", "t1")),
            PathBuf::from("/data/wtm/flow/42/snapshots/b1-t1.patch")
        );
        assert_eq!(
            dir.checkout("t1"),
            PathBuf::from("/data/wtm/flow/42/checkouts/t1")
        );
    }

    #[test]
    fn concurrent_operations_get_their_own_scratch_index() {
        let dir = FlowDir::new(Path::new("/f"), FlowId(1));
        let a = dir.scratch_index(&ScratchLabel::new("check-ui").unwrap());
        let b = dir.scratch_index(&ScratchLabel::new("check-e2e").unwrap());
        assert_ne!(a, b);
        assert!(a.starts_with("/f/1/scratch"));
    }

    #[test]
    fn a_scratch_label_cannot_point_outside_the_scratch_directory() {
        assert!(ScratchLabel::new("take").is_some());
        assert!(ScratchLabel::new("../../index").is_none());
        assert!(ScratchLabel::new("a/b").is_none());
        assert!(ScratchLabel::new("").is_none());
    }

    #[test]
    fn an_unchanged_tree_makes_nothing_stale() {
        let findings = [finding(1, Some("a.rs")), finding(2, None)];
        assert!(!TreeCheck::Unchanged.is_stale());
        assert!(
            TreeCheck::Unchanged
                .stale_findings(Workspace::Worktree, &findings)
                .is_empty()
        );
    }

    #[test]
    fn a_snapshot_readers_findings_go_stale_only_where_the_code_changed() {
        let findings = [
            finding(1, Some("a.rs")),
            finding(2, Some("b.rs")),
            finding(3, None),
        ];
        assert_eq!(
            changed(&["a.rs"]).stale_findings(Workspace::Snapshot, &findings),
            vec![FindingId(1), FindingId(3)]
        );
    }

    #[test]
    fn a_worktree_readers_findings_all_go_stale_when_anything_changed_during_its_run() {
        let findings = [finding(1, Some("a.rs")), finding(2, Some("b.rs"))];
        assert_eq!(
            changed(&["c.rs"]).stale_findings(Workspace::Worktree, &findings),
            vec![FindingId(1), FindingId(2)]
        );
    }

    #[test]
    fn an_incomplete_list_of_changes_makes_every_finding_stale() {
        let findings = [finding(1, Some("a.rs")), finding(2, Some("b.rs"))];
        let check = TreeCheck::Changed {
            tree_sha: "bbbb".to_owned(),
            paths: ["c.rs".to_owned()].into(),
            complete: false,
        };
        assert_eq!(
            check.stale_findings(Workspace::Snapshot, &findings),
            vec![FindingId(1), FindingId(2)]
        );
    }
}
