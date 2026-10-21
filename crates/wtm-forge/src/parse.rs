//! `gh`'s JSON, read tolerantly.
//!
//! Each reader takes the text `gh` printed and returns a domain value, or a sentence saying what was
//! missing. "Tolerant" has a precise meaning here: a field this build does not read is ignored, a
//! value it does not recognise becomes the domain's "unknown", and a field that is absent takes the
//! answer that claims least (not a draft, no maintainer edits, a check still pending). Only what a
//! review cannot proceed without is required: a pull request's number, its URL, which names the
//! base repository, and its head commit.

use std::collections::BTreeMap;

use serde_json::Value;
use wtm_core::flow::{
    ChangedLines, ChecksSummary, FileLines, LineRange, Permission, PrBase, PrHead, PrState,
    PrTarget, PullRequest, RemoteComment, RemoteReview, RemoteReviewState, RepoSlug, Reviewer,
};
use wtm_core::model::CommitId;
use wtm_core::ports::forge::RemotePullRequest;

/// `gh version 2.100.0 (2026-09-03)` → `(2, 100, 0)`. `None` for a build that prints something
/// else, such as `gh version DEV`.
pub(crate) fn version(printed: &str) -> Option<(u64, u64, u64)> {
    let word = printed.lines().next()?.split_whitespace().nth(2)?;
    let mut parts = word.trim_start_matches('v').split('.').map(|part| {
        let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
        digits.parse::<u64>().ok()
    });
    Some((parts.next()??, parts.next()??, parts.next()??))
}

/// The login `gh auth status --json hosts` reports for `host`'s active account, or why there is
/// none.
pub(crate) fn signed_in(json: &str, host: &str) -> Result<Option<String>, String> {
    let value = json_of(json)?;
    let accounts = value["hosts"][host].as_array().cloned().unwrap_or_default();
    let active = accounts
        .iter()
        .find(|account| account["active"].as_bool() == Some(true))
        .or_else(|| accounts.first());
    let Some(account) = active else {
        return Err(format!("No account is signed in to {host}."));
    };
    if account["state"].as_str() != Some("success") {
        // The error text is gh's account of the failure, such as "401 Unauthorized"; it never
        // holds the token itself.
        let reason = account["error"]
            .as_str()
            .and_then(|e| e.lines().next())
            .unwrap_or("its sign-in failed");
        return Err(format!("The account for {host} is not usable: {reason}"));
    }
    Ok(text(account, "login"))
}

pub(crate) fn pull_request(json: &str) -> Result<PullRequest, String> {
    read_pull_request(&json_of(json)?)
}

/// The pull request whose head is a branch of the repository itself, an open one first.
///
/// `gh pr list --head` matches the branch name in any repository, so a fork's pull request from
/// its own `main` would otherwise answer a question about yours.
pub(crate) fn pull_request_for_head(json: &str) -> Result<Option<PullRequest>, String> {
    let value = json_of(json)?;
    let list = value
        .as_array()
        .ok_or_else(|| "expected a list of pull requests".to_owned())?;
    let mut own: Vec<PullRequest> = list
        .iter()
        .map(read_pull_request)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|pr| !pr.cross_repository)
        .collect();
    // `gh` lists newest first; a stable sort keeps that order among the closed ones.
    own.sort_by_key(|pr| pr.state != PrState::Open);
    Ok(own.into_iter().next())
}

pub(crate) fn permission(json: &str) -> Result<Permission, String> {
    let value = json_of(json)?;
    Ok(
        match value["viewerPermission"].as_str().unwrap_or_default() {
            "ADMIN" => Permission::Admin,
            "MAINTAIN" => Permission::Maintain,
            "WRITE" => Permission::Write,
            "TRIAGE" => Permission::Triage,
            "READ" => Permission::Read,
            "NONE" => Permission::None,
            _ => Permission::Unknown,
        },
    )
}

/// What `GET /repos/{owner}/{repo}/branches/{branch}` says about protection.
pub(crate) struct BranchSummary {
    pub(crate) protected: Option<bool>,
    pub(crate) rules: Vec<String>,
}

/// `protected` covers rulesets and classic protection both; `protection.enabled` is classic
/// protection alone, which the rules endpoint does not list, so it is recorded as a rule of its own.
pub(crate) fn branch(json: &str) -> Result<BranchSummary, String> {
    let value = json_of(json)?;
    let mut rules = Vec::new();
    if value["protection"]["enabled"].as_bool() == Some(true) {
        rules.push("branch_protection".to_owned());
    }
    Ok(BranchSummary {
        protected: value["protected"].as_bool(),
        rules,
    })
}

/// The rule types `GET /repos/{owner}/{repo}/rules/branches/{branch}` lists, once each.
pub(crate) fn branch_rules(json: &str) -> Result<Vec<String>, String> {
    let value = json_of(json)?;
    let list = value
        .as_array()
        .ok_or_else(|| "expected a list of rules".to_owned())?;
    let mut rules: Vec<String> = Vec::new();
    for rule in list.iter().filter_map(|rule| text(rule, "type")) {
        if !rules.contains(&rule) {
            rules.push(rule);
        }
    }
    Ok(rules)
}

/// The lines a review comment can sit on, from `gh api repos/{o}/{r}/compare/{base}...{head}`: for
/// each file, the new side of every hunk in its patch. A file listed without a patch (binary, or
/// too large for the forge to show) has no lines to comment on.
pub(crate) fn changed_lines(json: &str) -> Result<ChangedLines, String> {
    let value = json_of(json)?;
    let files = value["files"]
        .as_array()
        .ok_or_else(|| "expected the comparison's files".to_owned())?;
    let mut lines = BTreeMap::new();
    for file in files {
        let Some(name) = text(file, "filename") else {
            continue;
        };
        let shown = match file["patch"].as_str() {
            Some(patch) => FileLines::Hunks(hunks(patch)),
            None => FileLines::NoDiff,
        };
        lines.insert(name, shown);
    }
    Ok(ChangedLines { files: lines })
}

/// The new side of each `@@ -a,b +c,d @@` header in a unified diff, as `c..=c+d-1`. A hunk that
/// only deletes has no new lines and is left out.
fn hunks(patch: &str) -> Vec<LineRange> {
    patch
        .lines()
        .filter_map(|line| {
            let header = line.strip_prefix("@@ ")?;
            let new = header
                .split_whitespace()
                .find(|part| part.starts_with('+'))?;
            let (start, count) = match new[1..].split_once(',') {
                Some((start, count)) => (start.parse::<u32>().ok()?, count.parse::<u32>().ok()?),
                None => (new[1..].parse::<u32>().ok()?, 1),
            };
            (count > 0).then(|| LineRange {
                start,
                end: start + count - 1,
            })
        })
        .collect()
}

/// Every review in a `gh api --paginate --slurp` of `pulls/{n}/reviews`: one list per page.
pub(crate) fn reviews(json: &str) -> Result<Vec<RemoteReview>, String> {
    pages(json)?.iter().map(read_review).collect()
}

/// One review, as creating or submitting it answers.
pub(crate) fn review(json: &str) -> Result<RemoteReview, String> {
    read_review(&json_of(json)?)
}

/// Every comment in a `gh api --paginate --slurp` of a review's comments.
pub(crate) fn review_comments(json: &str) -> Result<Vec<RemoteComment>, String> {
    pages(json)?
        .iter()
        .map(|value| {
            Ok(RemoteComment {
                id: value["id"]
                    .as_u64()
                    .ok_or_else(|| "a review comment without an id".to_owned())?,
                path: text(value, "path").unwrap_or_default(),
                body: value["body"].as_str().unwrap_or_default().to_owned(),
            })
        })
        .collect()
}

/// The items of every page, in order. `--slurp` prints a list of pages, and each page a list.
fn pages(json: &str) -> Result<Vec<Value>, String> {
    let value = json_of(json)?;
    let pages = value
        .as_array()
        .ok_or_else(|| "expected a list of pages".to_owned())?;
    Ok(pages
        .iter()
        .flat_map(|page| page.as_array().cloned().unwrap_or_default())
        .collect())
}

fn read_review(value: &Value) -> Result<RemoteReview, String> {
    let id = value["id"]
        .as_u64()
        .ok_or_else(|| "a review without an id".to_owned())?;
    let state = match value["state"].as_str().unwrap_or_default() {
        "PENDING" => RemoteReviewState::Pending,
        "COMMENTED" => RemoteReviewState::Commented,
        "CHANGES_REQUESTED" => RemoteReviewState::ChangesRequested,
        "APPROVED" => RemoteReviewState::Approved,
        "DISMISSED" => RemoteReviewState::Dismissed,
        _ => RemoteReviewState::Unknown,
    };
    Ok(RemoteReview {
        id,
        node_id: text(value, "node_id").unwrap_or_default(),
        author: text(&value["user"], "login"),
        state,
        body: value["body"].as_str().unwrap_or_default().to_owned(),
        commit_id: text(value, "commit_id"),
        url: text(value, "html_url").unwrap_or_default(),
    })
}

/// Every pull request in a `gh api --paginate --slurp` of `repos/{o}/{r}/pulls`.
pub(crate) fn remote_pull_requests(json: &str) -> Result<Vec<RemotePullRequest>, String> {
    pages(json)?.iter().map(read_remote_pull_request).collect()
}

/// One pull request as the REST API returns it from a create.
pub(crate) fn remote_pull_request(json: &str) -> Result<RemotePullRequest, String> {
    read_remote_pull_request(&json_of(json)?)
}

fn read_remote_pull_request(value: &Value) -> Result<RemotePullRequest, String> {
    let number = value["number"]
        .as_u64()
        .ok_or_else(|| "a pull request without a number".to_owned())?;
    let state = match (value["state"].as_str(), value["merged_at"].is_string()) {
        (_, true) => PrState::Merged,
        (Some("open"), _) => PrState::Open,
        (Some("closed"), _) => PrState::Closed,
        _ => PrState::Unknown,
    };
    Ok(RemotePullRequest {
        number,
        url: text(value, "html_url").unwrap_or_default(),
        state,
        head: text(&value["head"], "label").unwrap_or_default(),
        base: text(&value["base"], "ref").unwrap_or_default(),
        body: value["body"].as_str().unwrap_or_default().to_owned(),
    })
}

/// What a failed `gh api` request printed on stdout: GitHub's own reasons, which are either strings
/// or objects with a `message`, else its top-level message.
pub(crate) fn api_reasons(stdout: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<Value>(stdout) else {
        return Vec::new();
    };
    let listed: Vec<String> = value["errors"]
        .as_array()
        .map(|errors| {
            errors
                .iter()
                .filter_map(|error| {
                    error
                        .as_str()
                        .map(str::to_owned)
                        .or_else(|| text(error, "message"))
                })
                .collect()
        })
        .unwrap_or_default();
    if listed.is_empty() {
        text(&value, "message").into_iter().collect()
    } else {
        listed
    }
}

/// Whether a GraphQL `addPullRequestReviewThread` reply made a thread. GitHub answers `null` for a
/// line it will not put a comment on, with no error.
pub(crate) fn thread_added(json: &str) -> Result<bool, String> {
    let value = json_of(json)?;
    Ok(value["data"]["addPullRequestReviewThread"]["thread"].is_object())
}

fn json_of(text: &str) -> Result<Value, String> {
    serde_json::from_str(text).map_err(|e| format!("gh did not print JSON ({e})"))
}

/// A non-empty string field.
fn text(value: &Value, key: &str) -> Option<String> {
    value[key]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn read_pull_request(value: &Value) -> Result<PullRequest, String> {
    let number = value["number"]
        .as_u64()
        .ok_or_else(|| "a pull request without a number".to_owned())?;
    let url = text(value, "url").ok_or_else(|| format!("pull request #{number} has no URL"))?;
    // The URL is the one field that names the base repository with its host.
    let base_repo = PrTarget::parse(&url)
        .ok()
        .and_then(|target| target.repo)
        .ok_or_else(|| format!("`{url}` is not a pull request URL"))?;
    let head_sha = text(value, "headRefOid")
        .ok_or_else(|| format!("pull request #{number} reported no head commit"))?;

    let head_repo = head_repository(value, &base_repo.host);
    // Absent means this gh did not say. Treating the head as a fork's is the reading that costs
    // nothing if wrong: `pull/<n>/head` exists for every pull request.
    let cross_repository = value["isCrossRepository"].as_bool().unwrap_or_else(|| {
        head_repo
            .as_ref()
            .is_none_or(|head| !head.same_repository(&base_repo))
    });

    Ok(PullRequest {
        number,
        url,
        title: text(value, "title").unwrap_or_default(),
        author: text(&value["author"], "login"),
        state: match value["state"].as_str().unwrap_or_default() {
            "OPEN" => PrState::Open,
            "CLOSED" => PrState::Closed,
            "MERGED" => PrState::Merged,
            _ => PrState::Unknown,
        },
        draft: value["isDraft"].as_bool().unwrap_or(false),
        head: PrHead {
            repo: head_repo,
            branch: text(value, "headRefName").unwrap_or_default(),
            sha: CommitId::new(head_sha),
        },
        base: PrBase {
            repo: base_repo,
            branch: text(value, "baseRefName").unwrap_or_default(),
            sha: text(value, "baseRefOid").map(CommitId::new),
        },
        cross_repository,
        maintainer_can_modify: value["maintainerCanModify"].as_bool().unwrap_or(false),
        requested_reviewers: reviewers(&value["reviewRequests"]),
        checks: checks(&value["statusCheckRollup"]),
    })
}

/// The head repository from `headRepository.nameWithOwner`, or its `name` with
/// `headRepositoryOwner.login`. `None` when the fork is gone, which `gh` reports as `null`.
fn head_repository(value: &Value, host: &str) -> Option<RepoSlug> {
    let repo = &value["headRepository"];
    if let Some(full) = text(repo, "nameWithOwner") {
        return RepoSlug::from_full_name(host, &full);
    }
    let owner = text(&value["headRepositoryOwner"], "login")?;
    let name = text(repo, "name")?;
    Some(RepoSlug::new(host, owner, name))
}

/// `reviewRequests`: users by login, teams by `org/slug`, and a kind this build does not know by
/// whichever name it carries.
fn reviewers(value: &Value) -> Vec<Reviewer> {
    value
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|request| {
            let team = request["__typename"].as_str() == Some("Team");
            let name = if team {
                text(request, "slug").or_else(|| text(request, "name"))
            } else {
                text(request, "login")
                    .or_else(|| text(request, "slug"))
                    .or_else(|| text(request, "name"))
            }?;
            Some(Reviewer { name, team })
        })
        .collect()
}

/// `statusCheckRollup`, counted. A `CheckRun` has a status and, once completed, a conclusion; a
/// commit status (`StatusContext`) has a state. Anything unrecognised is pending, the only bucket
/// that never reads as a pass.
fn checks(value: &Value) -> ChecksSummary {
    let mut summary = ChecksSummary::default();
    for check in value.as_array().map(Vec::as_slice).unwrap_or_default() {
        let bucket = if let Some(state) = check["state"].as_str() {
            match state {
                "SUCCESS" => Bucket::Passed,
                "FAILURE" | "ERROR" => Bucket::Failed,
                _ => Bucket::Pending,
            }
        } else if check["status"].as_str() == Some("COMPLETED") {
            match check["conclusion"].as_str().unwrap_or_default() {
                "SUCCESS" => Bucket::Passed,
                "FAILURE" | "TIMED_OUT" | "STARTUP_FAILURE" | "ACTION_REQUIRED" => Bucket::Failed,
                "SKIPPED" | "NEUTRAL" | "CANCELLED" | "STALE" => Bucket::Skipped,
                _ => Bucket::Pending,
            }
        } else {
            Bucket::Pending
        };
        match bucket {
            Bucket::Passed => summary.passed += 1,
            Bucket::Failed => summary.failed += 1,
            Bucket::Pending => summary.pending += 1,
            Bucket::Skipped => summary.skipped += 1,
        }
    }
    summary
}

enum Bucket {
    Passed,
    Failed,
    Pending,
    Skipped,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_release_build_has_a_version_and_a_development_build_has_none() {
        assert_eq!(
            version(
                "gh version 2.100.0 (2026-09-03)\nhttps://github.com/cli/cli/releases/tag/v2.100.0\n"
            ),
            Some((2, 100, 0))
        );
        assert_eq!(version("gh version 2.81.0-rc1"), Some((2, 81, 0)));
        assert_eq!(version("gh version DEV"), None);
        assert_eq!(version(""), None);
    }

    #[test]
    fn each_hunks_new_side_is_read_and_a_hunk_that_only_deletes_has_no_lines() {
        let patch =
            "@@ -9,14 +9,15 @@ def header():\n x\n@@ -40 +41 @@\n-y\n+z\n@@ -60,3 +62,0 @@\n-a";
        assert_eq!(
            hunks(patch),
            [
                LineRange { start: 9, end: 23 },
                LineRange { start: 41, end: 41 }
            ]
        );
    }

    #[test]
    fn github_s_reasons_are_read_whether_strings_or_objects() {
        assert_eq!(
            api_reasons(
                r#"{"message":"Unprocessable Entity","errors":["Line could not be resolved"]}"#
            ),
            ["Line could not be resolved"]
        );
        assert_eq!(
            api_reasons(
                r#"{"message":"Validation Failed","errors":[{"resource":"Review","code":"custom","message":"x"}]}"#
            ),
            ["x"]
        );
        assert_eq!(api_reasons(r#"{"message":"Not Found"}"#), ["Not Found"]);
        assert!(api_reasons("not json").is_empty());
    }

    #[test]
    fn a_rollup_entry_of_a_kind_nobody_has_seen_counts_as_pending_never_as_passed() {
        let summary = checks(&serde_json::json!([
            { "__typename": "FutureCheck", "name": "x" },
            { "__typename": "CheckRun", "status": "COMPLETED", "conclusion": "SOMETHING_NEW" },
        ]));
        assert_eq!(summary.pending, 2);
        assert_eq!(summary.passed, 0);
    }
}
