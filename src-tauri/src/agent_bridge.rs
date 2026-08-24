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

use crate::app::App;

/// Event name for one normalized agent event.
pub const AGENT_EVENT: &str = "agent:event";

/// Event name for a session's process finishing.
pub const AGENT_EXIT_EVENT: &str = "agent:exit";

/// Event name for a session becoming able to accept turns.
pub const AGENT_READY_EVENT: &str = "agent:ready";

/// Event name for a session Rust opened on its own initiative.
///
/// Every other session in the app exists because the frontend asked for one and was handed an id.
/// A handoff inverts that: a child process asks, so the session is already running by the time the
/// window could possibly know, and without this it would be a CLI streaming events to a pane that
/// does not exist. The frontend adopts it — the same path a reload already uses for sessions that
/// outlived the webview.
pub const AGENT_SPAWNED_EVENT: &str = "agent:spawned";

/// A session the frontend should adopt a pane for.
///
/// Carries the model, effort and mode as well as the identity, because an adopted pane otherwise
/// renders its picker empty: the frontend normally learns those by *choosing* them before it opens a
/// session, and here it did not choose them.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpawnedSession {
    pub session: String,
    pub project: String,
    pub worktree: String,
    pub provider: String,
    pub model: Option<String>,
    pub effort: Option<wtm_core::model::Effort>,
    pub mode: Option<String>,
    pub parent_session: Option<String>,
    pub run: Option<String>,
    pub title: Option<String>,
}

/// Tell the window to open a pane for a session that already exists.
pub fn announce_spawn(handle: &AppHandle, spawned: &SpawnedSession) {
    if let Err(err) = handle.emit(AGENT_SPAWNED_EVENT, spawned) {
        // The window is gone. The session keeps running and is reachable again through
        // `list_agent_sessions` if a window comes back, which is the same degradation a reload has.
        tracing::debug!(error = %err, "could not announce a spawned session");
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentEventPayload<'a> {
    session: String,
    /// This event's position in the session's stream, so a pane that repainted from the replay
    /// buffer can tell what it has already drawn. `None` only when there is no app state to
    /// number it — a test sink, and a session already gone from the registry.
    seq: Option<u64>,
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
    handle: AppHandle,
    /// For the resume list. `None` in the one place a sink exists without app state — a test.
    app: Option<Arc<App>>,
}

impl std::fmt::Debug for AgentEventSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentEventSink").finish_non_exhaustive()
    }
}

impl AgentEventSink {
    #[must_use]
    pub fn new(handle: AppHandle) -> Arc<Self> {
        use tauri::Manager;

        // `try_state` rather than `state`: `state` panics on an unmanaged type, and a sink that
        // panicked would take a reader thread with it. Without app state the events still flow and
        // only the resume list goes unwritten, which is the right way for this to degrade.
        let app = handle.try_state::<Arc<App>>().map(|s| Arc::clone(&s));
        Arc::new(Self { handle, app })
    }
}

impl AgentEventSink {
    /// Record a session as resumable, and note the provider's id on it.
    ///
    /// Driven from `SessionReady` rather than from the first reply, because that is the first moment
    /// resuming is possible — so a session that fails mid-turn is still in the list, which is exactly
    /// when someone wants it back.
    fn remember(&self, session: &SessionId, event: &AgentEvent) {
        let AgentEvent::SessionReady {
            provider_session_id,
            model,
            effort,
            ..
        } = event
        else {
            return;
        };
        let Some(app) = &self.app else { return };
        let Some(facts) = app
            .live_agents()
            .into_iter()
            .find(|f| f.session == session.as_str())
        else {
            return;
        };

        app.note_provider_session(session.as_str(), provider_session_id);
        if facts.ephemeral {
            return;
        }
        app.remember_session(wtm_config::SessionRecord {
            provider: facts.provider,
            worktree: facts.worktree,
            provider_session: provider_session_id.clone(),
            // The first turn can be submitted before the handshake. The live entry caches its
            // label so SessionReady does not turn that ordinary race into “Untitled session”.
            title: app.session_title_of(session.as_str()),
            model: model.clone(),
            effort: effort.clone(),
            updated: Some(app.clock.now_iso()),
            extra: std::collections::BTreeMap::new(),
        });
    }

    /// Give a remembered session a label, from the first thing the user said to it.
    ///
    /// A session's own id is not something anyone recognises in a list, so the first prompt is the
    /// label. Truncated, because a prompt can be a whole stack trace.
    fn title(&self, session: &SessionId, event: &AgentEvent) {
        let AgentEvent::UserEcho { text } = event else {
            return;
        };
        let Some(app) = &self.app else { return };
        let Some(facts) = app
            .live_agents()
            .into_iter()
            .find(|f| f.session == session.as_str())
        else {
            return;
        };
        if facts.ephemeral {
            return;
        }
        let mut label: String = text.chars().take(72).collect();
        if text.chars().count() > 72 {
            label.push('…');
        }
        app.title_live_session(session.as_str(), &label);
    }
}

impl AgentSink for AgentEventSink {
    fn on_event(&self, session: &SessionId, event: &AgentEvent) {
        // Before the emit, so a slow write cannot delay what the user sees.
        self.remember(session, event);
        self.title(session, event);

        // Buffered before it is emitted, and numbered by the same call, so the window can never
        // see an event the buffer does not have. The other order would leave a hole exactly the
        // size of a reload that landed between the two.
        let seq = self
            .app
            .as_ref()
            .and_then(|app| app.record_agent_event(session.as_str(), event));

        let payload = AgentEventPayload {
            session: session.as_str().to_owned(),
            seq,
            event,
        };
        // A failed emit means the window is gone. Nothing useful to do, and it must not interrupt
        // the reader thread — the same judgement `pty_bridge` makes.
        if let Err(err) = self.handle.emit(AGENT_EVENT, payload) {
            tracing::debug!(error = %err, "could not emit an agent event");
        }
    }

    fn on_exit(&self, session: &SessionId, outcome: &ExitOutcome) {
        let payload = AgentExitPayload {
            session: session.as_str().to_owned(),
            outcome: outcome.clone(),
            summary: outcome.describe(),
        };
        if let Err(err) = self.handle.emit(AGENT_EXIT_EVENT, payload) {
            tracing::debug!(error = %err, "could not emit an agent exit");
        }
    }

    fn on_ready(&self, session: &SessionId) {
        let payload = AgentReadyPayload {
            session: session.as_str().to_owned(),
        };
        if let Err(err) = self.handle.emit(AGENT_READY_EVENT, payload) {
            tracing::debug!(error = %err, "could not emit agent readiness");
        }
    }
}
