//! An external view is an opaque document with an owner-scoped RPC, not privileged app JavaScript.
use crate::{
    app::App,
    commands::{AppState, Reply, blocking},
    prototypes::{self, Wrapper},
    view::{BrowserView, ErrorView},
};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use wtm_plugins::contributions::{Binding, Slot};

const SDK: &str = include_str!("../runtime/plugin-sdk.js");
pub const OWNER_PREFIX: &str = "plugin-slot:";
pub const PANEL_PREFIX: &str = "plugin-panel:";
/// The largest request a view may send, measured as the bytes the page posted.
const MAX_REQUEST: usize = 64 * 1024;
const TOO_LARGE: &str = "Request exceeds 64 KiB";
const TOO_MANY: &str = "This plugin already has 16 requests in flight; wait for one to finish";

/// A plugin view's frame. Forms are allowed because ordinary UI frameworks rely on their events;
/// the shared CSP still forbids submitting anywhere. The prototype marker stays because the
/// isolated runtime reports any fixed document ready by it.
const VIEW: Wrapper<'static> = Wrapper {
    title: "Plugin view",
    label: "Plugin view",
    markers: "data-wtm-plugin-view data-wtm-prototype-preview",
    sandbox: "allow-scripts allow-forms",
};

#[allow(clippy::needless_pass_by_value)]
fn failure(message: impl ToString) -> ErrorView {
    ErrorView::new("pluginView", message.to_string())
}

/// Whether a browser pane belongs to a plugin, by the owner the host gave it when it opened one.
///
/// Such a pane is the plugin's interface, carrying its authority over the request bridge, so no
/// agent may drive it and no other package may address it.
pub fn plugin_owned(owner: Option<&str>) -> bool {
    owner.is_some_and(|o| o.starts_with(OWNER_PREFIX) || o.starts_with(PANEL_PREFIX))
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
        if let Some(menu) = handle.menu() {
            // Best effort: a menu item that cannot be toggled is cosmetic, and failing here would
            // report the plugin change itself as failed after it was saved.
            let _ = crate::sync_home_menu(&menu, &app);
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
    prototypes::wrap(
        &format!("<!doctype html><script>{SDK}</script>{html}"),
        &VIEW,
    )
}

/// The package, contribution and (for a view) region a plugin pane was opened for, read from the
/// owner string the host gave it. `None` for any other owner, including one with more `:` parts
/// than the host ever writes, so a lookalike cannot be read as a different package.
pub fn identity(owner: &str) -> Option<(&str, &str, Option<Slot>)> {
    if let Some(rest) = owner.strip_prefix(PANEL_PREFIX) {
        let (plugin, panel) = rest.split_once(':')?;
        if plugin.is_empty() || panel.is_empty() || panel.contains(':') {
            return None;
        }
        return Some((plugin, panel, None));
    }
    let mut parts = owner.strip_prefix(OWNER_PREFIX)?.split(':');
    let plugin = parts.next().filter(|p| !p.is_empty())?;
    let contribution = parts.next().filter(|c| !c.is_empty())?;
    let slot = serde_json::from_value(Value::String(parts.next()?.into())).ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((plugin, contribution, Some(slot)))
}

fn owner_of(view: &BrowserView) -> Option<(&str, &str, Option<Slot>)> {
    identity(view.opened_by.as_deref()?)
}

/// The caller supplies only a webview id. Ownership and permissions come from host state.
///
/// `size` is the length of the message the page posted, so a large request is refused without
/// serialising it again. Every refusal is answered: a dropped request left the view's SDK waiting
/// out its timeout and then reporting that the outcome was unknown, which was never true.
pub fn dispatch(handle: &AppHandle, app: &Arc<App>, browser: &str, request: &Value, size: usize) {
    let Some(view) = app.browsers.view_of(browser) else {
        return;
    };
    let Some((plugin, _, _)) = owner_of(&view) else {
        return;
    };
    if size > MAX_REQUEST {
        reply(handle, browser, request, Err(TOO_LARGE.into()));
        return;
    }
    if !app.plugins.try_view_request(plugin) {
        reply(handle, browser, request, Err(TOO_MANY.into()));
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
        let result = call(&handle, &app, &browser, &request).map_err(|e| e.message);
        reply(&handle, &browser, &request, result);
    });
}

fn reply(handle: &AppHandle, browser: &str, request: &Value, result: Result<Value, String>) {
    // The SDK numbers its requests. An id that is not a plain number or short string was not
    // made by it, and echoing an arbitrary value back would hand the page a larger reply than
    // it sent.
    let Some(id) = request
        .get("id")
        .filter(|id| id.is_u64() || id.as_str().is_some_and(|s| s.len() <= 64))
    else {
        return;
    };
    let body = match result {
        Ok(result) => json!({"id": id, "result": result}),
        Err(error) => json!({"id": id, "error": error}),
    };
    crate::browser::fire(handle, browser, "pluginResponse", &body);
}

fn call(handle: &AppHandle, app: &Arc<App>, browser: &str, request: &Value) -> Reply<Value> {
    let page = app
        .browsers
        .view_of(browser)
        .ok_or_else(|| failure("View closed"))?;
    let (plugin, contribution, slot) =
        owner_of(&page).ok_or_else(|| failure("Not a plugin view"))?;
    // Revalidated on every request, so disabling or deselecting revokes a view at once, including
    // a webview still waiting to be closed. It reads no files: the document was checked at open.
    match slot {
        Some(slot) => app.plugins.view_selected(slot, plugin, contribution),
        None => app.plugins.panel_declared(plugin, contribution),
    }
    .map_err(failure)?;
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
            &json!({"projectId": page.project, "worktreeId": page.worktree}),
        )
        .map_err(failure),
        "catalog" => Ok(crate::extensions::catalog().clone()),
        "context" => Ok(json!({
            "api": 2,
            "plugin": plugin,
            "view": contribution,
            "slot": slot,
            "project": page.project,
            "worktree": page.worktree,
            "cwd": cwd,
        })),
        "workspace.projects" => projects(app, plugin),
        "workspace.worktrees" => {
            let id = params["project"]
                .as_str()
                .ok_or_else(|| failure("Project is required"))?;
            worktrees(app, plugin, id)
        }
        "command" => {
            let id = params["id"]
                .as_str()
                .ok_or_else(|| failure("Command id is required"))?;
            let wire = json!({
                "api": 1,
                "project": page.project,
                "worktree": page.worktree,
                "cwd": cwd,
                "params": params["input"],
            })
            .to_string();
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
            let navigation = json!({
                "target": target,
                "project": params["project"],
                "worktree": params["worktree"],
            });
            let _ = handle.emit("plugins:navigate", navigation);
            Ok(Value::Null)
        }
        _ => Err(failure("Unknown Plugin API 1 method")),
    }
}

/// Registered projects, or for a plugin limited to chosen workspaces, the ones holding one.
fn projects(app: &App, plugin: &str) -> Reply<Value> {
    let mut projects = app.projects()?;
    if let Some(scope) = app.plugins.extension_scope(plugin) {
        projects.retain(|p| {
            app.git
                .list_worktrees(Path::new(&p.root))
                .is_ok_and(|list| list.iter().any(|w| scope.contains(w.id.as_str())))
        });
    }
    serde_json::to_value(projects).map_err(failure)
}

/// A registered project's worktrees, filtered to the plugin's workspaces. Listing prunes, as the
/// sidebar does, which is why an unregistered directory is refused: Git would run there.
fn worktrees(app: &App, plugin: &str, project: &str) -> Reply<Value> {
    let project = crate::extensions::registered_project(app, project).map_err(failure)?;
    let mut worktrees = app.worktrees(&project)?;
    if let Some(scope) = app.plugins.extension_scope(plugin) {
        worktrees.retain(|w| scope.contains(&w.id));
    }
    serde_json::to_value(worktrees).map_err(failure)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_host_written_owner_names_its_package_contribution_and_region() {
        assert_eq!(
            identity("plugin-panel:example.tools:notes"),
            Some(("example.tools", "notes", None))
        );
        assert_eq!(
            identity("plugin-slot:example.tools:summary:worktree_bar"),
            Some(("example.tools", "summary", Some(Slot::WorktreeBar)))
        );
    }

    #[test]
    fn an_owner_the_host_would_not_write_names_no_plugin() {
        for owner in [
            "plugin:example.tools:notes",
            "plugin-panel:example.tools",
            "plugin-panel:example.tools:notes:extra",
            "plugin-panel::notes",
            "plugin-slot:example.tools:summary",
            "plugin-slot:example.tools:summary:not_a_region",
            "plugin-slot:example.tools:summary:code:extra",
            "plugin-slot::summary:code",
            "session-1234",
        ] {
            assert_eq!(identity(owner), None, "{owner}");
        }
    }

    #[test]
    fn panels_and_views_are_plugin_owned_and_agent_sessions_are_not() {
        assert!(plugin_owned(Some("plugin-panel:example.tools:notes")));
        assert!(plugin_owned(Some("plugin-slot:example.tools:summary:code")));
        assert!(!plugin_owned(Some("session-1234")));
        assert!(!plugin_owned(None));
    }
}
