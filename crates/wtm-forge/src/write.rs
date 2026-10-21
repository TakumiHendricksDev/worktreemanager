//! [`ReviewWriter`] and [`PullRequestWriter`] for [`GhForge`]: a review's writes, a fix's follow-up
//! pull request, and the reads that find them again.
//!
//! # Exactly these requests
//!
//! Four writes, each a single `gh api` request with its body on stdin, so a comment's text never
//! reaches an argv, where `ps` and a debug log would see it:
//!
//! - `POST repos/{o}/{r}/pulls/{n}/reviews` with no `event`, which starts a pending review;
//! - GraphQL `addPullRequestReviewThread`, which puts one comment on a pending review — REST has no
//!   way to add to one that exists;
//! - `POST repos/{o}/{r}/pulls/{n}/reviews/{id}/events`, which submits it;
//! - `POST repos/{o}/{r}/pulls`, which opens a follow-up pull request into someone's branch.
//!
//! `tests/gh_write.rs` holds the adapter to that list. The reads beside them are `--method GET`,
//! like every other read in this crate. Pushing the follow-up's branch is git's, not the forge's
//! (`wtm_git::push`).
//!
//! # GitHub's refusals, told apart
//!
//! A refused write is HTTP 422 with GitHub's reasons on stdout. "User can only have one pending
//! review per pull request" is [`ForgeError::PendingReviewExists`], because the outbox has choices to
//! offer for it; any other 422 is [`ForgeError::Rejected`] with GitHub's words, because the same
//! request would be refused again. Anything else that fails may or may not have reached GitHub, so
//! it stays [`ForgeError::Failed`] or [`ForgeError::Timeout`], which the outbox settles by reading
//! before anything is sent again. GraphQL needs one more case: for a line it will not comment on it
//! answers `thread: null` and no error at all.

use std::fmt::Write as _;
use std::path::Path;

use serde_json::{Value, json};
use wtm_core::flow::{RemoteComment, RemoteReview, RepoSlug, ReviewEvent};
use wtm_core::ports::forge::{
    ForgeError, PullRequestWriter, RemotePullRequest, ReviewWriter, SentComment,
};

use crate::gh::{GhForge, READ_TIMEOUT_MS};
use crate::parse;

/// One write. As long as a read: what is slow is the network, not the request.
const WRITE_TIMEOUT_MS: u64 = 30_000;

/// GitHub's reason for refusing a second pending review, as its REST API words it.
const ONE_PENDING_REVIEW: &str = "one pending review";

const ADD_THREAD: &str = "mutation($review: ID!, $path: String!, $line: Int!, $side: DiffSide, \
     $startLine: Int, $startSide: DiffSide, $body: String!) { addPullRequestReviewThread(input: \
     {pullRequestReviewId: $review, path: $path, line: $line, side: $side, startLine: $startLine, \
     startSide: $startSide, body: $body}) { thread { id } } }";

fn event_name(event: ReviewEvent) -> &'static str {
    match event {
        ReviewEvent::Comment => "COMMENT",
        ReviewEvent::RequestChanges => "REQUEST_CHANGES",
        ReviewEvent::Approve => "APPROVE",
    }
}

/// A comment as the REST API takes it, always on the new side: a review comments on the change.
fn rest_comment(comment: &SentComment) -> Value {
    let mut value = json!({
        "path": comment.path,
        "line": comment.line,
        "side": "RIGHT",
        "body": comment.body,
    });
    if let Some(start) = comment.start_line {
        value["start_line"] = json!(start);
        value["start_side"] = json!("RIGHT");
    }
    value
}

impl GhForge {
    /// `gh api` with `body` on stdin, and its failure read as GitHub meant it.
    fn send(
        &self,
        cwd: &Path,
        host: &str,
        args: &[&str],
        body: &Value,
        what: &str,
    ) -> Result<String, ForgeError> {
        let stdin = body.to_string();
        let (inv, out) = self.invoke(cwd, args, Some(&stdin), WRITE_TIMEOUT_MS)?;
        if out.is_success() {
            return Ok(out.stdout);
        }
        let reasons = parse::api_reasons(&out.stdout);
        let stderr = out.stderr.trim();
        let message = if reasons.is_empty() {
            stderr.to_owned()
        } else {
            reasons.join("; ")
        };
        if out.code == 4 {
            return Err(ForgeError::SignedOut {
                program: crate::PROGRAM.to_owned(),
                host: host.to_owned(),
                message,
            });
        }
        if reasons.iter().any(|r| r.contains(ONE_PENDING_REVIEW)) {
            return Err(ForgeError::PendingReviewExists {
                what: what.to_owned(),
            });
        }
        if stderr.contains("(HTTP 422)") {
            return Err(ForgeError::Rejected {
                what: what.to_owned(),
                reason: message,
            });
        }
        if stderr.contains("(HTTP 404)") || message.contains("Could not resolve to a") {
            return Err(ForgeError::NotFound {
                what: what.to_owned(),
            });
        }
        Err(ForgeError::Failed {
            argv: inv.display(),
            message,
        })
    }

    /// Every item of a paginated list, read with `--method GET`.
    fn list(
        &self,
        cwd: &Path,
        repo: &RepoSlug,
        path: &str,
        what: &str,
    ) -> Result<String, ForgeError> {
        self.gh(
            cwd,
            &[
                "api",
                "--method",
                "GET",
                "--hostname",
                repo.host.as_str(),
                "--paginate",
                "--slurp",
                path,
            ],
            READ_TIMEOUT_MS,
            &repo.host,
            what,
        )
    }
}

fn pull(repo: &RepoSlug, number: u64) -> String {
    format!("repos/{}/{}/pulls/{number}", repo.owner, repo.name)
}

impl ReviewWriter for GhForge {
    fn reviews(
        &self,
        cwd: &Path,
        repo: &RepoSlug,
        number: u64,
    ) -> Result<Vec<RemoteReview>, ForgeError> {
        let what = format!("the reviews on {repo}#{number}");
        let path = format!("{}/reviews?per_page=100", pull(repo, number));
        let stdout = self.list(cwd, repo, &path, &what)?;
        parse::reviews(&stdout).map_err(|message| ForgeError::Unreadable { what, message })
    }

    fn review_comments(
        &self,
        cwd: &Path,
        repo: &RepoSlug,
        number: u64,
        review: u64,
    ) -> Result<Vec<RemoteComment>, ForgeError> {
        let what = format!("the comments of review {review} on {repo}#{number}");
        let path = format!(
            "{}/reviews/{review}/comments?per_page=100",
            pull(repo, number)
        );
        let stdout = self.list(cwd, repo, &path, &what)?;
        parse::review_comments(&stdout).map_err(|message| ForgeError::Unreadable { what, message })
    }

    fn create_pending_review(
        &self,
        cwd: &Path,
        repo: &RepoSlug,
        number: u64,
        commit_id: &str,
        body: &str,
        comments: &[SentComment],
    ) -> Result<RemoteReview, ForgeError> {
        let what = format!("a pending review on {repo}#{number}");
        let path = format!("{}/reviews", pull(repo, number));
        // No `event`: that is what makes it pending rather than submitted.
        let request = json!({
            "commit_id": commit_id,
            "body": body,
            "comments": comments.iter().map(rest_comment).collect::<Vec<_>>(),
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
        parse::review(&stdout).map_err(|message| ForgeError::Unreadable { what, message })
    }

    fn add_review_comment(
        &self,
        cwd: &Path,
        repo: &RepoSlug,
        review_node_id: &str,
        comment: &SentComment,
    ) -> Result<(), ForgeError> {
        let place = format!("{}:{}", comment.path, comment.line);
        let what = format!("a comment on {place} in your pending review on {repo}");
        let request = json!({
            "query": ADD_THREAD,
            "variables": {
                "review": review_node_id,
                "path": comment.path,
                "line": comment.line,
                "side": "RIGHT",
                "startLine": comment.start_line,
                "startSide": comment.start_line.map(|_| "RIGHT"),
                "body": comment.body,
            },
        });
        let stdout = self.send(
            cwd,
            &repo.host,
            &[
                "api",
                "graphql",
                "--hostname",
                repo.host.as_str(),
                "--input",
                "-",
            ],
            &request,
            &what,
        )?;
        let added = parse::thread_added(&stdout).map_err(|message| ForgeError::Unreadable {
            what: what.clone(),
            message,
        })?;
        if added {
            Ok(())
        } else {
            Err(ForgeError::Rejected {
                what,
                reason: format!(
                    "it would not put a comment on {place}, which is not a line of the pull \
                     request's diff at this commit"
                ),
            })
        }
    }

    fn submit_review(
        &self,
        cwd: &Path,
        repo: &RepoSlug,
        number: u64,
        review: u64,
        event: ReviewEvent,
        body: &str,
    ) -> Result<RemoteReview, ForgeError> {
        let what = format!("your review on {repo}#{number}");
        let path = format!("{}/reviews/{review}/events", pull(repo, number));
        let request = json!({ "event": event_name(event), "body": body });
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
        parse::review(&stdout).map_err(|message| ForgeError::Unreadable { what, message })
    }
}

impl PullRequestWriter for GhForge {
    fn pull_requests_from(
        &self,
        cwd: &Path,
        repo: &RepoSlug,
        head: &str,
    ) -> Result<Vec<RemotePullRequest>, ForgeError> {
        let what = format!("the pull requests from {head} in {repo}");
        let path = format!(
            "repos/{}/{}/pulls?head={}&state=all&per_page=100",
            repo.owner,
            repo.name,
            query_value(head)
        );
        let stdout = self.list(cwd, repo, &path, &what)?;
        let mut found = parse::remote_pull_requests(&stdout)
            .map_err(|message| ForgeError::Unreadable { what, message })?;
        found.sort_by_key(|pr| std::cmp::Reverse(pr.number));
        Ok(found)
    }

    fn open_pull_request(
        &self,
        cwd: &Path,
        repo: &RepoSlug,
        base: &str,
        head: &str,
        title: &str,
        body: &str,
    ) -> Result<RemotePullRequest, ForgeError> {
        let what = format!("a pull request from {head} into {base} in {repo}");
        let path = format!("repos/{}/{}/pulls", repo.owner, repo.name);
        let request = json!({
            "title": title,
            "head": head,
            "base": base,
            "body": body,
            "maintainer_can_modify": true,
            "draft": false,
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
}

/// `text` as a query string value: a branch may hold `/`, which GitHub reads as itself, but `&`,
/// `#`, `+`, `%` and spaces would change the query.
fn query_value(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' | b':' => {
                out.push(char::from(byte));
            }
            other => {
                let _ = write!(out, "%{other:02X}");
            }
        }
    }
    out
}
