//! Native renderers use the host's identity, trust and process ownership, while the provider
//! retains its terminal and permission UI. Delivery receipts survive windows and app restarts.

pub mod delivery;
pub mod hooks;
mod transport;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, Weak, mpsc};
use std::time::Duration;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::Emitter;
use wtm_agent::Provider;
use wtm_agent::native::{CodexObserver, Status, Update};
use wtm_core::model::{AgendaStep, AgentEvent, SessionId, Usage};
use wtm_core::ports::{exec::Invocation, pipe::PipeHost};

use crate::app::{AgentSessionFacts, App, SessionScope};
use crate::commands::{AppState, Reply, SessionOptions, blocking};
use crate::view::ErrorView;
use delivery::{Delivery, DeliveryStatus, Owner, Resolution};

pub const EVENT: &str = "native:changed";
pub const ENV: &str = "WTM_NATIVE_SESSION";
pub const HOOK_FLAG: &str = "--native-agent-hook";

/// How long a WTM request to the provider is given before its delivery is called uncertain.
pub const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

/// Plugin events waiting for the dispatcher. Past this a plugin is not reading, and dropping its
/// events is better than letting it hold up the provider's reader thread.
const PLUGIN_BACKLOG: usize = 1024;

const UNREGISTERED: &str = "Native session is no longer registered";
const NOTHING_UNCERTAIN: &str = "No delivery in this conversation is uncertain.";
const CLAUDE_HISTORY: &str = "wtm cannot read Claude's conversation history. Check the conversation yourself, and mark the instruction as not delivered if it never arrived.";
const NOT_CONNECTED: &str = "Reconnect the native session before checking delivery.";
const STILL_UNKNOWN: &str = "The provider's history does not show this instruction. Missing history does not prove it was not delivered, so it stays uncertain.";
const UNCONFIRMED: &str =
    "Delivery is unconfirmed. Check the native conversation; wtm will not retry.";
pub(crate) const CLAUDE_STOP: &str =
    "Design cancelled. Stop the current turn in the native CLI if it is still working.";
const NO_INTEGRATION: &str = "This CLI version does not expose the required native integration. Update it or open a plain terminal.";
const EXITED_EARLY: &str = "The native CLI exited before its connection was established. Open its session details or try a plain terminal.";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
// Capabilities are independent provider facts, not mutually exclusive lifecycle states.
#[allow(clippy::struct_excessive_bools)]
pub struct Capabilities {
    pub send: bool,
    pub stop: bool,
    pub plans: bool,
    pub usage: bool,
    pub permission_owner: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Receipt {
    pub id: String,
    pub label: String,
    pub status: DeliveryStatus,
    pub error: Option<String>,
    pub prototype: Option<String>,
    pub review: Option<String>,
    /// Set when the user, not the provider, settled it. See [`Resolution`].
    pub resolution: Option<Resolution>,
    /// The provider conversation it was sent to.
    pub conversation: String,
    /// An unresolved receipt from a conversation this pane has since left, through `/clear` or
    /// `/new`. Listed so an uncertain delivery stays visible after the switch, as promised.
    pub earlier: bool,
}

impl Receipt {
    fn of(d: Delivery, current: &str) -> Self {
        Self {
            earlier: d.owner.conversation != current,
            conversation: d.owner.conversation,
            id: d.id,
            label: d.label,
            status: d.status,
            error: d.error,
            prototype: d.prototype,
            review: d.review,
            resolution: d.resolution,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub session: String,
    pub project: String,
    pub worktree: String,
    pub provider: String,
    pub conversation: String,
    pub status: Status,
    pub version: String,
    pub context_supplied: bool,
    pub tools_connected: bool,
    pub tools: Vec<String>,
    pub capabilities: Capabilities,
    pub title: Option<String>,
    pub model: Option<String>,
    pub plan: Vec<AgendaStep>,
    pub activity: Vec<String>,
    pub usage: Option<Usage>,
    pub deliveries: Vec<Receipt>,
    pub error: Option<String>,
    pub seq: u64,
}

impl Snapshot {
    pub fn owner(&self) -> Owner {
        Owner {
            project: self.project.clone(),
            worktree: self.worktree.clone(),
            provider: self.provider.clone(),
            conversation: self.conversation.clone(),
        }
    }
    pub fn facts(&self) -> AgentSessionFacts {
        AgentSessionFacts {
            session: self.session.clone(),
            scope: SessionScope::worktree(&self.project, &self.worktree),
            provider: self.provider.clone(),
            provider_session: self.conversation.clone(),
            ephemeral: false,
        }
    }
}

/// What "Check delivery" found, so the button can say so instead of silently doing nothing.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryCheck {
    /// The provider's history was actually read.
    pub checked: bool,
    /// At least one receipt was confirmed delivered by it.
    pub changed: bool,
    /// Why nothing was checked, or why a check left a receipt uncertain.
    pub reason: Option<String>,
    pub session: Snapshot,
}

/// Where a session reports its changes.
///
/// A seam rather than a `tauri::AppHandle` held by every session, for two reasons. A test binary
/// cannot construct a handle, so the session's rules could not be tested at all; and plugin
/// delivery writes to another process's stdin, which must never happen on the thread reading the
/// provider. [`HostEvents`] hands plugin events to one dispatcher thread.
pub trait Events: Send + Sync {
    /// The session's new state, for the webview.
    fn changed(&self, snapshot: &Snapshot);
    /// One plugin event. Must return without waiting for any plugin.
    fn plugin(&self, name: &'static str, payload: Value, worktree: String);
}

struct PluginEvent {
    name: &'static str,
    payload: Value,
    worktree: String,
}

struct HostEvents {
    handle: tauri::AppHandle,
    plugins: mpsc::SyncSender<PluginEvent>,
}

impl HostEvents {
    fn new(handle: tauri::AppHandle, app: Weak<App>) -> Self {
        let (plugins, events) = mpsc::sync_channel::<PluginEvent>(PLUGIN_BACKLOG);
        let dispatcher = handle.clone();
        let spawned = std::thread::Builder::new()
            .name("wtm-native-plugins".into())
            .spawn(move || {
                for event in events {
                    let Some(app) = app.upgrade() else { return };
                    crate::extensions::Runtime::event(
                        &dispatcher,
                        &app,
                        event.name,
                        &event.payload,
                        &event.worktree,
                    );
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "native plugin events have no dispatcher");
        }
        Self { handle, plugins }
    }
}

impl Events for HostEvents {
    fn changed(&self, snapshot: &Snapshot) {
        let _ = self.handle.emit(EVENT, snapshot);
    }
    fn plugin(&self, name: &'static str, payload: Value, worktree: String) {
        let event = PluginEvent {
            name,
            payload,
            worktree,
        };
        if self.plugins.try_send(event).is_err() {
            tracing::warn!(
                event = name,
                "a plugin is not keeping up; native event dropped"
            );
        }
    }
}

pub struct Hub {
    sessions: Mutex<BTreeMap<String, Arc<Session>>>,
    pub deliveries: delivery::Store,
    events: OnceLock<Arc<dyn Events>>,
}

impl std::fmt::Debug for Hub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeHub")
            .field("deliveries", &self.deliveries)
            .finish_non_exhaustive()
    }
}

impl Hub {
    pub fn new(root: PathBuf) -> Self {
        Self {
            sessions: Mutex::new(BTreeMap::new()),
            deliveries: delivery::Store::new(root),
            events: OnceLock::new(),
        }
    }
    pub fn get(&self, id: &str) -> Option<Arc<Session>> {
        self.sessions
            .lock()
            .values()
            .find(|s| s.state.lock().session == id)
            .cloned()
    }
    pub fn by_token(&self, token: &str) -> Option<Arc<Session>> {
        self.sessions.lock().get(token).cloned()
    }
    /// Snapshots are taken after the registry lock is released, so a slow one cannot hold up
    /// every other lookup.
    pub fn list(&self) -> Vec<Snapshot> {
        let sessions = self.sessions.lock().values().cloned().collect::<Vec<_>>();
        sessions
            .iter()
            .map(|s| s.snapshot())
            .filter(|s| !s.session.is_empty())
            .collect()
    }
    /// Make a session reachable by its token: from its hooks, its tools and the listings.
    pub fn register(&self, session: Arc<Session>) {
        self.sessions.lock().insert(session.token.clone(), session);
    }
    pub fn close(&self, id: &str) {
        let session = {
            let mut sessions = self.sessions.lock();
            let token = sessions
                .iter()
                .find(|(_, s)| s.state.lock().session == id)
                .map(|(t, _)| t.clone());
            token.and_then(|t| sessions.remove(&t))
        };
        // Outside the registry lock: shutdown removes the session from it too.
        if let Some(session) = session {
            session.shutdown();
        }
    }
    fn forget(&self, token: &str) {
        self.sessions.lock().remove(token);
    }
    fn events(&self, handle: &tauri::AppHandle, app: &Arc<App>) -> Arc<dyn Events> {
        Arc::clone(
            self.events
                .get_or_init(|| Arc::new(HostEvents::new(handle.clone(), Arc::downgrade(app)))),
        )
    }
}

#[derive(Debug)]
struct Pending {
    reply: mpsc::SyncSender<Result<Value, String>>,
    delivery: Option<Delivery>,
}

/// Everything a [`Session`] is made of, separate from launching a provider.
///
/// `open` fills it in from the resolved launch before spawning anything. Tests fill it in with a
/// fake pipe and a recording [`Events`], the only way to exercise the session's rules without a
/// webview.
pub struct Parts {
    /// The scoped MCP token, which is also how hooks find their session.
    pub token: String,
    pub project: String,
    pub worktree: String,
    pub provider: String,
    /// Empty until the provider names one, unless resuming.
    pub conversation: String,
    /// The worktree, canonical. Looked up once here so neither the relay nor a hook runs git.
    pub root: PathBuf,
    pub version: String,
    pub tools: Vec<String>,
    pub model: Option<String>,
    pub instructions: String,
    pub events: Arc<dyn Events>,
    pub pipe: Arc<dyn PipeHost>,
    pub reply_timeout: Duration,
    /// A private directory removed at shutdown. Only Codex needs one, for its relay socket.
    pub directory: Option<PathBuf>,
}

impl std::fmt::Debug for Parts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeParts")
            .field("provider", &self.provider)
            .field("worktree", &self.worktree)
            .finish_non_exhaustive()
    }
}

pub struct Session {
    app: Weak<App>,
    events: Arc<dyn Events>,
    host: Arc<dyn PipeHost>,
    token: String,
    root: PathBuf,
    state: Mutex<Snapshot>,
    /// Conversations this pane had before its current one, oldest first. See [`Receipt::earlier`].
    earlier: Mutex<Vec<String>>,
    instructions: String,
    pub closed: AtomicBool,
    /// Sticky: once the relay or backend is lost, nothing the provider reports afterwards can be
    /// what the user's terminal shows. Only a reconnect, which is a new session, clears it.
    disconnected: AtomicBool,
    pipe: Mutex<Option<SessionId>>,
    relay: Mutex<transport::Relay>,
    observer: Mutex<CodexObserver>,
    pending: Mutex<BTreeMap<String, Pending>>,
    admission: Mutex<()>,
    reply_timeout: Duration,
    directory: Option<PathBuf>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeSession")
            .field("session", &self.state.lock().session)
            .finish_non_exhaustive()
    }
}

impl Session {
    pub fn new(app: &Arc<App>, parts: Parts) -> Arc<Self> {
        let codex = parts.provider == "codex";
        Arc::new(Self {
            app: Arc::downgrade(app),
            events: parts.events,
            host: parts.pipe,
            token: parts.token,
            root: parts.root,
            instructions: parts.instructions,
            closed: AtomicBool::new(false),
            disconnected: AtomicBool::new(false),
            pipe: Mutex::new(None),
            relay: Mutex::new(transport::Relay::default()),
            observer: Mutex::new(CodexObserver::default()),
            pending: Mutex::new(BTreeMap::new()),
            admission: Mutex::new(()),
            earlier: Mutex::new(Vec::new()),
            reply_timeout: parts.reply_timeout,
            directory: parts.directory,
            state: Mutex::new(Snapshot {
                session: String::new(),
                project: parts.project,
                worktree: parts.worktree,
                provider: parts.provider,
                conversation: parts.conversation,
                status: Status::Starting,
                version: parts.version,
                context_supplied: false,
                tools_connected: false,
                tools: parts.tools,
                capabilities: Capabilities {
                    send: codex,
                    stop: codex,
                    plans: codex,
                    usage: codex,
                    permission_owner: "native_cli".into(),
                },
                title: None,
                model: parts.model,
                plan: Vec::new(),
                activity: Vec::new(),
                usage: None,
                deliveries: Vec::new(),
                error: None,
                seq: 0,
            }),
        })
    }

    /// Name the session after the terminal that hosts it. Listings skip it until then.
    pub fn bind(&self, id: &str) {
        id.clone_into(&mut self.state.lock().session);
    }

    /// The app-server pipe WTM's own requests and the TUI's frames are written to.
    pub fn attach_backend(&self, id: SessionId) {
        *self.pipe.lock() = Some(id);
    }

    pub fn capabilities(&self) -> Capabilities {
        self.state.lock().capabilities.clone()
    }

    pub fn snapshot(&self) -> Snapshot {
        let mut state = self.state.lock().clone();
        let earlier = self.earlier.lock().clone();
        if let Some(app) = self.app.upgrade() {
            match app
                .native_sessions
                .deliveries
                .receipts(&state.owner(), &earlier)
            {
                Ok(found) => {
                    state.deliveries = found
                        .into_iter()
                        .map(|d| Receipt::of(d, &state.conversation))
                        .collect();
                }
                Err(error) => {
                    state.error.get_or_insert(error);
                }
            }
        }
        state
    }

    fn receipt(&self, delivery: Delivery) -> Receipt {
        let current = self.state.lock().conversation.clone();
        Receipt::of(delivery, &current)
    }

    fn announce(&self) {
        self.state.lock().seq += 1;
        let snapshot = self.snapshot();
        if !snapshot.session.is_empty() {
            self.events.changed(&snapshot);
            let worktree = snapshot.worktree.clone();
            self.events
                .plugin("native.session.changed", json!(snapshot), worktree);
        }
    }

    fn update(&self, updates: Vec<Update>) {
        if updates.is_empty() || self.closed.load(Ordering::Acquire) {
            return;
        }
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let disconnected = self.disconnected.load(Ordering::Acquire);
        for update in updates {
            let event = {
                let mut state = self.state.lock();
                if self.closed.load(Ordering::Acquire)
                    || state.status == Status::Ended
                        && !matches!(&update, Update::Event(AgentEvent::SessionReady { .. }))
                {
                    return;
                }
                state.seq += 1;
                match update {
                    Update::Status(_) if disconnected => None,
                    Update::Status(status) => {
                        state.status = status;
                        None
                    }
                    Update::Activity(line) => {
                        state.activity.push(line);
                        trim(&mut state.activity);
                        None
                    }
                    Update::Event(event) => {
                        self.apply(&app, &mut state, &event);
                        trim(&mut state.activity);
                        Some((state.clone(), event))
                    }
                }
            };
            if let Some((state, event)) = event
                && !state.conversation.is_empty()
                && !state.session.is_empty()
            {
                if matches!(
                    event,
                    AgentEvent::SessionReady { .. } | AgentEvent::UserEcho { .. }
                ) {
                    app.remember_session(wtm_config::SessionRecord {
                        provider: state.provider.clone(),
                        worktree: state.worktree.clone(),
                        provider_session: state.conversation.clone(),
                        title: state.title.clone(),
                        model: state.model.clone(),
                        effort: None,
                        updated: Some(app.clock.now_iso()),
                        extra: BTreeMap::new(),
                    });
                }
                // The same skip as the WTM Chat sink: with no plugin runtime running, nothing
                // would receive the payload, so it is not built.
                if !app.extensions.idle() {
                    let payload = json!({
                        "session": state.session,
                        "seq": state.seq,
                        "event": event,
                        "projectId": state.project,
                        "worktreeId": state.worktree,
                        "renderer": "native",
                    });
                    self.events.plugin("chat.event", payload, state.worktree);
                }
            }
        }
        self.announce();
    }

    fn apply(&self, app: &App, state: &mut Snapshot, event: &AgentEvent) {
        match event {
            AgentEvent::SessionReady {
                provider_session_id,
                model,
                mode,
                effort,
                ..
            } => {
                state.status = Status::Starting;
                if state.conversation != *provider_session_id {
                    // Its unresolved receipts stay listed; see `Receipt::earlier`.
                    let mut earlier = self.earlier.lock();
                    if !state.conversation.is_empty() && !earlier.contains(&state.conversation) {
                        earlier.push(state.conversation.clone());
                    }
                    state.title = None;
                    state.plan.clear();
                    state.activity.clear();
                    state.usage = None;
                    state.error = None;
                }
                state.conversation.clone_from(provider_session_id);
                state.model.clone_from(model);
                state.context_supplied = true;
                if let Some(effort) = effort {
                    app.handoff.update_effort(&state.session, effort);
                }
                if let Some(mode) = mode {
                    app.handoff.update_mode(&state.session, mode);
                }
            }
            AgentEvent::ModeChanged { mode } => {
                app.handoff.update_mode(&state.session, mode);
            }
            AgentEvent::UserEcho { text } | AgentEvent::MessageIdentity { text, .. } => {
                if state.title.is_none() {
                    state.title = Some(
                        text.lines()
                            .next()
                            .unwrap_or_default()
                            .chars()
                            .take(100)
                            .collect(),
                    );
                }
            }
            AgentEvent::AgendaUpdated { steps, .. } => state.plan.clone_from(steps),
            AgentEvent::Usage(usage) => state.usage = Some(*usage),
            AgentEvent::ToolStarted { name, .. } => {
                state.activity.push(format!("Using {name}"));
            }
            AgentEvent::ToolFinished { ok, .. } => state
                .activity
                .push(if *ok { "Tool finished" } else { "Tool failed" }.into()),
            AgentEvent::CommandStarted { .. } => {
                state.activity.push("Running a command".into());
            }
            AgentEvent::CommandFinished { exit_code, .. } => {
                state.activity.push(format!(
                    "Command finished{}",
                    exit_code.map_or_else(String::new, |c| format!(" · exit {c}"))
                ));
            }
            AgentEvent::TurnStarted { .. } => {
                state.activity.push("Turn started".into());
            }
            AgentEvent::TurnFinished { .. } => {
                state.activity.push("Turn finished".into());
            }
            AgentEvent::Failed { message } => state.error = Some(message.clone()),
            _ => {}
        }
    }

    fn write(&self, frame: &Value) -> Result<(), String> {
        let id = self
            .pipe
            .lock()
            .clone()
            .ok_or("The native backend is not connected")?;
        self.host
            .write_line(&id, &frame.to_string())
            .map_err(|e| e.to_string())
    }

    pub fn client_message(&self, mut frame: Value) -> Result<(), String> {
        let _guard = self.admission.lock();
        if matches!(
            frame["method"].as_str(),
            Some("thread/start" | "thread/resume" | "thread/fork")
        ) {
            // A session token grants tools in one worktree. Changing directories needs a new pane.
            frame["params"]["cwd"] = json!(self.root);
            self.update(vec![Update::Status(Status::Starting)]);
        }
        self.observer.lock().client(&mut frame, &self.instructions);
        if frame["method"] == "turn/start"
            && frame["params"]["threadId"].as_str() == Some(&self.state.lock().conversation)
        {
            self.update(vec![Update::Status(Status::Working)]);
        }
        self.write(&frame)
    }

    /// True means a reply belongs to a WTM control request, so the TUI must not receive it.
    pub fn server_message(&self, frame: &Value) -> bool {
        // Only a frame without `method` is a reply: a provider request is never WTM's answer,
        // whatever id the provider happened to give it.
        let ours = frame
            .get("method")
            .is_none()
            .then(|| frame.get("id").and_then(Value::as_str))
            .flatten()
            .and_then(|id| self.pending.lock().remove(id));
        if let Some(pending) = ours {
            let result = if let Some(error) = frame.get("error") {
                Err(error["message"]
                    .as_str()
                    .unwrap_or("Provider rejected the request")
                    .to_owned())
            } else {
                Ok(frame["result"].clone())
            };
            if let Some(receipt) = &pending.delivery {
                self.settle(receipt, &result);
            }
            let _ = pending.reply.send(result);
            return true;
        }
        let updates = self.observer.lock().server(frame);
        self.update(updates);
        false
    }

    /// The provider's correlated answer to WTM's own `turn/start`, however late it arrives.
    fn settle(&self, receipt: &Delivery, result: &Result<Value, String>) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        let store = &app.native_sessions.deliveries;
        let settled = match result {
            Ok(_) => store.confirm(&receipt.id, &receipt.owner),
            Err(error) => store
                .transition(
                    &receipt.id,
                    &receipt.owner,
                    DeliveryStatus::Unknown,
                    DeliveryStatus::Rejected,
                    Some(error.clone()),
                )
                // The user already said it never arrived; the provider agrees.
                .or_else(|e| match store.read(&receipt.id) {
                    Ok(d) if d.status == DeliveryStatus::Rejected => Ok(d),
                    _ => Err(e),
                }),
        };
        self.state.lock().error = settled.err();
        if result.is_err() {
            self.update(vec![Update::Status(Status::Idle)]);
        }
        self.announce();
    }

    fn begin_request(
        &self,
        method: &str,
        params: &Value,
        delivery: Option<Delivery>,
    ) -> Result<mpsc::Receiver<Result<Value, String>>, String> {
        let id = format!("wtm-{}", uuid::Uuid::new_v4());
        let (tx, rx) = mpsc::sync_channel(1);
        {
            let mut pending = self.pending.lock();
            if pending.len() >= 32 {
                return Err(
                    "Too many unconfirmed native requests; reconnect before continuing".into(),
                );
            }
            pending.insert(
                id.clone(),
                Pending {
                    reply: tx,
                    delivery,
                },
            );
        }
        if let Err(error) = self.write(&json!({"id":id,"method":method,"params":params})) {
            self.pending.lock().remove(&id);
            return Err(format!(
                "Delivery is uncertain: {error}. wtm will not retry."
            ));
        }
        Ok(rx)
    }

    fn wait_reply(&self, rx: &mpsc::Receiver<Result<Value, String>>) -> Result<Value, String> {
        rx.recv_timeout(self.reply_timeout)
            .map_err(|_| UNCONFIRMED.to_owned())?
    }

    pub fn send(
        &self,
        label: &str,
        text: &str,
        review: Option<(&str, &str)>,
    ) -> Result<Receipt, String> {
        let app = self.app.upgrade().ok_or("wtm is closing")?;
        let store = &app.native_sessions.deliveries;
        let (delivery, request) = {
            let _guard = self.admission.lock();
            let state = self.state.lock().clone();
            let owner = state.owner();
            // An approval already on its way answers with what became of it, busy or not: the
            // idle gate below is about starting something, and this starts nothing. A pending
            // Codex receipt was never dispatched, so it still goes through the gate.
            if let Some((prototype, revision)) = review
                && let Some(existing) = store.existing(&owner, prototype, revision)?
                && (existing.status != DeliveryStatus::Pending || !state.capabilities.send)
            {
                return Ok(Receipt::of(existing, &state.conversation));
            }
            if self.closed.load(Ordering::Acquire) || state.status != Status::Idle {
                return Err("Wait until this native conversation is connected and idle".into());
            }
            let delivery = store.prepare(owner.clone(), label, text, review)?;
            if delivery.status != DeliveryStatus::Pending {
                return Ok(Receipt::of(delivery, &state.conversation));
            }
            let request = if state.capabilities.send {
                store.transition(
                    &delivery.id,
                    &owner,
                    DeliveryStatus::Pending,
                    DeliveryStatus::Unknown,
                    None,
                )?;
                self.state.lock().status = Status::Working;
                // Reserve and write while native input is serialized; waiting must release the gate.
                Some(self.begin_request(
                    "turn/start",
                    &turn_start(&delivery),
                    Some(delivery.clone()),
                ))
            } else {
                None
            };
            (delivery, request)
        };
        self.announce();
        if let Some(request) = request
            && let Err(error) = request.and_then(|rx| self.wait_reply(&rx))
        {
            self.state.lock().error = Some(error);
        }
        self.announce();
        store.read(&delivery.id).map(|d| self.receipt(d))
    }

    pub fn tools_connected(&self) {
        if !self.state.lock().tools_connected {
            self.state.lock().tools_connected = true;
            self.announce();
        }
    }

    /// Ask the provider's public history about every uncertain receipt this pane lists.
    ///
    /// Only Codex has such a history. Claude's answer is a reason, not an empty success: the
    /// button that asked has to be able to say that nothing was, or could be, checked.
    pub fn check_delivery(&self) -> Result<DeliveryCheck, String> {
        let state = self.snapshot();
        let mut conversations = state
            .deliveries
            .iter()
            .filter(|d| d.status == DeliveryStatus::Unknown)
            .map(|d| d.conversation.clone())
            .collect::<Vec<_>>();
        conversations.sort();
        conversations.dedup();
        let unchecked = |reason: &str| DeliveryCheck {
            checked: false,
            changed: false,
            reason: Some(reason.to_owned()),
            session: self.snapshot(),
        };
        if conversations.is_empty() {
            return Ok(unchecked(NOTHING_UNCERTAIN));
        }
        if state.provider != "codex" {
            return Ok(unchecked(CLAUDE_HISTORY));
        }
        if !matches!(
            state.status,
            Status::Idle | Status::Working | Status::NeedsApproval
        ) || self.disconnected.load(Ordering::Acquire)
        {
            return Ok(unchecked(NOT_CONNECTED));
        }
        let app = self.app.upgrade().ok_or("wtm is closing")?;
        let mut changed = false;
        for conversation in conversations {
            let result = self
                .begin_request(
                    "thread/read",
                    &json!({"threadId": conversation, "includeTurns": true}),
                    None,
                )
                .and_then(|rx| self.wait_reply(&rx))?;
            let owner = Owner {
                conversation,
                ..state.owner()
            };
            changed |= app
                .native_sessions
                .deliveries
                .reconcile_codex(&owner, &result["thread"])?;
        }
        if changed {
            self.state.lock().error = None;
            self.announce();
        }
        Ok(DeliveryCheck {
            checked: true,
            changed,
            reason: (!changed).then(|| STILL_UNKNOWN.to_owned()),
            session: self.snapshot(),
        })
    }

    /// The user's answer for an uncertain or unwanted receipt this pane lists. Replays nothing.
    pub fn resolve(&self, receipt: &str, resolution: Resolution) -> Result<Receipt, String> {
        let app = self.app.upgrade().ok_or("wtm is closing")?;
        let current = self.state.lock().owner();
        let earlier = self.earlier.lock().clone();
        let resolved = app.native_sessions.deliveries.resolve(
            receipt,
            |owner| {
                owner.project == current.project
                    && owner.worktree == current.worktree
                    && owner.provider == current.provider
                    && (owner.conversation == current.conversation
                        || earlier.contains(&owner.conversation))
            },
            resolution,
        )?;
        self.announce();
        Ok(self.receipt(resolved))
    }

    pub fn interrupt(&self) -> Result<(), String> {
        let (conversation, turn) = {
            let observer = self.observer.lock();
            (
                observer.conversation.clone(),
                observer
                    .turn
                    .clone()
                    .ok_or("No reported native turn is running; use the CLI's stop control")?,
            )
        };
        self.begin_request(
            "turn/interrupt",
            &json!({"threadId":conversation,"turnId":turn}),
            None,
        )
        .and_then(|rx| self.wait_reply(&rx))
        .map(|_| ())
    }

    /// The backend or the terminal's connection is gone. Hangs up the relay too, so the TUI
    /// says its connection was lost rather than waiting on frames that will not come.
    pub fn disconnect(&self, error: &str) {
        if self.closed.load(Ordering::Acquire) || self.disconnected.swap(true, Ordering::AcqRel) {
            return;
        }
        {
            let mut state = self.state.lock();
            state.error = Some(error.to_owned());
            state.status = Status::Disconnected;
        }
        self.relay.lock().hang_up();
        self.announce();
    }

    fn shutdown(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        self.state.lock().status = Status::Ended;
        self.pending.lock().clear();
        self.relay.lock().close();
        if let Some(id) = self.pipe.lock().take() {
            let _ = self.host.kill(&id);
        }
        if let Some(app) = self.app.upgrade() {
            let session = self.state.lock().session.clone();
            app.handoff.forget_session(&session);
            // An ended session is not a session: listings and lookups stop finding it here.
            app.native_sessions.forget(&self.token);
        }
        self.announce();
        if let Some(directory) = &self.directory {
            let _ = std::fs::remove_dir_all(directory);
        }
    }
}

fn trim(activity: &mut Vec<String>) {
    if activity.len() > 50 {
        activity.remove(0);
    }
}

/// WTM's own `turn/start` for a receipt.
///
/// `clientUserMessageId` is the receipt id so that a later `thread/read` can find this exact
/// message. It was captured working on `turn/steer` only (see `codex_mapping.rs`); no capture in
/// this repository shows `turn/start` honouring it. If Codex ignores it here, the correlated reply
/// still settles the receipt; only Check delivery after a lost reply can never match, which leaves
/// the receipt unknown — the safe direction, since unknown is never retried.
fn turn_start(delivery: &Delivery) -> Value {
    json!({
        "threadId": delivery.owner.conversation,
        "clientUserMessageId": delivery.id,
        "input": [{"type": "text", "text": delivery.text}],
    })
}

struct UnboundToken<'a> {
    app: &'a App,
    token: String,
}
impl Drop for UnboundToken<'_> {
    fn drop(&mut self) {
        self.app.handoff.forget_unbound(&self.token);
    }
}

fn failure(message: impl Into<String>) -> ErrorView {
    ErrorView::new("native", message)
}

pub fn open(
    handle: tauri::AppHandle,
    app: &Arc<App>,
    project_id: &str,
    worktree_id: &str,
    provider: &str,
    resume: Option<String>,
) -> Reply<String> {
    if !app.plugins.enabled("wtm.native-cli") {
        return Err(failure("Native CLI is disabled in Plugins settings"));
    }
    if !matches!(provider, "claude" | "codex") {
        return Err(failure("This native adapter supports Claude and Codex"));
    }
    let codex = provider == "codex";
    let project = app.project(project_id)?;
    if !project.offers_agent(provider) {
        return Err(failure(
            "This repository does not offer the selected provider",
        ));
    }
    let worktree = app.worktree(&project, worktree_id)?;
    if !app.files.exists(&worktree.path) {
        return Err(failure("The worktree directory no longer exists"));
    }
    let root = worktree
        .path
        .canonicalize()
        .map_err(|_| failure("The worktree directory no longer exists"))?;
    let entry = wtm_agent::entry(provider).ok_or_else(|| failure("Unknown provider"))?;
    let executable = app
        .agent_executable(entry)
        .ok_or_else(|| failure("Install the provider CLI and make it available on wtm's PATH"))?;
    let probe = |flag: &str| {
        app.runner
            .run(
                &Invocation::new(
                    vec![executable.to_string_lossy().into_owned(), flag.into()],
                    &worktree.path,
                    10_000,
                ),
                &wtm_core::ports::exec::CancelToken::new(),
            )
            .map(|out| out.stdout)
            .map_err(|e| failure(e.to_string()))
    };
    let version = probe("--version")?.trim().to_owned();
    let help = probe("--help")?;
    if codex && !help.contains("--remote") || !codex && !help.contains("--settings") {
        return Err(failure(NO_INTEGRATION));
    }
    let mut req = crate::commands::session_request_for(
        app,
        &project,
        &project.agent_spec(provider),
        entry,
        &worktree,
        Some(SessionOptions {
            resume,
            ..SessionOptions::default()
        }),
        None,
    )?;
    req.executable = Some(executable.to_string_lossy().into_owned());
    wtm_agent::native::validate_extra_args(provider, &req.extra_args).map_err(failure)?;
    let bridge = req
        .mcp
        .get_mut(crate::handoff::SERVER_NAME)
        .ok_or_else(|| failure("wtm's session tool bridge is unavailable"))?;
    // Only Claude receives receipts to acknowledge; Codex settles them from its own replies.
    if !codex {
        bridge.env.insert(ENV.into(), "on".into());
    }
    let token = bridge.env[crate::handoff::TOKEN_ENV].clone();
    // Claude runs its hooks as children of the CLI, and they reach WTM with this identity. No
    // other process needs it in its environment: each MCP server gets its own through its config.
    let hook_env = [crate::handoff::TOKEN_ENV, crate::handoff::SOCKET_ENV]
        .into_iter()
        .filter_map(|key| Some((key.to_owned(), bridge.env.get(key)?.clone())))
        .collect::<BTreeMap<_, _>>();
    let mut available_tools = vec!["Open Design".into(), "Code review comments".into()];
    if bridge
        .env
        .get(crate::handoff::BROWSER_TOOLS_ENV)
        .is_some_and(|v| v == "on")
    {
        available_tools.push("Browser panes".into());
    }
    if bridge
        .env
        .get(crate::handoff::AWARENESS_ENV)
        .is_some_and(|v| v == "on")
    {
        available_tools.push("Session awareness".into());
    }
    // Not gated on WTM Chat: delegation runs on the structured-session engine, which keeps
    // working with Chat turned off, so this would have hidden a tool the session does have.
    available_tools.push("Agent delegation".into());
    let _token_guard = UnboundToken {
        app,
        token: token.clone(),
    };
    let directory = if codex {
        let directory =
            std::env::temp_dir().join(format!("wtm-n-{}", uuid::Uuid::new_v4().simple()));
        delivery::private_dir(&directory, "The native session's private directory")
            .map_err(failure)?;
        Some(directory)
    } else {
        None
    };
    req.instructions = crate::commands::native_session_instructions(app, &project, provider);
    let instructions = format!(
        "{}\n\nYou are using the native {} terminal inside WTM. Permission prompts and interactive input stay in this CLI. The scoped WTM tools operate in {}. The initial Git branch is {}. Native transcript widgets remain provider-owned.",
        req.instructions.as_deref().unwrap_or_default(),
        provider,
        worktree.path.display(),
        worktree
            .branch()
            .map_or("detached HEAD", wtm_core::model::BranchRef::as_str)
    );
    req.instructions = Some(instructions.clone());
    let session = Session::new(
        app,
        Parts {
            token: token.clone(),
            project: project_id.into(),
            worktree: worktree_id.into(),
            provider: provider.into(),
            conversation: req.resume.clone().unwrap_or_default(),
            root,
            version,
            tools: available_tools,
            model: req.model.clone(),
            instructions,
            events: app.native_sessions.events(&handle, app),
            pipe: Arc::clone(&app.pipe) as Arc<dyn PipeHost>,
            reply_timeout: REPLY_TIMEOUT,
            directory: directory.clone(),
        },
    );
    app.native_sessions.register(Arc::clone(&session));
    // Here rather than at startup: this is already off the async runtime, and `Hub` is built
    // before the app has a clock to measure retention with.
    app.native_sessions
        .deliveries
        .prune(app.clock.now_unix_ms());
    let result = (|| {
        let argv = if let Some(directory) = &directory {
            let socket = directory.join("s");
            let (tx, rx) = mpsc::sync_channel(transport::FRAMES);
            // The same argv WTM Chat launches, extra_args included; see `codex_tui_argv`.
            let mut inv = Invocation::new(
                wtm_agent::codex::Codex.argv(&req),
                &worktree.path,
                crate::commands::SHELL_TIMEOUT_MS,
            );
            inv.env = req.env.clone();
            let backend = app
                .pipe
                .spawn(
                    &inv,
                    Some(worktree_id),
                    Arc::new(transport::Output {
                        session: Arc::clone(&session),
                        frames: tx,
                    }),
                )
                .map_err(|e| failure(e.to_string()))?;
            session.attach_backend(backend.session);
            transport::listen(&session, socket.clone(), rx).map_err(failure)?;
            wtm_agent::native::codex_tui_argv(&req, &socket)
        } else {
            wtm_agent::native::claude_argv(&req, hooks::settings()?)
        };
        let mut inv = Invocation::new(argv, &worktree.path, crate::commands::SHELL_TIMEOUT_MS);
        inv.env = req.env.clone();
        // This renderer is xterm even when the app inherited TERM=dumb from a GUI launcher. The
        // terminal alone gets it: the app-server is a JSON pipe, not something to colour.
        inv.env.insert("TERM".into(), "xterm-256color".into());
        if !codex {
            inv.env.extend(hook_env);
        }
        let sink = Arc::new(transport::TerminalOutput {
            session: Arc::clone(&session),
            downstream: crate::pty_bridge::EventSink::recording(handle, Arc::clone(app)),
        });
        let id = app
            .open_native_terminal(&worktree, project_id, inv, sink)
            .map_err(|e| failure(e.to_string()))?;
        session.bind(id.as_str());
        // Bound before checking, never after: a terminal that exits in between has already run
        // `shutdown`, whose `forget_session` could not see a token bound later, and that token
        // would then outlive its pane.
        app.handoff.bind_session(&token, id.as_str());
        if session.closed.load(Ordering::Acquire) {
            app.handoff.forget_session(id.as_str());
            app.close_shell(id.as_str());
            return Err(failure(EXITED_EARLY));
        }
        session.announce();
        Ok(id.as_str().to_owned())
    })();
    if result.is_err() {
        session.shutdown();
        app.handoff.forget_unbound(&token);
    }
    result
}

#[tauri::command]
pub async fn native_sessions(app: AppState<'_>) -> Reply<Vec<Snapshot>> {
    let app = Arc::clone(&app);
    blocking(move || Ok(app.native_sessions.list())).await
}

#[tauri::command]
pub async fn native_session(app: AppState<'_>, session: String) -> Reply<Snapshot> {
    let app = Arc::clone(&app);
    blocking(move || {
        app.native_sessions
            .get(&session)
            .map(|s| s.snapshot())
            .ok_or_else(|| failure(UNREGISTERED))
    })
    .await
}

/// "Check delivery". A provider round-trip, so it is its own command rather than a side effect of
/// reading a snapshot, which plugins and the UI do far more often.
#[tauri::command]
pub async fn check_native_delivery(app: AppState<'_>, session: String) -> Reply<DeliveryCheck> {
    let app = Arc::clone(&app);
    blocking(move || {
        app.native_sessions
            .get(&session)
            .ok_or_else(|| failure(UNREGISTERED))?
            .check_delivery()
            .map_err(failure)
    })
    .await
}

/// "It was not delivered" for an unknown receipt, or "withdraw" for a pending one. Either way
/// the receipt becomes rejected and nothing is sent; an explicit Implement may then try again.
#[tauri::command]
pub async fn resolve_native_delivery(
    handle: tauri::AppHandle,
    app: AppState<'_>,
    session: String,
    receipt: String,
    resolution: Resolution,
) -> Reply<Receipt> {
    let app = Arc::clone(&app);
    blocking(move || {
        let resolved = app
            .native_sessions
            .get(&session)
            .ok_or_else(|| failure(UNREGISTERED))?
            .resolve(&receipt, resolution)
            .map_err(failure)?;
        if let Some(prototype) = &resolved.prototype {
            crate::prototype_commands::refresh(&handle, &app, &session, prototype);
        }
        Ok(resolved)
    })
    .await
}

#[tauri::command]
pub async fn send_native_message(
    app: AppState<'_>,
    session: String,
    text: String,
) -> Reply<Receipt> {
    let app = Arc::clone(&app);
    blocking(move || {
        app.native_sessions
            .get(&session)
            .ok_or_else(|| failure(UNREGISTERED))?
            .send("WTM instruction", &text, None)
            .map_err(failure)
    })
    .await
}

#[tauri::command]
pub async fn interrupt_native_turn(app: AppState<'_>, session: String) -> Reply<()> {
    let app = Arc::clone(&app);
    blocking(move || {
        app.native_sessions
            .get(&session)
            .ok_or_else(|| failure(UNREGISTERED))?
            .interrupt()
            .map_err(failure)
    })
    .await
}

/// Whether `path` is inside the session's worktree, after resolving links on both sides.
fn within(path: &Path, root: &Path) -> Result<bool, String> {
    Ok(path
        .canonicalize()
        .map_err(|e| e.to_string())?
        .starts_with(root))
}

pub use hooks::{run_helper, serve_hook};
