//! [`GhForge`]: every question as one `gh` command through the runner.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;

use wtm_core::error::ExecError;
use wtm_core::flow::{
    BranchProtection, ChangedLines, ForgeAccount, Permission, PrTarget, PullRequest, RepoSlug,
};
use wtm_core::ports::exec::{CancelToken, CommandRunner, Invocation, Output};
use wtm_core::ports::forge::{Forge, ForgeError};

use crate::parse;

/// The CLI this adapter drives.
pub const PROGRAM: &str = "gh";

/// The oldest `gh` this adapter accepts: 2.81.0, the first release whose `gh auth status` takes
/// `--json`. Every `pr view` field read here is older than that, so this one floor covers them all.
pub const MINIMUM_VERSION: (u64, u64, u64) = (2, 81, 0);

/// `gh --version` reads a file and prints.
const VERSION_TIMEOUT_MS: u64 = 10_000;
/// `gh auth status` validates the token with a request to each host it checks.
const AUTH_TIMEOUT_MS: u64 = 20_000;
/// One GraphQL or REST round trip, with room for a slow network but not for a hang.
pub(crate) const READ_TIMEOUT_MS: u64 = 30_000;

/// The `pr view` / `pr list` fields a review reads. Narrow on purpose: `gh` fetches what it is
/// asked for, and `files` or `comments` on a large pull request cost seconds.
const PR_FIELDS: &str = "number,url,title,author,state,isDraft,headRefName,headRefOid,\
                         headRepository,headRepositoryOwner,baseRefName,baseRefOid,\
                         isCrossRepository,maintainerCanModify,reviewRequests,statusCheckRollup";

/// How many pull requests `pull_request_for_head` looks through. A branch is reused for a new
/// pull request after the last one closed, rarely more than a few times.
const HEAD_LIST_LIMIT: &str = "20";

/// [`Forge`] over `gh`.
pub struct GhForge {
    runner: Arc<dyn CommandRunner>,
}

impl std::fmt::Debug for GhForge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GhForge").finish_non_exhaustive()
    }
}

impl GhForge {
    #[must_use]
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }

    /// Run `gh` and return its stdout, or the error that says what to do about it.
    ///
    /// `host` is only for the signed-out message; `what` names the thing asked for, for
    /// [`ForgeError::NotFound`].
    pub(crate) fn gh(
        &self,
        cwd: &Path,
        args: &[&str],
        timeout_ms: u64,
        host: &str,
        what: &str,
    ) -> Result<String, ForgeError> {
        let (inv, out) = self.invoke(cwd, args, None, timeout_ms)?;
        if out.is_success() {
            return Ok(out.stdout);
        }

        let message = if out.stderr.trim().is_empty() {
            out.stdout.trim().to_owned()
        } else {
            out.stderr.trim().to_owned()
        };
        // gh's documented exit status for "authentication required".
        if out.code == 4 {
            return Err(ForgeError::SignedOut {
                program: PROGRAM.to_owned(),
                host: host.to_owned(),
                message,
            });
        }
        // GraphQL says "Could not resolve to a PullRequest/Repository"; REST says "(HTTP 404)".
        // Both are what a private repository you cannot see looks like too.
        if message.contains("Could not resolve to a") || message.contains("(HTTP 404)") {
            return Err(ForgeError::NotFound {
                what: what.to_owned(),
            });
        }
        Err(ForgeError::Failed {
            argv: inv.display(),
            message,
        })
    }

    /// Run `gh` with `args`, and `stdin` as its input when given, and hand back whatever it
    /// printed. Only a failure to run it at all is an error here; what a non-zero exit means is the
    /// caller's to read.
    pub(crate) fn invoke(
        &self,
        cwd: &Path,
        args: &[&str],
        stdin: Option<&str>,
        timeout_ms: u64,
    ) -> Result<(Invocation, Output), ForgeError> {
        let mut command = Vec::with_capacity(args.len() + 1);
        command.push(PROGRAM.to_owned());
        command.extend(args.iter().map(|arg| (*arg).to_owned()));
        let mut inv = Invocation::new(command, cwd, timeout_ms).with_env(env());
        if let Some(stdin) = stdin {
            inv = inv.with_stdin(stdin);
        }
        let out = self
            .runner
            .run_allow_failure(&inv, &CancelToken::new())
            .map_err(|err| exec_error(&inv, err))?;
        Ok((inv, out))
    }
}

/// What every `gh` call gets on top of the runner's environment.
///
/// Prompts off, so a missing choice is an error and never a wait on a stdin nobody can see; update
/// notices off, so stderr carries only the answer's own problems.
fn env() -> BTreeMap<String, String> {
    [
        ("GH_PROMPT_DISABLED", "1"),
        ("GH_NO_UPDATE_NOTIFIER", "1"),
        ("GH_NO_EXTENSION_UPDATE_NOTIFIER", "1"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect()
}

pub(crate) fn exec_error(inv: &Invocation, err: ExecError) -> ForgeError {
    match err {
        ExecError::ProgramNotFound { .. } => ForgeError::NotInstalled {
            program: PROGRAM.to_owned(),
        },
        ExecError::Timeout { timeout_ms, .. } => ForgeError::Timeout {
            argv: inv.display(),
            timeout_ms,
        },
        other => ForgeError::Failed {
            argv: inv.display(),
            message: other.to_string(),
        },
    }
}

/// `--repo`'s spelling of a repository: `HOST/OWNER/NAME`, so an Enterprise host is never
/// mistaken for github.com.
fn repo_arg(repo: &RepoSlug) -> String {
    format!("{}/{}/{}", repo.host, repo.owner, repo.name)
}

/// A branch name as a REST path segment. `/` stays, because the API takes `release/4.2` as is;
/// anything that would change the path's meaning is escaped.
pub(crate) fn path_segment(branch: &str) -> String {
    let mut out = String::with_capacity(branch.len());
    for byte in branch.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'/') {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

impl Forge for GhForge {
    fn check(&self, cwd: &Path, host: &str) -> Result<ForgeAccount, ForgeError> {
        if self.runner.which(PROGRAM).is_none() {
            return Err(ForgeError::NotInstalled {
                program: PROGRAM.to_owned(),
            });
        }

        let printed = self.gh(cwd, &["--version"], VERSION_TIMEOUT_MS, host, "gh")?;
        let version = parse::version(&printed);
        if let Some(found) = version
            && found < MINIMUM_VERSION
        {
            let (major, minor, patch) = MINIMUM_VERSION;
            return Err(ForgeError::TooOld {
                program: PROGRAM.to_owned(),
                found: format!("{}.{}.{}", found.0, found.1, found.2),
                required: format!("{major}.{minor}.{patch}"),
                reason: "the first release whose `gh auth status` answers in JSON".to_owned(),
            });
        }

        // `--json` makes `auth status` exit 0 whatever the state, so the answer is in the JSON.
        let status = self.gh(
            cwd,
            &["auth", "status", "--hostname", host, "--json", "hosts"],
            AUTH_TIMEOUT_MS,
            host,
            "gh's sign-in",
        )?;
        let login = parse::signed_in(&status, host).map_err(|message| ForgeError::SignedOut {
            program: PROGRAM.to_owned(),
            host: host.to_owned(),
            message,
        })?;
        Ok(ForgeAccount {
            host: host.to_owned(),
            login,
            version: version.map(|(a, b, c)| format!("{a}.{b}.{c}")),
        })
    }

    fn pull_request(&self, cwd: &Path, target: &PrTarget) -> Result<PullRequest, ForgeError> {
        let number = target.number.to_string();
        let repo = target.repo.as_ref().map(repo_arg);
        let mut args = vec!["pr", "view", number.as_str()];
        if let Some(repo) = &repo {
            args.extend(["--repo", repo.as_str()]);
        }
        args.extend(["--json", PR_FIELDS]);
        let host = target
            .repo
            .as_ref()
            .map_or("github.com", |r| r.host.as_str());
        let what = match &target.repo {
            Some(repo) => format!("pull request {repo}#{}", target.number),
            None => format!("pull request #{}", target.number),
        };

        let stdout = self.gh(cwd, &args, READ_TIMEOUT_MS, host, &what)?;
        parse::pull_request(&stdout).map_err(|message| ForgeError::Unreadable { what, message })
    }

    fn pull_request_for_head(
        &self,
        cwd: &Path,
        repo: &RepoSlug,
        branch: &str,
    ) -> Result<Option<PullRequest>, ForgeError> {
        let repo_name = repo_arg(repo);
        let args = [
            "pr",
            "list",
            "--repo",
            repo_name.as_str(),
            "--head",
            branch,
            "--state",
            "all",
            "--limit",
            HEAD_LIST_LIMIT,
            "--json",
            PR_FIELDS,
        ];
        let what = format!("pull requests for {repo}:{branch}");
        let stdout = self.gh(cwd, &args, READ_TIMEOUT_MS, &repo.host, &what)?;
        parse::pull_request_for_head(&stdout)
            .map_err(|message| ForgeError::Unreadable { what, message })
    }

    fn permission(&self, cwd: &Path, repo: &RepoSlug) -> Result<Permission, ForgeError> {
        let repo_name = repo_arg(repo);
        let what = format!("repository {repo}");
        let stdout = self.gh(
            cwd,
            &[
                "repo",
                "view",
                repo_name.as_str(),
                "--json",
                "viewerPermission",
            ],
            READ_TIMEOUT_MS,
            &repo.host,
            &what,
        )?;
        parse::permission(&stdout).map_err(|message| ForgeError::Unreadable { what, message })
    }

    fn branch_protection(
        &self,
        cwd: &Path,
        repo: &RepoSlug,
        branch: &str,
    ) -> Result<BranchProtection, ForgeError> {
        let segment = path_segment(branch);
        let base = format!("repos/{}/{}", repo.owner, repo.name);
        let what = format!("branch {branch} of {repo}");
        let get = |path: String| {
            self.gh(
                cwd,
                &[
                    "api",
                    "--method",
                    "GET",
                    "--hostname",
                    repo.host.as_str(),
                    path.as_str(),
                ],
                READ_TIMEOUT_MS,
                &repo.host,
                &what,
            )
        };

        // A branch that does not exist, or that this account cannot see, has no protection to
        // report. That is "unknown", not a failure of the whole review.
        let summary = match get(format!("{base}/branches/{segment}")) {
            Ok(stdout) => parse::branch(&stdout).map_err(|message| ForgeError::Unreadable {
                what: what.clone(),
                message,
            })?,
            Err(ForgeError::NotFound { .. }) => {
                return Ok(BranchProtection {
                    branch: branch.to_owned(),
                    protected: None,
                    rules: Vec::new(),
                });
            }
            Err(other) => return Err(other),
        };
        let mut rules = summary.rules;
        let stdout = get(format!("{base}/rules/branches/{segment}"))?;
        for rule in parse::branch_rules(&stdout).map_err(|message| ForgeError::Unreadable {
            what: what.clone(),
            message,
        })? {
            if !rules.contains(&rule) {
                rules.push(rule);
            }
        }
        Ok(BranchProtection {
            branch: branch.to_owned(),
            protected: summary.protected,
            rules,
        })
    }

    fn changed_lines(
        &self,
        cwd: &Path,
        repo: &RepoSlug,
        base: &str,
        head: &str,
    ) -> Result<ChangedLines, ForgeError> {
        // Three dots: from where `head` left `base`, which is what a pull request's page shows.
        let path = format!(
            "repos/{}/{}/compare/{}...{}",
            repo.owner,
            repo.name,
            path_segment(base),
            path_segment(head)
        );
        let what = format!("the changes in {repo} from {base} to {head}");
        let stdout = self.gh(
            cwd,
            &[
                "api",
                "--method",
                "GET",
                "--hostname",
                repo.host.as_str(),
                path.as_str(),
            ],
            READ_TIMEOUT_MS,
            &repo.host,
            &what,
        )?;
        parse::changed_lines(&stdout).map_err(|message| ForgeError::Unreadable { what, message })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_branch_name_keeps_its_slashes_and_escapes_what_would_change_the_path() {
        assert_eq!(path_segment("release/4.2"), "release/4.2");
        assert_eq!(path_segment("fix?x=1#y"), "fix%3Fx%3D1%23y");
        assert_eq!(path_segment("a b"), "a%20b");
    }

    #[test]
    fn an_enterprise_repository_is_named_with_its_host() {
        assert_eq!(
            repo_arg(&RepoSlug::new("git.example.com", "acme", "webapp")),
            "git.example.com/acme/webapp"
        );
    }
}
