//! Panes in windows of their own.
//!
//! A shell, an agent chat or a browser can leave the main window's tiling for an OS window, and go
//! back. This module is the meeting point: it makes the windows, remembers which pane each one
//! holds, and tells the main window when one goes away.
//!
//! # Who owns a pane while it is out
//!
//! The pane *window* does. It runs the same frontend as the main window, holding one pane, and every
//! change to that pane — a message sent, a model picked, a restart — is made there. The main window
//! keeps its record of the pane and goes on recording the session's events, which is what keeps the
//! sidebar, the dock badge and notifications right for a pane it is not showing, and what means a
//! pane coming back already has its whole transcript.
//!
//! What the events do not carry — the composer draft, the queue, the model picked — the pane window
//! sends here as [`Entry::state`], and this module passes it to the main window. The blob is opaque
//! on purpose: it is the frontend's own record of a pane, and a typed mirror of it here would be a
//! second copy of `sessions.svelte.ts` that nothing on this side ever reads.
//!
//! # Why closing the window puts the pane back
//!
//! Because a session is expensive to lose and cheap to close deliberately: the pane still has its
//! own Close button. So every route that ends a pane window — its Put back button, the traffic
//! light, ⌘W, the main window's Put back — goes through [`tauri::Window::close`], which fires
//! `CloseRequested`, and never through `destroy`, which does not. `CloseRequested` is where a browser
//! held by the window is moved back to the main window; Tauri destroys every webview a closing
//! window holds, and a browser destroyed that way would take the user's page with it.
//!
//! # Lock discipline
//!
//! `browser.rs`'s rule, for the same reason: the registry lock is held to copy facts out and never
//! across a Tauri call. The window-event callback runs on the main thread and takes this lock, so
//! a command holding it across `build` or `close` — both of which wait on the main thread — would
//! deadlock.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager, WindowEvent};

use crate::app::App;
use crate::browser::Bounds;
use crate::view::ErrorView;

/// Every pane window's label starts with this. `capabilities/default.json` names it with a glob.
///
/// Distinct from `browser::LABEL_PREFIX`, and `tests/capability_set.rs` proves the glob can never
/// match a browser's label: the capability is what lets this app's own frontend run in a pane
/// window, and a browser must never be named by it.
pub const LABEL_PREFIX: &str = "popout-";

/// The window every pane starts in and goes back to.
pub const MAIN_WINDOW: &str = "main";

/// Sent to the main window when a pane window has gone, with the pane's last state.
pub const CLOSED_EVENT: &str = "pane-window:closed";

/// Sent to the main window when a pane window reports a change to its pane.
pub const SYNCED_EVENT: &str = "pane-window:synced";

/// Sent to the main window when a pane window gains or loses focus.
///
/// So attention can tell a pane the user is watching in its own window from one nobody is looking
/// at. The main window's own focus is read from the DOM, which cannot see another window.
pub const FOCUS_EVENT: &str = "pane-window:focus";

/// The smallest a pane window opens, in logical pixels.
///
/// The main window's own minimum is for a sidebar, a title bar and a tiling; one pane needs far
/// less, and a tile a third of the main window's width is a perfectly good terminal.
pub const MIN_WIDTH: f64 = 360.0;
pub const MIN_HEIGHT: f64 = 240.0;

/// One pane window.
#[derive(Debug)]
struct Entry {
    pane_id: String,
    /// The frontend's latest record of the pane. See the module docs for why it is opaque.
    state: Value,
    /// Set once the window has been asked to close.
    ///
    /// A browser pane in a closing window must not be adopted by it again: its frontend is still
    /// running for a moment, and a bounds update from it arriving after the browser was moved home
    /// would carry the browser back into a window about to destroy it.
    closing: bool,
}

/// Which pane each pane window holds.
#[derive(Debug, Default)]
pub struct Registry {
    entries: parking_lot::Mutex<BTreeMap<String, Entry>>,
}

impl Registry {
    /// Remember a new window. `false` if one with that label is already remembered.
    fn insert(&self, label: &str, pane_id: &str, state: Value) -> bool {
        let mut entries = self.entries.lock();
        if entries.contains_key(label) {
            return false;
        }
        entries.insert(
            label.to_owned(),
            Entry {
                pane_id: pane_id.to_owned(),
                state,
                closing: false,
            },
        );
        true
    }

    /// The pane a window holds and its latest state.
    #[must_use]
    pub fn state_of(&self, label: &str) -> Option<(String, Value)> {
        self.entries
            .lock()
            .get(label)
            .map(|entry| (entry.pane_id.clone(), entry.state.clone()))
    }

    /// Replace a window's state, returning the pane it holds.
    fn sync(&self, label: &str, state: Value) -> Option<String> {
        let mut entries = self.entries.lock();
        let entry = entries.get_mut(label)?;
        entry.state = state;
        Some(entry.pane_id.clone())
    }

    fn pane_of(&self, label: &str) -> Option<String> {
        self.entries
            .lock()
            .get(label)
            .map(|entry| entry.pane_id.clone())
    }

    fn mark_closing(&self, label: &str) {
        if let Some(entry) = self.entries.lock().get_mut(label) {
            entry.closing = true;
        }
    }

    /// Whether a window is on its way out. A window this registry does not know counts as one.
    #[must_use]
    pub fn is_closing(&self, label: &str) -> bool {
        self.entries
            .lock()
            .get(label)
            .is_none_or(|entry| entry.closing)
    }

    fn remove(&self, label: &str) -> Option<(String, Value)> {
        self.entries
            .lock()
            .remove(label)
            .map(|entry| (entry.pane_id, entry.state))
    }

    /// Every pane window, for a main window that has reloaded and lost track of them.
    #[must_use]
    pub fn list(&self) -> Vec<(String, Value)> {
        self.entries
            .lock()
            .values()
            .filter(|entry| !entry.closing)
            .map(|entry| (entry.pane_id.clone(), entry.state.clone()))
            .collect()
    }
}

/// A pane window, as the frontend sees it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaneWindowView {
    pub pane_id: String,
    pub state: Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct FocusPayload {
    pane_id: String,
    focused: bool,
}

/// The window label for a pane, or `None` for an id this app would never have minted.
///
/// Refused rather than escaped because a pane id is `pane-N` and nothing else; anything outside
/// that alphabet reached here from somewhere it should not have, and a label is also a capability
/// match target.
#[must_use]
pub fn label_for(pane_id: &str) -> Option<String> {
    let valid = !pane_id.is_empty()
        && pane_id.len() <= 64
        && pane_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    valid.then(|| format!("{LABEL_PREFIX}{pane_id}"))
}

#[must_use]
pub fn is_pane_window(label: &str) -> bool {
    label.starts_with(LABEL_PREFIX)
}

/// Where a pane window opens: over the tile it came from, in screen coordinates.
///
/// Opening on top of the tile is what makes the gesture read as *lifting the pane out* rather than
/// as a new window appearing somewhere. `origin` is the main window's content area on screen and
/// `tile` is in its CSS pixels, both logical, so nothing here multiplies by a scale factor.
#[must_use]
pub fn popout_frame(origin: (f64, f64), tile: Bounds, min: (f64, f64)) -> Bounds {
    Bounds {
        x: origin.0 + tile.x,
        y: origin.1 + tile.y,
        w: tile.w.max(min.0),
        h: tile.h.max(min.1),
    }
}

/// What a browser's bounds update means, given which window sent it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// The window holding the browser wants it shown here.
    Place,
    /// The window holding the browser wants it hidden.
    Hide,
    /// Another window wants the browser: move it there, then place it.
    Adopt,
    /// Nothing to do.
    Ignore,
}

/// Decide what a bounds update from `caller` does to a browser held by `host`.
///
/// # Why a hide from another window is ignored
///
/// Because it is always stale. When a pane moves, the window it left unmounts its browser pane,
/// and that unmount sends a hide; the window it arrived in sends a show. Nothing orders the two, and
/// a hide that lands second would blank a page the user just moved. Only the window holding a
/// browser can hide it.
///
/// # Why a closing window cannot adopt
///
/// See [`Entry::closing`].
#[must_use]
pub fn placement(host: &str, caller: &str, caller_closing: bool, show: bool) -> Placement {
    match (host == caller, show) {
        (true, true) => Placement::Place,
        (true, false) => Placement::Hide,
        (false, false) => Placement::Ignore,
        (false, true) if caller_closing => Placement::Ignore,
        (false, true) => Placement::Adopt,
    }
}

fn window_error(what: &str, error: &tauri::Error) -> ErrorView {
    ErrorView::new("window", format!("could not {what}: {error}"))
}

/// Move a pane into a window of its own, or bring its window forward if it already has one.
///
/// Main-window only: a pane window holds one pane and has nowhere to move another from.
///
/// # Errors
///
/// If the caller is not the main window, the pane id is not one this app mints, or the window
/// cannot be built.
pub fn pop_out(
    handle: &AppHandle,
    app: &Arc<App>,
    caller: &tauri::Window,
    pane_id: &str,
    title: &str,
    state: Value,
    rect: Bounds,
) -> Result<(), ErrorView> {
    if caller.label() != MAIN_WINDOW {
        return Err(ErrorView::new(
            "window",
            "only the main window can move a pane out",
        ));
    }
    let label = label_for(pane_id)
        .ok_or_else(|| ErrorView::new("window", "that is not a pane this app opened"))?;
    if let Some(existing) = handle.get_webview_window(&label) {
        return existing
            .set_focus()
            .map_err(|e| window_error("bring the pane's window forward", &e));
    }

    let scale = caller
        .scale_factor()
        .map_err(|e| window_error("read the display scale", &e))?;
    let origin = caller
        .inner_position()
        .map_err(|e| window_error("find the main window", &e))?
        .to_logical::<f64>(scale);
    let frame = popout_frame((origin.x, origin.y), rect, (MIN_WIDTH, MIN_HEIGHT));

    // The main window's own configuration, re-labelled and re-sized, so `tauri.conf.json` stays the
    // one place the chrome is described: the overlay title bar, the traffic lights' inset, the
    // vibrancy, file drops. A builder spelling those out again would drift from it the first time
    // either changed.
    let mut config = handle
        .config()
        .app
        .windows
        .iter()
        .find(|window| window.label == MAIN_WINDOW)
        .cloned()
        .ok_or_else(|| ErrorView::new("window", "the main window's configuration is missing"))?;
    config.label.clone_from(&label);
    title.clone_into(&mut config.title);
    config.x = Some(frame.x);
    config.y = Some(frame.y);
    config.width = frame.w;
    config.height = frame.h;
    config.min_width = Some(MIN_WIDTH);
    config.min_height = Some(MIN_HEIGHT);
    config.center = false;

    // Remembered before the build, because the window's frontend asks for its state as soon as it
    // runs, and that can be before `build` has returned here.
    if !app.pane_windows.insert(&label, pane_id, state) {
        return Err(ErrorView::new(
            "window",
            "that pane is already on its way out",
        ));
    }
    let built = tauri::WebviewWindowBuilder::from_config(handle, &config)
        .and_then(tauri::WebviewWindowBuilder::build);
    if let Err(error) = built {
        app.pane_windows.remove(&label);
        return Err(window_error("open a window for the pane", &error));
    }
    tracing::debug!(%label, "pane moved to its own window");
    Ok(())
}

/// The pane a pane window holds and its state, for that window's frontend as it starts.
#[must_use]
pub fn state_for(app: &App, caller: &tauri::Window) -> Option<PaneWindowView> {
    app.pane_windows
        .state_of(caller.label())
        .map(|(pane_id, state)| PaneWindowView { pane_id, state })
}

/// Record a pane window's latest state and hand it to the main window.
pub fn sync(handle: &AppHandle, app: &App, caller: &tauri::Window, state: Value) {
    let Some(pane_id) = app.pane_windows.sync(caller.label(), state.clone()) else {
        return;
    };
    if let Err(error) = handle.emit_to(MAIN_WINDOW, SYNCED_EVENT, PaneWindowView { pane_id, state })
    {
        tracing::debug!(%error, "could not tell the main window about a pane window's change");
    }
}

/// Put a pane back by closing its window. See the module docs for why this is always `close`.
///
/// # Errors
///
/// If the window exists and refuses to close.
pub fn close(handle: &AppHandle, app: &App, pane_id: &str) -> Result<(), ErrorView> {
    let Some(label) = label_for(pane_id) else {
        return Ok(());
    };
    if let Some(window) = handle.get_webview_window(&label) {
        return window
            .close()
            .map_err(|e| window_error("close the pane's window", &e));
    }
    // Remembered with no window behind it: the build failed after the insert, or the window died
    // without an event. Report it gone so the pane is not stranded outside every window.
    if let Some((pane_id, state)) = app.pane_windows.remove(&label) {
        announce_closed(handle, pane_id, state);
    }
    Ok(())
}

/// Bring a pane's window forward, or the main window when `pane_id` is `None`.
///
/// # Errors
///
/// If there is no such window, or it cannot be focused.
pub fn focus(handle: &AppHandle, pane_id: Option<&str>) -> Result<(), ErrorView> {
    let label = match pane_id {
        Some(pane_id) => label_for(pane_id)
            .ok_or_else(|| ErrorView::new("window", "that is not a pane this app opened"))?,
        None => MAIN_WINDOW.to_owned(),
    };
    let window = handle
        .get_webview_window(&label)
        .ok_or_else(|| ErrorView::new("window", "that window is no longer open"))?;
    // A minimised window ignores focus, so it has to be brought back first.
    let _ = window.unminimize();
    window
        .set_focus()
        .map_err(|e| window_error("bring the window forward", &e))
}

fn announce_closed(handle: &AppHandle, pane_id: String, state: Value) {
    if let Err(error) = handle.emit_to(MAIN_WINDOW, CLOSED_EVENT, PaneWindowView { pane_id, state })
    {
        tracing::debug!(%error, "could not tell the main window a pane window closed");
    }
}

/// Every window event, for the main window and for pane windows.
pub fn on_window_event(window: &tauri::Window, event: &WindowEvent) {
    let label = window.label();
    let handle = window.app_handle();

    if label == MAIN_WINDOW {
        // Closing the main window has always quit the app, because it was the last window and Tauri
        // exits when the last one goes. A pane window would otherwise keep the process alive with no
        // way back to the main window, so it is made to keep meaning that.
        if matches!(event, WindowEvent::Destroyed)
            && handle.webview_windows().keys().any(|l| is_pane_window(l))
        {
            handle.exit(0);
        }
        return;
    }
    if !is_pane_window(label) {
        return;
    }
    let Some(app) = handle.try_state::<Arc<App>>() else {
        return;
    };

    match event {
        WindowEvent::CloseRequested { .. } => {
            app.pane_windows.mark_closing(label);
            crate::browser::rehome(handle, &app, label);
        }
        WindowEvent::Destroyed => {
            if let Some((pane_id, state)) = app.pane_windows.remove(label) {
                announce_closed(handle, pane_id, state);
            }
        }
        WindowEvent::Focused(focused) => {
            if let Some(pane_id) = app.pane_windows.pane_of(label) {
                let payload = FocusPayload {
                    pane_id,
                    focused: *focused,
                };
                if let Err(error) = handle.emit_to(MAIN_WINDOW, FOCUS_EVENT, payload) {
                    tracing::debug!(%error, "could not report a pane window's focus");
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hide_from_a_window_that_no_longer_hosts_the_browser_is_ignored() {
        // The main window unmounting a pane it just gave away sends exactly this, and it can land
        // after the pane window's show. Obeying it would blank the page the user just moved.
        assert_eq!(
            placement("popout-pane-3", MAIN_WINDOW, false, false),
            Placement::Ignore
        );
        assert_eq!(
            placement(MAIN_WINDOW, "popout-pane-3", true, false),
            Placement::Ignore
        );
    }

    #[test]
    fn a_closing_window_can_never_adopt_a_browser() {
        // The browser has just been moved home; carrying it back would put it in a window that is
        // about to destroy every webview it holds.
        assert_eq!(
            placement(MAIN_WINDOW, "popout-pane-3", true, true),
            Placement::Ignore
        );
        assert_eq!(
            placement(MAIN_WINDOW, "popout-pane-3", false, true),
            Placement::Adopt
        );
    }

    #[test]
    fn the_window_holding_a_browser_places_and_hides_it_as_before() {
        assert_eq!(
            placement(MAIN_WINDOW, MAIN_WINDOW, false, true),
            Placement::Place
        );
        assert_eq!(
            placement(MAIN_WINDOW, MAIN_WINDOW, false, false),
            Placement::Hide
        );
    }

    #[test]
    fn a_pop_out_label_is_derived_from_a_pane_id_and_refuses_anything_else() {
        assert_eq!(label_for("pane-12").as_deref(), Some("popout-pane-12"));
        assert!(is_pane_window("popout-pane-12"));
        for bad in [
            "",
            "pane 1",
            "../main",
            "pane-1*",
            "main\n",
            &"x".repeat(65),
        ] {
            assert_eq!(label_for(bad), None, "{bad:?} must be refused");
        }
    }

    #[test]
    fn a_pop_out_opens_over_its_tile_and_never_smaller_than_the_minimum() {
        let tile = Bounds {
            x: 300.0,
            y: 80.0,
            w: 200.0,
            h: 500.0,
        };
        let frame = popout_frame((1000.0, 40.0), tile, (MIN_WIDTH, MIN_HEIGHT));
        assert_eq!(
            frame,
            Bounds {
                x: 1300.0,
                y: 120.0,
                w: MIN_WIDTH,
                h: 500.0,
            }
        );
    }

    #[test]
    fn a_window_the_registry_does_not_know_counts_as_closing() {
        // Which is what stops a stray label from adopting a browser at all.
        let registry = Registry::default();
        assert!(registry.is_closing("popout-pane-1"));
        assert!(registry.insert("popout-pane-1", "pane-1", Value::Null));
        assert!(!registry.is_closing("popout-pane-1"));
        assert!(
            !registry.insert("popout-pane-1", "pane-1", Value::Null),
            "one window per pane"
        );
        registry.mark_closing("popout-pane-1");
        assert!(registry.is_closing("popout-pane-1"));
        assert!(
            registry.list().is_empty(),
            "a closing window is not reported to a reloading main window"
        );
    }

    #[test]
    fn a_pane_windows_latest_state_is_what_the_main_window_gets_back() {
        let registry = Registry::default();
        registry.insert(
            "popout-pane-1",
            "pane-1",
            serde_json::json!({ "draft": "" }),
        );
        assert_eq!(
            registry.sync(
                "popout-pane-1",
                serde_json::json!({ "draft": "half a thought" })
            ),
            Some("pane-1".to_owned())
        );
        assert_eq!(
            registry.remove("popout-pane-1"),
            Some((
                "pane-1".to_owned(),
                serde_json::json!({ "draft": "half a thought" })
            ))
        );
        assert_eq!(registry.sync("popout-pane-1", Value::Null), None);
    }
}
