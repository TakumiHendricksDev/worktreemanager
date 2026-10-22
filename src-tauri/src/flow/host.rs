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

use serde::Serialize;
use tauri::Manager;
use wtm_agent::session::AgentSink;
use wtm_core::flow::{LimitReading, RoleProvider, limit_reading};
use wtm_core::model::{AgentEvent, ExitOutcome, SessionId};
use wtm_core::ports::PtySink;

use super::Refusal;
use crate::app::App;
use crate::flow_session::RoleSession;

/// Both real hosts' preflight: F3's, with the providers it refused over billing kept apart.
fn checked(app: &App, providers: &[&str]) -> Result<(), Refusal> {
    crate::run_profile::preflight_checked(app, providers)
        .map(|_| ())
        .map_err(|refused| Refusal {
            message: refused.message,
            billing: refused.billing,
            credits: Vec::new(),
        })
}

/// Both real hosts' recheck: ask the provider now, through the same record the usage view shows,
/// and read it for `model`. A failed ask is unknown, not the last figures: those were read before
/// whatever made the recheck necessary.
fn reading(
    app: &Arc<App>,
    handle: Option<&tauri::AppHandle>,
    provider: RoleProvider,
    model: Option<&str>,
) -> LimitReading {
    let account = crate::usage::recheck(app, handle, provider.id());
    if account.error.is_some() {
        return LimitReading::Unknown;
    }
    limit_reading(&account.limits, model, app.clock.now_unix_ms() / 1000)
}

/// Opens, drives and closes a run's role sessions.
pub trait RoleHost: Send + Sync {
    /// Refuse a run whose providers would not bill a subscription (F3), before anything is
    /// recorded or opened.
    ///
    /// # Errors
    ///
    /// The refusal, naming the provider and how it is signed in.
    fn preflight(&self, providers: &[&str]) -> Result<(), Refusal>;

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

    /// Stop the turn the session is running, as the CLI's own interrupt does: the provider ends the
    /// turn and keeps the conversation. The planner's `flow_ask` uses it to end the turn itself.
    ///
    /// # Errors
    ///
    /// When the session cannot be interrupted; the engine closes it instead.
    fn interrupt(&self, session: &SessionId) -> Result<(), String> {
        Err(format!("{session} cannot be interrupted by this host"))
    }

    /// What `provider`'s own figures say now about running `model`, asked fresh: after the Mac
    /// slept through a wait at a limit, or when a wait is picked up at launch. A host that cannot
    /// ask says it does not know, which is never read as room.
    fn limits(&self, provider: RoleProvider, model: Option<&str>) -> LimitReading {
        let _ = (provider, model);
        LimitReading::Unknown
    }

    /// Where the setup of a worktree Start Work makes sends its terminal: kept for a window to
    /// attach to, by the real host. A test's host, which has no window, drops it.
    fn setup_sink(&self) -> Arc<dyn PtySink> {
        Arc::new(Unseen)
    }

    /// Open the browser pane a runtime role drives, on `url`, which the environment has just
    /// verified, and wait for its first load. A host with no window has no pane to give.
    ///
    /// # Errors
    ///
    /// Why it did not open, in a sentence.
    fn open_runtime_browser(
        &self,
        project: &str,
        worktree: &str,
        url: &str,
    ) -> Result<RuntimePane, String> {
        let _ = (project, worktree, url);
        Err("this run has no window to open a browser pane in".to_owned())
    }

    /// Close a runtime role's pane, which drops its store. Idempotent.
    fn close_runtime_browser(&self, pane: &str) {
        let _ = pane;
    }

    /// A PNG of a runtime pane's page, and its address at that moment.
    ///
    /// # Errors
    ///
    /// When the pane is gone or the picture could not be taken.
    fn screenshot(&self, pane: &str) -> Result<(Vec<u8>, String), String> {
        let _ = pane;
        Err("this run has no window to take a screenshot in".to_owned())
    }
}

/// A terminal nobody is shown.
struct Unseen;

impl PtySink for Unseen {
    fn on_output(&self, _session: &SessionId, _chunk: &[u8]) {}
    fn on_exit(&self, _session: &SessionId, _outcome: &ExitOutcome) {}
}

/// Where a browser pane keeps its cookies and storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneStore {
    /// In memory, made for this pane and dropped with it. What a runtime role always gets.
    Incognito,
    /// The jar every user-opened pane shares across launches, where your sign-ins live.
    Persistent,
}

/// The pane a runtime role was given.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimePane {
    /// The browser's id, which the role's token is scoped to.
    pub id: String,
    /// The verified preview it opened on.
    pub url: String,
    /// The store it was built on, as the host built it. The engine refuses anything but
    /// [`PaneStore::Incognito`].
    pub store: PaneStore,
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
    fn preflight(&self, providers: &[&str]) -> Result<(), Refusal> {
        checked(&*self.app()?, providers)
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

    fn interrupt(&self, session: &SessionId) -> Result<(), String> {
        self.app()?
            .with_agent(session.as_str(), wtm_agent::AgentSession::interrupt)
            .map_err(|e| e.to_string())
    }

    fn limits(&self, provider: RoleProvider, model: Option<&str>) -> LimitReading {
        match self.app() {
            Ok(app) => reading(&app, Some(&self.handle), provider, model),
            Err(_) => LimitReading::Unknown,
        }
    }

    /// Recorded, as a Home job's setup is, so the issue workspace can attach to the terminal
    /// after the setup started and still show what it printed first.
    fn setup_sink(&self) -> Arc<dyn PtySink> {
        match self.app() {
            Ok(app) => crate::pty_bridge::EventSink::recording(self.handle.clone(), app),
            Err(_) => Arc::new(Unseen),
        }
    }

    fn open_runtime_browser(
        &self,
        project: &str,
        worktree: &str,
        url: &str,
    ) -> Result<RuntimePane, String> {
        let app = self.app()?;
        let view = crate::browser::open_runtime(&self.handle, &app, project, worktree, url)
            .map_err(|e| e.message)?;
        crate::browser::wait_for_load(&app, &view.id, crate::browser::LOAD_TIMEOUT);
        if let Some(error) = app.browsers.view_of(&view.id).and_then(|v| v.load_error) {
            crate::browser::close(&self.handle, &app, &view.id);
            return Err(format!("the preview did not load in the pane: {error}"));
        }
        Ok(RuntimePane {
            id: view.id,
            url: url.to_owned(),
            store: crate::browser::store_for(crate::browser::PaneKind::Runtime),
        })
    }

    fn close_runtime_browser(&self, pane: &str) {
        if let Ok(app) = self.app() {
            crate::browser::close(&self.handle, &app, pane);
        }
    }

    fn screenshot(&self, pane: &str) -> Result<(Vec<u8>, String), String> {
        let app = self.app()?;
        let png =
            crate::browser::snapshot_png(&self.handle, &app, pane, None).map_err(|e| e.message)?;
        let url = app
            .browsers
            .view_of(pane)
            .map(|v| v.url)
            .unwrap_or_default();
        Ok((png, url))
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
    fn preflight(&self, providers: &[&str]) -> Result<(), Refusal> {
        checked(&self.app, providers)
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

    fn interrupt(&self, session: &SessionId) -> Result<(), String> {
        self.app
            .with_agent(session.as_str(), wtm_agent::AgentSession::interrupt)
            .map_err(|e| e.to_string())
    }

    fn limits(&self, provider: RoleProvider, model: Option<&str>) -> LimitReading {
        reading(&self.app, None, provider, model)
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
