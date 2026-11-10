//! Plugin entry points share the same trust and process adapters as the rest of the app.
use crate::app::App;
use crate::commands::{AppState, Reply, blocking};
use crate::extensions::Runtime;
use crate::plugin_views::identity;
use crate::view::ErrorView;
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use wtm_plugins::{Review, Snapshot};

const CLOSE_HOME_FIRST: &str =
    "Close Home's running sessions before disabling Home. Saved conversations are kept.";

fn error(message: String) -> ErrorView {
    ErrorView::new("plugin", message)
}

#[tauri::command]
pub async fn plugins_state(app: AppState<'_>) -> Reply<Snapshot> {
    let app = Arc::clone(&app);
    blocking(move || Ok(app.plugins.snapshot())).await
}
/// Prepare a review. `token`, a UUID the caller chose, lets [`plugin_cancel`] stop the
/// preparation while a clone is still running; without one there is nothing to cancel until the
/// review exists.
#[tauri::command]
pub async fn plugin_prepare(
    app: AppState<'_>,
    source: String,
    kind: String,
    revision: String,
    token: Option<String>,
) -> Reply<Review> {
    let app = Arc::clone(&app);
    // A local folder is typed the way the Create dialog's is, so `~` means the home folder in
    // both. Git sources are URLs and are left exactly as given.
    let source = if kind == "local" {
        crate::app::expand_tilde(std::path::Path::new(&source))
            .to_string_lossy()
            .into_owned()
    } else {
        source
    };
    blocking(move || {
        app.plugins
            .prepare_as(&source, &kind, &revision, token.as_deref())
            .map_err(error)
    })
    .await
}
/// Stop a preparation in flight, or discard a finished review. Either way its copy is removed.
#[tauri::command]
pub async fn plugin_cancel(app: AppState<'_>, token: String) -> Reply<()> {
    let app = Arc::clone(&app);
    blocking(move || app.plugins.cancel(&token).map_err(error)).await
}

/// Close a package's panes and views and stop its runtime.
///
/// Disable, uninstall and recovery do this before they change the registry, which releases the
/// package's sessions in the same step, and none of them is refused because a call is running: a
/// misbehaving package must not be able to hold off the switch that turns it off. Calls still in
/// flight fail their revalidation when they finish.
fn revoke(handle: &AppHandle, app: &Arc<App>, plugin: &str) {
    for view in app.browsers.list(None) {
        let owner = view.opened_by.as_deref().and_then(identity);
        if owner.is_some_and(|(owner, _, _)| owner == plugin) {
            crate::browser::close(handle, app, &view.id);
        }
    }
    app.extensions.reset(app, plugin);
}

fn directory(snapshot: &Snapshot, plugin: &str) -> Option<String> {
    snapshot
        .registry
        .installed
        .get(plugin)
        .map(|p| p.current.directory.clone())
}

#[tauri::command]
pub async fn plugin_change(
    handle: AppHandle,
    app: AppState<'_>,
    action: String,
    id: String,
    enabled: bool,
) -> Reply<Snapshot> {
    let app = Arc::clone(&app);
    blocking(move || {
        if action == "enable" && !enabled && id == "wtm.home" && app.has_home_sessions() {
            return Err(error(CLOSE_HOME_FIRST.into()));
        }
        let before = app.plugins.snapshot();
        match action.as_str() {
            "recover" => {
                for plugin in before.registry.installed.keys() {
                    revoke(&handle, &app, plugin);
                }
            }
            "uninstall" => revoke(&handle, &app, &id),
            "enable" if !enabled => revoke(&handle, &app, &id),
            // The old process must not answer for a new digest, so it stops before code changes.
            "rollback" | "enable" => app.extensions.reset(&app, &id),
            "install" => {
                if let Some(review) = app.plugins.review(&id) {
                    app.extensions.reset(&app, &review.package.manifest.id);
                }
            }
            _ => {}
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
        let result = match result {
            Ok(result) => result,
            Err(e) => {
                Runtime::reconcile(&handle, &app);
                return Err(error(e));
            }
        };
        for plugin in result.registry.installed.keys() {
            if directory(&before, plugin) != directory(&result, plugin) {
                app.extensions.reset(&app, plugin);
            }
        }
        // A view of a package whose code changed, or that is no longer enabled, is stale.
        for view in app.browsers.list(None) {
            let Some((plugin, _, _)) = view.opened_by.as_deref().and_then(identity) else {
                continue;
            };
            if directory(&before, plugin) != directory(&result, plugin)
                || !app.plugins.enabled(plugin)
            {
                crate::browser::close(&handle, &app, &view.id);
            }
        }
        if let Some(menu) = handle.menu() {
            // Best effort: a menu item that cannot be toggled is cosmetic, and failing here would
            // report the plugin change itself as failed after it was saved.
            let _ = crate::sync_home_menu(&menu, &app);
        }
        Runtime::reconcile(&handle, &app);
        let _ = handle.emit("plugins:changed", &result);
        Ok(result)
    })
    .await
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
        let invocation = app
            .plugins
            .terminal_invocation(&plugin, &contribution, &worktree.path)
            .map_err(error)?;
        let sink = crate::pty_bridge::EventSink::recording(handle, Arc::clone(&app));
        app.open_native_terminal(&worktree, &project_id, invocation, sink)
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
