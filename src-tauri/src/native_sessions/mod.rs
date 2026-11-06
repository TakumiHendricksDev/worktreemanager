//! Native renderers use the host's identity, trust and process ownership, while the provider
//! retains its terminal and permission UI. Delivery receipts survive windows and app restarts.

pub mod delivery;
pub mod hooks;
mod transport;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak, mpsc};
use std::time::Duration;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::Emitter;
use wtm_agent::native::{CodexObserver, Status, Update};
use wtm_core::model::{AgendaStep, AgentEvent, SessionId, Usage};
use wtm_core::ports::{exec::Invocation, pipe::PipeHost};

use crate::app::{AgentSessionFacts, App, SessionScope};
use crate::commands::{AppState, Reply, SessionOptions, blocking};
use crate::view::ErrorView;
use delivery::{Delivery, DeliveryStatus, Owner};

pub const EVENT: &str = "native:changed";
pub const ENV: &str = "WTM_NATIVE_SESSION";
pub const HOOK_FLAG: &str = "--native-agent-hook";

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
}

impl From<Delivery> for Receipt {
    fn from(d: Delivery) -> Self {
        Self {
            id: d.id,
            label: d.label,
            status: d.status,
            error: d.error,
            prototype: d.prototype,
            review: d.review,
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

#[derive(Debug)]
pub struct Hub {
    sessions: Mutex<BTreeMap<String, Arc<Session>>>,
    pub deliveries: delivery::Store,
}

impl Hub {
    pub fn new(root: PathBuf) -> Self {
        Self {
            sessions: Mutex::new(BTreeMap::new()),
            deliveries: delivery::Store::new(root),
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
    pub fn list(&self) -> Vec<Snapshot> {
        self.sessions
            .lock()
            .values()
            .map(|s| s.snapshot())
            .filter(|s| !s.session.is_empty())
            .collect()
    }
    pub fn close(&self, id: &str) {
        let token = self
            .sessions
            .lock()
            .iter()
            .find(|(_, s)| s.state.lock().session == id)
            .map(|(t, _)| t.clone());
        if let Some(token) = token
            && let Some(session) = self.sessions.lock().remove(&token)
        {
            session.shutdown();
        }
    }
    pub fn ended(&self, id: &str) {
        if let Some(session) = self.get(id) {
            session.shutdown();
        }
    }
}

#[derive(Debug)]
struct Pending {
    reply: mpsc::SyncSender<Result<Value, String>>,
    delivery: Option<Delivery>,
}

pub struct Session {
    app: Weak<App>,
    handle: tauri::AppHandle,
    state: Mutex<Snapshot>,
    instructions: String,
    pub closed: AtomicBool,
    pipe: Mutex<Option<SessionId>>,
    observer: Mutex<CodexObserver>,
    pending: Mutex<BTreeMap<String, Pending>>,
    admission: Mutex<()>,
    directory: PathBuf,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeSession")
            .field("session", &self.state.lock().session)
            .finish_non_exhaustive()
    }
}

impl Session {
    pub fn snapshot(&self) -> Snapshot {
        let mut state = self.state.lock().clone();
        if let Some(app) = self.app.upgrade() {
            state.deliveries = app
                .native_sessions
                .deliveries
                .list(&state.owner())
                .into_iter()
                .map(Receipt::from)
                .collect();
        }
        state
    }

    fn announce(&self) {
        self.state.lock().seq += 1;
        let snapshot = self.snapshot();
        if !snapshot.session.is_empty() {
            let _ = self.handle.emit(EVENT, &snapshot);
            if let Some(app) = self.app.upgrade() {
                crate::extensions::Runtime::event(
                    &self.handle,
                    &app,
                    "native.session.changed",
                    &json!(snapshot),
                    &snapshot.worktree,
                );
            }
        }
    }

    fn update(&self, updates: Vec<Update>) {
        if updates.is_empty() || self.closed.load(Ordering::Acquire) {
            return;
        }
        let Some(app) = self.app.upgrade() else {
            return;
        };
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
                    Update::Status(status) => {
                        state.status = status;
                        None
                    }
                    Update::Event(event) => {
                        match &event {
                            AgentEvent::SessionReady {
                                provider_session_id,
                                model,
                                mode,
                                effort,
                                ..
                            } => {
                                state.status = Status::Starting;
                                if state.conversation != *provider_session_id {
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
                            AgentEvent::UserEcho { text }
                            | AgentEvent::MessageIdentity { text, .. } => {
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
                        if state.activity.len() > 50 {
                            state.activity.remove(0);
                        }
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
                crate::extensions::Runtime::event(
                    &self.handle,
                    &app,
                    "chat.event",
                    &json!({"session":state.session,"seq":state.seq,"event":event,"projectId":state.project,"worktreeId":state.worktree,"renderer":"native"}),
                    &state.worktree,
                );
            }
        }
        self.announce();
    }

    fn write(&self, frame: &Value) -> Result<(), String> {
        let app = self.app.upgrade().ok_or("WTM is closing")?;
        let id = self
            .pipe
            .lock()
            .clone()
            .ok_or("The native backend is not connected")?;
        app.pipe
            .write_line(&id, &frame.to_string())
            .map_err(|e| e.to_string())
    }

    pub fn client_message(&self, mut frame: Value) -> Result<(), String> {
        let _guard = self.admission.lock();
        if matches!(
            frame["method"].as_str(),
            Some("thread/start" | "thread/resume" | "thread/fork")
        ) {
            let app = self.app.upgrade().ok_or("WTM is closing")?;
            let state = self.state.lock().clone();
            let project = app.project(&state.project).map_err(|e| e.to_string())?;
            let root = app
                .worktree(&project, &state.worktree)
                .map_err(|e| e.to_string())?
                .path;
            // A session token grants tools in one worktree. Changing directories needs a new pane.
            frame["params"]["cwd"] = json!(root);
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
        let id = frame.get("id").and_then(Value::as_str);
        if let Some(id) = id
            && let Some(pending) = self.pending.lock().remove(id)
        {
            let result = if let Some(error) = frame.get("error") {
                Err(error["message"]
                    .as_str()
                    .unwrap_or("Provider rejected the request")
                    .to_owned())
            } else {
                Ok(frame["result"].clone())
            };
            if let Some(receipt) = pending.delivery
                && let Some(app) = self.app.upgrade()
            {
                let owner = receipt.owner;
                let target = if result.is_ok() {
                    DeliveryStatus::Delivered
                } else {
                    DeliveryStatus::Rejected
                };
                let settled = app.native_sessions.deliveries.transition(
                    &receipt.id,
                    &owner,
                    DeliveryStatus::Unknown,
                    target,
                    result.as_ref().err().cloned(),
                );
                if let Err(e) = settled {
                    if !app
                        .native_sessions
                        .deliveries
                        .read(&receipt.id)
                        .is_ok_and(|d| {
                            result.is_ok()
                                && d.owner == owner
                                && d.status == DeliveryStatus::Delivered
                        })
                    {
                        self.state.lock().error = Some(e);
                    }
                } else {
                    self.state.lock().error = None;
                }
                if result.is_err() {
                    self.update(vec![Update::Status(Status::Idle)]);
                }
                self.announce();
            }
            let _ = pending.reply.send(result);
            return true;
        }
        let updates = self.observer.lock().server(frame);
        self.update(updates);
        false
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
                "Delivery is uncertain: {error}. WTM will not retry."
            ));
        }
        Ok(rx)
    }

    fn wait_reply(rx: &mpsc::Receiver<Result<Value, String>>) -> Result<Value, String> {
        rx.recv_timeout(Duration::from_secs(10)).map_err(|_| {
            "Delivery is unconfirmed. Check the native conversation; WTM will not retry.".to_owned()
        })?
    }

    pub fn send(
        &self,
        label: &str,
        text: &str,
        review: Option<(&str, &str)>,
    ) -> Result<Receipt, String> {
        let app = self.app.upgrade().ok_or("WTM is closing")?;
        let (delivery, request) = {
            let _guard = self.admission.lock();
            let state = self.state.lock().clone();
            if self.closed.load(Ordering::Acquire) || state.status != Status::Idle {
                return Err("Wait until this native conversation is connected and idle".into());
            }
            let delivery =
                app.native_sessions
                    .deliveries
                    .prepare(state.owner(), label, text, review)?;
            if delivery.status != DeliveryStatus::Pending {
                return Ok(delivery.into());
            }
            let request = if state.capabilities.send {
                app.native_sessions.deliveries.transition(
                    &delivery.id,
                    &state.owner(),
                    DeliveryStatus::Pending,
                    DeliveryStatus::Unknown,
                    None,
                )?;
                self.state.lock().status = Status::Working;
                // Reserve and write while native input is serialized; waiting must release the gate.
                Some(self.begin_request("turn/start", &json!({"threadId":delivery.owner.conversation,"clientUserMessageId":delivery.id,"input":[{"type":"text","text":delivery.text}]}), Some(delivery.clone())))
            } else {
                None
            };
            (delivery, request)
        };
        self.announce();
        if let Some(request) = request
            && let Err(error) = request.and_then(|rx| Self::wait_reply(&rx))
        {
            self.state.lock().error = Some(error);
        }
        self.announce();
        app.native_sessions
            .deliveries
            .read(&delivery.id)
            .map(Receipt::from)
    }

    pub fn tools_connected(&self) {
        if !self.state.lock().tools_connected {
            self.state.lock().tools_connected = true;
            self.announce();
        }
    }

    pub fn refresh_delivery(&self) -> Result<(), String> {
        let state = self.snapshot();
        if state.provider != "codex"
            || !matches!(
                state.status,
                Status::Idle | Status::Working | Status::NeedsApproval
            )
            || !state
                .deliveries
                .iter()
                .any(|d| d.status == DeliveryStatus::Unknown)
        {
            return Ok(());
        }
        let app = self.app.upgrade().ok_or("WTM is closing")?;
        let result = self
            .begin_request(
                "thread/read",
                &json!({"threadId":state.conversation,"includeTurns":true}),
                None,
            )
            .and_then(|rx| Self::wait_reply(&rx))?;
        if app
            .native_sessions
            .deliveries
            .reconcile_codex(&state.owner(), &result["thread"])?
        {
            self.state.lock().error = None;
            self.announce();
        }
        Ok(())
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
        .and_then(|rx| Self::wait_reply(&rx))
        .map(|_| ())
    }

    pub fn disconnect(&self, error: &str) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        self.state.lock().error = Some(error.to_owned());
        self.update(vec![Update::Status(Status::Disconnected)]);
    }

    fn shutdown(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        self.state.lock().status = Status::Ended;
        self.pending.lock().clear();
        if let Some(app) = self.app.upgrade() {
            if let Some(id) = self.pipe.lock().take() {
                let _ = app.pipe.kill(&id);
            }
            app.handoff.forget_session(&self.state.lock().session);
        }
        self.announce();
        let _ = std::fs::remove_dir_all(&self.directory);
    }
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
    let entry = wtm_agent::entry(provider).ok_or_else(|| failure("Unknown provider"))?;
    let executable = app
        .agent_executable(entry)
        .ok_or_else(|| failure("Install the provider CLI and make it available on WTM's PATH"))?;
    let version = app
        .runner
        .run(
            &Invocation::new(
                vec![
                    executable.to_string_lossy().into_owned(),
                    "--version".into(),
                ],
                &worktree.path,
                10_000,
            ),
            &wtm_core::ports::exec::CancelToken::new(),
        )
        .map_err(|e| failure(e.to_string()))?;
    let version = version.stdout.trim().to_owned();
    let help = app
        .runner
        .run(
            &Invocation::new(
                vec![executable.to_string_lossy().into_owned(), "--help".into()],
                &worktree.path,
                10_000,
            ),
            &wtm_core::ports::exec::CancelToken::new(),
        )
        .map_err(|e| failure(e.to_string()))?;
    if provider == "codex" && !help.stdout.contains("--remote")
        || provider == "claude" && !help.stdout.contains("--settings")
    {
        return Err(failure(
            "This CLI version does not expose the required native integration. Update it or open a plain terminal.",
        ));
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
    let bridge = req
        .mcp
        .get_mut(crate::handoff::SERVER_NAME)
        .ok_or_else(|| failure("WTM's session tool bridge is unavailable"))?;
    bridge.env.insert(ENV.into(), "on".into());
    let token = bridge.env[crate::handoff::TOKEN_ENV].clone();
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
    if app.plugins.enabled("wtm.chat") {
        available_tools.push("Agent delegation".into());
    }
    let _token_guard = UnboundToken {
        app,
        token: token.clone(),
    };
    req.env.extend(bridge.env.clone());
    // This renderer is xterm even when the app inherited TERM=dumb from a GUI launcher.
    req.env.insert("TERM".into(), "xterm-256color".into());
    wtm_agent::native::validate_extra_args(&req.extra_args).map_err(failure)?;
    let directory = std::env::temp_dir().join(format!("wtm-n-{}", uuid::Uuid::new_v4().simple()));
    crate::prototypes::private_dir(&directory)?;
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
    let session = Arc::new(Session {
        app: Arc::downgrade(app),
        handle: handle.clone(),
        instructions,
        closed: AtomicBool::new(false),
        pipe: Mutex::new(None),
        observer: Mutex::new(CodexObserver::default()),
        pending: Mutex::new(BTreeMap::new()),
        admission: Mutex::new(()),
        directory: directory.clone(),
        state: Mutex::new(Snapshot {
            session: String::new(),
            project: project_id.into(),
            worktree: worktree_id.into(),
            provider: provider.into(),
            conversation: req.resume.clone().unwrap_or_default(),
            status: Status::Starting,
            version,
            context_supplied: false,
            tools_connected: false,
            tools: available_tools,
            capabilities: Capabilities {
                send: provider == "codex",
                stop: provider == "codex",
                plans: provider == "codex",
                usage: provider == "codex",
                permission_owner: "native_cli".into(),
            },
            title: None,
            model: req.model.clone(),
            plan: Vec::new(),
            activity: Vec::new(),
            usage: None,
            deliveries: Vec::new(),
            error: None,
            seq: 0,
        }),
    });
    app.native_sessions
        .sessions
        .lock()
        .insert(token.clone(), Arc::clone(&session));
    let result = (|| {
        let argv = if provider == "codex" {
            let socket = directory.join("s");
            let (tx, rx) = mpsc::sync_channel(512);
            let mut argv = vec![
                req.executable.clone().unwrap_or_default(),
                "app-server".into(),
                "--stdio".into(),
            ];
            argv.extend(wtm_agent::codex::mcp_overrides(&req.mcp));
            let mut inv = Invocation::new(argv, &worktree.path, crate::commands::SHELL_TIMEOUT_MS);
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
            *session.pipe.lock() = Some(backend.session);
            transport::listen(&session, socket.clone(), rx).map_err(failure)?;
            let mut tui = vec![req.executable.clone().unwrap_or_default()];
            if let Some(resume) = &req.resume {
                tui.extend(["resume".into(), resume.clone()]);
            }
            tui.extend([
                "--remote".into(),
                format!("unix://{}", socket.display()),
                "--cd".into(),
                req.cwd.clone(),
            ]);
            tui.extend(wtm_agent::native::codex_options(&req));
            tui.extend(req.extra_args.clone());
            tui
        } else {
            wtm_agent::native::claude_argv(&req, &hooks::settings(&req)?)
        };
        let mut inv = Invocation::new(argv, &worktree.path, crate::commands::SHELL_TIMEOUT_MS);
        inv.env = req.env.clone();
        let sink = Arc::new(transport::TerminalOutput {
            session: Arc::clone(&session),
            downstream: crate::pty_bridge::EventSink::recording(handle, Arc::clone(app)),
        });
        let id = app
            .open_native_terminal(&worktree, project_id, inv, sink)
            .map_err(|e| failure(e.to_string()))?;
        id.as_str().clone_into(&mut session.state.lock().session);
        if session.closed.load(Ordering::Acquire) {
            app.close_shell(id.as_str());
            return Err(failure(
                "The native CLI exited before its connection was established. Open its session details or try a plain terminal.",
            ));
        }
        app.handoff.bind_session(&token, id.as_str());
        session.announce();
        Ok(id.as_str().to_owned())
    })();
    if result.is_err() {
        session.shutdown();
        app.native_sessions.sessions.lock().remove(&token);
        app.handoff.forget_unbound(&token);
    }
    result
}

#[tauri::command]
pub async fn native_sessions(app: AppState<'_>) -> Reply<Vec<Snapshot>> {
    Ok(app.native_sessions.list())
}

#[tauri::command]
pub async fn native_session(app: AppState<'_>, session: String) -> Reply<Snapshot> {
    let app = Arc::clone(&app);
    blocking(move || {
        let session = app
            .native_sessions
            .get(&session)
            .ok_or_else(|| failure("Native session is no longer registered"))?;
        session.refresh_delivery().map_err(failure)?;
        Ok(session.snapshot())
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
            .ok_or_else(|| failure("Native session is no longer registered"))?
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
            .ok_or_else(|| failure("Native session is no longer registered"))?
            .interrupt()
            .map_err(failure)
    })
    .await
}

pub use hooks::{run_helper, serve_hook};
