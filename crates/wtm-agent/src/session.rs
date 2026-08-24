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
use wtm_core::model::{AgentAttachment, AgentEvent, ApprovalAnswer, ExitOutcome, SessionId};
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
    /// Held rather than passed per call, because `close` has to emit too — the declines it sends
    /// on the way out are transcript events, and a `close` that took a sink argument would let a
    /// caller hand it a different one from the session's own.
    events: Arc<dyn AgentSink>,
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

        let mut inv = Invocation::new(provider.argv(req), req.cwd.clone(), timeout_ms);
        // This is a JSON protocol child, never a terminal. Codex otherwise colours tracing output
        // on stderr when wtm was launched from Finder without an ambient `NO_COLOR`, and those
        // escape bytes have no meaning in a transcript.
        inv.env.insert("NO_COLOR".to_owned(), "1".to_owned());
        let spawned = host.spawn(&inv, worktree, relay as Arc<dyn PipeSink>)?;

        let session = spawned.session;

        // The handshake, applied after the sink exists so a reply cannot arrive before there is
        // anywhere to route it.
        let mut steps = driver.lock().open();

        // The composer's `/` list, from disk, ahead of anything the CLI says about itself.
        //
        // Emitted here rather than from the protocol because it is not protocol: no frame was sent
        // and no reply is being read. It is emitted *first* so a pane that has never been spoken to
        // still has a menu — Claude Code sends nothing at all until it receives a turn, which is
        // exactly when somebody wants to type a skill name. Whatever the session reports later
        // replaces this wholesale, and `commandsFor` keeps the descriptions only this can supply.
        let seeded = provider.seed_skills(req);
        if !seeded.is_empty() {
            steps.insert(0, Step::Emit(AgentEvent::SkillsListed { skills: seeded }));
        }

        apply(&session, &host, events, steps);

        Ok(Self {
            session,
            host,
            driver,
            events: Arc::clone(events),
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
    pub fn send_turn(&self, text: &str, attachments: &[AgentAttachment]) -> Result<(), ExecError> {
        let steps = self.driver.lock().send_turn(text, attachments);
        run(&self.session, &self.host, &self.events, steps)
    }

    /// Change the model, effort or mode without restarting. `None` leaves one alone.
    ///
    /// # Errors
    ///
    /// If the session's stdin is gone.
    pub fn reconfigure(
        &self,
        model: Option<&str>,
        effort: Option<&str>,
        mode: Option<&str>,
    ) -> Result<(), ExecError> {
        let steps = self.driver.lock().reconfigure(model, effort, mode);
        run(&self.session, &self.host, &self.events, steps)
    }

    /// Answer an outstanding approval.
    ///
    /// # Errors
    ///
    /// If the session's stdin is gone.
    pub fn answer(&self, id: &str, answer: &ApprovalAnswer) -> Result<(), ExecError> {
        let steps = self.driver.lock().answer(id, answer);
        run(&self.session, &self.host, &self.events, steps)
    }

    /// Ask the provider to stop the running turn.
    ///
    /// # Errors
    ///
    /// If the session's stdin is gone.
    pub fn interrupt(&self) -> Result<(), ExecError> {
        let steps = self.driver.lock().interrupt();
        run(&self.session, &self.host, &self.events, steps)
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
        // Declined first, then EOF, then the kill. A server blocked on an approval reply does not
        // read its stdin closing at all, so closing without this leaves a child that only the kill
        // reaches — reported as `Signalled`, which in the UI reads as a crash rather than an end.
        let steps = self.driver.lock().abandon();
        if !steps.is_empty() {
            // Errors ignored: this is a best-effort courtesy on the way out, and the pipe being
            // gone already is the ordinary case when the CLI exited on its own.
            let _ = run(&self.session, &self.host, &self.events, steps);
        }
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
                // `NO_COLOR` handles cooperative CLIs. Strip terminal sequences too because stderr
                // is third-party output and a transcript must stay readable if one ignores it.
                message: strip_terminal_sequences(line),
            },
        );
    }

    fn on_exit(&self, session: &SessionId, outcome: &ExitOutcome) {
        tracing::debug!(provider = %self.provider, session = %session, "agent session ended");
        self.events.on_exit(session, outcome);
    }
}

/// Remove common ANSI/ECMA-48 terminal sequences from a line destined for the transcript.
///
/// Covers CSI styling/cursor sequences plus OSC/DCS-style strings terminated by BEL or ST. The
/// input is already valid UTF-8, so walking chars keeps non-ASCII diagnostics intact.
fn strip_terminal_sequences(line: &str) -> String {
    let mut clean = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            clean.push(ch);
            continue;
        }

        match chars.next() {
            // Control Sequence Introducer: parameters/intermediates end at the first final byte.
            Some('[') => {
                for part in chars.by_ref() {
                    if ('@'..='~').contains(&part) {
                        break;
                    }
                }
            }
            // Operating-system commands and the other string controls end at BEL or ESC `\\`.
            Some(']' | 'P' | 'X' | '^' | '_') => {
                let mut saw_escape = false;
                for part in chars.by_ref() {
                    if part == '\u{7}' || (saw_escape && part == '\\') {
                        break;
                    }
                    saw_escape = part == '\u{1b}';
                }
            }
            // A two-byte escape sequence. Both bytes are control data and are discarded.
            Some(_) | None => {}
        }
    }

    clean
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

#[cfg(test)]
mod tests {
    use super::strip_terminal_sequences;

    #[test]
    fn terminal_styling_is_removed_without_damaging_text() {
        let line = "\u{1b}[2m2026-08-11\u{1b}[0m \u{1b}[31mERROR\u{1b}[0m: café";
        assert_eq!(strip_terminal_sequences(line), "2026-08-11 ERROR: café");
    }

    #[test]
    fn terminal_string_controls_are_removed_too() {
        let line = "before \u{1b}]8;;https://example.com\u{1b}\\link\u{1b}]8;;\u{7} after";
        assert_eq!(strip_terminal_sequences(line), "before link after");
    }
}
