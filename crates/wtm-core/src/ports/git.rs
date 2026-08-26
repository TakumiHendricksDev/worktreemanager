//! The git port.
//!
//! # Why the CLI and not a library
//!
//! The implementation shells out to `git`. That is a deliberate choice, not
//! laziness: the porcelain interface *is* the compatibility contract, and shelling
//! out means the user's git config, credential helpers, hooks and commit signing
//! behave identically to their terminal. (`git2`'s worktree support is also
//! incomplete.)
//!
//! # Why this trait exists anyway
//!
//! Because the use-cases must be testable without a repository, and because the
//! *parsing* of porcelain output is the part most likely to be wrong. The adapter
//! is tested against a real `git` in a temporary directory; the use-cases are
//! tested against a fake.

use std::path::{Path, PathBuf};

use crate::error::GitError;
use crate::model::{BranchRef, CommitId, TrackMode, WorkingTreeStatus, Worktree};

/// Which refs to list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchFilter {
    Local,
    /// Remote-tracking branches for the given remote, with the prefix stripped.
    Remote,
    Both,
}

/// Arguments for `git worktree add`.
#[derive(Debug, Clone)]
pub struct AddOptions {
    /// Absolute target directory.
    pub path: PathBuf,
    /// `None` means `--detach`.
    pub branch: Option<BranchRef>,
    /// The start point: a branch, a remote-tracking branch, or a commit.
    pub start_point: String,
    pub track: TrackMode,
    /// Create the branch (`-b`) versus check out an existing one.
    pub create_branch: bool,
}

/// Read and write git state.
///
/// Implementations must be `Send + Sync` because a single instance is shared
/// across the blocking pool.
pub trait Git: Send + Sync {
    /// Absolute repo root (`--show-toplevel`) for a path inside a repository.
    fn repo_root(&self, any_path: &Path) -> Result<PathBuf, GitError>;

    /// The *common* git directory — shared by every worktree of a repository.
    ///
    /// Deliberately not `--git-dir`: inside a linked worktree, `.git` is a file
    /// and `--git-dir` points at a per-worktree subdirectory. Config stored under
    /// `--git-dir` would silently differ depending on which worktree you opened
    /// the app from.
    fn git_common_dir(&self, repo_root: &Path) -> Result<PathBuf, GitError>;

    /// Every worktree, main first.
    ///
    /// Must be implemented over `--porcelain -z`. The human-readable form has
    /// elastic column widths and cannot represent a path containing a space.
    fn list_worktrees(&self, repo_root: &Path) -> Result<Vec<Worktree>, GitError>;

    /// Drop admin entries for worktrees whose directories are gone.
    ///
    /// Worth calling before listing: git keeps reporting deleted worktrees as
    /// `prunable`, which would otherwise pollute the sidebar and any "is this
    /// branch already checked out" check.
    fn prune_worktrees(&self, repo_root: &Path) -> Result<(), GitError>;

    fn branches(&self, repo_root: &Path, filter: BranchFilter) -> Result<Vec<BranchRef>, GitError>;

    /// Configured remote names. A slash in a ref is not enough to identify one: local branch names
    /// routinely contain slashes (`epic/thing-api`), so callers must compare the prefix with this
    /// list before treating it as `remote/ref`.
    fn remotes(&self, repo_root: &Path) -> Result<Vec<String>, GitError>;

    /// Resolve a revision to a commit. `Ok(None)` when the ref simply does not
    /// exist — that is an expected state during planning, not an error.
    fn rev_parse(&self, repo_root: &Path, rev: &str) -> Result<Option<CommitId>, GitError>;

    fn status(&self, worktree_path: &Path) -> Result<WorkingTreeStatus, GitError>;

    /// Divergence of `branch` from `base`, as `(ahead, behind)`.
    fn ahead_behind(
        &self,
        repo_root: &Path,
        branch: &BranchRef,
        base: &str,
    ) -> Result<(u32, u32), GitError>;

    /// Fetch one ref from a remote.
    ///
    /// Callers must treat failure as non-fatal: working from a slightly stale base
    /// beats refusing to work on a train.
    fn fetch(&self, repo_root: &Path, remote: &str, refspec: &str) -> Result<(), GitError>;

    /// `git worktree add`. The first mutating call in the create pipeline.
    fn add_worktree(&self, repo_root: &Path, opts: &AddOptions) -> Result<Worktree, GitError>;

    fn remove_worktree(
        &self,
        repo_root: &Path,
        worktree_path: &Path,
        force: bool,
    ) -> Result<(), GitError>;

    fn delete_branch(
        &self,
        repo_root: &Path,
        branch: &BranchRef,
        force: bool,
    ) -> Result<(), GitError>;

    /// Whether every commit on `branch` is reachable from `base` — used to warn
    /// before deleting a branch that still has unique work on it.
    fn is_merged(&self, repo_root: &Path, branch: &BranchRef, base: &str)
    -> Result<bool, GitError>;

    /// The argv a given [`AddOptions`] will produce, without running it.
    ///
    /// Exists so the review screen can show the exact command before stage 8, and
    /// so a test can assert on the argv without a repository. The default
    /// implementation is the single source of truth for the flag ordering, which
    /// keeps the preview honest: there is no second code path to drift.
    fn add_worktree_argv(&self, opts: &AddOptions) -> Vec<String> {
        let mut argv = vec!["git".to_owned(), "worktree".to_owned(), "add".to_owned()];
        match (&opts.branch, opts.track, opts.create_branch) {
            (None, _, _) => argv.push("--detach".to_owned()),
            (Some(branch), track, create) => {
                match track {
                    TrackMode::NoTrack => argv.push("--no-track".to_owned()),
                    TrackMode::Track => argv.push("--track".to_owned()),
                    TrackMode::Detach => {}
                }
                if create {
                    argv.push("-b".to_owned());
                    argv.push(branch.as_str().to_owned());
                }
            }
        }
        argv.push("--".to_owned());
        // Paths cross IPC and config as UTF-8 strings; lossy conversion is the seam
        // that records that assumption rather than silently inventing a different path.
        argv.push(opts.path.to_string_lossy().into_owned());
        argv.push(opts.start_point.clone());
        argv
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal implementor so the default `add_worktree_argv` can be exercised.
    struct ArgvOnly;

    macro_rules! unused {
        () => {
            unimplemented!("not needed for argv tests")
        };
    }

    impl Git for ArgvOnly {
        fn repo_root(&self, _: &Path) -> Result<PathBuf, GitError> {
            unused!()
        }
        fn git_common_dir(&self, _: &Path) -> Result<PathBuf, GitError> {
            unused!()
        }
        fn list_worktrees(&self, _: &Path) -> Result<Vec<Worktree>, GitError> {
            unused!()
        }
        fn prune_worktrees(&self, _: &Path) -> Result<(), GitError> {
            unused!()
        }
        fn branches(&self, _: &Path, _: BranchFilter) -> Result<Vec<BranchRef>, GitError> {
            unused!()
        }
        fn remotes(&self, _: &Path) -> Result<Vec<String>, GitError> {
            unused!()
        }
        fn rev_parse(&self, _: &Path, _: &str) -> Result<Option<CommitId>, GitError> {
            unused!()
        }
        fn status(&self, _: &Path) -> Result<WorkingTreeStatus, GitError> {
            unused!()
        }
        fn ahead_behind(&self, _: &Path, _: &BranchRef, _: &str) -> Result<(u32, u32), GitError> {
            unused!()
        }
        fn fetch(&self, _: &Path, _: &str, _: &str) -> Result<(), GitError> {
            unused!()
        }
        fn add_worktree(&self, _: &Path, _: &AddOptions) -> Result<Worktree, GitError> {
            unused!()
        }
        fn remove_worktree(&self, _: &Path, _: &Path, _: bool) -> Result<(), GitError> {
            unused!()
        }
        fn delete_branch(&self, _: &Path, _: &BranchRef, _: bool) -> Result<(), GitError> {
            unused!()
        }
        fn is_merged(&self, _: &Path, _: &BranchRef, _: &str) -> Result<bool, GitError> {
            unused!()
        }
    }

    #[test]
    fn new_branch_uses_no_track_by_default() {
        // Reproduces the deliberate `--no-track -b` in the reference project: a
        // branch cut from origin/develop must not inherit it as upstream, or a
        // reflexive `git push` targets develop.
        let argv = ArgvOnly.add_worktree_argv(&AddOptions {
            path: PathBuf::from("/x/ACME-1234-slug"),
            branch: Some(BranchRef::new("task/ACME-1234-slug")),
            start_point: "origin/develop".to_owned(),
            track: TrackMode::NoTrack,
            create_branch: true,
        });
        assert_eq!(
            argv,
            vec![
                "git",
                "worktree",
                "add",
                "--no-track",
                "-b",
                "task/ACME-1234-slug",
                "--",
                "/x/ACME-1234-slug",
                "origin/develop",
            ]
        );
    }

    #[test]
    fn adopting_a_remote_branch_tracks_it() {
        let argv = ArgvOnly.add_worktree_argv(&AddOptions {
            path: PathBuf::from("/x/feature"),
            branch: Some(BranchRef::new("feature")),
            start_point: "origin/feature".to_owned(),
            track: TrackMode::Track,
            create_branch: true,
        });
        assert!(argv.contains(&"--track".to_owned()));
        assert!(!argv.contains(&"--no-track".to_owned()));
    }

    #[test]
    fn checking_out_an_existing_local_branch_omits_dash_b() {
        let argv = ArgvOnly.add_worktree_argv(&AddOptions {
            path: PathBuf::from("/x/existing"),
            branch: Some(BranchRef::new("existing")),
            start_point: "existing".to_owned(),
            track: TrackMode::Detach,
            create_branch: false,
        });
        assert!(!argv.contains(&"-b".to_owned()));
        assert_eq!(argv.last().unwrap(), "existing");
    }

    #[test]
    fn no_branch_means_detach() {
        let argv = ArgvOnly.add_worktree_argv(&AddOptions {
            path: PathBuf::from("/x/tmp"),
            branch: None,
            start_point: "HEAD".to_owned(),
            track: TrackMode::Detach,
            create_branch: false,
        });
        assert!(argv.contains(&"--detach".to_owned()));
    }

    #[test]
    fn a_path_that_looks_like_a_flag_is_separated_from_options() {
        // Without `--`, git would parse `-sneaky` as an unknown switch instead of a path.
        let argv = ArgvOnly.add_worktree_argv(&AddOptions {
            path: PathBuf::from("-sneaky"),
            branch: None,
            start_point: "-HEAD".to_owned(),
            track: TrackMode::Detach,
            create_branch: false,
        });
        let sep = argv
            .iter()
            .position(|a| a == "--")
            .expect("a `--` separator");
        assert_eq!(&argv[sep + 1], "-sneaky");
        assert_eq!(&argv[sep + 2], "-HEAD");
    }
}
