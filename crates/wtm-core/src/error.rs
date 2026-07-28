//! Error types.
//!
//! # Two audiences, one type
//!
//! These errors have to serve a Rust caller (match on the variant, decide what to
//! retry) *and* a webview (render a message, maybe with a file and line to jump
//! to). Rather than maintain a parallel "DTO" hierarchy that drifts, every error
//! here derives [`serde::Serialize`] with an internal `kind` tag, so the frontend
//! gets a discriminated union it can switch on, generated from the same source of
//! truth the Rust code matches against.
//!
//! Errors deliberately carry *data*, not pre-formatted prose: a
//! [`ConfigError::Invalid`] knows its file, line and the offending key, so the UI
//! can offer "open `wtm.toml` at line 41" instead of printing a sentence.

use std::fmt;
use std::path::PathBuf;

use serde::Serialize;

/// The top-level error every use-case returns.
#[derive(Debug, thiserror::Error, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WtmError {
    #[error(transparent)]
    Config(#[from] ConfigError),

    #[error(transparent)]
    Git(#[from] GitError),

    #[error(transparent)]
    Exec(#[from] ExecError),

    #[error(transparent)]
    Render(#[from] RenderError),

    /// The submitted form is not valid. Carries every field's problem at once so
    /// the UI can show all of them inline rather than one per round-trip.
    #[error("{} field(s) need attention", .0.len())]
    Validation(Vec<FieldProblem>),

    /// Preflight found blocking conditions. Nothing has been mutated — this is
    /// always recoverable by changing the form or the repo state.
    #[error("{} preflight check(s) failed", .0.len())]
    Preflight(Vec<crate::model::PreflightItem>),

    /// The user cancelled, or a cancellation token was tripped.
    #[error("cancelled")]
    Cancelled,

    /// A project id that isn't registered.
    #[error("unknown project: {0}")]
    UnknownProject(String),

    /// A worktree id that no longer resolves — usually because it was removed
    /// outside the app.
    #[error("unknown worktree: {0}")]
    UnknownWorktree(String),

    /// A reveal was asked for a key the project's display sources do not expose.
    ///
    /// Names the key and never any part of a value, since this is the error path of the
    /// one call that handles secrets.
    #[error("`{0}` is not set for this worktree")]
    UnknownEnvKey(String),
}

/// One field's validation problem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FieldProblem {
    /// The `key` of the offending [`crate::model::FieldSpec`].
    pub field: String,
    pub message: String,
}

impl FieldProblem {
    pub fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

/// Where a config problem came from. Reported so the UI can say which of the four
/// layers actually supplied a bad value — the most common confusion when a repo
/// config and a local override disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigLayer {
    /// `defaults/wtm.default.toml`, compiled in.
    BuiltIn,
    /// `~/.config/wtm/config.toml`.
    User,
    /// `<repo>/wtm.toml`, committed and team-shared.
    Repo,
    /// `<git-common-dir>/wtm.local.toml`, untracked.
    Local,
}

impl fmt::Display for ConfigLayer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::BuiltIn => "built-in defaults",
            Self::User => "user config",
            Self::Repo => "repo wtm.toml",
            Self::Local => "local wtm.local.toml",
        };
        f.write_str(s)
    }
}

#[derive(Debug, thiserror::Error, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConfigError {
    #[error("{path}: {message}")]
    Io { path: PathBuf, message: String },

    /// Malformed TOML, or a value the schema rejects. `line`/`column` come from
    /// the TOML span when available so the UI can jump straight there.
    #[error("{path}{}: {message}", .line.map(|l| format!(":{l}")).unwrap_or_default())]
    Invalid {
        path: PathBuf,
        layer: ConfigLayer,
        line: Option<usize>,
        column: Option<usize>,
        /// The dotted config key, e.g. `naming.branch`.
        key: Option<String>,
        message: String,
    },

    /// A template referenced a token that cannot exist at that position — for
    /// example `worktree.path` inside `naming.branch`, which is evaluated before
    /// any worktree exists. Caught at load time, not create time.
    #[error("{path}: `{key}` uses `{token}`, which is not available here ({reason})")]
    TokenOutOfScope {
        path: PathBuf,
        key: String,
        token: String,
        reason: String,
    },

    /// A newer `schema_version` than this build understands.
    #[error("{path}: schema_version {found} is newer than the supported {supported}")]
    UnsupportedSchema {
        path: PathBuf,
        found: u32,
        supported: u32,
    },

    /// The config declares shell commands and has not been approved.
    ///
    /// Not a failure so much as a gate: the UI turns this into the trust prompt,
    /// showing `commands` verbatim so the user can read what would run.
    #[error("{path} is not trusted yet")]
    Untrusted {
        path: PathBuf,
        /// Every distinct argv the config would run, for display.
        commands: Vec<Vec<String>>,
        /// Hash of the reviewed content, so approval binds to *these* commands.
        content_hash: String,
    },

    /// A configured command matched a `[[guards.forbid]]` rule.
    #[error("refusing to run `{argv}`: {reason}")]
    Forbidden { argv: String, reason: String },
}

#[derive(Debug, thiserror::Error, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GitError {
    /// `git` exited non-zero. `stderr` is kept verbatim — git's own messages are
    /// better than anything we would paraphrase.
    #[error("git {argv} failed ({code}): {stderr}")]
    Failed {
        argv: String,
        code: i32,
        stderr: String,
    },

    /// Output that didn't match the porcelain grammar. Should be impossible;
    /// carries the raw bytes (lossily decoded) so a bug report is actionable.
    #[error("could not parse git output: {message}")]
    Unparsable { message: String, raw: String },

    #[error("not a git repository: {0}")]
    NotARepository(PathBuf),

    #[error("{0} is not a valid ref")]
    BadRef(String),
}

#[derive(Debug, thiserror::Error, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExecError {
    /// The program isn't on the resolved PATH.
    ///
    /// The overwhelmingly likely cause on macOS: a bundled `.app` launched from
    /// Finder inherits `PATH=/usr/bin:/bin:/usr/sbin:/sbin`, while Homebrew tools
    /// live in `/opt/homebrew/bin`. `searched` is included so the message can say
    /// where we actually looked.
    #[error("`{program}` not found on PATH")]
    ProgramNotFound { program: String, searched: String },

    #[error("`{argv}` exited with {code}")]
    NonZeroExit {
        argv: String,
        code: i32,
        stdout: String,
        stderr: String,
    },

    /// Hard-killed after exceeding its timeout.
    ///
    /// This is not a rare edge case. Some project scripts prompt on stdin and
    /// loop forever on EOF (`webapp`' `confirm()` does exactly this), so a timeout
    /// is the only thing standing between the app and a permanent hang. The whole
    /// process *group* is signalled, because the real tree is
    /// script → shell → docker.
    #[error("`{argv}` timed out after {timeout_ms}ms and was killed")]
    Timeout { argv: String, timeout_ms: u64 },

    #[error("could not spawn `{argv}`: {message}")]
    Spawn { argv: String, message: String },

    #[error("`{argv}` produced output that is not valid UTF-8")]
    NotUtf8 { argv: String },

    #[error("pty session {0} is gone")]
    NoSuchSession(String),
}

#[derive(Debug, thiserror::Error, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RenderError {
    #[error("template `{key}` is invalid: {message}")]
    Syntax { key: String, message: String },

    #[error("template `{key}` failed: {message}")]
    Eval { key: String, message: String },

    /// The template rendered, but to something unusable — an empty branch name,
    /// or a name that fails the configured `branch_must_match`.
    #[error("`{key}` rendered to {rendered:?}, which is not usable: {message}")]
    Unusable {
        key: String,
        rendered: String,
        message: String,
    },
}
