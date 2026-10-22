//! Sending a local fix: reading a remote branch, pushing one commit to one branch, and the fix's
//! diff.
//!
//! # Exactly one shape of push
//!
//! [`push`] takes a refspec the outbox built and an approval covered, `<commit>:refs/heads/<branch>`,
//! and nothing else. It refuses a `+`, a wildcard, or a source that is not a commit id, so no caller
//! can turn it into a force push or a push of whatever a local branch happens to point at. There is
//! no `--force` or `--force-with-lease` in this file: a push that is not a fast-forward is refused
//! by the remote, and the outbox reads the remote again to say what happened.
//!
//! # Hooks are off here too
//!
//! A push runs `pre-push`, and in a review worktree a hooks path inside the tree is the pull
//! request author's code. So every command here runs with the checkpoint's `core.hooksPath` override
//! and the push with `--no-verify` as well, for the reason `checkpoint.rs` gives for both.
//!
//! # A push's own answer, read whole
//!
//! `--porcelain` prints one line per ref with a flag saying what happened to it, on success and on
//! refusal alike, so the outbox can tell "rejected: not a fast-forward" (nothing changed, the branch
//! moved) from a dropped connection (unknown: read the remote again).

use std::path::Path;

use wtm_core::error::{ExecError, GitError};
use wtm_core::model::CommitId;
use wtm_core::ports::exec::{CancelToken, CommandRunner, Invocation};

use crate::checkpoint::argv;
use crate::cli::{FETCH_TIMEOUT, QUERY_TIMEOUT};

/// What a push did to its one ref.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pushed {
    /// The branch moved to the commit: created, or fast-forwarded.
    Updated,
    /// The branch was already at the commit.
    UpToDate,
    /// The remote refused it, with git's reason (`non-fast-forward`, `fetch first`, or the
    /// remote's own words). Nothing changed.
    Rejected { reason: String },
}

fn failed(inv: &Invocation, code: i32, stderr: &str) -> GitError {
    GitError::Failed {
        argv: inv.display(),
        code,
        stderr: stderr.trim().to_owned(),
    }
}

fn exec_failed(inv: &Invocation, error: &ExecError) -> GitError {
    GitError::Failed {
        argv: inv.display(),
        code: -1,
        stderr: error.to_string(),
    }
}

/// The commit `reference` points at on `remote` (a remote's name or a URL), or `None` when the
/// remote has no such ref.
///
/// # Errors
///
/// When the remote cannot be read.
pub fn remote_head(
    runner: &dyn CommandRunner,
    repo: &Path,
    remote: &str,
    reference: &str,
) -> Result<Option<CommitId>, GitError> {
    let inv = Invocation::new(
        argv(&["ls-remote", "--refs", "--", remote, reference]),
        repo,
        FETCH_TIMEOUT,
    );
    let out = runner
        .run_allow_failure(&inv, &CancelToken::new())
        .map_err(|e| exec_failed(&inv, &e))?;
    if !out.is_success() {
        return Err(failed(&inv, out.code, &out.stderr));
    }
    Ok(out.stdout.lines().find_map(|line| {
        let (sha, name) = line.split_once('\t')?;
        (name.trim() == reference).then(|| CommitId::new(sha.trim()))
    }))
}

/// Whether `refspec` is the one shape [`push`] sends: a full commit id to `refs/heads/<branch>`.
#[must_use]
pub fn is_exact_refspec(refspec: &str) -> bool {
    let Some((src, dst)) = refspec.split_once(':') else {
        return false;
    };
    matches!(src.len(), 40 | 64)
        && src.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        && dst
            .strip_prefix("refs/heads/")
            .is_some_and(|branch| !branch.is_empty())
        && !dst.contains(['*', ':', '+', ' '])
}

/// The argv a push of `refspec` to `remote` runs. Shown before approving, and what runs.
#[must_use]
pub fn push_argv(remote: &str, refspec: &str) -> Vec<String> {
    argv(&["push", "--no-verify", "--porcelain", "--", remote, refspec])
}

/// Push `refspec` to `remote`, as a fast-forward or not at all.
///
/// # Errors
///
/// When `refspec` is not exact ([`is_exact_refspec`]), and when git fails without saying what
/// happened to the ref, which may mean it never reached the remote or that its answer was lost:
/// the caller reads the remote to tell.
pub fn push(
    runner: &dyn CommandRunner,
    repo: &Path,
    remote: &str,
    refspec: &str,
) -> Result<Pushed, GitError> {
    if !is_exact_refspec(refspec) {
        return Err(GitError::BadRef(refspec.to_owned()));
    }
    let inv = Invocation::new(push_argv(remote, refspec), repo, FETCH_TIMEOUT);
    let out = runner
        .run_allow_failure(&inv, &CancelToken::new())
        .map_err(|e| exec_failed(&inv, &e))?;
    let (_, dst) = refspec.split_once(':').unwrap_or_default();
    match porcelain(&out.stdout, dst) {
        Some(pushed) => Ok(pushed),
        None => Err(failed(&inv, out.code, &out.stderr)),
    }
}

/// What `--porcelain` said about `dst`, if it said anything.
fn porcelain(stdout: &str, dst: &str) -> Option<Pushed> {
    stdout.lines().find_map(|line| {
        let mut fields = line.split('\t');
        let flag = fields.next()?;
        let refs = fields.next()?;
        let summary = fields.next().unwrap_or("").trim();
        if refs.rsplit_once(':').map(|(_, to)| to) != Some(dst) {
            return None;
        }
        Some(match flag {
            " " | "*" | "+" => Pushed::Updated,
            "=" => Pushed::UpToDate,
            _ => Pushed::Rejected {
                reason: summary.to_owned(),
            },
        })
    })
}

/// Which diff of a fix: the one its hash is of, the one suggestions are built from, or the one a
/// person reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffStyle {
    /// `--binary`: everything, binary files included. What an approval pins the hash of.
    Binary,
    /// No context lines and no renames: each hunk exactly the lines it replaces.
    Suggestions,
    /// The ordinary diff, with context, for reading.
    Reading,
}

/// `git diff` of `base..head` in `repo`, in `style`.
///
/// # Errors
///
/// If git fails.
pub fn diff(
    runner: &dyn CommandRunner,
    repo: &Path,
    base: &str,
    head: &str,
    style: DiffStyle,
) -> Result<String, GitError> {
    let mut args = vec!["diff", "--no-color", "--no-ext-diff", "--no-textconv"];
    match style {
        DiffStyle::Binary => args.push("--binary"),
        DiffStyle::Suggestions => args.extend(["--unified=0", "--no-renames"]),
        DiffStyle::Reading => {}
    }
    args.extend([base, head, "--"]);
    let inv = Invocation::new(argv(&args), repo, QUERY_TIMEOUT);
    runner
        .run(&inv, &CancelToken::new())
        .map(|out| out.stdout)
        .map_err(|error| match error {
            ExecError::NonZeroExit { code, stderr, .. } => failed(&inv, code, &stderr),
            other => exec_failed(&inv, &other),
        })
}

/// Each file `base..head` changes, with its added and removed line counts (`None` for a binary
/// file), as `git diff --numstat` reports them.
///
/// # Errors
///
/// If git fails.
pub fn numstat(
    runner: &dyn CommandRunner,
    repo: &Path,
    base: &str,
    head: &str,
) -> Result<Vec<(String, Option<u32>, Option<u32>)>, GitError> {
    let inv = Invocation::new(
        argv(&[
            "diff",
            "--numstat",
            "--no-renames",
            "--no-color",
            base,
            head,
            "--",
        ]),
        repo,
        QUERY_TIMEOUT,
    );
    let out = runner
        .run(&inv, &CancelToken::new())
        .map_err(|error| match error {
            ExecError::NonZeroExit { code, stderr, .. } => failed(&inv, code, &stderr),
            other => exec_failed(&inv, &other),
        })?;
    Ok(out
        .stdout
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(3, '\t');
            let added = fields.next()?.parse().ok();
            let removed = fields.next()?.parse().ok();
            Some((fields.next()?.to_owned(), added, removed))
        })
        .collect())
}

/// One line of `path` at `commit`, 1-based, or `None` when there is no such line.
///
/// # Errors
///
/// If git fails for any reason but the file being absent.
pub fn line_at(
    runner: &dyn CommandRunner,
    repo: &Path,
    commit: &str,
    path: &str,
    line: u32,
) -> Result<Option<String>, GitError> {
    let spec = format!("{commit}:{path}");
    let inv = Invocation::new(argv(&["show", &spec]), repo, QUERY_TIMEOUT);
    let out = runner
        .run_allow_failure(&inv, &CancelToken::new())
        .map_err(|e| exec_failed(&inv, &e))?;
    if !out.is_success() {
        return Ok(None);
    }
    let index = usize::try_from(line).unwrap_or(usize::MAX).checked_sub(1);
    Ok(index.and_then(|i| out.stdout.lines().nth(i).map(str::to_owned)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIX: &str = "f1a2b3c4d5e6f708090a0b0c0d0e0f1011121314";

    #[test]
    fn only_a_commit_id_to_a_full_branch_ref_is_a_refspec_this_module_pushes() {
        assert!(is_exact_refspec(&format!("{FIX}:refs/heads/feature/csv")));
        for refspec in [
            format!("+{FIX}:refs/heads/feature"),
            format!("{FIX}:refs/heads/*"),
            "HEAD:refs/heads/feature".to_owned(),
            format!("{FIX}:feature"),
            format!("{FIX}:refs/tags/v1"),
            format!("{FIX}:refs/heads/"),
            "refs/heads/*:refs/heads/*".to_owned(),
            FIX.to_owned(),
        ] {
            assert!(!is_exact_refspec(&refspec), "{refspec}");
        }
    }

    #[test]
    fn a_push_never_forces_and_runs_no_hook() {
        let argv = push_argv("origin", &format!("{FIX}:refs/heads/x"));
        assert_eq!(
            argv[..6],
            [
                "git",
                "-c",
                "core.hooksPath=/dev/null",
                "push",
                "--no-verify",
                "--porcelain"
            ]
        );
        assert!(!argv.iter().any(|a| a.contains("force") || a == "-f"));
    }

    #[test]
    fn a_pushs_porcelain_answer_says_what_happened_to_the_ref() {
        let dst = "refs/heads/feature";
        let updated = format!("To /srv/x\n \t{FIX}:{dst}\tabc..def\nDone\n");
        assert_eq!(porcelain(&updated, dst), Some(Pushed::Updated));
        let created = format!("To /srv/x\n*\t{FIX}:{dst}\t[new branch]\nDone\n");
        assert_eq!(porcelain(&created, dst), Some(Pushed::Updated));
        let same = format!("To /srv/x\n=\t{FIX}:{dst}\t[up to date]\nDone\n");
        assert_eq!(porcelain(&same, dst), Some(Pushed::UpToDate));
        let refused = format!("To /srv/x\n!\t{FIX}:{dst}\t[rejected] (non-fast-forward)\nDone\n");
        assert_eq!(
            porcelain(&refused, dst),
            Some(Pushed::Rejected {
                reason: "[rejected] (non-fast-forward)".to_owned()
            })
        );
        assert_eq!(porcelain("fatal: unable to access\n", dst), None);
    }
}
