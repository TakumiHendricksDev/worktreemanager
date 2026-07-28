//! What git says exists.
//!
//! # The one rule that matters here
//!
//! **Never infer a branch from a directory name.** All three of these are live in
//! the reference repo right now, which is why they're modelled explicitly rather
//! than assumed away:
//!
//! - a detached worktree (no branch at all),
//! - a worktree outside the repo's parent directory,
//! - a directory whose name disagrees with its branch — e.g. the directory
//!   `ACME-4567-move-account-settings-to-the-spa-pattern` checked out on
//!   branch `experiment/ACME-0000-move-account-setting-configurations`.
//!
//! [`Checkout`] therefore has no "probably this branch" case, and [`Worktree`]
//! exposes [`Worktree::dirname`] and [`Worktree::branch`] as unrelated facts.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A short branch name, as `git branch --list` prints it — no `refs/heads/`
/// prefix. May contain slashes (`task/ACME-1234-…`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BranchRef(String);

impl BranchRef {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The part after the first `/`, or the whole name if there is no slash.
    ///
    /// This is the "branch name minus its type prefix" rule that turns
    /// `task/ACME-1234-slug` into the directory name `ACME-1234-slug`. Note it
    /// strips only up to the *first* slash, matching the shell's `${branch#*/}`.
    #[must_use]
    pub fn without_prefix(&self) -> &str {
        self.0
            .split_once('/')
            .map_or(self.0.as_str(), |(_, rest)| rest)
    }

    /// The part before the first `/`, if any — the `task`/`bug`/`experiment` kind
    /// of prefix.
    #[must_use]
    pub fn prefix(&self) -> Option<&str> {
        self.0.split_once('/').map(|(head, _)| head)
    }
}

impl std::fmt::Display for BranchRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A commit hash, as git printed it. Not normalized to full length — porcelain
/// gives full hashes, `--short` gives short ones, and we keep whichever we got.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CommitId(String);

impl CommitId {
    pub fn new(sha: impl Into<String>) -> Self {
        Self(sha.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// First 10 characters, matching what `git worktree list` displays.
    #[must_use]
    pub fn short(&self) -> &str {
        let end = self
            .0
            .char_indices()
            .nth(10)
            .map_or(self.0.len(), |(i, _)| i);
        &self.0[..end]
    }
}

/// What a worktree has checked out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Checkout {
    Branch {
        branch: BranchRef,
    },
    /// `git worktree list --porcelain` printed `detached`. There is genuinely no
    /// branch — do not substitute the directory name.
    Detached,
}

impl Checkout {
    #[must_use]
    pub fn branch(&self) -> Option<&BranchRef> {
        match self {
            Self::Branch { branch } => Some(branch),
            Self::Detached => None,
        }
    }
}

/// Stable identity for a worktree across refreshes.
///
/// The absolute path is the identity: it is what git keys on, it survives branch
/// switches, and it is unique by construction. Using the branch instead would
/// break on detached worktrees and on branch renames.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorktreeId(String);

impl WorktreeId {
    #[must_use]
    pub fn from_path(path: &Path) -> Self {
        Self(path.to_string_lossy().into_owned())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for WorktreeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One entry from `git worktree list --porcelain -z`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Worktree {
    pub id: WorktreeId,
    /// Absolute path, as git reported it.
    pub path: PathBuf,
    pub head: Option<CommitId>,
    pub checkout: Checkout,
    /// True for the first record, which git guarantees is the main worktree.
    pub is_main: bool,
    pub is_bare: bool,
    /// `Some(reason)` when git printed `locked`; the reason may be empty.
    pub locked: Option<String>,
    /// `Some(reason)` when git printed `prunable` — the directory is gone but the
    /// admin entry survives. These should be pruned, not shown as real worktrees.
    pub prunable: Option<String>,
}

impl Worktree {
    /// The final path component. A *display* name — see the module docs on why
    /// this must never be used to guess the branch.
    #[must_use]
    pub fn dirname(&self) -> &str {
        self.path
            .file_name()
            .map_or("", |n| n.to_str().unwrap_or(""))
    }

    #[must_use]
    pub fn branch(&self) -> Option<&BranchRef> {
        self.checkout.branch()
    }
}

/// Working-tree cleanliness and divergence, gathered separately from the listing
/// because it costs a `git status` per worktree.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkingTreeStatus {
    /// Tracked files modified relative to HEAD.
    ///
    /// This is the same question `git diff-index --quiet HEAD` answers, which is
    /// what the reference project's `remove` gates on — so untracked files alone
    /// do *not* set this. [`Self::untracked`] is reported separately, because
    /// `git worktree remove` itself refuses on untracked files even though the
    /// script's own check would have passed.
    pub dirty_tracked: bool,
    pub untracked: usize,
    pub staged: usize,
    pub ahead: u32,
    pub behind: u32,
}

impl WorkingTreeStatus {
    #[must_use]
    pub fn is_clean(&self) -> bool {
        !self.dirty_tracked && self.untracked == 0 && self.staged == 0
    }
}

/// A read-only snapshot of the repository, gathered once during planning so the
/// pure planning code can run without a [`crate::ports::Git`] in hand.
///
/// This is what makes the plan function genuinely pure and therefore trivially
/// testable: everything it needs to decide is in here.
#[derive(Debug, Clone, Default)]
pub struct RepoFacts {
    /// Every worktree, first entry being the main one.
    pub worktrees: Vec<Worktree>,
    pub local_branches: Vec<BranchRef>,
    /// Remote branches with the remote prefix stripped (`develop`, not
    /// `origin/develop`).
    pub remote_branches: Vec<BranchRef>,
    /// Resolved commit for the requested base ref, if it resolved at all.
    pub base_commit: Option<CommitId>,
    pub main_status: WorkingTreeStatus,
}

impl RepoFacts {
    #[must_use]
    pub fn main_worktree(&self) -> Option<&Worktree> {
        self.worktrees.first()
    }

    /// Whether `branch` is already checked out somewhere. git refuses to check the
    /// same branch out twice, so this is a hard preflight failure rather than a
    /// warning.
    #[must_use]
    pub fn worktree_holding(&self, branch: &BranchRef) -> Option<&Worktree> {
        self.worktrees.iter().find(|w| w.branch() == Some(branch))
    }

    #[must_use]
    pub fn has_local_branch(&self, branch: &BranchRef) -> bool {
        self.local_branches.contains(branch)
    }

    #[must_use]
    pub fn has_remote_branch(&self, branch: &BranchRef) -> bool {
        self.remote_branches.contains(branch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_prefix_strips_only_the_first_segment() {
        // Matches the shell's ${branch#*/}: `a/b/c` keeps the inner slash.
        assert_eq!(
            BranchRef::new("task/ACME-1234-slug").without_prefix(),
            "ACME-1234-slug"
        );
        assert_eq!(BranchRef::new("a/b/c").without_prefix(), "b/c");
        assert_eq!(BranchRef::new("noslash").without_prefix(), "noslash");
    }

    #[test]
    fn prefix_is_none_without_a_slash() {
        assert_eq!(BranchRef::new("task/x").prefix(), Some("task"));
        assert_eq!(BranchRef::new("main").prefix(), None);
    }

    #[test]
    fn short_commit_is_ten_chars_and_never_panics_on_shorter() {
        assert_eq!(CommitId::new("d2e33557e627bb73").short(), "d2e33557e6");
        assert_eq!(CommitId::new("abc").short(), "abc");
        assert_eq!(CommitId::new("").short(), "");
    }

    #[test]
    fn detached_checkout_yields_no_branch() {
        assert!(Checkout::Detached.branch().is_none());
    }

    #[test]
    fn dirname_and_branch_are_independent() {
        // The real-world case: directory says ACME-4567, branch says ACME-0000.
        let wt = Worktree {
            id: WorktreeId::from_path(Path::new("/x/ACME-4567-move-account-settings")),
            path: PathBuf::from("/x/ACME-4567-move-account-settings"),
            head: Some(CommitId::new("0122276878")),
            checkout: Checkout::Branch {
                branch: BranchRef::new("experiment/ACME-0000-migrate-api-key-settings"),
            },
            is_main: false,
            is_bare: false,
            locked: None,
            prunable: None,
        };
        assert_eq!(wt.dirname(), "ACME-4567-move-account-settings");
        assert_eq!(
            wt.branch().map(BranchRef::as_str),
            Some("experiment/ACME-0000-migrate-api-key-settings")
        );
    }

    #[test]
    fn worktree_holding_finds_the_conflicting_checkout() {
        let branch = BranchRef::new("task/x");
        let facts = RepoFacts {
            worktrees: vec![Worktree {
                id: WorktreeId::from_path(Path::new("/a")),
                path: PathBuf::from("/a"),
                head: None,
                checkout: Checkout::Branch {
                    branch: branch.clone(),
                },
                is_main: true,
                is_bare: false,
                locked: None,
                prunable: None,
            }],
            ..RepoFacts::default()
        };
        assert!(facts.worktree_holding(&branch).is_some());
        assert!(facts.worktree_holding(&BranchRef::new("other")).is_none());
    }
}
