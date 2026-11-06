//! An external view is an opaque document with an owner-scoped RPC, not privileged app JavaScript.
use crate::{
    app::App,
    commands::{AppState, Reply, blocking},
    view::{BrowserView, ErrorView},
};
use serde_json::{Value, json};
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use wtm_plugins::contributions::{Binding, Slot};

const SDK: &str = include_str!("../runtime/plugin-sdk.js");
pub const OWNER_PREFIX: &str = "plugin-slot:";
pub const PANEL_PREFIX: &str = "plugin-panel:";

#[allow(clippy::needless_pass_by_value)]
fn failure(message: impl ToString) -> ErrorView {
    ErrorView::new("pluginView", message.to_string())
}

#[tauri::command]
pub async fn plugin_bind(
    handle: AppHandle,
    app: AppState<'_>,
    slot: Slot,
    binding: Binding,
) -> Reply<wtm_plugins::Snapshot> {
    let app = Arc::clone(&app);
    blocking(move || {
        let state = app.plugins.bind(slot, binding).map_err(failure)?;
        if let Some(menu) = handle.menu()
            && let Some(item) = menu
                .get(crate::HOME_EVENT)
                .and_then(|i| i.as_menuitem().cloned())
        {
            let _ = item.set_enabled(app.plugins.binding(Slot::Home) != Binding::Hidden);
        }
        let _ = handle.emit("plugins:changed", &state);
        Ok(state)
    })
    .await
}

#[tauri::command]
pub async fn plugin_view(
    handle: AppHandle,
    app: AppState<'_>,
    project: String,
    worktree: String,
    slot: Slot,
    plugin: String,
    view: String,
) -> Reply<BrowserView> {
    let app = Arc::clone(&app);
    blocking(move || {
        let _activation = app.plugins.activation(&plugin);
        if !project.is_empty() && !worktree.is_empty() {
            let p = app.project(&project)?;
            app.worktree(&p, &worktree)?;
        }
        let html = app.plugins.view(slot, &plugin, &view).map_err(failure)?;
        crate::prototypes::validate_artifact(&html)?;
        let slot = serde_json::to_value(slot).map_err(failure)?;
        let slot = slot.as_str().ok_or_else(|| failure("Invalid region"))?;
        let owner = format!("{OWNER_PREFIX}{plugin}:{view}:{slot}");
        let identity = format!("{owner}/{}", uuid::Uuid::new_v4());
        let document = document(&html);
        let result = crate::browser::open_prototype(
            &handle, &app, &project, &worktree, &document, &owner, &identity,
        );
        if result.is_err() {
            for page in app.browsers.list(None) {
                if page.prototype_id.as_deref() == Some(&identity) {
                    crate::browser::close(&handle, &app, &page.id);
                }
            }
        }
        result
    })
    .await
}

pub fn document(html: &str) -> String {
    let html = format!("<!doctype html><script>{SDK}</script>{html}");
    crate::prototypes::review_document(&html)
        .replace(
            "<title>Prototype review</title>",
            "<title>Plugin view</title>",
        )
        .replace(
            "data-wtm-prototype-preview",
            "data-wtm-plugin-view data-wtm-prototype-preview",
        )
        .replace("Reviewed design snapshot", "Plugin view")
        // Form events are needed by ordinary UI frameworks; CSP still forbids submission URLs.
        .replace(
            "sandbox=\"allow-scripts\"",
            "sandbox=\"allow-scripts allow-forms\"",
        )
}

fn identity(view: &BrowserView) -> Option<(&str, &str, Option<Slot>)> {
    if let Some(owner) = view
        .opened_by
        .as_deref()
        .and_then(|o| o.strip_prefix(PANEL_PREFIX))
    {
        let (plugin, panel) = owner.split_once(':')?;
        if panel.contains(':') {
            return None;
        }
        return Some((plugin, panel, None));
    }
    let mut parts = view
        .opened_by
        .as_deref()?
        .strip_prefix(OWNER_PREFIX)?
        .split(':');
    let plugin = parts.next()?;
    let contribution = parts.next()?;
    let slot = serde_json::from_value(Value::String(parts.next()?.into())).ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((plugin, contribution, Some(slot)))
}

/// The caller supplies only a webview id. Ownership and permissions come from host state.
pub fn dispatch(handle: &AppHandle, app: &Arc<App>, browser: &str, request: &Value) {
    if request.to_string().len() > 65536 {
        return;
    }
    let Some(view) = app.browsers.view_of(browser) else {
        return;
    };
    let Some((plugin, _, _)) = identity(&view) else {
        return;
    };
    if !app.plugins.try_view_request(plugin) {
        return;
    }
    let (handle, app, browser, request, plugin) = (
        handle.clone(),
        Arc::clone(app),
        browser.to_owned(),
        request.clone(),
        plugin.to_owned(),
    );
    tauri::async_runtime::spawn_blocking(move || {
        struct Finish<'a>(&'a wtm_plugins::Store, &'a str);
        impl Drop for Finish<'_> {
            fn drop(&mut self) {
                self.0.finish_view_request(self.1);
            }
        }
        let _finish = Finish(&app.plugins, &plugin);
        let result = call(&handle, &app, &browser, &request);
        let reply = match result {
            Ok(result) => json!({"id": request["id"], "result": result}),
            Err(error) => json!({"id": request["id"], "error": error.message}),
        };
        crate::browser::fire(&handle, &browser, "pluginResponse", &reply);
    });
}

fn call(handle: &AppHandle, app: &Arc<App>, browser: &str, request: &Value) -> Reply<Value> {
    let page = app
        .browsers
        .view_of(browser)
        .ok_or_else(|| failure("View closed"))?;
    let (plugin, contribution, slot) =
        identity(&page).ok_or_else(|| failure("Not a plugin view"))?;
    let _activation = app.plugins.activation(plugin);
    // Revalidation revokes a view immediately, including a stale webview waiting to be closed.
    if let Some(slot) = slot {
        app.plugins
            .view(slot, plugin, contribution)
            .map_err(failure)?;
    } else {
        app.plugins.panel(plugin, contribution).map_err(failure)?;
    }
    let params = &request["params"];
    let cwd = if page.project.is_empty() || page.worktree.is_empty() {
        app.config.paths().config_dir.clone()
    } else {
        let project = app.project(&page.project)?;
        app.worktree(&project, &page.worktree)?.path
    };
    match request["method"].as_str().unwrap_or_default() {
        "host" => crate::extensions::Runtime::host(
            handle,
            app,
            plugin,
            params["operation"].as_str().unwrap_or_default(),
            &params["arguments"],
            &json!({"projectId":page.project,"worktreeId":page.worktree}),
        )
        .map_err(failure),
        "catalog" => Ok(crate::extensions::catalog()),
        "context" => Ok(
            json!({"api":2,"plugin":plugin,"view":contribution,"slot":slot,"project":page.project,"worktree":page.worktree,"cwd":cwd}),
        ),
        "workspace.projects" => serde_json::to_value(app.projects()?).map_err(failure),
        "workspace.worktrees" => {
            let id = params["project"]
                .as_str()
                .ok_or_else(|| failure("Project is required"))?;
            let project = app.project(id)?;
            serde_json::to_value(app.worktrees(&project)?).map_err(failure)
        }
        "command" => {
            let id = params["id"]
                .as_str()
                .ok_or_else(|| failure("Command id is required"))?;
            let wire = json!({"api":1,"project":page.project,"worktree":page.worktree,"cwd":cwd,"params":params["input"]}).to_string();
            app.plugins
                .invoke(plugin, id, false, &wire, &cwd)
                .map(Value::String)
                .map_err(failure)
        }
        "session" => {
            let backend = params["backend"]
                .as_str()
                .ok_or_else(|| failure("Backend id is required"))?;
            let mut call: wtm_plugins::session::Request =
                serde_json::from_value(params["request"].clone()).map_err(failure)?;
            if !call.params.is_object() {
                call.params = json!({});
            }
            call.params["client"] = Value::String(browser.into());
            let result = app
                .plugins
                .session_call(plugin, backend, &call, &cwd)
                .map_err(failure);
            if app.browsers.view_of(browser).is_none() {
                app.plugins.release_client(browser);
            }
            result
        }
        "navigate" => {
            let target = params["target"]
                .as_str()
                .ok_or_else(|| failure("Navigation target is required"))?;
            if !["home", "worktree", "code", "database", "new", "settings"].contains(&target) {
                return Err(failure("Unknown navigation target"));
            }
            let _ = handle.emit(
                "plugins:navigate",
                json!({"target":target,"project":params["project"],"worktree":params["worktree"]}),
            );
            Ok(Value::Null)
        }
        _ => Err(failure("Unknown Plugin API 1 method")),
    }
}
