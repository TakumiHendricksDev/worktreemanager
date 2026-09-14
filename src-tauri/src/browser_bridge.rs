//! Telling the window about its browser panes.
//!
//! The counterpart to [`crate::agent_bridge`], and smaller for a reason: a browser has no
//! transcript to stream. What the window needs to hear is that a browser's facts changed (URL,
//! title, loading, the agent toggle), that one exists which the window did not ask for, and that
//! one is gone. Three events, each carrying a whole [`BrowserView`] or an id — never a delta the
//! frontend would have to merge.
//!
//! Every emit failure is downgraded to `tracing::debug!`, as `agent_bridge` does: a failed emit
//! means the window is gone, and a page-load callback on the main thread is the wrong place to
//! do anything about that.

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::view::BrowserView;

/// A browser's facts changed. Payload: the whole [`BrowserView`].
pub const BROWSER_STATE_EVENT: &str = "browser:state";

/// A browser exists that the window did not open — an agent asked for it. Payload: [`BrowserView`].
///
/// The same inversion `agent:spawned` handles: the frontend adopts a pane for something Rust
/// already created, rather than being handed an id it asked for.
pub const BROWSER_OPENED_EVENT: &str = "browser:opened";

/// A browser is gone.
pub const BROWSER_CLOSED_EVENT: &str = "browser:closed";

/// A browser's comments changed. Payload: the browser id and the whole list, never a delta.
pub const BROWSER_COMMENTS_EVENT: &str = "browser:comments";

/// The user clicked an element in comment mode, or an existing pin. Payload: [`BrowserPick`].
pub const BROWSER_PICK_EVENT: &str = "browser:pick";

/// A chord or gesture happened inside the page, where the app's own document cannot see it.
pub const BROWSER_SHORTCUT_EVENT: &str = "browser:shortcut";

/// Why a browser went, when the window did not ask for it to.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserClosed {
    pub id: String,
    /// `None` when the close was ordinary; a sentence when the pane should say why it ended.
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserComments<'a> {
    pub id: &'a str,
    pub comments: &'a [crate::browser::Comment],
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserPick {
    pub id: String,
    /// The pin that was clicked, when one was; `None` for a fresh element pick.
    pub comment_id: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserShortcut<'a> {
    pub id: &'a str,
    /// One of the runtime's action names; the frontend's `BrowserShortcut` union mirrors them.
    pub action: &'a str,
}

pub fn announce_comments(handle: &AppHandle, id: &str, comments: &[crate::browser::Comment]) {
    if let Err(error) = handle.emit(BROWSER_COMMENTS_EVENT, BrowserComments { id, comments }) {
        tracing::debug!(%error, "could not announce a browser's comments");
    }
}

pub fn announce_pick(handle: &AppHandle, id: &str, comment_id: Option<u32>) {
    let payload = BrowserPick {
        id: id.to_owned(),
        comment_id,
    };
    if let Err(error) = handle.emit(BROWSER_PICK_EVENT, &payload) {
        tracing::debug!(%error, "could not announce a comment pick");
    }
}

pub fn announce_shortcut(handle: &AppHandle, id: &str, action: &str) {
    if let Err(error) = handle.emit(BROWSER_SHORTCUT_EVENT, BrowserShortcut { id, action }) {
        tracing::debug!(%error, "could not relay a browser shortcut");
    }
}

pub fn announce_state(handle: &AppHandle, view: &BrowserView) {
    if let Err(error) = handle.emit(BROWSER_STATE_EVENT, view) {
        tracing::debug!(%error, "could not announce a browser's state");
    }
}

pub fn announce_opened(handle: &AppHandle, view: &BrowserView) {
    if let Err(error) = handle.emit(BROWSER_OPENED_EVENT, view) {
        tracing::debug!(%error, "could not announce an opened browser");
    }
}

pub fn announce_closed(handle: &AppHandle, id: &str, summary: Option<String>) {
    let payload = BrowserClosed {
        id: id.to_owned(),
        summary,
    };
    if let Err(error) = handle.emit(BROWSER_CLOSED_EVENT, &payload) {
        tracing::debug!(%error, "could not announce a closed browser");
    }
}
