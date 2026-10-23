//! [`OwnPullWriter`] for [`GhForge`]: the issue's own pull request, drafted, kept up to date, marked
//! ready and merged.
//!
//! # Exactly these requests
//!
//! - `POST repos/{o}/{r}/pulls` with `draft: true`, its text on stdin, which opens the draft;
//! - `PATCH repos/{o}/{r}/pulls/{n}` with the title and description on stdin;
//! - `gh pr ready {n} --repo {host}/{o}/{r}`, which marks it ready: REST has no way to, and the
//!   command names nothing but the pull request;
//! - `gh pr merge {n} --repo {host}/{o}/{r} --{method} --match-head-commit {sha}`, the plan's own
//!   spelling of a merge: GitHub itself refuses it when the head is no longer the one you approved.
//!   No `--admin`, `--auto` or `--delete-branch`: it merges as you could from the page, once.
//!
//! `tests/gh_write.rs` holds the adapter to that list, beside the review's writes.

use std::path::Path;

use serde_json::json;
use wtm_core::flow::{MergeMethod, RepoSlug};
use wtm_core::ports::forge::{ForgeError, OwnPullWriter, RemotePullRequest};

use crate::gh::GhForge;
use crate::parse;

/// One write. As long as a read: what is slow is the network, not the request.
const WRITE_TIMEOUT_MS: u64 = 30_000;

/// What GitHub says when a merge names a head the pull request has moved past.
const HEAD_MODIFIED: &str = "Head branch was modified";

impl GhForge {
    /// `gh pr …` with no stdin, and its failure read as GitHub meant it.
    fn pr_command(
        &self,
        cwd: &Path,
        host: &str,
        args: &[&str],
        what: &str,
    ) -> Result<(), ForgeError> {
        let (inv, out) = self.invoke(cwd, args, None, WRITE_TIMEOUT_MS)?;
        if out.is_success() {
            return Ok(());
        }
        let message = if out.stderr.trim().is_empty() {
            out.stdout.trim().to_owned()
        } else {
            out.stderr.trim().to_owned()
        };
        if out.code == 4 {
            return Err(ForgeError::SignedOut {
                program: crate::PROGRAM.to_owned(),
                host: host.to_owned(),
                message,
            });
        }
        // Refusals the same request would meet again: a moved head, a pull request that cannot
        // merge as it stands, or one that is not there.
        if message.contains(HEAD_MODIFIED)
            || message.contains("(HTTP 405)")
            || message.contains("(HTTP 409)")
            || message.contains("(HTTP 422)")
            || message.contains("is not mergeable")
        {
            return Err(ForgeError::Rejected {
                what: what.to_owned(),
                reason: message,
            });
        }
        if message.contains("Could not resolve to a") || message.contains("no pull requests found")
        {
            return Err(ForgeError::NotFound {
                what: what.to_owned(),
            });
        }
        Err(ForgeError::Failed {
            argv: inv.display(),
            message,
        })
    }
}

/// `host/owner/name`, as `gh pr --repo` takes it.
fn repo_arg(repo: &RepoSlug) -> String {
    format!("{}/{}/{}", repo.host, repo.owner, repo.name)
}

impl OwnPullWriter for GhForge {
    fn open_draft(
        &self,
        cwd: &Path,
        repo: &RepoSlug,
        base: &str,
        head: &str,
        title: &str,
        body: &str,
    ) -> Result<RemotePullRequest, ForgeError> {
        let what = format!("a draft pull request from {head} into {base} in {repo}");
        let path = format!("repos/{}/{}/pulls", repo.owner, repo.name);
        let request = json!({
            "title": title,
            "head": head,
            "base": base,
            "body": body,
            "draft": true,
            "maintainer_can_modify": true,
        });
        let stdout = self.send(
            cwd,
            &repo.host,
            &[
                "api",
                "--method",
                "POST",
                "--hostname",
                repo.host.as_str(),
                path.as_str(),
                "--input",
                "-",
            ],
            &request,
            &what,
        )?;
        parse::remote_pull_request(&stdout)
            .map_err(|message| ForgeError::Unreadable { what, message })
    }

    fn update(
        &self,
        cwd: &Path,
        repo: &RepoSlug,
        number: u64,
        title: &str,
        body: &str,
    ) -> Result<RemotePullRequest, ForgeError> {
        let what = format!("{repo}#{number}'s title and description");
        let path = format!("repos/{}/{}/pulls/{number}", repo.owner, repo.name);
        let stdout = self.send(
            cwd,
            &repo.host,
            &[
                "api",
                "--method",
                "PATCH",
                "--hostname",
                repo.host.as_str(),
                path.as_str(),
                "--input",
                "-",
            ],
            &json!({ "title": title, "body": body }),
            &what,
        )?;
        parse::remote_pull_request(&stdout)
            .map_err(|message| ForgeError::Unreadable { what, message })
    }

    fn mark_ready(&self, cwd: &Path, repo: &RepoSlug, number: u64) -> Result<(), ForgeError> {
        let number = number.to_string();
        let repo_arg = repo_arg(repo);
        self.pr_command(
            cwd,
            &repo.host,
            &["pr", "ready", number.as_str(), "--repo", repo_arg.as_str()],
            &format!("{repo}#{number} marked ready"),
        )
    }

    fn merge(
        &self,
        cwd: &Path,
        repo: &RepoSlug,
        number: u64,
        head: &str,
        method: MergeMethod,
    ) -> Result<(), ForgeError> {
        let number = number.to_string();
        let repo_arg = repo_arg(repo);
        self.pr_command(
            cwd,
            &repo.host,
            &[
                "pr",
                "merge",
                number.as_str(),
                "--repo",
                repo_arg.as_str(),
                method.flag(),
                "--match-head-commit",
                head,
            ],
            &format!("{repo}#{number} merged at {head}"),
        )
    }
}
