//! Long-running processes wtm starts and holds: a worktree's services.
//!
//! # Why this is not `CommandRunner`
//!
//! A captured command has a deadline, and expiry kills its process group (see
//! [`super::exec`]). A dev server has no deadline: it runs until someone stops it. Running one
//! through [`CommandRunner`](super::exec::CommandRunner) would either kill it at the deadline or
//! need an "infinite" deadline that every other caller could then reach for. So a service gets a
//! port of its own, and nothing else may use it.
//!
//! # What a service's process is
//!
//! A group of its own, so stopping it stops the shell, the server and whatever the server started.
//! Its stdout and stderr go straight to a log file rather than through a pipe wtm drains: a server
//! that outlives a crashed wtm keeps writing its log, and the next launch can show it.
//!
//! # A pid is not enough to signal
//!
//! Every way to stop a process here takes a [`ProcessIdentity`], and an implementation checks that
//! the process running under that pid now started when the recorded one did. A pid reused by
//! something else is never signalled.

use std::path::Path;

use crate::error::ExecError;
use crate::flow::ProcessIdentity;
use crate::model::ExitOutcome;
use crate::ports::exec::Invocation;

/// What happened when wtm tried to end a service's process group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Termination {
    /// It was running, it was ours, and it was sent `SIGTERM` then `SIGKILL`.
    Ended,
    /// Nothing runs under its pid any more.
    Gone,
    /// Something runs under its pid, and it is not the process wtm started. Left alone.
    NotOurs,
}

/// Called once, from another thread, when a launched process exits.
pub type OnExit = Box<dyn FnOnce(ExitOutcome) + Send>;

pub trait ServiceHost: Send + Sync {
    /// Start `inv` in a process group of its own, with stdout and stderr appended to `log`, and
    /// return the identity that recognises it later. `inv.timeout_ms` is ignored, for the reason in
    /// the module docs. `on_exit` runs once when it exits, however it exits.
    ///
    /// # Errors
    ///
    /// When it could not be started, or its start time could not be read, in which case it has
    /// been ended again: a process wtm could not recognise later is one it could never stop safely.
    fn launch(
        &self,
        inv: &Invocation,
        log: &Path,
        on_exit: OnExit,
    ) -> Result<ProcessIdentity, ExecError>;

    /// What runs under `pid` now, or `None` when nothing does.
    fn identify(&self, pid: u32) -> Option<ProcessIdentity>;

    /// End the process group `process` leads, `SIGTERM` then `SIGKILL`, if and only if what runs
    /// under its pid now is that process.
    fn terminate(&self, process: &ProcessIdentity) -> Termination;
}
