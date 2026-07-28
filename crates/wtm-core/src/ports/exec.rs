//! Running a command and capturing its output.
//!
//! # The timeout is not optional
//!
//! [`Invocation::timeout_ms`] is a plain `u64`, not an `Option`, and that is the
//! most important design decision in this file.
//!
//! Project scripts prompt on stdin. Some of them prompt in a loop that never
//! terminates on EOF — the reference project's `confirm()` helper reads, fails on
//! EOF, leaves its variable stale, hits the fallback arm, prints "Please enter y or
//! n." and loops forever. Redirecting stdin from `/dev/null` does not save you;
//! only a deadline does. Making the deadline unrepresentable-as-absent means no
//! call site can forget it.
//!
//! Implementations must also kill the process *group*, not the direct child: the
//! real tree is `script → shell → docker`, and signalling only the script leaves
//! grandchildren running.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::ExecError;

/// One command to run, fully resolved — every template already rendered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invocation {
    /// argv. Never a shell string: no shell means no quoting bugs, and a Jira
    /// summary containing a backtick cannot become code.
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    /// Variables layered over the sanitized base environment.
    pub env: BTreeMap<String, String>,
    /// Hard deadline. See the module docs for why this is mandatory.
    pub timeout_ms: u64,
}

impl Invocation {
    pub fn new(argv: Vec<String>, cwd: impl Into<PathBuf>, timeout_ms: u64) -> Self {
        Self {
            argv,
            cwd: cwd.into(),
            env: BTreeMap::new(),
            timeout_ms,
        }
    }

    #[must_use]
    pub fn program(&self) -> &str {
        self.argv.first().map_or("", String::as_str)
    }

    /// Single-line form for logs, errors and guard matching. Not shell-quoted,
    /// because it is never handed to a shell.
    #[must_use]
    pub fn display(&self) -> String {
        self.argv.join(" ")
    }

    #[must_use]
    pub fn with_env(mut self, env: BTreeMap<String, String>) -> Self {
        self.env = env;
        self
    }
}

/// A finished command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
}

impl Output {
    #[must_use]
    pub fn is_success(&self) -> bool {
        self.code == 0
    }

    /// stdout split into non-empty trimmed lines — the common case for turning
    /// command output into a list of select options.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        self.stdout
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect()
    }
}

/// Cooperative cancellation.
///
/// Checked between pipeline stages and passed into long-running calls, so a user
/// pressing Cancel during a multi-gigabyte volume clone gets a response rather
/// than a frozen window. Cheap to clone; all clones share one flag.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    /// `Err(Cancelled)` if cancellation has been requested.
    pub fn check(&self) -> Result<(), crate::error::WtmError> {
        if self.is_cancelled() {
            Err(crate::error::WtmError::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// Run a command to completion and capture its output.
///
/// For anything interactive, or anything whose progress the user should watch, use
/// [`super::pty::PtyHost`] instead.
pub trait CommandRunner: Send + Sync {
    /// Run and capture. A non-zero exit is returned as
    /// [`ExecError::NonZeroExit`], not as an `Output` with a bad code — callers
    /// that genuinely tolerate failure say so explicitly via [`Self::run_allow_failure`].
    fn run(&self, inv: &Invocation, cancel: &CancelToken) -> Result<Output, ExecError>;

    /// Run and return the [`Output`] whatever the exit code, failing only if the
    /// command could not be spawned or timed out.
    fn run_allow_failure(
        &self,
        inv: &Invocation,
        cancel: &CancelToken,
    ) -> Result<Output, ExecError>;

    /// Absolute path of `program` on the resolved PATH.
    ///
    /// Used by preflight so a missing tool is reported before anything is created,
    /// rather than as a confusing failure halfway through setup.
    fn which(&self, program: &str) -> Option<PathBuf>;

    /// The PATH this runner actually uses.
    ///
    /// Surfaced because of the single most likely production failure on macOS: a
    /// bundled `.app` launched from Finder inherits
    /// `PATH=/usr/bin:/bin:/usr/sbin:/sbin`, while Homebrew tools live in
    /// `/opt/homebrew/bin`. The adapter probes a login shell to recover a usable
    /// PATH, and this exposes the result for display in a diagnostics panel.
    fn resolved_path(&self) -> String;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_is_shared_between_clones() {
        let a = CancelToken::new();
        let b = a.clone();
        assert!(!b.is_cancelled());
        a.cancel();
        assert!(b.is_cancelled(), "clones must observe the same flag");
        assert!(b.check().is_err());
    }

    #[test]
    fn lines_drops_blanks_and_trims() {
        let out = Output {
            code: 0,
            stdout: "  develop \n\n main\n\t\n".to_owned(),
            stderr: String::new(),
            duration_ms: 1,
        };
        assert_eq!(out.lines(), vec!["develop", "main"]);
    }

    #[test]
    fn display_joins_argv_without_quoting() {
        let inv = Invocation::new(
            vec!["git".to_owned(), "worktree".to_owned(), "add".to_owned()],
            "/x",
            1000,
        );
        assert_eq!(inv.display(), "git worktree add");
        assert_eq!(inv.program(), "git");
    }

    #[test]
    fn empty_argv_has_an_empty_program_rather_than_panicking() {
        let inv = Invocation::new(vec![], "/x", 1);
        assert_eq!(inv.program(), "");
    }
}
