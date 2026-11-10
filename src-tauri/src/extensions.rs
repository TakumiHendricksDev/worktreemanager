//! One activation process per reviewed package. Host calls retain owner identity across the pipe.
use crate::{
    app::App,
    commands::{AppState, Reply, blocking},
    view::ErrorView,
};
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, LazyLock, Weak, mpsc},
    time::Duration,
};
use tauri::{AppHandle, Emitter};
use wtm_core::{
    model::{ExitOutcome, Project, SessionId},
    ports::{
        config::ConfigStore,
        pipe::{PipeHost, PipeSink},
    },
};
use wtm_plugins::catalog::{Resource, Scope};

/// One JSON-RPC frame in either direction.
const MAX_FRAME: usize = 1_048_576;
/// Frames waiting for a runtime to read them. Events past this are dropped, never queued.
const QUEUE: usize = 32;
const MAX_OUTSTANDING: usize = 16;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

const TIMED_OUT: &str =
    "Extension timed out and was stopped. The request is not automatically repeated.";
const UNANSWERED: &str = "Workbench did not answer. The operation may have completed; it will not \
    be retried automatically.";
const EVENTS_DROPPED: &str = "The runtime fell behind and some events were not delivered. Read a \
    fresh snapshot rather than relying on every event.";
const INPUT_CLOSED: &str = "Runtime input closed. Use Restart runtime to try again.";
const EXITED: &str = "Runtime exited. Use Restart runtime to try again.";
const FAILED: &str = "Runtime stopped after a failure. Use Restart runtime to try again.";
const IN_FLIGHT: &str = "Plugin runtime stopped; an in-flight operation may have completed";
const UNSCOPED: &str =
    "This operation reaches beyond one workspace, and this plugin is limited to chosen workspaces";
const NO_ADDRESS: &str =
    "Opening a page at an address is network egress, which a plugin cannot declare";

type Answer = Result<Value, String>;
#[derive(Debug)]
struct Pending {
    plugin: String,
    sender: mpsc::SyncSender<Answer>,
}
/// A running runtime's input, with what the event fan-out needs to route to it.
///
/// Kept here rather than read from the registry per event: chat events arrive many times a
/// second on the agent's thread, and cloning the whole registry for each was the fan-out's cost.
#[derive(Debug)]
struct Writer {
    sender: mpsc::SyncSender<String>,
    events: BTreeSet<String>,
    scope: Option<BTreeSet<String>>,
    /// Whether this runtime has been told it missed events. Once per process, not per event.
    dropped: bool,
}
#[derive(Debug, Default)]
pub struct Runtime {
    processes: Mutex<BTreeMap<String, SessionId>>,
    /// Serialises start, stop and failure. Never taken on an event path: a runtime that is slow
    /// to start must not stall the agent thread that publishes chat events.
    starting: Mutex<()>,
    writers: Mutex<BTreeMap<String, Writer>>,
    pending: Mutex<BTreeMap<String, Pending>>,
    problems: Mutex<BTreeMap<String, String>>,
    failed: Mutex<BTreeSet<String>>,
}
fn error(e: impl std::fmt::Display) -> ErrorView {
    ErrorView::new("extension", e.to_string())
}
impl Runtime {
    /// Whether no plugin runtime is running, so a publisher can skip building an event payload
    /// that nothing would receive. One lock and a length; never the lifecycle lock.
    pub fn idle(&self) -> bool {
        self.processes.lock().is_empty()
    }
    pub fn reset(&self, app: &App, plugin: &str) {
        let _guard = self.starting.lock();
        self.stop_locked(app, plugin);
        self.failed.lock().remove(plugin);
        self.problems.lock().remove(plugin);
    }
    fn fail(&self, handle: &AppHandle, app: &App, plugin: &str, reason: &str) {
        let _guard = self.starting.lock();
        self.fail_locked(handle, app, plugin, reason);
    }
    fn fail_locked(&self, handle: &AppHandle, app: &App, plugin: &str, reason: &str) {
        self.failed.lock().insert(plugin.into());
        self.stop_locked(app, plugin);
        self.problem(handle, plugin, reason);
    }
    pub fn answer(&self, id: &str, result: Value, problem: Option<String>) {
        let pending = self.pending.lock();
        if let Some(p) = pending.get(id) {
            let _ = p.sender.try_send(problem.map_or(Ok(result), Err));
        }
    }
    pub fn stop(&self, app: &App, plugin: &str) {
        let _guard = self.starting.lock();
        self.stop_locked(app, plugin);
    }
    fn stop_locked(&self, app: &App, plugin: &str) {
        self.writers.lock().remove(plugin);
        let process = self.processes.lock().remove(plugin);
        if let Some(process) = process {
            let _ = app.pipe.kill(&process);
        }
        self.pending.lock().retain(|_, pending| {
            if pending.plugin == plugin {
                let _ = pending.sender.try_send(Err(IN_FLIGHT.into()));
                false
            } else {
                true
            }
        });
    }
    pub fn reconcile(handle: &AppHandle, app: &Arc<App>) {
        let snapshot = app.plugins.snapshot();
        let names: Vec<_> = app.extensions.processes.lock().keys().cloned().collect();
        for id in names {
            if !app.plugins.enabled(&id) {
                app.extensions.stop(app, &id);
            }
        }
        for (id, installed) in snapshot.registry.installed {
            if app.plugins.enabled(&id)
                && !installed.current.manifest.extensions.runtime.is_empty()
                && let Err(e) = Self::start(handle, app, &id)
            {
                app.extensions.problem(handle, &id, &e);
            }
        }
    }
    fn problem(&self, handle: &AppHandle, plugin: &str, text: &str) {
        let text: String = text.chars().take(4096).collect();
        self.problems.lock().insert(plugin.into(), text.clone());
        let _ = handle.emit(
            "extensions:problem",
            json!({"plugin": plugin, "message": text}),
        );
    }
    fn start(handle: &AppHandle, app: &Arc<App>, plugin: &str) -> Result<SessionId, String> {
        let _guard = app.extensions.starting.lock();
        if app.extensions.failed.lock().contains(plugin) {
            return Err(FAILED.into());
        }
        if let Some(id) = app.extensions.processes.lock().get(plugin) {
            return Ok(id.clone());
        }
        // Starting a runtime is an activation boundary: this re-hashes the snapshot.
        let inv = app.plugins.runtime_invocation(plugin)?;
        let (package, _) = app.plugins.active(plugin)?;
        let sink = Arc::new(Sink {
            handle: handle.clone(),
            app: Arc::downgrade(app),
            plugin: plugin.into(),
        });
        let spawned = app
            .pipe
            .spawn(&inv, None, sink)
            .map_err(|e| e.to_string())?;
        let id = spawned.session;
        app.extensions
            .processes
            .lock()
            .insert(plugin.into(), id.clone());
        let (sender, receiver) = mpsc::sync_channel::<String>(QUEUE);
        let worker_app = Arc::downgrade(app);
        let worker_id = id.clone();
        let worker_handle = handle.clone();
        let worker_plugin = plugin.to_owned();
        let worker = std::thread::Builder::new()
            .name(format!("plugin-writer-{plugin}"))
            .spawn(move || {
                while let Ok(frame) = receiver.recv() {
                    let Some(app) = worker_app.upgrade() else {
                        break;
                    };
                    if app.pipe.write_line(&worker_id, &frame).is_err() {
                        let _guard = app.extensions.starting.lock();
                        if app.extensions.processes.lock().get(&worker_plugin) == Some(&worker_id) {
                            let (handle, plugin) = (&worker_handle, &worker_plugin);
                            app.extensions
                                .fail_locked(handle, &app, plugin, INPUT_CLOSED);
                        }
                        break;
                    }
                }
            });
        if let Err(e) = worker {
            app.extensions
                .fail_locked(handle, app, plugin, &e.to_string());
            return Err(e.to_string());
        }
        let preferences = app.plugins.snapshot().registry.extensions.remove(plugin);
        app.extensions.writers.lock().insert(
            plugin.into(),
            Writer {
                sender,
                events: package.manifest.extensions.events.into_iter().collect(),
                scope: app.plugins.extension_scope(plugin),
                dropped: false,
            },
        );
        let activate = json!({
            "jsonrpc": "2.0",
            "method": "activate",
            "params": {
                "api": 2,
                "plugin": plugin,
                "preferences": preferences,
                "catalog": catalog(),
            },
        });
        if let Err(e) = app.extensions.write(plugin, &activate.to_string()) {
            app.extensions.fail_locked(handle, app, plugin, &e);
            return Err(e);
        }
        app.extensions.problems.lock().remove(plugin);
        Ok(id)
    }
    pub fn call(
        handle: &AppHandle,
        app: &Arc<App>,
        plugin: &str,
        method: &str,
        params: &Value,
    ) -> Answer {
        Self::start(handle, app, plugin)?;
        let (id, receiver) = app.extensions.reserve(plugin)?;
        let payload =
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string();
        if let Err(e) = app.extensions.write(plugin, &payload) {
            app.extensions.pending.lock().remove(&id);
            return Err(e);
        }
        let result = receiver.recv_timeout(REQUEST_TIMEOUT).unwrap_or_else(|_| {
            app.extensions.fail(handle, app, plugin, TIMED_OUT);
            Err(TIMED_OUT.into())
        });
        app.extensions.pending.lock().remove(&id);
        if let Err(e) = &result {
            app.extensions.problem(handle, plugin, e);
        }
        result
    }
    /// Queue a frame for a runtime. A full queue is reported, not fatal: the caller decides.
    fn write(&self, plugin: &str, frame: &str) -> Result<(), String> {
        if frame.len() > MAX_FRAME {
            return Err("Runtime frame exceeds one MiB".into());
        }
        self.writers
            .lock()
            .get(plugin)
            .ok_or("Runtime is not active")?
            .sender
            .try_send(frame.to_owned())
            .map_err(|e| match e {
                mpsc::TrySendError::Full(_) => "Runtime input queue is full".into(),
                mpsc::TrySendError::Disconnected(_) => "Runtime input is closed".into(),
            })
    }
    /// Deliver a host event to every running runtime that subscribed to it, best effort.
    ///
    /// Events are notifications, not requests: a runtime that cannot keep up loses some and is
    /// told so once, rather than being killed for it, and one without a running process is simply
    /// not a subscriber. Called on agent threads and from a synchronous command, so it takes no
    /// lifecycle lock and reads nothing from the package registry.
    pub fn event(handle: &AppHandle, app: &App, name: &str, payload: &Value, worktree: &str) {
        let mut frame: Option<String> = None;
        let mut dropped = Vec::new();
        {
            let mut writers = app.extensions.writers.lock();
            for (plugin, writer) in writers.iter_mut() {
                if !writer.events.contains(name)
                    || writer.scope.as_ref().is_some_and(|s| !s.contains(worktree))
                {
                    continue;
                }
                let frame = frame.get_or_insert_with(|| {
                    let params = json!({"name": name, "payload": payload});
                    json!({"jsonrpc": "2.0", "method": "event", "params": params}).to_string()
                });
                let delivered = frame.len() <= MAX_FRAME
                    && !matches!(
                        writer.sender.try_send(frame.clone()),
                        Err(mpsc::TrySendError::Full(_))
                    );
                if !delivered && !writer.dropped {
                    writer.dropped = true;
                    dropped.push(plugin.clone());
                }
            }
        }
        for plugin in dropped {
            app.extensions.problem(handle, &plugin, EVENTS_DROPPED);
        }
    }
    /// Tell a running runtime its own preferences changed. Restarting it for a setting would drop
    /// whatever it was doing; the activation message's `preferences` is what this replaces.
    fn preferences_changed(
        &self,
        plugin: &str,
        preferences: Option<&Value>,
        scope: Option<BTreeSet<String>>,
    ) {
        let params = json!({
            "name": "settings.changed",
            "payload": {"plugin": plugin, "preferences": preferences},
        });
        let frame = json!({"jsonrpc": "2.0", "method": "event", "params": params}).to_string();
        let mut writers = self.writers.lock();
        if let Some(writer) = writers.get_mut(plugin) {
            writer.scope = scope;
            let _ = writer.sender.try_send(frame);
        }
    }
    fn reserve(&self, plugin: &str) -> Result<(String, mpsc::Receiver<Answer>), String> {
        let mut pending = self.pending.lock();
        if pending.values().filter(|p| p.plugin == plugin).count() >= MAX_OUTSTANDING {
            return Err("Too many outstanding extension requests".into());
        }
        let id = uuid::Uuid::new_v4().to_string();
        let (sender, receiver) = mpsc::sync_channel(1);
        pending.insert(
            id.clone(),
            Pending {
                plugin: plugin.into(),
                sender,
            },
        );
        Ok((id, receiver))
    }
    pub fn host(
        handle: &AppHandle,
        app: &Arc<App>,
        plugin: &str,
        operation: &str,
        arguments: &Value,
        context: &Value,
    ) -> Answer {
        Self::authorize(app, plugin, operation, arguments, context)?;
        let (id, receiver) = app.extensions.reserve(plugin)?;
        let request = json!({
            "id": id,
            "plugin": plugin,
            "operation": operation,
            "arguments": arguments,
            "context": context,
        });
        if let Err(e) = handle.emit_to("main", "extensions:host", request) {
            app.extensions.pending.lock().remove(&id);
            return Err(e.to_string());
        }
        let result = receiver
            .recv_timeout(REQUEST_TIMEOUT)
            .unwrap_or_else(|_| Err(UNANSWERED.into()));
        app.extensions.pending.lock().remove(&id);
        result
    }
    /// Whether `plugin` may run `operation` with these arguments, failing closed.
    ///
    /// The catalog says how each public operation stays inside a workspace (see
    /// `wtm_plugins::catalog::Scope`). Every argument it lists as naming a project, worktree or
    /// session is resolved here, against what WTM actually has: an id that resolves to nothing is
    /// refused rather than passed through, because "not found here" was how an earlier check let
    /// another workspace's database session and arbitrary directories reach commands.
    pub fn authorize(
        app: &App,
        plugin: &str,
        operation: &str,
        arguments: &Value,
        context: &Value,
    ) -> Result<(), String> {
        let contract = wtm_plugins::catalog::catalog()
            .public(operation)
            .ok_or("Host operation is protected or unknown")?;
        let worktree = context["worktreeId"].as_str().unwrap_or_default();
        app.plugins.extension_allowed(plugin, operation, worktree)?;
        if arguments["options"]["mode"].is_string() || arguments.get("mode").is_some() {
            return Err("Permission mode is controlled by the host picker".into());
        }
        if operation == "ui.panels.open" && arguments.get("url").is_some_and(|u| !u.is_null()) {
            return Err(NO_ADDRESS.into());
        }
        let project = context["projectId"].as_str().unwrap_or_default();
        let place = if worktree.is_empty() {
            None
        } else {
            if project.is_empty() {
                return Err("Project context is required".into());
            }
            let found = registered_project(app, project)?;
            app.worktree(&found, worktree).map_err(|e| e.to_string())?;
            Some((project, worktree))
        };
        let scope = app.plugins.extension_scope(plugin);
        match contract.scope {
            Some(Scope::Global) if scope.is_some() => Err(UNSCOPED.into()),
            Some(Scope::None | Scope::Context | Scope::Global) => Ok(()),
            Some(Scope::Arguments) => {
                let resolver = Resolver {
                    app,
                    place,
                    scope: scope.as_ref(),
                };
                resolver.check(&contract.resources, arguments)
            }
            None => Err("Host operation has no declared workspace scope".into()),
        }
    }
}

/// Resolves the resource arguments of one host call against what WTM has.
struct Resolver<'a> {
    app: &'a App,
    /// The caller's validated project and worktree, if it has one.
    place: Option<(&'a str, &'a str)>,
    /// `None` when the plugin may act in every workspace.
    scope: Option<&'a BTreeSet<String>>,
}
impl Resolver<'_> {
    fn check(
        &self,
        resources: &BTreeMap<String, Resource>,
        arguments: &Value,
    ) -> Result<(), String> {
        let named = |kind: Resource| {
            resources
                .iter()
                .filter(move |(_, k)| **k == kind)
                .map(|(path, _)| (path.as_str(), argument(arguments, path)))
        };
        let mut project = None;
        for (path, value) in named(Resource::Project) {
            if let Some(id) = self.identifier(path, value)? {
                project = Some((id, self.project(id)?));
            }
        }
        for (path, value) in named(Resource::Worktree) {
            if let Some(id) = self.identifier(path, value)? {
                self.worktree(id, project.as_ref().map(|(_, p)| p))?;
            }
        }
        for (path, value) in named(Resource::Session) {
            if let Some(id) = self.identifier(path, value)? {
                self.session(id)?;
            }
        }
        Ok(())
    }
    /// The id an argument holds. Absent is allowed only to a plugin that may act everywhere,
    /// since several operations read "no worktree" as "every worktree".
    fn identifier<'v>(&self, path: &str, value: &'v Value) -> Result<Option<&'v str>, String> {
        match value {
            Value::Null if self.scope.is_none() => Ok(None),
            Value::Null => Err(format!(
                "`{path}` must name a workspace this plugin may use"
            )),
            Value::String(id) if !id.is_empty() => Ok(Some(id)),
            _ => Err(format!("`{path}` must be a non-empty identifier")),
        }
    }
    fn in_scope(&self, worktree: Option<&str>) -> Result<(), String> {
        match (self.scope, worktree) {
            (None, _) => Ok(()),
            (Some(scope), Some(w)) if scope.contains(w) => Ok(()),
            _ => Err("Plugin is not active for this worktree".into()),
        }
    }
    fn project(&self, id: &str) -> Result<Project, String> {
        let project = registered_project(self.app, id)?;
        let Some(scope) = self.scope else {
            return Ok(project);
        };
        if self.place.is_some_and(|(p, _)| p == id) {
            return Ok(project);
        }
        let worktrees = self
            .app
            .git
            .list_worktrees(&project.root)
            .map_err(|e| e.to_string())?;
        if worktrees.iter().any(|w| scope.contains(w.id.as_str())) {
            Ok(project)
        } else {
            Err("Plugin is not active for this project".into())
        }
    }
    /// Worktree ids are paths, and the commands that take one trust it as a directory, so one
    /// that is not a worktree of a registered project is refused for every plugin.
    fn worktree(&self, id: &str, project: Option<&Project>) -> Result<(), String> {
        self.in_scope(Some(id))?;
        if self.place.is_some_and(|(_, w)| w == id) && project.is_none() {
            return Ok(());
        }
        if let Some(project) = project {
            return self
                .app
                .worktree(project, id)
                .map(|_| ())
                .map_err(|e| e.to_string());
        }
        let roots = self.app.config.projects().map_err(|e| e.to_string())?;
        let known = roots.iter().any(|root| {
            self.app
                .git
                .list_worktrees(root)
                .is_ok_and(|list| list.iter().any(|w| w.id.as_str() == id))
        });
        if known {
            Ok(())
        } else {
            Err(format!("{id} is not a worktree of a registered project"))
        }
    }
    /// A live terminal, agent, native session or browser pane, and the worktree it belongs to.
    /// Panes a plugin owns are not resources: one package must not drive another's view.
    fn session(&self, id: &str) -> Result<(), String> {
        let app = self.app;
        let worktree: Option<Option<String>> = app
            .live_shells()
            .into_iter()
            .find(|s| s.session == id)
            .map(|s| Some(s.worktree))
            .or_else(|| {
                app.conversation_sessions()
                    .into_iter()
                    .find(|s| s.session == id)
                    .map(|s| s.scope.worktree_id().map(str::to_owned))
            })
            .or_else(|| {
                app.browsers
                    .view_of(id)
                    .filter(|v| !crate::plugin_views::plugin_owned(v.opened_by.as_deref()))
                    .map(|v| Some(v.worktree))
            });
        let worktree = worktree.ok_or_else(|| format!("{id} is not a live session"))?;
        self.in_scope(worktree.as_deref())
    }
}

/// The value at a dot-separated argument path, or null.
fn argument<'v>(arguments: &'v Value, path: &str) -> &'v Value {
    path.split('.').fold(arguments, |value, key| &value[key])
}

/// A project the user registered. `App::project` loads any directory that has a configuration,
/// and Git then runs there, so an id a plugin supplied is checked against the registry first.
pub(crate) fn registered_project(app: &App, id: &str) -> Result<Project, String> {
    let roots = app.config.projects().map_err(|e| e.to_string())?;
    if !roots.iter().any(|root| root.as_os_str() == id) {
        return Err(format!("{id} is not a registered project"));
    }
    app.project(id).map_err(|e| e.to_string())
}

#[derive(Debug)]
struct Sink {
    handle: AppHandle,
    app: Weak<App>,
    plugin: String,
}
impl Sink {
    fn current(&self, app: &App, session: &SessionId) -> bool {
        app.extensions.processes.lock().get(&self.plugin) == Some(session)
    }
}
impl PipeSink for Sink {
    fn on_line(&self, session: &SessionId, line: &str) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        // A process this runtime already replaced may still be flushing; it speaks for nothing.
        if !self.current(&app, session) {
            return;
        }
        if line.len() > MAX_FRAME {
            app.extensions
                .problem(&self.handle, &self.plugin, "Runtime frame exceeds one MiB");
            app.extensions.stop(&app, &self.plugin);
            return;
        }
        let Ok(frame) = serde_json::from_str::<Value>(line) else {
            app.extensions
                .problem(&self.handle, &self.plugin, "Runtime emitted invalid JSON");
            return;
        };
        if frame.get("method").is_none() {
            if let Some(id) = frame["id"].as_str() {
                let pending = app.extensions.pending.lock();
                if let Some(p) = pending.get(id).filter(|p| p.plugin == self.plugin) {
                    let result = match frame.get("error") {
                        Some(error) => Err(runtime_error(error)),
                        None => Ok(frame["result"].clone()),
                    };
                    let _ = p.sender.try_send(result);
                }
            }
        } else if frame["method"] == "host" {
            if !app.plugins.try_view_request(&self.plugin) {
                let error = json!({"message": "Too many host requests"});
                let reply = json!({"jsonrpc": "2.0", "id": frame["id"], "error": error});
                let _ = app.extensions.write(&self.plugin, &reply.to_string());
                return;
            }
            let (handle, plugin, session) =
                (self.handle.clone(), self.plugin.clone(), session.clone());
            tauri::async_runtime::spawn_blocking(move || {
                struct Admission(Arc<App>, String);
                impl Drop for Admission {
                    fn drop(&mut self) {
                        self.0.plugins.finish_view_request(&self.1);
                    }
                }
                let _admission = Admission(Arc::clone(&app), plugin.clone());
                let params = &frame["params"];
                let result = Runtime::host(
                    &handle,
                    &app,
                    &plugin,
                    params["operation"].as_str().unwrap_or_default(),
                    &params["arguments"],
                    &params["context"],
                );
                let reply = match result {
                    Ok(value) => json!({"jsonrpc": "2.0", "id": frame["id"], "result": value}),
                    Err(e) => {
                        json!({"jsonrpc": "2.0", "id": frame["id"], "error": {"message": e}})
                    }
                };
                if app.extensions.processes.lock().get(&plugin) == Some(&session) {
                    let _ = app.extensions.write(&plugin, &reply.to_string());
                }
            });
        }
    }
    fn on_stderr(&self, _: &SessionId, line: &str) {
        if let Some(app) = self.app.upgrade() {
            app.extensions.problem(&self.handle, &self.plugin, line);
        }
    }
    fn on_exit(&self, session: &SessionId, _: &ExitOutcome) {
        if let Some(app) = self.app.upgrade() {
            let _guard = app.extensions.starting.lock();
            let mut processes = app.extensions.processes.lock();
            if processes.get(&self.plugin) == Some(session) {
                processes.remove(&self.plugin);
                drop(processes);
                app.extensions
                    .fail_locked(&self.handle, &app, &self.plugin, EXITED);
            }
        }
    }
}

/// A JSON-RPC error object as a sentence. Users read this in a banner, not a JSON viewer.
fn runtime_error(error: &Value) -> String {
    match error["message"].as_str().map(str::trim) {
        Some(message) if !message.is_empty() => message.chars().take(4096).collect(),
        _ => "The plugin runtime reported an error without a message".into(),
    }
}

static CATALOG: LazyLock<Value> = LazyLock::new(|| {
    let mut value: Value =
        serde_json::from_str(wtm_plugins::catalog::SOURCE).expect("checked contract catalog");
    value["wire"] = serde_json::from_str(include_str!(
        "../../crates/wtm-plugins/contracts/operations.json"
    ))
    .expect("generated operation schemas");
    value
});

/// The catalog as authors read it, with the generated argument schemas under `wire`.
pub fn catalog() -> &'static Value {
    &CATALOG
}

#[tauri::command]
pub async fn extension_validate(
    app: AppState<'_>,
    plugin: String,
    operation: String,
    arguments: Value,
    context: Value,
) -> Reply<()> {
    let app = Arc::clone(&app);
    blocking(move || {
        Runtime::authorize(&app, &plugin, &operation, &arguments, &context).map_err(error)
    })
    .await
}
#[tauri::command]
pub async fn extension_configure(
    handle: AppHandle,
    app: AppState<'_>,
    plugin: String,
    change: Value,
) -> Reply<wtm_plugins::Snapshot> {
    let app = Arc::clone(&app);
    blocking(move || {
        let result = app
            .plugins
            .extension_configure(&plugin, &change)
            .map_err(error)?;
        let preferences = result
            .registry
            .extensions
            .get(&plugin)
            .and_then(|p| serde_json::to_value(p).ok());
        let scope = app.plugins.extension_scope(&plugin);
        app.extensions
            .preferences_changed(&plugin, preferences.as_ref(), scope);
        let _ = handle.emit("plugins:changed", &result);
        Ok(result)
    })
    .await
}
#[tauri::command]
pub async fn extension_start(
    handle: AppHandle,
    app: AppState<'_>,
) -> Reply<BTreeMap<String, String>> {
    let app = Arc::clone(&app);
    blocking(move || {
        Runtime::reconcile(&handle, &app);
        Ok(app.extensions.problems.lock().clone())
    })
    .await
}
#[tauri::command]
pub async fn extension_restart(handle: AppHandle, app: AppState<'_>, plugin: String) -> Reply<()> {
    let app = Arc::clone(&app);
    blocking(move || {
        app.extensions.reset(&app, &plugin);
        Runtime::start(&handle, &app, &plugin).map_err(error)?;
        Ok(())
    })
    .await
}
#[tauri::command]
pub async fn extension_call(
    handle: AppHandle,
    app: AppState<'_>,
    plugin: String,
    method: String,
    params: Value,
) -> Reply<Value> {
    let app = Arc::clone(&app);
    blocking(move || Runtime::call(&handle, &app, &plugin, &method, &params).map_err(error)).await
}
#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn extension_reply(
    window: tauri::WebviewWindow,
    app: AppState<'_>,
    id: String,
    result: Value,
    problem: Option<String>,
) -> Reply<()> {
    if window.label() != "main" {
        return Err(error("Only the main workbench handles host requests"));
    }
    app.extensions.answer(&id, result, problem);
    Ok(())
}

#[tauri::command]
pub async fn extension_host(
    handle: AppHandle,
    app: AppState<'_>,
    plugin: String,
    operation: String,
    arguments: Value,
    context: Value,
) -> Reply<Value> {
    let app = Arc::clone(&app);
    blocking(move || {
        Runtime::host(&handle, &app, &plugin, &operation, &arguments, &context).map_err(error)
    })
    .await
}

#[tauri::command]
#[allow(clippy::needless_pass_by_value)]
pub fn extension_event(
    window: tauri::WebviewWindow,
    handle: AppHandle,
    app: AppState<'_>,
    name: String,
    payload: Value,
    worktree: String,
) -> Reply<()> {
    if window.label() != "main"
        || ![
            "operation.completed",
            "workspace.changed",
            "panels.changed",
            "settings.changed",
        ]
        .contains(&name.as_str())
    {
        return Err(error("Only the host publishes this event"));
    }
    Runtime::event(&handle, &app, &name, &payload, &worktree);
    Ok(())
}
