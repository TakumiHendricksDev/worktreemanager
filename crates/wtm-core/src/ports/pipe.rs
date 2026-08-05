//! A long-lived child process that speaks newline-delimited JSON.
//!
//! # Why this is not [`PtyHost`](super::pty::PtyHost)
//!
//! Both spawn a child and stream its output, and the temptation is to make this a mode of
//! that. It cannot be, and the reason is the terminal line discipline rather than taste:
//!
//! - A pty **echoes** what is written to it, so every prompt sent to the child comes back
//!   as output and lands in the transcript twice.
//! - A pty translates `\n` to `\r\n` on the way out, which corrupts a protocol whose frame
//!   delimiter is the byte `\n`.
//! - Decisively: canonical mode caps a single line at `MAX_CANON` — 1024 bytes on macOS,
//!   4096 on Linux — and **discards the remainder rather than failing**. A prompt carrying a
//!   file's contents is one long JSON line, so it would be silently truncated into invalid
//!   JSON, and the symptom would be an agent that mysteriously ignores long messages.
//!
//! So the pipe/pty distinction is not an implementation detail this port could hide; it is
//! the entire reason the port exists. Anything a human types into goes through `PtyHost`.
//! Anything a program parses goes through here.
//!
//! # Why this is not [`CommandRunner`](super::exec::CommandRunner)
//!
//! That runs to completion behind a mandatory deadline and hands back the whole output. A
//! session here runs for as long as the user keeps it, and the point is the streaming.
//!
//! # Lines, not bytes
//!
//! [`PtySink::on_output`](super::pty::PtySink::on_output) deliberately forwards bytes,
//! because terminal output does not split on UTF-8 boundaries and reassembly is the
//! emulator's job. This port makes the opposite choice for the mirror-image reason: the
//! framing here **is** newlines, the adapter is the only layer that knows that, and handing
//! a partial line upward would just move the reassembly somewhere with less information.
//!
//! # The deadline is inert, and that is deliberate
//!
//! [`Invocation::timeout_ms`](super::exec::Invocation::timeout_ms) is a plain `u64` rather
//! than an `Option` so that no call site can forget it — but the thing that *enforces* a
//! deadline is a `wait`, and this trait deliberately has none. Nothing in the app waits on an
//! agent session any more than anything waits on the terminal dock's shell. Callers pass the
//! same one-week sentinel the dock uses, and that constant's comment explains why it is a week
//! rather than `u64::MAX`.

use std::sync::Arc;

use crate::error::ExecError;
use crate::model::{ExitOutcome, SessionId};

use super::exec::Invocation;

/// Receives output from a running session, one line at a time.
///
/// Implemented in `src-tauri` by something that emits Tauri events.
pub trait PipeSink: Send + Sync {
    /// One complete line of stdout, with the trailing newline already stripped.
    ///
    /// Never called with a partial line: the adapter buffers across read boundaries, because a
    /// JSON frame is routinely larger than any single `read` returns.
    fn on_line(&self, session: &SessionId, line: &str);

    /// A line the child wrote to **stderr**.
    ///
    /// Separate from [`Self::on_line`] rather than merged into it. Both CLIs use stderr for
    /// diagnostics that are not part of the protocol — a deprecation notice, a
    /// failed-to-start MCP server — and interleaving those into the JSON stream would turn a
    /// useful message into a parse error. Kept rather than discarded because when a handshake
    /// fails, this is usually the only place that says why.
    fn on_stderr(&self, session: &SessionId, line: &str);

    /// Called exactly once per session.
    fn on_exit(&self, session: &SessionId, outcome: &ExitOutcome);
}

/// A running session, as reported by [`PipeHost::sessions`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipeSession {
    pub session: SessionId,
    pub argv: Vec<String>,
    /// The worktree this session belongs to, when it has one.
    ///
    /// Unlike [`PtySession::worktree`](super::pty::PtySession::worktree) this is *not* usable
    /// as a key: a worktree may have several agent sessions at once, which is the whole point
    /// of the feature. It is here for display and for teardown when a worktree goes away.
    pub worktree: Option<String>,
}

/// Spawn and manage line-protocol sessions.
pub trait PipeHost: Send + Sync {
    /// Start a session. Returns as soon as the child is spawned — output arrives on `sink`,
    /// and completion is reported through [`PipeSink::on_exit`].
    ///
    /// Deliberately has no `rows`/`cols`: there is no terminal to size, and a caller that
    /// thinks there is has reached for the wrong port.
    fn spawn(
        &self,
        inv: &Invocation,
        worktree: Option<&str>,
        sink: Arc<dyn PipeSink>,
    ) -> Result<super::pty::Spawned, ExecError>;

    /// Write one frame to the child's stdin, appending the newline that terminates it.
    ///
    /// Takes a `&str` rather than bytes because the caller is always serializing JSON, and
    /// accepting bytes here would invite someone to send a frame with an embedded newline —
    /// which is two frames, one of them invalid.
    fn write_line(&self, session: &SessionId, line: &str) -> Result<(), ExecError>;

    /// Close the child's stdin without killing it.
    ///
    /// The graceful shutdown for a protocol that ends on EOF, which lets the child flush a
    /// final message and exit with a real status instead of being reported as signalled.
    fn close_stdin(&self, session: &SessionId) -> Result<(), ExecError>;

    /// Kill the session's process group.
    fn kill(&self, session: &SessionId) -> Result<(), ExecError>;

    fn sessions(&self) -> Vec<PipeSession>;
}
