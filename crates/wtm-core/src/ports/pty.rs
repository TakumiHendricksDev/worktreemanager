//! Running a command under a pseudo-terminal.
//!
//! # Why a PTY at all
//!
//! Because the app has to drive scripts it did not write, and those scripts prompt.
//! A captured pipe turns a prompt into a hang; a PTY turns it into a question the
//! user can answer in a terminal pane. It also means progress output that only
//! appears when `isatty()` — spinners, colour, `docker`'s layer progress — shows up
//! the way it does in a real shell.
//!
//! # Threads, not tasks
//!
//! A pty master is a blocking `Read`. The adapter runs one OS thread per session
//! that reads and forwards chunks to a [`PtySink`]. For a handful of terminals
//! that is simpler and cheaper than dragging an async runtime through the domain.

use crate::error::ExecError;
use crate::model::{ExitOutcome, SessionId};

use super::exec::{CancelToken, Invocation};

/// Receives output from a running session.
///
/// Implemented in `src-tauri` by something that emits Tauri events. Chunks are
/// delivered as bytes, not `String`: terminal output is not guaranteed to split on
/// UTF-8 boundaries, and re-assembling is the terminal emulator's job.
pub trait PtySink: Send + Sync {
    fn on_output(&self, session: &SessionId, chunk: &[u8]);
    /// Called exactly once per session.
    fn on_exit(&self, session: &SessionId, outcome: &ExitOutcome);
}

/// A live session handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spawned {
    pub session: SessionId,
    /// The argv actually spawned, for the transcript header — so a saved log shows
    /// what produced it.
    pub argv: Vec<String>,
}

/// A running session, as reported by [`PtyHost::sessions`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PtySession {
    pub session: SessionId,
    pub argv: Vec<String>,
    /// The worktree this session belongs to, when it has one. Used to enforce
    /// per-worktree concurrency and to route a terminal pane to its tab.
    pub worktree: Option<String>,
}

/// Spawn and manage pseudo-terminal sessions.
pub trait PtyHost: Send + Sync {
    /// Start a session. Returns as soon as the child is spawned — output arrives
    /// on `sink`, and completion is reported through [`PtySink::on_exit`].
    fn spawn(
        &self,
        inv: &Invocation,
        rows: u16,
        cols: u16,
        worktree: Option<&str>,
        sink: std::sync::Arc<dyn PtySink>,
    ) -> Result<Spawned, ExecError>;

    /// Block until the session finishes, its timeout expires, or `cancel` trips.
    ///
    /// A timeout or a cancel must kill the whole process *group*. Signalling only
    /// the direct child leaves grandchildren (a shell, a `docker` client) running.
    fn wait(&self, session: &SessionId, cancel: &CancelToken) -> Result<ExitOutcome, ExecError>;

    /// Forward user keystrokes.
    fn write(&self, session: &SessionId, data: &[u8]) -> Result<(), ExecError>;

    /// Tell the child its window changed, so full-screen output reflows.
    fn resize(&self, session: &SessionId, rows: u16, cols: u16) -> Result<(), ExecError>;

    /// Kill the session's process group.
    fn kill(&self, session: &SessionId) -> Result<(), ExecError>;

    fn sessions(&self) -> Vec<PtySession>;

    /// Whether a session is already running for `worktree`, used to enforce
    /// `one_per_worktree` concurrency before spawning a second setup.
    fn has_session_for(&self, worktree: &str) -> bool {
        self.sessions()
            .iter()
            .any(|s| s.worktree.as_deref() == Some(worktree))
    }
}
