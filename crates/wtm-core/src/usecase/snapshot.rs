//! Snapshots: recording exactly what a review read, while you keep working.
//!
//! # Why a review reads a snapshot and not the worktree
//!
//! Reviewers take minutes, and you do not stop editing for them. A role that read `a.ts` before
//! your edit and `b.ts` after it reviewed a tree that never existed, and its findings would cite
//! lines that have since moved. So a review reads a snapshot: the worktree's whole content at one
//! instant, untracked files included, as a git tree.
//!
//! - [`Snapshots::take`] records `{head, tree, base}` and writes the change as a binary patch under
//!   the flow's directory. The tree is an object nothing refers to, so `git gc` may delete it in a
//!   couple of weeks; the patch is the durable evidence, and its hash goes in the record.
//! - [`Snapshots::open_checkout`] gives static roles a detached checkout of the snapshot under the
//!   flow's directory, and [`Snapshots::close_checkout`] removes it when their stage ends.
//! - [`Snapshots::check`] compares the worktree with the snapshot again. At the end of a role that
//!   read the worktree, it is the end-of-run check; later, it is how the Work view learns which
//!   files changed since a review and so which findings are stale.
//!
//! # What this writes
//!
//! Objects into the repository's store, the patch and a scratch index under the flow's directory,
//! and, while a checkout is open, that checkout and git's administrative entry for it. Never a
//! ref, never the reviewed worktree's files or index. ARCHITECTURE §5 states this as the
//! snapshot's relaxation of the no-mutation boundary.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;

use crate::error::{ConfigError, GitError};
use crate::flow::{BlobRef, FlowDir, ScratchLabel, Snapshot, TreeCheck};
use crate::model::CommitId;
use crate::ports::{FileStore, Git};

/// Takes, checks out and re-checks snapshots of a worktree.
pub struct Snapshots {
    pub git: Arc<dyn Git>,
    /// Only to hash the patch and to ask whether a directory exists. Everything written is
    /// written by git.
    pub files: Arc<dyn FileStore>,
}

impl std::fmt::Debug for Snapshots {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Snapshots").finish_non_exhaustive()
    }
}

/// A detached checkout of a snapshot, for roles that read the snapshot rather than the worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotCheckout {
    pub path: PathBuf,
    /// The commit checked out: the snapshot's tree on top of its head, referenced by nothing.
    pub commit: CommitId,
}

#[derive(Debug, thiserror::Error, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SnapshotError {
    /// A repository with no commit yet. There is nothing to measure a change from.
    #[error("{} has no commit to measure a change from", worktree.display())]
    NoHead { worktree: PathBuf },

    /// `base` does not resolve, or shares no history with the worktree's head.
    #[error("{} shares no history with `{base}`", worktree.display())]
    NoBase { worktree: PathBuf, base: String },

    /// Should be impossible. Checked because the id becomes part of a file name.
    #[error("git answered `{0}` where an object id belongs")]
    NotAnObjectId(String),

    #[error(transparent)]
    Git(#[from] GitError),

    #[error(transparent)]
    File(#[from] ConfigError),
}

impl Snapshots {
    /// Snapshot `worktree` as it is now, measured from where it diverged from `base`.
    ///
    /// The head is read before the tree. If a commit lands in between, the tree still holds
    /// exactly what is on disk, which is what was reviewed; only the recorded head is one commit
    /// behind, and the patch is measured from the base, not from it.
    pub fn take(
        &self,
        worktree: &Path,
        base: &str,
        dir: &FlowDir,
        label: &ScratchLabel,
    ) -> Result<Snapshot, SnapshotError> {
        let head = self
            .git
            .rev_parse(worktree, "HEAD")?
            .ok_or_else(|| SnapshotError::NoHead {
                worktree: worktree.to_path_buf(),
            })?;
        let tree = object_id(
            self.git
                .snapshot_tree(worktree, &dir.scratch_index(label))?,
        )?;
        let base_sha = self
            .git
            .merge_base(worktree, head.as_str(), base)?
            .ok_or_else(|| SnapshotError::NoBase {
                worktree: worktree.to_path_buf(),
                base: base.to_owned(),
            })?;
        let base_sha = object_id(base_sha.as_str().to_owned())?;

        let relative = FlowDir::patch(&base_sha, &tree);
        let path = dir.resolve(&relative);
        self.git
            .write_binary_diff(worktree, &base_sha, &tree, &path)?;
        let digest = self.files.digest(&path)?;

        Ok(Snapshot {
            head_sha: head.as_str().to_owned(),
            tree_sha: tree,
            base_sha,
            patch: BlobRef {
                path: relative,
                sha256: digest.sha256,
                bytes: digest.bytes,
            },
        })
    }

    /// The worktree as it is now, untracked files included, compared with `snapshot`.
    ///
    /// Run at the end of a role that read the worktree, this is the check that marks its results
    /// stale if you edited while it ran; see [`TreeCheck::stale_findings`]. Run later, it is the
    /// query for which files changed since the snapshot.
    pub fn check(
        &self,
        worktree: &Path,
        snapshot: &Snapshot,
        dir: &FlowDir,
        label: &ScratchLabel,
    ) -> Result<TreeCheck, SnapshotError> {
        let now = object_id(
            self.git
                .snapshot_tree(worktree, &dir.scratch_index(label))?,
        )?;
        if now == snapshot.tree_sha {
            return Ok(TreeCheck::Unchanged);
        }
        let listed = self.git.tree_changes(worktree, &snapshot.tree_sha, &now)?;
        Ok(TreeCheck::Changed {
            tree_sha: now,
            paths: listed.paths.into_iter().collect(),
            complete: !listed.truncated,
        })
    }

    /// Check out `snapshot`, detached, under the flow's directory.
    ///
    /// A checkout already there was left by a run that stopped mid-stage. It is removed and made
    /// again rather than reused: recreating it costs a checkout, and trusting it would mean
    /// trusting that nothing touched it in between.
    pub fn open_checkout(
        &self,
        worktree: &Path,
        snapshot: &Snapshot,
        dir: &FlowDir,
    ) -> Result<SnapshotCheckout, SnapshotError> {
        let path = dir.checkout(&snapshot.tree_sha);
        if self.files.exists(&path) {
            self.remove(worktree, &path)?;
        }
        let commit = self.git.commit_tree(
            worktree,
            &snapshot.tree_sha,
            &CommitId::new(&snapshot.head_sha),
        )?;
        self.git.add_snapshot_checkout(worktree, &path, &commit)?;
        Ok(SnapshotCheckout { path, commit })
    }

    /// Remove a checkout once the roles reading it have finished.
    pub fn close_checkout(
        &self,
        worktree: &Path,
        checkout: &SnapshotCheckout,
    ) -> Result<(), SnapshotError> {
        self.remove(worktree, &checkout.path)
    }

    fn remove(&self, worktree: &Path, path: &Path) -> Result<(), SnapshotError> {
        // Forced: a role reading a snapshot may still leave build output behind, and the checkout
        // is the app's own, holding nothing anybody wrote.
        match self.git.remove_worktree(worktree, path, true) {
            Ok(()) => Ok(()),
            // Somebody deleted the directory. git still has its administrative entry, which would
            // keep the checkout in `git worktree list` until a prune.
            Err(_) if !self.files.exists(path) => Ok(self.git.prune_worktrees(worktree)?),
            Err(error) => Err(error.into()),
        }
    }
}

/// `id`, if it is a full SHA-1 or SHA-256 object id.
fn object_id(id: String) -> Result<String, SnapshotError> {
    let valid = matches!(id.len(), 40 | 64) && id.chars().all(|c| c.is_ascii_hexdigit());
    if valid {
        Ok(id)
    } else {
        Err(SnapshotError::NotAnObjectId(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_full_object_id_can_become_part_of_a_file_name() {
        assert!(object_id("4b825dc642cb6eb9a060e54bf8d69288fbee4904".to_owned()).is_ok());
        assert!(object_id("4b825dc".to_owned()).is_err());
        assert!(object_id("../../../../etc/passwd/xxxxxxxxxxxxxxxxxxxxxxxx".to_owned()).is_err());
    }
}
