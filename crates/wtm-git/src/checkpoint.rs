//! The app's own commits in a workflow run's worktree, and the git directories a run must not write.
//!
//! # Why the app commits and the agent does not
//!
//! A run's agents cannot commit at all: their sandbox denies every git directory, so `git commit`
//! dies on `index.lock`, which the F2 spike measured for both CLIs and both roles. A writer's work
//! becomes a commit when it calls `flow_checkpoint`, and the app makes that commit here, outside the
//! sandbox, with the user's own identity and signing — which is why it has to be careful about
//! what it runs on the way.
//!
//! # Why hooks are off
//!
//! A writer may edit anything in the worktree, and a repository may set `core.hooksPath` to a
//! directory in it (`.githooks`, `.husky`). F2's hooks demo planted a `pre-commit` there and an
//! ordinary app commit ran it — the agent's script, outside the agent's sandbox, as the user.
//! `-c core.hooksPath=/dev/null` on every command here is what stopped it. Turning hooks off loses
//! a repository's pre-commit checks for these commits, which is the right trade: the run's checks
//! are its own, and they run where the app can see them.
//!
//! The same command also refuses a change to agent or hook configuration, through a predicate the
//! caller supplies, so a writer that planted one fails its checkpoint rather than having the app
//! sign the plant.

use std::path::{Path, PathBuf};

use wtm_core::error::{ExecError, GitError};
use wtm_core::model::CommitId;
use wtm_core::ports::exec::{CancelToken, CommandRunner, Invocation};

use crate::cli::{MUTATE_TIMEOUT, QUERY_TIMEOUT};

/// Prepended to every git command an app checkpoint runs. See the module docs.
pub const NO_HOOKS: [&str; 2] = ["-c", "core.hooksPath=/dev/null"];

/// Why a checkpoint was not made.
#[derive(Debug, thiserror::Error)]
pub enum CheckpointError {
    #[error("nothing has changed since the last checkpoint")]
    NothingToCommit,
    /// A changed path the caller's predicate refused, with its reason.
    #[error("{path} is {why}, which a run may not change")]
    Protected { path: String, why: String },
    #[error(transparent)]
    Git(#[from] GitError),
}

/// A checkpoint that was made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkpoint {
    pub commit: CommitId,
    /// Every path in it, relative to the worktree, as git reported them.
    pub paths: Vec<String>,
}

/// `git <NO_HOOKS> <args>`.
#[must_use]
pub fn argv(args: &[&str]) -> Vec<String> {
    std::iter::once("git")
        .chain(NO_HOOKS)
        .chain(args.iter().copied())
        .map(str::to_owned)
        .collect()
}

fn run(
    runner: &dyn CommandRunner,
    cwd: &Path,
    args: &[&str],
    timeout_ms: u64,
) -> Result<String, GitError> {
    let inv = Invocation::new(argv(args), cwd, timeout_ms);
    runner
        .run(&inv, &CancelToken::new())
        .map(|out| out.stdout)
        .map_err(|error| match error {
            ExecError::NonZeroExit { code, stderr, .. } => GitError::Failed {
                argv: inv.display(),
                code,
                stderr: stderr.trim().to_owned(),
            },
            other => GitError::Failed {
                argv: inv.display(),
                code: -1,
                stderr: other.to_string(),
            },
        })
}

/// Every path that differs from `HEAD` in `worktree`, untracked files included, as `git status`
/// names them. A rename lists both names, so neither side of one can slip past the predicate.
///
/// # Errors
///
/// If git fails.
pub fn changed(runner: &dyn CommandRunner, worktree: &Path) -> Result<Vec<String>, GitError> {
    let out = run(
        runner,
        worktree,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        QUERY_TIMEOUT,
    )?;
    let mut paths = Vec::new();
    let mut fields = out.split('\0').filter(|field| !field.is_empty());
    while let Some(entry) = fields.next() {
        let (status, path) = entry.split_at(entry.len().min(3));
        paths.push(path.to_owned());
        // `-z` puts a rename's source in the next field, with no status of its own.
        if (status.starts_with('R') || status.starts_with('C'))
            && let Some(source) = fields.next()
        {
            paths.push(source.to_owned());
        }
    }
    Ok(paths)
}

/// Commit everything that changed in `worktree`, as the user, with no hook run.
///
/// `refuse` names why a path may not be committed, or `None`. Checked against every changed path
/// before anything is staged, so a refusal leaves the index exactly as it was.
///
/// # Errors
///
/// When nothing changed, a path is refused, or git fails.
pub fn commit(
    runner: &dyn CommandRunner,
    worktree: &Path,
    message: &str,
    refuse: impl Fn(&str) -> Option<&'static str>,
) -> Result<Checkpoint, CheckpointError> {
    let paths = changed(runner, worktree)?;
    if paths.is_empty() {
        return Err(CheckpointError::NothingToCommit);
    }
    if let Some((path, why)) = paths
        .iter()
        .find_map(|path| refuse(path).map(|why| (path, why)))
    {
        return Err(CheckpointError::Protected {
            path: path.clone(),
            why: format!("under {why}"),
        });
    }
    run(runner, worktree, &["add", "--all"], MUTATE_TIMEOUT)?;
    // `--no-verify` as well as the hooks path: it is the documented way to skip `pre-commit` and
    // `commit-msg`, and saying both means neither has to be trusted alone.
    run(
        runner,
        worktree,
        &["commit", "--no-verify", "--quiet", "-m", message],
        MUTATE_TIMEOUT,
    )?;
    let head = run(runner, worktree, &["rev-parse", "HEAD"], QUERY_TIMEOUT)?;
    Ok(Checkpoint {
        commit: CommitId::new(head.trim()),
        paths,
    })
}

/// The git directories behind `worktree`: its common directory, its own gitdir, and its `.git`
/// entry, deduplicated. For a main checkout the first two are the same directory.
///
/// These are what a run's sandbox denies writes to. All three, because each is a different way
/// in: the common directory holds the refs and the config, the gitdir holds the index a commit
/// takes a lock on, and the `.git` entry of a linked worktree is a file that says where the gitdir
/// is — rewrite it and the next git command works on another repository.
///
/// # Errors
///
/// If `worktree` is not in a repository.
pub fn git_dirs(runner: &dyn CommandRunner, worktree: &Path) -> Result<Vec<PathBuf>, GitError> {
    let out = run(
        runner,
        worktree,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-common-dir",
            "--absolute-git-dir",
        ],
        QUERY_TIMEOUT,
    )?;
    let mut dirs: Vec<PathBuf> = out.lines().map(PathBuf::from).collect();
    dirs.push(worktree.join(".git"));
    // By where each one really is: git answers with resolved paths and the worktree may have been
    // named through a link (`/var` is `/private/var` on macOS), so a main checkout's `.git` would
    // otherwise be listed twice under two spellings.
    let mut seen: Vec<PathBuf> = Vec::with_capacity(dirs.len());
    let mut kept = Vec::with_capacity(dirs.len());
    for dir in dirs {
        let real = dir.canonicalize().unwrap_or_else(|_| dir.clone());
        if !seen.contains(&real) {
            seen.push(real);
            kept.push(dir);
        }
    }
    Ok(kept)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_a_checkpoint_runs_turns_hooks_off_before_its_subcommand() {
        assert_eq!(
            argv(&["commit", "-m", "x"]),
            ["git", "-c", "core.hooksPath=/dev/null", "commit", "-m", "x"]
        );
    }
}
