//! Where the engine's role sessions come from: the real CLIs behind the run's rail, or a test's
//! stand-ins.
//!
//! # Why a seam here and nowhere else
//!
//! Everything the engine decides — which role runs, when it has timed out, what a limit means,
//! whether a submission counts — is a decision about events. The one thing it cannot do without a
//! window is open a session, because a run session is announced to the window and drawn behind its
//! rail. So opening, sending and closing are this trait, and everything a session says comes back
//! through the sink the engine hands [`RoleHost::open`]. A test's fake agent implements the trait,
//! emits events into that sink and calls the engine's tools, and the engine cannot tell it from a
//! CLI. That is what lets timeouts, retries, limits and a quit be tested with full-sentence tests
//! that take milliseconds, and leaves the real CLIs to one real test.
//!
//! # The sink is the session's own
//!
//! The engine's sink is attached when the session opens, not looked up by session id afterwards.
//! A CLI starts talking before `open` returns its id — the skills a role is checked against can
//! arrive in that window — and a registry keyed by the id would drop what it said first.

use std::sync::Arc;

use tauri::Manager;
use wtm_agent::session::AgentSink;
use wtm_core::model::{AgentEvent, ExitOutcome, SessionId};

use crate::app::App;
use crate::flow_session::RoleSession;

/// Opens, drives and closes a run's role sessions.
pub trait RoleHost: Send + Sync {
    /// Refuse a run whose providers would not bill a subscription (F3), before anything is
    /// recorded or opened.
    ///
    /// # Errors
    ///
    /// The refusal, naming the provider and how it is signed in.
    fn preflight(&self, providers: &[&str]) -> Result<(), String>;

    /// Open `spec`'s session, with `sink` hearing everything it says from its first event.
    ///
    /// # Errors
    ///
    /// Every reason it did not open, in a sentence: a refused preflight, a failed canary, a CLI
    /// that would not start. Nothing is left running.
    fn open(&self, spec: &RoleSession, sink: Arc<dyn AgentSink>) -> Result<SessionId, String>;

    /// Send the session a turn.
    ///
    /// # Errors
    ///
    /// When the session will not take one, typically because its process is gone.
    fn send(&self, session: &SessionId, text: &str) -> Result<(), String>;

    /// End the session and its process. Idempotent.
    fn close(&self, session: &SessionId);
}

/// The real host: [`crate::flow_session::open`], headless behind the run's rail.
pub struct TauriHost {
    handle: tauri::AppHandle,
}

impl std::fmt::Debug for TauriHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TauriHost").finish_non_exhaustive()
    }
}

impl TauriHost {
    #[must_use]
    pub fn new(handle: tauri::AppHandle) -> Self {
        Self { handle }
    }

    fn app(&self) -> Result<Arc<App>, String> {
        self.handle
            .try_state::<Arc<App>>()
            .map(|app| Arc::clone(&app))
            .ok_or_else(|| "the app is shutting down".to_owned())
    }
}

impl RoleHost for TauriHost {
    fn preflight(&self, providers: &[&str]) -> Result<(), String> {
        crate::run_profile::preflight(&*self.app()?, providers).map(|_| ())
    }

    fn open(&self, spec: &RoleSession, sink: Arc<dyn AgentSink>) -> Result<SessionId, String> {
        crate::flow_session::open_tapped(&self.handle, &self.app()?, spec, Some(sink))
    }

    fn send(&self, session: &SessionId, text: &str) -> Result<(), String> {
        self.app()?
            .send_agent_turn(session.as_str(), text, &[])
            .map_err(|e| e.to_string())
    }

    fn close(&self, session: &SessionId) {
        if let Ok(app) = self.app() {
            app.close_agent(session.as_str());
            crate::agent_bridge::announce_released(&self.handle, &[session.as_str().to_owned()]);
        }
    }
}

/// The real CLIs, profiles, canaries and bridge, with no window: a run nobody is watching at all.
///
/// The app uses [`TauriHost`], which is this plus the pane behind the run's rail. This exists for
/// the real-CLI test, which has no window, and which needs the bridge it starts to be the built
/// `wtm` and the socket it calls to be the test's own rather than the one a running wtm serves.
pub struct HeadlessHost {
    app: Arc<App>,
    endpoint: crate::flow_session::BridgeEndpoint,
}

impl std::fmt::Debug for HeadlessHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HeadlessHost")
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

impl HeadlessHost {
    #[must_use]
    pub fn new(app: Arc<App>, endpoint: crate::flow_session::BridgeEndpoint) -> Self {
        Self { app, endpoint }
    }
}

impl RoleHost for HeadlessHost {
    fn preflight(&self, providers: &[&str]) -> Result<(), String> {
        crate::run_profile::preflight(&self.app, providers).map(|_| ())
    }

    fn open(&self, spec: &RoleSession, sink: Arc<dyn AgentSink>) -> Result<SessionId, String> {
        crate::flow_session::open_headless(&self.app, spec, &sink, &self.endpoint)
            .map(|opened| opened.session)
    }

    fn send(&self, session: &SessionId, text: &str) -> Result<(), String> {
        self.app
            .send_agent_turn(session.as_str(), text, &[])
            .map_err(|e| e.to_string())
    }

    fn close(&self, session: &SessionId) {
        self.app.close_agent(session.as_str());
    }
}

/// Two sinks as one: the window's, which draws the pane and settles its turns, and the engine's.
///
/// The window's goes first, so a turn's last event is on screen before the engine reacts to it by
/// closing the session.
pub struct Tee {
    pub first: Arc<dyn AgentSink>,
    pub second: Arc<dyn AgentSink>,
}

impl std::fmt::Debug for Tee {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tee").finish_non_exhaustive()
    }
}

impl AgentSink for Tee {
    fn on_event(&self, session: &SessionId, event: &AgentEvent) {
        self.first.on_event(session, event);
        self.second.on_event(session, event);
    }

    fn on_exit(&self, session: &SessionId, outcome: &ExitOutcome) {
        self.first.on_exit(session, outcome);
        self.second.on_exit(session, outcome);
    }

    fn on_ready(&self, session: &SessionId) {
        self.first.on_ready(session);
        self.second.on_ready(session);
    }
}
