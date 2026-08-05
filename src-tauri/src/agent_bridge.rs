//! Streaming an agent session to the webview.
//!
//! The counterpart to [`crate::pty_bridge`], and the differences are the interesting part.
//!
//! # Why these events are typed where pty output is bytes
//!
//! `pty:output` carries base64 because terminal output is arbitrary bytes that do not split on
//! UTF-8 boundaries. An agent event has already been parsed into a domain enum by `wtm-agent`, so
//! it crosses as JSON with a `kind` tag and the frontend switches on it — the same contract
//! `wtm:progress` uses. Nothing here re-encodes anything.
//!
//! # Why there is no coalescing
//!
//! The obvious worry is an event storm: Tauri serialises every emit once per listening pane, and
//! a token-by-token stream sounds like a lot of them. Measured against what the app already
//! does, it is not. The pty path emits one event per 8 KiB read and carries a full `bun install`
//! without complaint, while a model streams on the order of tens of deltas a second and Codex
//! chunks command output server-side rather than per byte.
//!
//! So: no accumulate-and-flush, and no timer thread to drive one. A buffer would need a clock to
//! avoid stalling a slow stream, which is a third thread per session bought on a guess. If a
//! chatty tool ever does make this hurt, the fix is to coalesce `CommandOutput` specifically —
//! it is the only genuinely high-rate variant — and the place to do it is here, where the
//! session boundary already is. Recorded rather than done, because ARCHITECTURE §7's line about
//! build performance applies to runtime too: measure first.

use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Emitter};
use wtm_agent::session::AgentSink;
use wtm_core::model::{AgentEvent, ExitOutcome, SessionId};

/// Event name for one normalized agent event.
pub const AGENT_EVENT: &str = "agent:event";

/// Event name for a session's process finishing.
pub const AGENT_EXIT_EVENT: &str = "agent:exit";

/// Event name for a session becoming able to accept turns.
pub const AGENT_READY_EVENT: &str = "agent:ready";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentEventPayload<'a> {
    session: String,
    event: &'a AgentEvent,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentExitPayload {
    session: String,
    outcome: ExitOutcome,
    /// A short human sentence, so the UI does not have to switch on the enum to say something
    /// useful. Same reasoning as `pty:exit`'s.
    summary: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentReadyPayload {
    session: String,
}

/// Forwards a session's events to the window as Tauri events.
pub struct AgentEventSink {
    app: AppHandle,
}

impl std::fmt::Debug for AgentEventSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentEventSink").finish_non_exhaustive()
    }
}

impl AgentEventSink {
    #[must_use]
    pub fn new(app: AppHandle) -> Arc<Self> {
        Arc::new(Self { app })
    }
}

impl AgentSink for AgentEventSink {
    fn on_event(&self, session: &SessionId, event: &AgentEvent) {
        let payload = AgentEventPayload {
            session: session.as_str().to_owned(),
            event,
        };
        // A failed emit means the window is gone. Nothing useful to do, and it must not interrupt
        // the reader thread — the same judgement `pty_bridge` makes.
        if let Err(err) = self.app.emit(AGENT_EVENT, payload) {
            tracing::debug!(error = %err, "could not emit an agent event");
        }
    }

    fn on_exit(&self, session: &SessionId, outcome: &ExitOutcome) {
        let payload = AgentExitPayload {
            session: session.as_str().to_owned(),
            outcome: outcome.clone(),
            summary: outcome.describe(),
        };
        if let Err(err) = self.app.emit(AGENT_EXIT_EVENT, payload) {
            tracing::debug!(error = %err, "could not emit an agent exit");
        }
    }

    fn on_ready(&self, session: &SessionId) {
        let payload = AgentReadyPayload {
            session: session.as_str().to_owned(),
        };
        if let Err(err) = self.app.emit(AGENT_READY_EVENT, payload) {
            tracing::debug!(error = %err, "could not emit agent readiness");
        }
    }
}
