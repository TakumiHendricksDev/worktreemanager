//! One live agent session: a protocol driver bolted to a [`PipeHost`].
//!
//! This is the only place in the crate that touches a port, and it is deliberately thin — it
//! applies [`Step`]s and nothing else. All the judgement is in the provider, where it can be
//! tested by feeding it lines.
//!
//! # Why the driver is behind a mutex rather than owned by the reader thread
//!
//! Lines arrive on the host's reader thread; turns arrive from a Tauri command on a
//! `spawn_blocking` worker. Both drive the same state machine, so one of them has to lock. The
//! mutex is held only across `on_line` / `send_turn`, which are pure — no I/O happens under it,
//! which is the same discipline `PipeHostImpl::with_session` follows and for the same reason.

use std::sync::Arc;

use parking_lot::Mutex;
use wtm_core::error::ExecError;
use wtm_core::model::{AgentEvent, ApprovalAnswer, ExitOutcome, SessionId};
use wtm_core::ports::exec::Invocation;
use wtm_core::ports::pipe::{PipeHost, PipeSink};

use crate::provider::{Protocol, Provider, SessionRequest, Step};

/// Where a session's normalized events go.
///
/// A second sink alongside [`PipeSink`], rather than reusing it, because the two carry different
/// things: `PipeSink` carries raw lines and is an implementation detail of the transport, while
/// this carries domain events and is what the UI subscribes to. Implemented in `src-tauri` by
/// something that emits Tauri events.
pub trait AgentSink: Send + Sync {
    fn on_event(&self, session: &SessionId, event: &AgentEvent);
    /// Called once, when the session's process is gone.
    fn on_exit(&self, session: &SessionId, outcome: &ExitOutcome);
    /// The handshake finished and turns may now be sent.
    fn on_ready(&self, session: &SessionId);
}

/// A running agent session.
pub struct AgentSession {
    session: SessionId,
    host: Arc<dyn PipeHost>,
    driver: Arc<Mutex<Box<dyn Protocol>>>,
}

impl std::fmt::Debug for AgentSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentSession")
            .field("session", &self.session)
            .finish_non_exhaustive()
    }
}

impl AgentSession {
    /// Spawn the CLI and drive its handshake.
    ///
    /// The pipe sink is installed *before* the spawn returns, because a provider's first frames
    /// go out immediately and its reply can land before this function does. That is the same
    /// attach race `Terminal.svelte` solves on the frontend by buffering, and solving it here
    /// instead means the frontend never has to.
    ///
    /// # Errors
    ///
    /// If the CLI is not on the resolved `PATH`, or the child cannot be spawned.
    pub fn open(
        provider: &'static (dyn Provider + Sync),
        req: &SessionRequest,
        host: Arc<dyn PipeHost>,
        events: &Arc<dyn AgentSink>,
        timeout_ms: u64,
        worktree: Option<&str>,
    ) -> Result<Self, ExecError> {
        let driver: Arc<Mutex<Box<dyn Protocol>>> = Arc::new(Mutex::new(provider.protocol(req)));

        let relay = Arc::new(Relay {
            host: Arc::clone(&host),
            driver: Arc::clone(&driver),
            events: Arc::clone(events),
            provider: provider.id().as_str().to_owned(),
        });

        let inv = Invocation::new(provider.argv(req), req.cwd.clone(), timeout_ms);
        let spawned = host.spawn(&inv, worktree, relay as Arc<dyn PipeSink>)?;

        let session = spawned.session;

        // The handshake, applied after the sink exists so a reply cannot arrive before there is
        // anywhere to route it.
        let steps = driver.lock().open();
        apply(&session, &host, events, steps);

        Ok(Self {
            session,
            host,
            driver,
        })
    }

    #[must_use]
    pub fn id(&self) -> &SessionId {
        &self.session
    }

    /// Send a turn. Queued by the provider if the handshake has not finished.
    ///
    /// # Errors
    ///
    /// If the session's stdin is gone.
    pub fn send_turn(&self, events: &Arc<dyn AgentSink>, text: &str) -> Result<(), ExecError> {
        let steps = self.driver.lock().send_turn(text);
        run(&self.session, &self.host, events, steps)
    }

    /// Answer an outstanding approval.
    ///
    /// # Errors
    ///
    /// If the session's stdin is gone.
    pub fn answer(
        &self,
        events: &Arc<dyn AgentSink>,
        id: &str,
        answer: &ApprovalAnswer,
    ) -> Result<(), ExecError> {
        let steps = self.driver.lock().answer(id, answer);
        run(&self.session, &self.host, events, steps)
    }

    /// Ask the provider to stop the running turn.
    ///
    /// # Errors
    ///
    /// If the session's stdin is gone.
    pub fn interrupt(&self, events: &Arc<dyn AgentSink>) -> Result<(), ExecError> {
        let steps = self.driver.lock().interrupt();
        run(&self.session, &self.host, events, steps)
    }

    /// End the session.
    ///
    /// Closes stdin first, so a CLI that exits on EOF reports a real status instead of being
    /// recorded as signalled — which in the UI is the difference between "ended" and "crashed".
    /// The kill is the backstop for one that does not.
    ///
    /// # Errors
    ///
    /// If the session is already gone.
    pub fn close(&self) -> Result<(), ExecError> {
        let _ = self.host.close_stdin(&self.session);
        self.host.kill(&self.session)
    }
}

/// Routes raw lines into the driver and the driver's output onward.
struct Relay {
    host: Arc<dyn PipeHost>,
    driver: Arc<Mutex<Box<dyn Protocol>>>,
    events: Arc<dyn AgentSink>,
    provider: String,
}

impl PipeSink for Relay {
    fn on_line(&self, session: &SessionId, line: &str) {
        let steps = self.driver.lock().on_line(line);
        apply(session, &self.host, &self.events, steps);
    }

    fn on_stderr(&self, session: &SessionId, line: &str) {
        // Surfaced as a notice rather than discarded. When a handshake fails this is usually the
        // only thing that says why, and a silent session with no transcript is the worst
        // possible presentation of "your CLI is not authenticated".
        self.events.on_event(
            session,
            &AgentEvent::Notice {
                level: wtm_core::model::NoticeLevel::Warn,
                message: line.to_owned(),
            },
        );
    }

    fn on_exit(&self, session: &SessionId, outcome: &ExitOutcome) {
        tracing::debug!(provider = %self.provider, session = %session, "agent session ended");
        self.events.on_exit(session, outcome);
    }
}

/// Apply steps, reporting a failed write as a transcript event.
///
/// Used from the reader thread, where there is no caller to return an error to — so a broken
/// pipe has to become something the user can see rather than a dropped `Result`.
fn apply(
    session: &SessionId,
    host: &Arc<dyn PipeHost>,
    events: &Arc<dyn AgentSink>,
    steps: Vec<Step>,
) {
    if let Err(e) = run(session, host, events, steps) {
        events.on_event(
            session,
            &AgentEvent::Failed {
                message: e.to_string(),
            },
        );
    }
}

/// Apply steps in order.
///
/// In order, and that is the whole reason [`Step`] is one enum rather than a pair of vectors: a
/// provider that emits `SessionReady` before writing a queued turn is a different provider from
/// one that writes first, and two vectors cannot say which.
///
/// One pass is enough by construction. A driver returns everything a single input produced —
/// Codex's `initialize` reply yields `initialized`, the thread open, `SessionReady`, `Ready` and
/// any queued turns, all in one vector — so there is no follow-on queue here. If a provider ever
/// needs to react to its own write, that belongs in the driver where the state is.
fn run(
    session: &SessionId,
    host: &Arc<dyn PipeHost>,
    events: &Arc<dyn AgentSink>,
    steps: Vec<Step>,
) -> Result<(), ExecError> {
    for step in steps {
        match step {
            Step::Emit(event) => events.on_event(session, &event),
            Step::Write(frame) => host.write_line(session, &frame)?,
            Step::Ready => events.on_ready(session),
        }
    }
    Ok(())
}
