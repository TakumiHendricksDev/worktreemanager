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
    sync::{Arc, Weak, mpsc},
    time::Duration,
};
use tauri::{AppHandle, Emitter};
use wtm_core::{
    model::{ExitOutcome, SessionId},
    ports::pipe::{PipeHost, PipeSink},
};

type Answer = Result<Value, String>;
#[derive(Debug)]
struct Pending {
    plugin: String,
    sender: mpsc::SyncSender<Answer>,
}
#[derive(Debug, Default)]
pub struct Runtime {
    processes: Mutex<BTreeMap<String, SessionId>>,
    starting: Mutex<()>,
    writers: Mutex<BTreeMap<String, mpsc::SyncSender<String>>>,
    pending: Mutex<BTreeMap<String, Pending>>,
    problems: Mutex<BTreeMap<String, String>>,
    failed: Mutex<BTreeSet<String>>,
}
fn error(e: impl std::fmt::Display) -> ErrorView {
    ErrorView::new("extension", e.to_string())
}
impl Runtime {
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
                let _ = pending.sender.try_send(Err(
                    "Plugin runtime stopped; an in-flight operation may have completed".into(),
                ));
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
            json!({"plugin":plugin,"message":text}),
        );
    }
    fn start(handle: &AppHandle, app: &Arc<App>, plugin: &str) -> Result<SessionId, String> {
        let _guard = app.extensions.starting.lock();
        app.plugins.active(plugin)?;
        if app.extensions.failed.lock().contains(plugin) {
            return Err(
                "Runtime stopped after a failure. Use Restart runtime to try again.".into(),
            );
        }
        if let Some(id) = app.extensions.processes.lock().get(plugin) {
            return Ok(id.clone());
        }
        let inv = app.plugins.runtime_invocation(plugin)?;
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
        let (sender, receiver) = mpsc::sync_channel::<String>(32);
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
                            app.extensions.fail_locked(
                                &worker_handle,
                                &app,
                                &worker_plugin,
                                "Runtime input closed. Use Restart runtime to try again.",
                            );
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
        app.extensions.writers.lock().insert(plugin.into(), sender);
        let snapshot = app.plugins.snapshot();
        if let Err(e) = app.extensions.write(plugin,&json!({"jsonrpc":"2.0","method":"activate","params":{"api":2,"plugin":plugin,"preferences":snapshot.registry.extensions.get(plugin),"catalog":catalog()}}).to_string()) {
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
        let payload = json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}).to_string();
        if payload.len() > 1_048_576 {
            app.extensions.pending.lock().remove(&id);
            return Err("Extension request exceeds one MiB".into());
        }
        if let Err(e) = app.extensions.write(plugin, &payload) {
            app.extensions.pending.lock().remove(&id);
            app.extensions.fail(handle, app, plugin, &e);
            return Err(e);
        }
        let result=receiver.recv_timeout(Duration::from_secs(30)).unwrap_or_else(|_|{let reason="Extension timed out and was stopped. The request is not automatically repeated.";app.extensions.fail(handle,app,plugin,reason);Err(reason.into())});
        app.extensions.pending.lock().remove(&id);
        if let Err(e) = &result {
            app.extensions.problem(handle, plugin, e);
        }
        result
    }
    fn write(&self, plugin: &str, frame: &str) -> Result<(), String> {
        if frame.len() > 1_048_576 {
            return Err("Runtime frame exceeds one MiB".into());
        }
        self.writers
            .lock()
            .get(plugin)
            .ok_or("Runtime is not active")?
            .try_send(frame.to_owned())
            .map_err(|_| "Runtime input queue is full or closed".into())
    }
    pub fn event(handle: &AppHandle, app: &App, name: &str, payload: &Value, worktree: &str) {
        if app.extensions.processes.lock().is_empty() {
            return;
        }
        let snapshot = app.plugins.snapshot();
        let frame =
            json!({"jsonrpc":"2.0","method":"event","params":{"name":name,"payload":payload}})
                .to_string();
        for (plugin, installed) in snapshot.registry.installed {
            if !installed
                .current
                .manifest
                .extensions
                .events
                .iter()
                .any(|e| e == name)
                || !app.plugins.enabled(&plugin)
            {
                continue;
            }
            if snapshot
                .registry
                .extensions
                .get(&plugin)
                .is_some_and(|p| !p.worktrees.is_empty() && !p.worktrees.contains(worktree))
            {
                continue;
            }
            if let Err(e) = app.extensions.write(&plugin, &frame) {
                app.extensions.fail(handle, app, &plugin, &e);
            }
        }
    }
    fn reserve(&self, plugin: &str) -> Result<(String, mpsc::Receiver<Answer>), String> {
        let mut pending = self.pending.lock();
        if pending.values().filter(|p| p.plugin == plugin).count() >= 16 {
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
        let emitted=handle.emit_to("main","extensions:host",json!({"id":id,"plugin":plugin,"operation":operation,"arguments":arguments,"context":context}));
        if let Err(e) = emitted {
            app.extensions.pending.lock().remove(&id);
            return Err(e.to_string());
        }
        let result=receiver.recv_timeout(Duration::from_secs(30)).unwrap_or_else(|_|Err("Workbench did not answer. The operation may have completed; it will not be retried automatically.".into()));
        app.extensions.pending.lock().remove(&id);
        result
    }
    fn authorize(
        app: &App,
        plugin: &str,
        operation: &str,
        arguments: &Value,
        context: &Value,
    ) -> Result<(), String> {
        let worktree = context["worktreeId"].as_str().unwrap_or_default();
        app.plugins.extension_allowed(plugin, operation, worktree)?;
        if arguments["options"]["mode"].is_string() || arguments.get("mode").is_some() {
            return Err("Permission mode is controlled by the host picker".into());
        }
        if operation == "set_pref" && arguments["key"] == "exec.path" {
            return Err("Executable search paths are controlled by host settings".into());
        }
        if let Some(target) = context["worktreeId"].as_str().filter(|v| !v.is_empty()) {
            let project = context["projectId"]
                .as_str()
                .ok_or("Project context is required")?;
            let project = app.project(project).map_err(|e| e.to_string())?;
            app.worktree(&project, target).map_err(|e| e.to_string())?;
        }
        let public = catalog()["operations"].as_array().is_some_and(|ops| {
            ops.iter()
                .any(|o| o["id"] == operation && o["exposure"] == "public")
        });
        if !public {
            return Err("Host operation is protected or unknown".into());
        }
        // A context selected in another window cannot extend a workspace-scoped grant.
        if let Some(target) = arguments["worktreeId"].as_str() {
            app.plugins.extension_allowed(plugin, operation, target)?;
        }
        if let Some(session) = arguments["session"].as_str()
            && let Some(facts) = app.live_agents().iter().find(|f| f.session == session)
        {
            app.plugins.extension_allowed(
                plugin,
                operation,
                facts.scope.worktree_id().unwrap_or_default(),
            )?;
        }
        for key in ["session", "parentSession", "id", "browser"] {
            if let Some(id) = arguments[key].as_str() {
                if let Some(shell) = app.live_shells().iter().find(|s| s.session == id) {
                    app.plugins
                        .extension_allowed(plugin, operation, &shell.worktree)?;
                }
                if let Some(browser) = app.browsers.view_of(id) {
                    app.plugins
                        .extension_allowed(plugin, operation, &browser.worktree)?;
                }
                if let Some(agent) = app.live_agents().iter().find(|s| s.session == id) {
                    app.plugins.extension_allowed(
                        plugin,
                        operation,
                        agent.scope.worktree_id().unwrap_or_default(),
                    )?;
                }
            }
        }
        Ok(())
    }
}
#[derive(Debug)]
struct Sink {
    handle: AppHandle,
    app: Weak<App>,
    plugin: String,
}
impl PipeSink for Sink {
    fn on_line(&self, session: &SessionId, line: &str) {
        let Some(app) = self.app.upgrade() else {
            return;
        };
        if line.len() > 1_048_576 {
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
        if app.extensions.processes.lock().get(&self.plugin) != Some(session) {
            return;
        }
        if frame.get("method").is_none() {
            if let Some(id) = frame["id"].as_str() {
                let pending = app.extensions.pending.lock();
                if let Some(p) = pending.get(id).filter(|p| p.plugin == self.plugin) {
                    let result = if frame.get("error").is_some() {
                        Err(frame["error"].to_string())
                    } else {
                        Ok(frame["result"].clone())
                    };
                    let _ = p.sender.try_send(result);
                }
            }
        } else if frame["method"] == "host" {
            if !app.plugins.try_view_request(&self.plugin) {
                let _ = app.extensions.write(
                    &self.plugin,
                    &json!({"id":frame["id"],"error":{"message":"Too many host requests"}})
                        .to_string(),
                );
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
                    Ok(value) => json!({"jsonrpc":"2.0","id":frame["id"],"result":value}),
                    Err(e) => json!({"jsonrpc":"2.0","id":frame["id"],"error":{"message":e}}),
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
                app.extensions.fail_locked(
                    &self.handle,
                    &app,
                    &self.plugin,
                    "Runtime exited. Use Restart runtime to try again.",
                );
            }
        }
    }
}
pub fn catalog() -> Value {
    let mut value: Value = serde_json::from_str(include_str!(
        "../../crates/wtm-plugins/contracts/catalog.json"
    ))
    .expect("checked contract catalog");
    value["wire"] = serde_json::from_str(include_str!(
        "../../crates/wtm-plugins/contracts/operations.json"
    ))
    .expect("generated operation schemas");
    value
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
        app.extensions.reset(&app, &plugin);
        Runtime::reconcile(&handle, &app);
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
