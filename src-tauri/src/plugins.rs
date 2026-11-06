//! Plugin entry points share the same trust and process adapters as the rest of the app.
use crate::commands::{AppState, Reply, blocking};
use crate::view::ErrorView;
use std::sync::Arc;
use tauri::Emitter;
use wtm_plugins::{Review, Snapshot};
fn error(message: String) -> ErrorView {
    ErrorView::new("plugin", message)
}

#[tauri::command]
pub async fn plugins_state(app: AppState<'_>) -> Reply<Snapshot> {
    let app = Arc::clone(&app);
    blocking(move || Ok(app.plugins.snapshot())).await
}
#[tauri::command]
pub async fn plugin_prepare(
    app: AppState<'_>,
    source: String,
    kind: String,
    revision: String,
) -> Reply<Review> {
    let app = Arc::clone(&app);
    blocking(move || {
        app.plugins
            .prepare(&source, &kind, &revision)
            .map_err(error)
    })
    .await
}
#[tauri::command]
pub async fn plugin_cancel(app: AppState<'_>, token: String) -> Reply<()> {
    let app = Arc::clone(&app);
    blocking(move || app.plugins.cancel(&token).map_err(error)).await
}

#[tauri::command]
pub async fn plugin_change(
    handle: tauri::AppHandle,
    app: AppState<'_>,
    action: String,
    id: String,
    enabled: bool,
) -> Reply<Snapshot> {
    let app = Arc::clone(&app);
    blocking(move || {
        if action == "enable" && !enabled && id == "wtm.home" && app.has_home_sessions() {
            return Err(error("Close Home's running sessions before disabling Home. Saved conversations are kept.".into()));
        }
        let before = app.plugins.snapshot();
        // Revoke callbacks before replacing code; the old process must not answer for a new digest.
        if ["rollback", "uninstall", "enable"].contains(&action.as_str()) { app.extensions.reset(&app, &id); }
        if action == "recover" || action == "install" {
            for plugin in before.registry.installed.keys() { app.extensions.stop(&app, plugin); }
        }
        let result = match action.as_str() {
            "install" => app.plugins.install(&id),
            "enable" => app.plugins.set_enabled(&id, enabled),
            "experience" => app.plugins.experience(&id),
            "rollback" => app.plugins.rollback(&id),
            "uninstall" => app.plugins.uninstall(&id),
            "recover" => app.plugins.recover(),
            _ => Err("Unknown plugin operation".into()),
        };
        if result.is_err() { crate::extensions::Runtime::reconcile(&handle,&app); }
        let result=result.map_err(error)?;
        for (plugin, installed) in &result.registry.installed {
            if before.registry.installed.get(plugin).map(|old| &old.current.directory) != Some(&installed.current.directory) { app.extensions.reset(&app, plugin); }
        }
        if let Some(menu) = handle.menu()
            && let Some(item) = menu.get(crate::HOME_EVENT).and_then(|i| i.as_menuitem().cloned()) {
            let _ = item.set_enabled(app.plugins.binding(wtm_plugins::contributions::Slot::Home) != wtm_plugins::contributions::Binding::Hidden);
        }
        for view in app.browsers.list(None) {
            let owner = view.opened_by.as_deref().unwrap_or_default();
            let plugin = owner.strip_prefix("plugin:").or_else(|| owner.strip_prefix(crate::plugin_views::PANEL_PREFIX).and_then(|s|s.split(':').next())).or_else(|| owner.strip_prefix(crate::plugin_views::OWNER_PREFIX).and_then(|s| s.split(':').next()));
            if let Some(plugin) = plugin {
                let old = before.registry.installed.get(plugin).map(|p| &p.current.directory);
                let new = result.registry.installed.get(plugin).map(|p| &p.current.directory);
                if old != new || !app.plugins.enabled(plugin) || action == "recover" {
                    crate::browser::close(&handle, &app, &view.id);
                }
            }
        }
        crate::extensions::Runtime::reconcile(&handle, &app);
        let _ = handle.emit("plugins:changed", &result);
        Ok(result)
    }).await
}

#[tauri::command]
pub async fn plugin_run(
    app: AppState<'_>,
    project_id: String,
    worktree_id: String,
    plugin: String,
    contribution: String,
) -> Reply<String> {
    let app = Arc::clone(&app);
    blocking(move || {
        let project = app.project(&project_id)?;
        let worktree = app.worktree(&project, &worktree_id)?;
        let request =
            serde_json::json!({"api":1,"worktree":worktree.path,"project":project_id}).to_string();
        app.plugins
            .invoke(&plugin, &contribution, false, &request, &worktree.path)
            .map_err(error)
    })
    .await
}
#[tauri::command]
pub async fn plugin_session(
    app: AppState<'_>,
    plugin: String,
    backend: String,
    request: wtm_plugins::session::Request,
) -> Reply<serde_json::Value> {
    let app = Arc::clone(&app);
    blocking(move || {
        app.plugins
            .session_call(&plugin, &backend, &request, &app.config.paths().config_dir)
            .map_err(error)
    })
    .await
}
#[tauri::command]
pub async fn plugin_panel(
    handle: tauri::AppHandle,
    app: AppState<'_>,
    project_id: String,
    worktree_id: String,
    plugin: String,
    contribution: String,
) -> Reply<crate::view::BrowserView> {
    let app = Arc::clone(&app);
    blocking(move || {
        let project = app.project(&project_id)?;
        app.worktree(&project, &worktree_id)?;
        let _activation = app.plugins.activation(&plugin);
        let html = app.plugins.panel(&plugin, &contribution).map_err(error)?;
        crate::prototypes::validate_artifact(&html)?;
        let doc = crate::plugin_views::document(&html);
        crate::browser::open_prototype(
            &handle,
            &app,
            &project_id,
            &worktree_id,
            &doc,
            &format!(
                "{}{}:{}",
                crate::plugin_views::PANEL_PREFIX,
                plugin,
                contribution
            ),
            &format!("plugin:{plugin}:{contribution}"),
        )
    })
    .await
}
#[tauri::command]
pub async fn open_plugin_terminal(
    handle: tauri::AppHandle,
    app: AppState<'_>,
    project_id: String,
    worktree_id: String,
    plugin: String,
    contribution: String,
) -> Reply<String> {
    let app = Arc::clone(&app);
    blocking(move || {
        let _activation = app.plugins.activation(&plugin);
        let project = app.project(&project_id)?;
        let worktree = app.worktree(&project, &worktree_id)?;
        let argv = app
            .plugins
            .terminal(&plugin, &contribution)
            .map_err(error)?;
        let sink = crate::pty_bridge::EventSink::recording(handle, Arc::clone(&app));
        app.open_shell(&worktree, &project_id, argv, 24, 100, sink)
            .map(|s| s.as_str().to_owned())
            .map_err(|e| error(e.to_string()))
    })
    .await
}

#[tauri::command]
pub async fn open_native_cli(
    handle: tauri::AppHandle,
    app: AppState<'_>,
    project_id: String,
    worktree_id: String,
    provider: String,
    resume: Option<String>,
    integrated: Option<bool>,
) -> Reply<String> {
    let app = Arc::clone(&app);
    blocking(move || {
        if integrated == Some(false) {
            let project = app.project(&project_id)?;
            let worktree = app.worktree(&project, &worktree_id)?;
            if !project.offers_agent(&provider) {
                return Err(error("This repository does not offer this provider".into()));
            }
            let entry =
                wtm_agent::entry(&provider).ok_or_else(|| error("Unknown provider".into()))?;
            let executable = app
                .agent_executable(entry)
                .ok_or_else(|| error("Provider CLI is unavailable".into()))?;
            let sink = crate::pty_bridge::EventSink::recording(handle, Arc::clone(&app));
            return app
                .open_native_terminal(
                    &worktree,
                    &project_id,
                    wtm_core::ports::exec::Invocation::new(
                        vec![executable.to_string_lossy().into_owned()],
                        &worktree.path,
                        crate::commands::SHELL_TIMEOUT_MS,
                    )
                    .with_env(std::collections::BTreeMap::from([(
                        "TERM".into(),
                        "xterm-256color".into(),
                    )])),
                    sink,
                )
                .map(|s| s.as_str().to_owned())
                .map_err(|e| error(e.to_string()));
        }
        crate::native_sessions::open(handle, &app, &project_id, &worktree_id, &provider, resume)
    })
    .await
}
