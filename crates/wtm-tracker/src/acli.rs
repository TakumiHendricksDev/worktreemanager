//! [`AcliTracker`]: every question as one `acli` command through the runner.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use wtm_core::error::ExecError;
use wtm_core::flow::{
    Issue, IssueDetail, IssueQuery, Sprint, SprintState, TrackerAccount, Transition,
};
use wtm_core::ports::exec::{CancelToken, CommandRunner, Invocation};
use wtm_core::ports::tracker::{Tracker, TrackerError};

use crate::parse;

/// The CLI this adapter drives.
pub const PROGRAM: &str = "acli";

/// The oldest `acli` this adapter accepts: 1.3.18, the release every reply in `tests/fixtures` was
/// recorded from. Older releases are not known to lack anything used here; they are not known to
/// have it either, and an unknown-flag error from one would read as a broken tracker.
pub const MINIMUM_VERSION: (u64, u64, u64) = (1, 3, 18);

/// `acli --version` prints and, sometimes, checks for an update.
const VERSION_TIMEOUT_MS: u64 = 10_000;
/// `acli jira auth status` reads its stored sign-in.
const AUTH_TIMEOUT_MS: u64 = 20_000;
/// One read, which with `--paginate` is a request per page inside one process: a sprint of a few
/// hundred issues is several pages, so this allows more than a single round trip.
const READ_TIMEOUT_MS: u64 = 60_000;

/// What a queue row needs. `workitem search` accepts no more than its own short list (see the crate
/// docs), and every one of these is on it.
const ROW_FIELDS: &str = "key,summary,status,assignee,issuetype,priority,labels";

/// What the issue workspace shows, before the project's mapped fields.
const DETAIL_FIELDS: [&str; 11] = [
    "key",
    "summary",
    "status",
    "issuetype",
    "priority",
    "assignee",
    "labels",
    "description",
    "issuelinks",
    "parent",
    "updated",
];

/// [`Tracker`] over `acli`.
pub struct AcliTracker {
    runner: Arc<dyn CommandRunner>,
}

impl std::fmt::Debug for AcliTracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcliTracker").finish_non_exhaustive()
    }
}

impl AcliTracker {
    #[must_use]
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }

    /// Run `acli` and return what it printed, or the error that says what to do about it.
    ///
    /// `what` names the thing asked for, for [`TrackerError::NotFound`].
    fn acli(
        &self,
        cwd: &Path,
        args: &[&str],
        timeout_ms: u64,
        what: &str,
    ) -> Result<String, TrackerError> {
        let mut command = Vec::with_capacity(args.len() + 1);
        command.push(PROGRAM.to_owned());
        command.extend(args.iter().map(|arg| (*arg).to_owned()));
        let inv = Invocation::new(command, cwd, timeout_ms).with_env(
            // ANSI colour in an answer would be in the way of every reader below, and a CLI that
            // honours the convention costs nothing to ask.
            [("NO_COLOR".to_owned(), "1".to_owned())]
                .into_iter()
                .collect(),
        );
        let out = self
            .runner
            .run_allow_failure(&inv, &CancelToken::new())
            .map_err(|err| exec_error(&inv, err))?;
        if out.is_success() {
            return Ok(out.stdout);
        }
        Err(failure(&inv, what, &out.stdout, &out.stderr))
    }

    fn rows(&self, cwd: &Path, args: &[&str], limit: u32) -> Result<Vec<Issue>, TrackerError> {
        let what = "the queue's issues";
        let stdout = self.acli(cwd, args, READ_TIMEOUT_MS, what)?;
        let mut issues = parse::issues(&stdout).map_err(|message| TrackerError::Unreadable {
            what: what.to_owned(),
            message,
        })?;
        issues.truncate(limit as usize);
        Ok(issues)
    }
}

/// The CLI's account of a failure, sorted by what can be done about it.
///
/// `acli` prints its errors as `✗ Error: …`, sometimes on stderr and sometimes split across both
/// streams (a missing sprint says "command execution failed" on one and "could not find the sprint"
/// on the other), so both are read.
fn failure(inv: &Invocation, what: &str, stdout: &str, stderr: &str) -> TrackerError {
    let message = [stderr, stdout]
        .iter()
        .flat_map(|stream| stream.lines())
        .map(|line| {
            line.trim()
                .trim_start_matches('✗')
                .trim()
                .trim_start_matches("Error:")
                .trim()
        })
        .filter(|line| !line.is_empty() && *line != "command execution failed")
        .collect::<Vec<_>>()
        .join(" ");
    let lower = message.to_ascii_lowercase();
    if lower.contains("does not exist or you do not have permission to see")
        || lower.contains("could not find")
        || lower.contains("either does not exist or you do not have permission")
    {
        return TrackerError::NotFound {
            what: what.to_owned(),
        };
    }
    if lower.contains("jql") {
        return TrackerError::BadQuery { message };
    }
    if lower.contains("is not allowed") || lower.contains("are not allowed") {
        return TrackerError::Unsupported {
            what: what.to_owned(),
            reason: message,
        };
    }
    if lower.contains("unauthorized")
        || lower.contains("not authenticated")
        || lower.contains("auth login")
    {
        return TrackerError::SignedOut {
            program: PROGRAM.to_owned(),
            message,
        };
    }
    TrackerError::Failed {
        argv: inv.display(),
        message,
    }
}

fn exec_error(inv: &Invocation, err: ExecError) -> TrackerError {
    match err {
        ExecError::ProgramNotFound { .. } => TrackerError::NotInstalled {
            program: PROGRAM.to_owned(),
        },
        ExecError::Timeout { timeout_ms, .. } => TrackerError::Timeout {
            argv: inv.display(),
            timeout_ms,
        },
        other => TrackerError::Failed {
            argv: inv.display(),
            message: other.to_string(),
        },
    }
}

/// Whether `key` is an issue key, so it can be a positional argument: one that began with `-`
/// would be read as a flag.
fn is_issue_key(key: &str) -> bool {
    let Some((project, number)) = key.rsplit_once('-') else {
        return false;
    };
    project.starts_with(|c: char| c.is_ascii_uppercase())
        && project
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        && !number.is_empty()
        && number.chars().all(|c| c.is_ascii_digit())
}

impl Tracker for AcliTracker {
    fn check(&self, cwd: &Path) -> Result<TrackerAccount, TrackerError> {
        if self.runner.which(PROGRAM).is_none() {
            return Err(TrackerError::NotInstalled {
                program: PROGRAM.to_owned(),
            });
        }

        let printed = self.acli(cwd, &["--version"], VERSION_TIMEOUT_MS, "acli")?;
        let version = parse::version(&printed);
        if let Some(found) = version
            && found < MINIMUM_VERSION
        {
            let (major, minor, patch) = MINIMUM_VERSION;
            return Err(TrackerError::TooOld {
                program: PROGRAM.to_owned(),
                found: format!("{}.{}.{}", found.0, found.1, found.2),
                required: format!("{major}.{minor}.{patch}"),
                reason: "the release wtm's reading of its replies was recorded from".to_owned(),
            });
        }

        // Signed out is an answer here, not a failure: whatever the exit code, the words decide.
        let status = match self.acli(cwd, &["jira", "auth", "status"], AUTH_TIMEOUT_MS, "sign-in") {
            Ok(printed) => printed,
            Err(TrackerError::Failed { message, .. } | TrackerError::SignedOut { message, .. }) => {
                return Err(TrackerError::SignedOut {
                    program: PROGRAM.to_owned(),
                    message,
                });
            }
            Err(other) => return Err(other),
        };
        let (site, login) =
            parse::signed_in(&status).map_err(|message| TrackerError::SignedOut {
                program: PROGRAM.to_owned(),
                message,
            })?;
        Ok(TrackerAccount {
            site,
            login,
            version: version.map(|(a, b, c)| format!("{a}.{b}.{c}")),
        })
    }

    fn search(&self, cwd: &Path, query: &IssueQuery) -> Result<Vec<Issue>, TrackerError> {
        // `--flag=value` throughout: a query that begins with `-` is still the query.
        let jql = format!("--jql={}", query.query);
        let limit = format!("--limit={}", query.limit);
        let fields = format!("--fields={ROW_FIELDS}");
        match query.sprint {
            Some(scope) => {
                let sprint = format!("--sprint={}", scope.sprint);
                let board = format!("--board={}", scope.board);
                self.rows(
                    cwd,
                    &[
                        "jira",
                        "sprint",
                        "list-workitems",
                        &sprint,
                        &board,
                        &jql,
                        &fields,
                        &limit,
                        "--paginate",
                        "--json",
                    ],
                    query.limit,
                )
            }
            None => self.rows(
                cwd,
                &[
                    "jira",
                    "workitem",
                    "search",
                    &jql,
                    &fields,
                    &limit,
                    "--paginate",
                    "--json",
                ],
                query.limit,
            ),
        }
    }

    fn issue(
        &self,
        cwd: &Path,
        key: &str,
        fields: &BTreeMap<String, String>,
    ) -> Result<IssueDetail, TrackerError> {
        let what = format!("issue {key}");
        if !is_issue_key(key) {
            return Err(TrackerError::NotFound { what });
        }
        let mut wanted: Vec<&str> = DETAIL_FIELDS.to_vec();
        for id in fields.values() {
            if !wanted.contains(&id.as_str()) {
                wanted.push(id);
            }
        }
        let wanted = format!("--fields={}", wanted.join(","));
        let stdout = self.acli(
            cwd,
            &["jira", "workitem", "view", key, &wanted, "--json"],
            READ_TIMEOUT_MS,
            &what,
        )?;
        parse::detail(&stdout, fields).map_err(|message| TrackerError::Unreadable { what, message })
    }

    fn sprints(
        &self,
        cwd: &Path,
        board: u64,
        states: &[SprintState],
    ) -> Result<Vec<Sprint>, TrackerError> {
        let what = format!("board {board}'s sprints");
        let id = format!("--id={board}");
        let states = format!(
            "--state={}",
            states
                .iter()
                .map(|state| state.as_str())
                .collect::<Vec<_>>()
                .join(",")
        );
        let stdout = self.acli(
            cwd,
            &[
                "jira",
                "board",
                "list-sprints",
                &id,
                &states,
                "--paginate",
                "--json",
            ],
            READ_TIMEOUT_MS,
            &what,
        )?;
        parse::sprints(&stdout).map_err(|message| TrackerError::Unreadable { what, message })
    }

    fn sprint(&self, cwd: &Path, id: u64) -> Result<Sprint, TrackerError> {
        let what = format!("sprint {id}");
        let arg = format!("--id={id}");
        let stdout = self.acli(
            cwd,
            &["jira", "sprint", "view", &arg, "--json"],
            READ_TIMEOUT_MS,
            &what,
        )?;
        parse::sprint(&stdout).map_err(|message| TrackerError::Unreadable { what, message })
    }

    fn transitions(&self, _cwd: &Path, key: &str) -> Result<Vec<Transition>, TrackerError> {
        Err(TrackerError::Unsupported {
            what: format!("listing {key}'s transitions"),
            reason: "acli has no command that lists them; `acli jira workitem transition \
                     --status` moves an issue by status name without one"
                .to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_issue_key_is_letters_then_a_dash_then_digits() {
        assert!(is_issue_key("ACME-12"));
        assert!(is_issue_key("AB2_C-7"));
        assert!(!is_issue_key("-x"));
        assert!(!is_issue_key("acme-12"));
        assert!(!is_issue_key("ACME-"));
        assert!(!is_issue_key("ACME-1 --web"));
    }
}
