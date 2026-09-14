//! The embedded browser pane: a second webview, positioned over a tile.
//!
//! # Why a second webview and not an iframe
//!
//! The app webview's CSP has no `frame-src`, deliberately — `tests/network_boundary.rs` pins the
//! policy that the frontend cannot reach the network at all. An `<iframe>` would need that widened
//! and would still lose to any site sending `X-Frame-Options`. A *child webview* is a separate
//! native view with its own origin, its own storage and its own network access, laid over the
//! tile's rectangle by the frontend telling Rust where the rectangle is. The app webview's CSP is
//! untouched and the claim in ARCHITECTURE §6a stays true as written.
//!
//! # Three fences between a page and the app
//!
//! A remote page in the child cannot reach this app's commands, and that rests on three things:
//!
//! 1. Tauri's IPC refuses a remote origin unless a capability names it with `remote`, and
//!    `capabilities/default.json` has no such key.
//! 2. The capability lists `windows: ["main"]`, and the child *lives in* the main window — so a
//!    child that ever loaded a **local** origin would inherit `core:default`. That is why
//!    [`navigation_allowed`] is an allowlist of `http`, `https` and `about:` and not a denylist
//!    of `tauri:`; it is the fence that matters, and `tests/capability_set.rs` pins the other two.
//! 3. The webview label carries [`LABEL_PREFIX`] and no capability names it.
//!
//! # The runtime, and why it lives in an isolated world
//!
//! Agents read and drive the page through `runtime/browser.js`, installed by `wtm-webview` into a
//! `WKContentWorld` the page cannot see. Rust asks by evaluating `__wtm.dispatch(id, method, args)`
//! there and the answer comes back through a message handler registered in the same world, so a
//! page's CSP cannot block it and a page's scripts cannot forge it. `runtime/page-hook.js` is the
//! one thing installed in the *page* world — console capture and History API hooks — and every
//! message from it is treated as untrusted: a `navigated` report makes Rust re-read the real URL,
//! never believe the page's.
//!
//! # Why a browser is remembered by label rather than by handle
//!
//! `tauri::Webview` is `Clone + Send + Sync` and could sit in the registry. It is not, because a
//! handle to a closed webview is a handle that answers every call with an error, and the registry
//! would have no way to tell that from a live one. Re-resolving the label with
//! `Manager::get_webview` at each call means a webview that is gone reads as `None`, which is a
//! fact rather than a failure.
//!
//! # Lock discipline
//!
//! The same rule `App::with_agent` documents: a registry lock is held long enough to clone facts
//! out and never across `add_child`, `with_webview`, an emit or a wait. The builder's callbacks
//! and the runtime's message handlers run on the main thread and take the same locks, so holding
//! one across the Tauri call that fires them would deadlock exactly as the agent map once did. The
//! pending-reply map is a separate mutex so a reply never contends with the registry.
//!
//! # Caps
//!
//! Every child is a WebContent process. Four per worktree and eight in total, refused rather than
//! evicted — an evicted browser might be the one showing a form the user half filled in.

use std::collections::{BTreeMap, VecDeque};
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tauri::webview::{DownloadEvent, NewWindowResponse, PageLoadEvent, WebviewBuilder};
use tauri::{
    AppHandle, LogicalPosition, LogicalSize, Manager, Position, Rect, Size, Url, WebviewUrl,
};
use wtm_webview::{Handle, Message, World};

use crate::app::App;
use crate::browser_bridge;
use crate::view::{BrowserView, ErrorView};

/// Every browser webview's label starts with this, so a test can prove no capability matches one.
pub const LABEL_PREFIX: &str = "browser-";

/// Browsers one worktree may hold. A WebContent process each; refused rather than evicted.
pub const MAX_PER_WORKTREE: usize = 4;

/// Browsers the app may hold in total.
pub const MAX_TOTAL: usize = 8;

/// The window every child is added to. The only window this app has.
const MAIN_WINDOW: &str = "main";

/// What an empty pane shows. Kept as a constant because the frontend compares against it.
pub const BLANK: &str = "about:blank";

/// The content world the runtime lives in. Any name the page does not know is as good as another.
pub const WORLD: &str = "wtm";

/// The runtime, embedded so the CI job that builds without `dist/` keeps working and so nothing
/// is ever fetched: `network_boundary.rs` pins that this is a literal.
pub const RUNTIME_JS: &str = include_str!("../runtime/browser.js");
const PAGE_HOOK_JS: &str = include_str!("../runtime/page-hook.js");

/// The message handler names the two scripts post to.
const RUNTIME_HANDLER: &str = "wtm";
const PAGE_HANDLER: &str = "wtmPage";

/// How long an action in the page may take before the caller is told it did not answer.
pub const ACTION_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a navigation may take to finish loading before the caller reads the page as it is.
pub const LOAD_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a snapshot may take. Painting is fast; the bound is for a view that is not painting.
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(5);

/// The console ring's two bounds. Both, because one long line is as much of a leak as many short.
const MAX_CONSOLE_ENTRIES: usize = 200;
const MAX_CONSOLE_BYTES: usize = 64 * 1024;

/// Where the child sits, in the app webview's CSS pixels with the window's content origin.
///
/// Logical units end to end: the frontend measures `getBoundingClientRect()` and Rust hands the
/// numbers to Tauri as `Logical`, so neither side multiplies by a device pixel ratio and a Retina
/// display cannot introduce a factor of two.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bounds {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// The toolbar's four history controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HistoryAction {
    Back,
    Forward,
    Reload,
    Stop,
}

/// Whether this build can show a browser pane at all, and drive it, and why not.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Availability {
    /// Whether a pane can be shown. The child-webview API exists on every platform Tauri ships to.
    pub available: bool,
    /// Whether the runtime — agent tools, comments, snapshots — can be installed. macOS only, so far.
    pub runtime: bool,
    pub reason: Option<String>,
}

/// One comment the user left on an element of the page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Comment {
    /// Per browser, from one, in the order the comments were made. Never reused within a browser.
    pub id: u32,
    pub text: String,
    pub status: CommentStatus,
    pub anchor: CommentAnchor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CommentStatus {
    Open,
    Resolved,
}

/// What a comment is attached to, as the runtime described it at the moment of the click.
///
/// Every field but `selector` and `tag` defaults, because the anchor is written by a script in a
/// page this app did not author, and a missing field is not a reason to lose the comment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommentAnchor {
    pub selector: String,
    pub tag: String,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub rect: Option<AnchorRect>,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub page_title: String,
    #[serde(default)]
    pub nearest_heading: Option<String>,
    /// A handful of computed styles, passed through as the runtime reported them.
    #[serde(default)]
    pub styles: Value,
}

/// A rectangle in page coordinates — where the element was when the comment was made.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AnchorRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// One line the page wrote to its console, or one uncaught error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConsoleEntry {
    pub level: String,
    pub text: String,
}

/// One browser and what it belongs to. The registry's value type; never leaves this module.
///
/// Five independent facts that happen to be booleans, each read by name on the wire — the same
/// judgement `WorktreeView` records for its flags. An enum would trade five obvious fields for a
/// state machine nothing here needs.
#[allow(clippy::struct_excessive_bools)]
struct Entry {
    project: String,
    worktree: String,
    url: String,
    title: String,
    loading: bool,
    /// A navigation to `url` was asked for and its load has not started yet.
    ///
    /// Every browser is born at `about:blank` and navigated a moment later (see [`open`]), and the
    /// blank page's own load events arrive in that moment. Without this flag they would read as
    /// "the page finished loading", wake a caller waiting for the real page, and hand it a document
    /// with no runtime in it — which is exactly the failure the first end-to-end run produced.
    awaiting: bool,
    can_go_back: bool,
    can_go_forward: bool,
    /// The per-pane toggle. On by default: an agent that can already run `curl` here is not newly
    /// empowered by a browser it drives visibly — see ARCHITECTURE §6b for the same argument.
    agent_access: bool,
    /// The agent session that asked for this browser, when one did. Such a browser closes with its
    /// opener, the way a delegated child does.
    opened_by: Option<String>,
    /// The label of the agent acting on the page right now. One at a time per browser.
    driving: Option<String>,
    comment_mode: bool,
    comments: Vec<Comment>,
    next_comment: u32,
    console: VecDeque<ConsoleEntry>,
    console_bytes: usize,
    shown: bool,
}

/// A reply the page owes: which browser asked, and where to send the answer.
type Pending = (String, mpsc::Sender<Result<Value, String>>);

/// The browser registry. Owned by [`App`], like `handoff::Hub`.
#[derive(Default)]
pub struct Host {
    entries: parking_lot::Mutex<BTreeMap<String, Entry>>,
    /// Requests the runtime has not answered yet, by request id.
    pending: parking_lot::Mutex<BTreeMap<u64, Pending>>,
    /// Threads waiting for a browser's next page load to finish, by browser id.
    load_waiters: parking_lot::Mutex<BTreeMap<String, Vec<mpsc::Sender<()>>>>,
    /// Request ids. Zero is the id a fire-and-forget call uses, whose reply is dropped.
    seq: AtomicU64,
}

impl fmt::Debug for Host {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Host")
            .field("browsers", &self.entries.lock().len())
            .field("pending", &self.pending.lock().len())
            .field("load_waiters", &self.load_waiters.lock().len())
            .field("seq", &self.seq.load(Ordering::Relaxed))
            .finish()
    }
}

impl Host {
    fn view(id: &str, entry: &Entry) -> BrowserView {
        BrowserView {
            id: id.to_owned(),
            project: entry.project.clone(),
            worktree: entry.worktree.clone(),
            url: entry.url.clone(),
            title: entry.title.clone(),
            loading: entry.loading,
            can_go_back: entry.can_go_back,
            can_go_forward: entry.can_go_forward,
            agent_access: entry.agent_access,
            opened_by: entry.opened_by.clone(),
            comment_mode: entry.comment_mode,
            agent_driving: entry.driving.clone(),
        }
    }

    /// Every browser, or every browser in one worktree.
    pub fn list(&self, worktree: Option<&str>) -> Vec<BrowserView> {
        self.entries
            .lock()
            .iter()
            .filter(|(_, entry)| worktree.is_none_or(|w| entry.worktree == w))
            .map(|(id, entry)| Self::view(id, entry))
            .collect()
    }

    pub fn view_of(&self, id: &str) -> Option<BrowserView> {
        self.entries
            .lock()
            .get(id)
            .map(|entry| Self::view(id, entry))
    }

    pub fn ids_in(&self, worktree: &str) -> Vec<String> {
        self.entries
            .lock()
            .iter()
            .filter(|(_, entry)| entry.worktree == worktree)
            .map(|(id, _)| id.clone())
            .collect()
    }

    pub fn ids_opened_by(&self, session: &str) -> Vec<String> {
        self.entries
            .lock()
            .iter()
            .filter(|(_, entry)| entry.opened_by.as_deref() == Some(session))
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// The worktree a browser belongs to, for the handoff path's scoping check.
    pub fn worktree_of(&self, id: &str) -> Option<String> {
        self.entries
            .lock()
            .get(id)
            .map(|entry| entry.worktree.clone())
    }

    /// Whether a browser is currently shown, or `None` if it is gone.
    fn shown(&self, id: &str) -> Option<bool> {
        self.entries.lock().get(id).map(|entry| entry.shown)
    }

    /// Reserve a slot and mint the label, refusing at the caps.
    ///
    /// Inserted *before* the webview is built, so two concurrent opens cannot both pass the count
    /// and both build. The caller forgets the entry if the build fails.
    fn register(
        &self,
        project: &str,
        worktree: &str,
        url: &str,
        opened_by: Option<&str>,
    ) -> Result<String, ErrorView> {
        let mut entries = self.entries.lock();
        if entries.len() >= MAX_TOTAL {
            return Err(ErrorView::new(
                "browserCap",
                format!("wtm holds at most {MAX_TOTAL} browser panes at once; close one first"),
            ));
        }
        let here = entries
            .values()
            .filter(|entry| entry.worktree == worktree)
            .count();
        if here >= MAX_PER_WORKTREE {
            return Err(ErrorView::new(
                "browserCap",
                format!(
                    "this worktree already has {MAX_PER_WORKTREE} browser panes; close one first"
                ),
            ));
        }
        let id = format!("{LABEL_PREFIX}{}", uuid::Uuid::new_v4());
        entries.insert(
            id.clone(),
            Entry {
                project: project.to_owned(),
                worktree: worktree.to_owned(),
                url: url.to_owned(),
                title: String::new(),
                loading: false,
                awaiting: false,
                can_go_back: false,
                can_go_forward: false,
                agent_access: true,
                opened_by: opened_by.map(str::to_owned),
                driving: None,
                comment_mode: false,
                comments: Vec::new(),
                next_comment: 1,
                console: VecDeque::new(),
                console_bytes: 0,
                shown: false,
            },
        );
        Ok(id)
    }

    fn forget(&self, id: &str) -> Option<BrowserView> {
        let view = self
            .entries
            .lock()
            .remove(id)
            .map(|entry| Self::view(id, &entry));
        // Anyone waiting on this browser hears "closed" now rather than at their timeout: dropping
        // the sender disconnects the receiver.
        self.pending.lock().retain(|_, (owner, _)| owner != id);
        self.load_waiters.lock().remove(id);
        view
    }

    /// Change one entry and hand back its view, or `None` if the browser is gone.
    fn update(&self, id: &str, change: impl FnOnce(&mut Entry)) -> Option<BrowserView> {
        let mut entries = self.entries.lock();
        let entry = entries.get_mut(id)?;
        change(entry);
        Some(Self::view(id, entry))
    }

    fn comments_of(&self, id: &str) -> Option<Vec<Comment>> {
        self.entries
            .lock()
            .get(id)
            .map(|entry| entry.comments.clone())
    }

    fn console_of(&self, id: &str, clear: bool) -> Option<Vec<ConsoleEntry>> {
        let mut entries = self.entries.lock();
        let entry = entries.get_mut(id)?;
        let lines: Vec<ConsoleEntry> = entry.console.iter().cloned().collect();
        if clear {
            entry.console.clear();
            entry.console_bytes = 0;
        }
        Some(lines)
    }

    /// Wake everyone waiting for this browser's load. Called from the page-load callback.
    fn loaded(&self, id: &str) {
        if let Some(waiters) = self.load_waiters.lock().remove(id) {
            for waiter in waiters {
                let _ = waiter.send(());
            }
        }
    }

    /// How many of a browser's comments are still open, for the tool that lists panes.
    pub fn list_comments_open(&self, id: &str) -> Option<usize> {
        self.entries.lock().get(id).map(|entry| {
            entry
                .comments
                .iter()
                .filter(|comment| comment.status == CommentStatus::Open)
                .count()
        })
    }

    /// Deliver the runtime's answer to whoever asked. Silently nothing for an unknown or zero id.
    fn resolve(&self, request: u64, answer: Result<Value, String>) {
        if let Some((_, sender)) = self.pending.lock().remove(&request) {
            let _ = sender.send(answer);
        }
    }

    /// Mark a browser as driven by an agent, or refuse if another call is in flight.
    pub fn begin_driving(&self, id: &str, label: &str) -> Result<BrowserView, String> {
        let mut entries = self.entries.lock();
        let entry = entries
            .get_mut(id)
            .ok_or_else(|| "that browser pane is no longer open".to_owned())?;
        if let Some(busy) = &entry.driving {
            return Err(format!(
                "{busy} is already acting on this browser; try again shortly"
            ));
        }
        entry.driving = Some(label.to_owned());
        Ok(Self::view(id, entry))
    }

    pub fn end_driving(&self, id: &str) -> Option<BrowserView> {
        self.update(id, |entry| entry.driving = None)
    }
}

/// The allowlist a page in the child is held to.
///
/// An allowlist and not a denylist of `tauri:`, because the failure being prevented is a child
/// arriving at a *local* origin and inheriting the main window's capability (module docs, fence 2).
/// `about:` covers `about:blank`, which an empty pane shows, and `about:srcdoc`, which an iframe
/// with inline content is.
#[must_use]
pub fn navigation_allowed(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https" | "about")
}

/// Turn what a person typed or an agent sent into a URL the pane may show.
///
/// Same two checks as `commands::open_url`, for the same reasons: a scheme other than http(s) is
/// refused rather than handed to a webview that would happily open `file:`, and whitespace or a
/// control character is malformed input rather than a URL.
pub fn parse_target(raw: &str) -> Result<Url, ErrorView> {
    let raw = raw.trim();
    if raw == BLANK {
        return Url::parse(BLANK).map_err(|e| ErrorView::new("badUrl", e.to_string()));
    }
    if raw.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(ErrorView::new(
            "badUrl",
            "refusing a URL containing whitespace",
        ));
    }
    let url = Url::parse(raw).map_err(|e| ErrorView::new("badUrl", format!("`{raw}`: {e}")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(ErrorView::new(
            "badUrl",
            format!("refusing to open `{raw}`: only http and https are allowed"),
        ));
    }
    Ok(url)
}

/// Whether a browser pane can be shown on this build, and whether it can be driven.
///
/// The pane itself is available everywhere Tauri ships a child-webview API. The runtime needs
/// `wtm-webview`'s native arm, which exists on macOS today; elsewhere the pane is a plain browser
/// and the tools say so.
#[must_use]
pub fn availability() -> Availability {
    let runtime = wtm_webview::available();
    Availability {
        available: true,
        runtime,
        reason: (!runtime).then(|| {
            "Agent tools and comments need the native WebKit bridge, which this build does not \
             have yet; the pane itself works."
                .to_owned()
        }),
    }
}

fn webview_of(handle: &AppHandle, id: &str) -> Result<tauri::Webview, ErrorView> {
    handle.get_webview(id).ok_or_else(gone)
}

fn webview_error(what: &str, error: &tauri::Error) -> ErrorView {
    ErrorView::new("webview", format!("could not {what}: {error}"))
}

fn gone() -> ErrorView {
    ErrorView::new("noBrowser", "that browser pane is no longer open")
}

/// Open a browser in a worktree.
///
/// Must run off the main thread: `add_child` builds the view *on* the main thread and blocks the
/// caller until it has, so calling it from there is a deadlock. Every command goes through
/// `blocking`, and the handoff path is its own thread, so this holds for both callers.
///
/// Born at `about:blank`, then navigated. The runtime is installed between the two, and a script
/// added to a webview applies to the *next* load — so a webview born at its destination would show
/// its first page without one. `about:blank` loads in the same event-loop turn and costs nothing.
pub fn open(
    handle: &AppHandle,
    app: &Arc<App>,
    project: &str,
    worktree: &str,
    url: Option<&str>,
    opened_by: Option<&str>,
) -> Result<BrowserView, ErrorView> {
    debug_assert!(
        !wtm_webview::on_main_thread(),
        "browser::open blocks on the main thread and must not be called from it"
    );
    let target = match url {
        Some(raw) => parse_target(raw)?,
        None => Url::parse(BLANK).map_err(|e| ErrorView::new("badUrl", e.to_string()))?,
    };
    let blank = Url::parse(BLANK).map_err(|e| ErrorView::new("badUrl", e.to_string()))?;
    let window = handle
        .get_window(MAIN_WINDOW)
        .ok_or_else(|| ErrorView::new("webview", "the main window is gone"))?;

    let id = app
        .browsers
        .register(project, worktree, target.as_str(), opened_by)?;

    let new_window_handle = handle.clone();
    let new_window_label = id.clone();
    let builder = WebviewBuilder::new(id.clone(), WebviewUrl::External(blank))
        .on_navigation(navigation_allowed)
        // A popup becomes a navigation of this same pane. Tiling a second child for every
        // `target="_blank"` would spend a WebContent process on a link the user can press Back from.
        .on_new_window(move |url, _features| {
            if navigation_allowed(&url)
                && let Some(webview) = new_window_handle.get_webview(&new_window_label)
                && let Err(error) = webview.navigate(url)
            {
                tracing::debug!(%error, "a popup could not be opened in place");
            }
            NewWindowResponse::Deny
        })
        // Downloads go to the user's browser. The pane has no download UI, no place to put a file,
        // and no business writing to disk on a page's say-so.
        .on_download(|webview, event| {
            if let DownloadEvent::Requested { url, .. } = event {
                hand_to_system_browser(webview.app_handle(), url.clone());
            }
            false
        })
        .on_page_load(|webview, payload| {
            let Some(app) = webview.app_handle().try_state::<Arc<App>>() else {
                return;
            };
            let url = payload.url().to_string();
            // The blank page a browser is born on loads while the real navigation is still queued.
            // Its events are not the page's; see `Entry::awaiting`.
            if url == BLANK
                && app
                    .browsers
                    .view_of(webview.label())
                    .is_some_and(|view| view.url != BLANK)
            {
                return;
            }
            tracing::debug!(id = webview.label(), %url, event = ?payload.event(), "browser page load");
            let view = match payload.event() {
                PageLoadEvent::Started => app.browsers.update(webview.label(), |entry| {
                    entry.loading = true;
                    entry.awaiting = false;
                    entry.url = url;
                }),
                PageLoadEvent::Finished => {
                    let view = app.browsers.update(webview.label(), |entry| {
                        entry.loading = false;
                        entry.awaiting = false;
                        entry.url = url;
                    });
                    app.browsers.loaded(webview.label());
                    view
                }
            };
            if let Some(view) = view {
                browser_bridge::announce_state(webview.app_handle(), &view);
            }
            if matches!(payload.event(), PageLoadEvent::Finished) {
                refresh_history(&webview);
                // The runtime starts afresh with every page, so a mode the user turned on before
                // the navigation has to be said again.
                if app
                    .browsers
                    .view_of(webview.label())
                    .is_some_and(|view| view.comment_mode)
                {
                    fire(
                        webview.app_handle(),
                        webview.label(),
                        "setCommentMode",
                        &json!({ "on": true }),
                    );
                }
                // Pins are drawn by the runtime, which a fresh page has just re-created empty.
                if let Some(comments) = app.browsers.comments_of(webview.label())
                    && !comments.is_empty()
                {
                    fire(
                        webview.app_handle(),
                        webview.label(),
                        "renderPins",
                        &json!({ "comments": comments }),
                    );
                }
            }
        })
        .on_document_title_changed(|webview, title| {
            let Some(app) = webview.app_handle().try_state::<Arc<App>>() else {
                return;
            };
            if let Some(view) = app.browsers.update(webview.label(), |entry| {
                entry.title = title;
            }) {
                browser_bridge::announce_state(webview.app_handle(), &view);
            }
        })
        // Its own cookie jar and storage, apart from the app webview's. The identifier is the app's
        // bundle id folded to sixteen bytes, so it is the same jar on every launch. Honoured on
        // macOS 14+; wry falls back to the default store below that, which is still a different
        // *origin* from anything the app stores, so nothing leaks either way.
        .data_store_identifier(data_store_id())
        .zoom_hotkeys_enabled(true)
        .devtools(cfg!(debug_assertions))
        // A hidden pane still answers an agent, so its timers must keep running.
        .background_throttling(tauri::utils::config::BackgroundThrottlingPolicy::Disabled);

    // Born at one pixel and hidden, because the frontend has not yet said where the tile is. The
    // first `set_bounds` shows it.
    let created = window.add_child(
        builder,
        Position::Logical(LogicalPosition::new(0.0, 0.0)),
        Size::Logical(LogicalSize::new(1.0, 1.0)),
    );
    let webview = match created {
        Ok(webview) => webview,
        Err(error) => {
            app.browsers.forget(&id);
            return Err(webview_error("create the browser", &error));
        }
    };
    if let Err(error) = webview.hide() {
        tracing::debug!(%error, "a new browser could not be hidden before placement");
    }
    install(handle, app, &webview);
    if target.as_str() != BLANK {
        // Marked before the navigation is queued, so a caller that waits for the load right after
        // `open` returns finds a load to wait for rather than a blank page that is "done".
        app.browsers.update(&id, |entry| {
            entry.loading = true;
            entry.awaiting = true;
        });
        if let Err(error) = webview.navigate(target) {
            tracing::debug!(%error, "a new browser could not start its first navigation");
            app.browsers.update(&id, |entry| {
                entry.loading = false;
                entry.awaiting = false;
            });
        }
    }
    tracing::debug!(%id, project, worktree, ?opened_by, "browser opened");

    let view = app.browsers.view_of(&id).ok_or_else(gone)?;
    if opened_by.is_some() {
        browser_bridge::announce_opened(handle, &view);
    }
    Ok(view)
}

/// Put the two scripts and the two message handlers into a webview that was just created.
///
/// Queued on the main thread behind the creation and ahead of the first navigation, so the scripts
/// are in place for the first real page. Where there is no native arm the closure finds no handle
/// and the pane stays a plain browser.
fn install(handle: &AppHandle, app: &Arc<App>, webview: &tauri::Webview) {
    let label = webview.label().to_owned();
    let (runtime_handle, runtime_app, runtime_label) =
        (handle.clone(), Arc::clone(app), label.clone());
    let (page_handle, page_app, page_label) = (handle.clone(), Arc::clone(app), label.clone());
    let queued = webview.with_webview(move |platform| {
        let Some(native) = Handle::attach(platform.inner(), platform.controller()) else {
            tracing::debug!(%label, "no native webview to install the browser runtime into");
            return;
        };
        let isolated = World::Isolated(WORLD.to_owned());
        native.install_script(&isolated, RUNTIME_JS, true);
        native.install_script(&World::Page, PAGE_HOOK_JS, true);
        native.add_message_handler(&isolated, RUNTIME_HANDLER, move |message| {
            on_runtime_message(&runtime_handle, &runtime_app, &runtime_label, &message);
        });
        native.add_message_handler(&World::Page, PAGE_HANDLER, move |message| {
            on_page_message(&page_handle, &page_app, &page_label, &message);
        });
    });
    if let Err(error) = queued {
        tracing::debug!(%error, "the browser runtime could not be queued for installation");
    }
}

/// Ask the webview itself whether Back and Forward mean anything, after a load finished.
///
/// Runs on the main thread already (a page-load callback), where `with_webview` executes inline.
fn refresh_history(webview: &tauri::Webview) {
    let handle = webview.app_handle().clone();
    let label = webview.label().to_owned();
    let queued = webview.with_webview(move |platform| {
        let Some(native) = Handle::attach(platform.inner(), platform.controller()) else {
            return;
        };
        let (back, forward) = native.history();
        let Some(app) = handle.try_state::<Arc<App>>() else {
            return;
        };
        if let Some(view) = app.browsers.update(&label, |entry| {
            entry.can_go_back = back;
            entry.can_go_forward = forward;
        }) {
            browser_bridge::announce_state(&handle, &view);
        }
    });
    if let Err(error) = queued {
        tracing::debug!(%error, "could not read a browser's history state");
    }
}

/// A message from the runtime in the isolated world: a reply to a request, or something the page did.
fn on_runtime_message(handle: &AppHandle, app: &Arc<App>, id: &str, message: &Message) {
    if !message.main_frame {
        return;
    }
    let Ok(body) = serde_json::from_str::<Value>(&message.body) else {
        return;
    };
    if let Some(request) = body.get("id").and_then(Value::as_u64) {
        let answer = if body.get("ok").and_then(Value::as_bool) == Some(true) {
            Ok(body.get("value").cloned().unwrap_or(Value::Null))
        } else {
            Err(body
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("the page reported an error without saying which")
                .to_owned())
        };
        app.browsers.resolve(request, answer);
        return;
    }
    match body.get("event").and_then(Value::as_str) {
        Some("focus") => browser_bridge::announce_shortcut(handle, id, "focus"),
        Some("shortcut") => {
            if let Some(action) = body.get("action").and_then(Value::as_str) {
                browser_bridge::announce_shortcut(handle, id, action);
            }
        }
        Some("comment-add") => {
            let Some(text) = body.get("text").and_then(Value::as_str) else {
                return;
            };
            let Some(anchor) = body
                .get("anchor")
                .cloned()
                .and_then(|anchor| serde_json::from_value::<CommentAnchor>(anchor).ok())
            else {
                return;
            };
            add_comment(handle, app, id, text, anchor);
        }
        Some("comment-pick") => {
            let picked = body
                .get("commentId")
                .and_then(Value::as_u64)
                .and_then(|n| u32::try_from(n).ok());
            browser_bridge::announce_pick(handle, id, picked);
        }
        _ => {}
    }
}

/// A message from the page world. Untrusted: it can trigger a re-read, never supply a fact.
fn on_page_message(handle: &AppHandle, app: &Arc<App>, id: &str, message: &Message) {
    if !message.main_frame {
        return;
    }
    let Ok(body) = serde_json::from_str::<Value>(&message.body) else {
        return;
    };
    match body.get("event").and_then(Value::as_str) {
        Some("console") => {
            let level = body
                .get("level")
                .and_then(Value::as_str)
                .unwrap_or("log")
                .to_owned();
            let text = body
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            app.browsers
                .update(id, |entry| push_console(entry, level, text));
        }
        Some("navigated") => {
            // The page said its address changed; the webview says what to.
            if let Some(webview) = handle.get_webview(id)
                && let Ok(url) = webview.url()
                && let Some(view) = app.browsers.update(id, |entry| entry.url = url.to_string())
            {
                browser_bridge::announce_state(handle, &view);
            }
        }
        _ => {}
    }
}

/// Lines the app's own injected scripts write when a remote page refuses them.
///
/// Tauri installs its IPC and plugin bootstrap into every webview it creates, this one included,
/// and in a remote page those scripts fail exactly as the fences intend — the IPC probe cannot
/// load a `tauri://` resource and the notification plugin's permission check is refused. That is
/// the app talking to itself, not the page talking, and an agent asked about the page's console
/// should not have to read past it.
fn is_app_noise(text: &str) -> bool {
    text.contains("Tauri will now use the postMessage interface")
        || text.contains("Permissions associated with this command")
        || text.contains("__TAURI")
}

fn push_console(entry: &mut Entry, level: String, text: String) {
    if is_app_noise(&text) {
        return;
    }
    let bytes = text.len() + level.len();
    entry.console.push_back(ConsoleEntry { level, text });
    entry.console_bytes += bytes;
    while entry.console.len() > MAX_CONSOLE_ENTRIES || entry.console_bytes > MAX_CONSOLE_BYTES {
        let Some(dropped) = entry.console.pop_front() else {
            break;
        };
        entry.console_bytes = entry
            .console_bytes
            .saturating_sub(dropped.text.len() + dropped.level.len());
    }
}

/// The sixteen bytes WebKit keys a persistent data store by. Stable across launches by construction.
fn data_store_id() -> [u8; 16] {
    let mut id = [0u8; 16];
    for (slot, byte) in id
        .iter_mut()
        .zip(b"dev.takumihendricks.wtm.browser".iter().cycle())
    {
        *slot = *byte;
    }
    id
}

/// Open a URL in the user's own browser, off the main thread.
///
/// The download callback runs on the main thread and the opener is a captured command with a
/// ten-second deadline, which is ten seconds the UI must not spend frozen.
fn hand_to_system_browser(handle: &AppHandle, url: Url) {
    let Some(app) = handle.try_state::<Arc<App>>() else {
        return;
    };
    let app = Arc::clone(&app);
    std::thread::spawn(move || {
        let inv = wtm_core::ports::exec::Invocation::new(
            vec![crate::openers::OPENER.to_owned(), url.to_string()],
            std::env::temp_dir(),
            10_000,
        );
        if let Err(error) = app
            .runner
            .run(&inv, &wtm_core::ports::exec::CancelToken::new())
        {
            tracing::debug!(%error, "a download could not be handed to the system browser");
        }
    });
}

// ─────────────────────────────── talking to the runtime ───────────────────────────────

/// Ask the runtime to do something and wait for its answer.
///
/// The request is evaluated in the isolated world with a fresh id; the runtime posts
/// `{id, ok, value|error}` to its handler, [`on_runtime_message`] resolves the pending sender, and
/// this thread wakes. Never called on the main thread — the answer arrives *on* the main thread,
/// so waiting there would wait forever — which the debug assertion guards and which holds for both
/// callers (commands run under `blocking`, tool calls on the handoff socket's thread).
pub fn call(
    handle: &AppHandle,
    app: &Arc<App>,
    id: &str,
    method: &str,
    args: &Value,
    timeout: Duration,
) -> Result<Value, String> {
    debug_assert!(
        !wtm_webview::on_main_thread(),
        "browser::call waits for the main thread and must not run on it"
    );
    if !wtm_webview::available() {
        return Err(availability()
            .reason
            .unwrap_or_else(|| "the browser runtime is not available".to_owned()));
    }
    let webview = handle
        .get_webview(id)
        .ok_or_else(|| "that browser pane is no longer open".to_owned())?;
    let request = app.browsers.seq.fetch_add(1, Ordering::Relaxed) + 1;
    let (tx, rx) = mpsc::channel();
    app.browsers
        .pending
        .lock()
        .insert(request, (id.to_owned(), tx));
    dispatch(&webview, request, method, args);
    match rx.recv_timeout(timeout) {
        Ok(answer) => answer,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            app.browsers.pending.lock().remove(&request);
            Err(format!(
                "the page did not answer `{method}` within {} s — it may still be loading, or it \
                 may not be a page the runtime could attach to",
                timeout.as_secs()
            ))
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err("that browser pane closed before it could answer".to_owned())
        }
    }
}

/// Block until the browser's current load finishes, or `timeout` passes, or the browser closes.
///
/// Returns immediately when nothing is loading — the ordinary case for a fast local page whose
/// load finished between the navigation and this call. Never called on the main thread, where the
/// load callback that would wake it runs.
pub fn wait_for_load(app: &App, id: &str, timeout: Duration) -> bool {
    debug_assert!(!wtm_webview::on_main_thread());
    let (tx, rx) = mpsc::channel();
    {
        let mut waiters = app.browsers.load_waiters.lock();
        let Some(loading) = app.browsers.view_of(id).map(|view| view.loading) else {
            return false;
        };
        if !loading {
            return true;
        }
        waiters.entry(id.to_owned()).or_default().push(tx);
    }
    rx.recv_timeout(timeout).is_ok()
}

/// Tell the runtime something without waiting. Request id zero; the runtime's reply is dropped.
///
/// Safe from the main thread, which is where the callbacks that need it run.
pub fn fire(handle: &AppHandle, id: &str, method: &str, args: &Value) {
    if let Some(webview) = handle.get_webview(id) {
        dispatch(&webview, 0, method, args);
    }
}

fn dispatch(webview: &tauri::Webview, request: u64, method: &str, args: &Value) {
    // Both arguments are JSON-encoded, so a method name or a string argument containing a quote
    // is data to the runtime rather than syntax.
    let js = format!(
        "__wtm.dispatch({request}, {}, {})",
        serde_json::to_string(method).unwrap_or_else(|_| "\"\"".to_owned()),
        serde_json::to_string(args).unwrap_or_else(|_| "{}".to_owned())
    );
    let queued = webview.with_webview(move |platform| {
        if let Some(native) = Handle::attach(platform.inner(), platform.controller()) {
            // The reply travels through the message handler, not the evaluation result, so the
            // receiver `evaluate` hands back is dropped on purpose.
            drop(native.evaluate(&World::Isolated(WORLD.to_owned()), &js));
        }
    });
    if let Err(error) = queued {
        tracing::debug!(%error, "a browser runtime call could not be queued");
    }
}

/// A PNG of the page as it is shown. Refused while the pane is hidden, because WebKit paints
/// nothing for a hidden view and an all-black picture would be worse than a refusal.
pub fn snapshot_png(
    handle: &AppHandle,
    app: &Arc<App>,
    id: &str,
    rect: Option<Bounds>,
) -> Result<Vec<u8>, ErrorView> {
    debug_assert!(!wtm_webview::on_main_thread());
    if !app.browsers.shown(id).ok_or_else(gone)? {
        return Err(ErrorView::new(
            "browserHidden",
            "the browser pane is hidden right now, so there is nothing to capture; show it first",
        ));
    }
    let webview = webview_of(handle, id)?;
    let (tx, rx) = mpsc::channel();
    let queued = webview.with_webview(move |platform| {
        let reply = Handle::attach(platform.inner(), platform.controller())
            .map(|native| native.snapshot(rect.map(|r| (r.x, r.y, r.w, r.h))));
        let _ = tx.send(reply);
    });
    queued.map_err(|e| webview_error("reach the browser", &e))?;
    let reply = rx
        .recv_timeout(SNAPSHOT_TIMEOUT)
        .map_err(|_| ErrorView::new("webview", "the browser did not respond"))?
        .ok_or_else(|| {
            ErrorView::new(
                "browserUnavailable",
                "snapshots need the native WebKit bridge, which this build does not have",
            )
        })?;
    reply
        .wait(SNAPSHOT_TIMEOUT)
        .map_err(|e| ErrorView::new("snapshot", e.to_string()))
}

// ─────────────────────────────────── the pane's controls ───────────────────────────────────

pub fn navigate(
    handle: &AppHandle,
    app: &Arc<App>,
    id: &str,
    raw: &str,
) -> Result<BrowserView, ErrorView> {
    let url = parse_target(raw)?;
    let webview = webview_of(handle, id)?;
    webview
        .navigate(url.clone())
        .map_err(|e| webview_error("navigate", &e))?;
    let view = app
        .browsers
        .update(id, |entry| {
            entry.loading = true;
            entry.awaiting = true;
            entry.url = url.to_string();
        })
        .ok_or_else(gone)?;
    browser_bridge::announce_state(handle, &view);
    Ok(view)
}

pub fn history(handle: &AppHandle, id: &str, action: HistoryAction) -> Result<(), ErrorView> {
    let webview = webview_of(handle, id)?;
    match action {
        HistoryAction::Reload => webview.reload().map_err(|e| webview_error("reload", &e)),
        // Page-world JS is fine for these three: they are the page's own history and its own load,
        // and there is nothing for a hostile page to gain by intercepting a request to stop itself.
        HistoryAction::Back => webview
            .eval("history.back()")
            .map_err(|e| webview_error("go back", &e)),
        HistoryAction::Forward => webview
            .eval("history.forward()")
            .map_err(|e| webview_error("go forward", &e)),
        HistoryAction::Stop => webview
            .eval("window.stop()")
            .map_err(|e| webview_error("stop loading", &e)),
    }
}

/// Place the child over the tile, or hide it.
///
/// `None` hides rather than moving the view off-screen, because a hidden WKWebView stops painting
/// and an off-screen one does not — and because "hidden" is a fact the snapshot path needs.
pub fn set_bounds(
    handle: &AppHandle,
    app: &Arc<App>,
    id: &str,
    bounds: Option<Bounds>,
) -> Result<(), ErrorView> {
    let webview = webview_of(handle, id)?;
    if let Some(rect) = bounds {
        webview
            .set_bounds(Rect {
                position: Position::Logical(LogicalPosition::new(rect.x, rect.y)),
                size: Size::Logical(LogicalSize::new(rect.w.max(1.0), rect.h.max(1.0))),
            })
            .map_err(|e| webview_error("place the browser", &e))?;
        webview
            .show()
            .map_err(|e| webview_error("show the browser", &e))?;
        app.browsers.update(id, |entry| entry.shown = true);
        tracing::debug!(%id, ?rect, "browser placed");
    } else {
        webview
            .hide()
            .map_err(|e| webview_error("hide the browser", &e))?;
        app.browsers.update(id, |entry| entry.shown = false);
        tracing::debug!(%id, "browser hidden");
    }
    Ok(())
}

pub fn focus(handle: &AppHandle, id: &str) -> Result<(), ErrorView> {
    webview_of(handle, id)?
        .set_focus()
        .map_err(|e| webview_error("focus the browser", &e))
}

pub fn zoom(handle: &AppHandle, id: &str, factor: f64) -> Result<(), ErrorView> {
    webview_of(handle, id)?
        .set_zoom(factor.clamp(0.25, 5.0))
        .map_err(|e| webview_error("zoom the browser", &e))
}

/// The Web Inspector, in a development build. A release build has no inspector to open.
pub fn open_devtools(handle: &AppHandle, id: &str) -> Result<(), ErrorView> {
    let webview = webview_of(handle, id)?;
    #[cfg(debug_assertions)]
    {
        webview.open_devtools();
        Ok(())
    }
    #[cfg(not(debug_assertions))]
    {
        drop(webview);
        Err(ErrorView::new(
            "unavailable",
            "the Web Inspector is only available in a development build",
        ))
    }
}

pub fn set_agent_access(
    handle: &AppHandle,
    app: &Arc<App>,
    id: &str,
    enabled: bool,
) -> Result<BrowserView, ErrorView> {
    let view = app
        .browsers
        .update(id, |entry| entry.agent_access = enabled)
        .ok_or_else(gone)?;
    browser_bridge::announce_state(handle, &view);
    Ok(view)
}

/// Turn comment mode on or off. The runtime draws the overlay; Rust owns the fact.
pub fn set_comment_mode(
    handle: &AppHandle,
    app: &Arc<App>,
    id: &str,
    enabled: bool,
) -> Result<BrowserView, ErrorView> {
    let view = app
        .browsers
        .update(id, |entry| entry.comment_mode = enabled)
        .ok_or_else(gone)?;
    fire(handle, id, "setCommentMode", &json!({ "on": enabled }));
    if enabled && let Some(comments) = app.browsers.comments_of(id) {
        fire(handle, id, "renderPins", &json!({ "comments": comments }));
    }
    browser_bridge::announce_state(handle, &view);
    Ok(view)
}

/// Hand the runtime the app's colours, so its pins and popover match the theme around them.
pub fn set_theme(handle: &AppHandle, id: &str, tokens: &Value) -> Result<(), ErrorView> {
    webview_of(handle, id)?;
    fire(handle, id, "setTheme", tokens);
    Ok(())
}

// ─────────────────────────────────────── comments ───────────────────────────────────────

fn add_comment(handle: &AppHandle, app: &Arc<App>, id: &str, text: &str, anchor: CommentAnchor) {
    let comments = {
        let mut entries = app.browsers.entries.lock();
        let Some(entry) = entries.get_mut(id) else {
            return;
        };
        entry.comments.push(Comment {
            id: entry.next_comment,
            text: text.trim().to_owned(),
            status: CommentStatus::Open,
            anchor,
        });
        entry.next_comment += 1;
        entry.comments.clone()
    };
    published(handle, id, &comments);
}

/// Tell the window and the page about a comment list that changed.
fn published(handle: &AppHandle, id: &str, comments: &[Comment]) {
    browser_bridge::announce_comments(handle, id, comments);
    fire(handle, id, "renderPins", &json!({ "comments": comments }));
}

pub fn list_comments(app: &App, id: &str) -> Result<Vec<Comment>, ErrorView> {
    app.browsers.comments_of(id).ok_or_else(gone)
}

/// Change a comment's text, or its status, or remove it. One function so the publish step is one.
fn edit_comments(
    handle: &AppHandle,
    app: &Arc<App>,
    id: &str,
    edit: impl FnOnce(&mut Vec<Comment>) -> bool,
) -> Result<Vec<Comment>, ErrorView> {
    let comments = {
        let mut entries = app.browsers.entries.lock();
        let entry = entries.get_mut(id).ok_or_else(gone)?;
        if !edit(&mut entry.comments) {
            return Err(ErrorView::new(
                "noComment",
                "there is no comment with that id on this page",
            ));
        }
        entry.comments.clone()
    };
    published(handle, id, &comments);
    Ok(comments)
}

pub fn update_comment(
    handle: &AppHandle,
    app: &Arc<App>,
    id: &str,
    comment: u32,
    text: &str,
) -> Result<Vec<Comment>, ErrorView> {
    edit_comments(handle, app, id, |comments| {
        comments
            .iter_mut()
            .find(|c| c.id == comment)
            .map(|c| text.trim().clone_into(&mut c.text))
            .is_some()
    })
}

pub fn remove_comment(
    handle: &AppHandle,
    app: &Arc<App>,
    id: &str,
    comment: u32,
) -> Result<Vec<Comment>, ErrorView> {
    edit_comments(handle, app, id, |comments| {
        let before = comments.len();
        comments.retain(|c| c.id != comment);
        comments.len() != before
    })
}

pub fn set_comment_status(
    handle: &AppHandle,
    app: &Arc<App>,
    id: &str,
    comment: u32,
    status: CommentStatus,
) -> Result<Vec<Comment>, ErrorView> {
    edit_comments(handle, app, id, |comments| {
        comments
            .iter_mut()
            .find(|c| c.id == comment)
            .map(|c| c.status = status)
            .is_some()
    })
}

/// Recent console output. `clear` empties the ring after reading, so an agent can watch a delta.
pub fn console(app: &App, id: &str, clear: bool) -> Result<Vec<ConsoleEntry>, ErrorView> {
    app.browsers.console_of(id, clear).ok_or_else(gone)
}

// ─────────────────────────────────────── teardown ───────────────────────────────────────

/// Close one browser: forget it, drop the webview, tell the window.
///
/// Forgetting first, like `App::close_shell`, so a callback firing during the close finds no entry
/// to update rather than resurrecting one. Returns whether there was anything to close.
pub fn close(handle: &AppHandle, app: &Arc<App>, id: &str) -> bool {
    let Some(_view) = app.browsers.forget(id) else {
        return false;
    };
    if let Some(webview) = handle.get_webview(id)
        && let Err(error) = webview.close()
    {
        tracing::debug!(%id, %error, "a browser webview did not close cleanly");
    }
    browser_bridge::announce_closed(handle, id, None);
    tracing::debug!(%id, "browser closed");
    true
}

/// Close every browser in a worktree. Worktree removal, before `git worktree remove` runs.
pub fn close_all_in(handle: &AppHandle, app: &Arc<App>, worktree: &str) -> Vec<String> {
    let ids = app.browsers.ids_in(worktree);
    for id in &ids {
        close(handle, app, id);
    }
    ids
}

/// Close the browsers an agent session opened. Runs when that session closes.
pub fn close_opened_by(handle: &AppHandle, app: &Arc<App>, session: &str) -> Vec<String> {
    let ids = app.browsers.ids_opened_by(session);
    for id in &ids {
        close(handle, app, id);
    }
    ids
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn url(raw: &str) -> Url {
        Url::parse(raw).unwrap()
    }

    fn anchor(selector: &str) -> CommentAnchor {
        CommentAnchor {
            selector: selector.to_owned(),
            tag: "button".to_owned(),
            role: None,
            name: None,
            text: String::new(),
            rect: None,
            url: String::new(),
            page_title: String::new(),
            nearest_heading: None,
            styles: Value::Null,
        }
    }

    #[test]
    fn navigation_is_allowed_only_for_http_https_and_about() {
        assert!(navigation_allowed(&url("http://localhost:5173/")));
        assert!(navigation_allowed(&url("https://example.com/a?b=c")));
        assert!(navigation_allowed(&url("about:blank")));
        assert!(!navigation_allowed(&url("file:///etc/passwd")));
        assert!(!navigation_allowed(&url("javascript:alert(1)")));
        assert!(!navigation_allowed(&url("data:text/html,hi")));
        assert!(!navigation_allowed(&url("asset://localhost/x")));
    }

    #[test]
    fn a_page_cannot_steer_the_browser_onto_the_apps_own_origin() {
        // Fence 2 in the module docs: a child at a local origin would inherit the main window's
        // capability, so this is the one refusal that must never regress.
        assert!(!navigation_allowed(&url("tauri://localhost/index.html")));
        assert!(!navigation_allowed(&url("ipc://localhost/")));
    }

    #[test]
    fn a_typed_target_is_held_to_http_or_https_with_no_whitespace() {
        assert_eq!(
            parse_target("  https://example.com/x ").unwrap().as_str(),
            "https://example.com/x"
        );
        assert_eq!(parse_target("about:blank").unwrap().as_str(), BLANK);
        assert_eq!(parse_target("file:///tmp").unwrap_err().kind, "badUrl");
        assert_eq!(parse_target("http://a b").unwrap_err().kind, "badUrl");
        assert_eq!(parse_target("localhost:5173").unwrap_err().kind, "badUrl");
    }

    #[test]
    fn the_worktree_and_global_caps_refuse_rather_than_evict() {
        let host = Host::default();
        for _ in 0..MAX_PER_WORKTREE {
            host.register("p", "wt-a", BLANK, None).unwrap();
        }
        let refused = host.register("p", "wt-a", BLANK, None).unwrap_err();
        assert_eq!(refused.kind, "browserCap");
        assert_eq!(host.list(Some("wt-a")).len(), MAX_PER_WORKTREE);

        for _ in MAX_PER_WORKTREE..MAX_TOTAL {
            host.register("p", "wt-b", BLANK, None).unwrap();
        }
        let refused = host.register("p", "wt-c", BLANK, None).unwrap_err();
        assert_eq!(refused.kind, "browserCap");
        assert_eq!(host.list(None).len(), MAX_TOTAL);
    }

    #[test]
    fn a_browser_label_carries_the_prefix_and_is_never_a_session_id() {
        let host = Host::default();
        let id = host.register("p", "wt", BLANK, Some("sess-1")).unwrap();
        assert!(id.starts_with(LABEL_PREFIX));
        assert_ne!(id, "sess-1");
        assert_eq!(host.ids_opened_by("sess-1"), vec![id.clone()]);
        assert_eq!(
            host.view_of(&id).unwrap().opened_by.as_deref(),
            Some("sess-1")
        );
    }

    #[test]
    fn a_forgotten_browser_answers_nothing() {
        let host = Host::default();
        let id = host.register("p", "wt", BLANK, None).unwrap();
        assert!(host.forget(&id).is_some());
        assert!(host.view_of(&id).is_none());
        assert!(host.update(&id, |entry| entry.title = "x".into()).is_none());
        assert!(host.forget(&id).is_none());
    }

    #[test]
    fn a_runtime_reply_resolves_exactly_the_request_that_asked() {
        let host = Host::default();
        let (tx1, rx1) = mpsc::channel();
        let (tx2, rx2) = mpsc::channel();
        host.pending.lock().insert(1, ("b".to_owned(), tx1));
        host.pending.lock().insert(2, ("b".to_owned(), tx2));
        host.resolve(2, Ok(json!("second")));
        assert_eq!(rx2.try_recv().unwrap().unwrap(), json!("second"));
        assert!(rx1.try_recv().is_err());
        // Zero is the fire-and-forget id and never resolves anything.
        host.resolve(0, Ok(Value::Null));
        assert!(rx1.try_recv().is_err());
    }

    #[test]
    fn closing_a_browser_disconnects_the_replies_it_still_owed() {
        let host = Host::default();
        let id = host.register("p", "wt", BLANK, None).unwrap();
        let (tx, rx) = mpsc::channel();
        host.pending.lock().insert(7, (id.clone(), tx));
        host.forget(&id);
        assert!(matches!(
            rx.try_recv(),
            Err(mpsc::TryRecvError::Disconnected)
        ));
    }

    #[test]
    fn comments_are_owned_by_their_browser_and_die_with_it() {
        let host = Host::default();
        let id = host.register("p", "wt", BLANK, None).unwrap();
        host.update(&id, |entry| {
            entry.comments.push(Comment {
                id: entry.next_comment,
                text: "make it blue".to_owned(),
                status: CommentStatus::Open,
                anchor: anchor("#save"),
            });
            entry.next_comment += 1;
        });
        assert_eq!(host.comments_of(&id).unwrap().len(), 1);
        host.forget(&id);
        assert!(host.comments_of(&id).is_none());
    }

    #[test]
    fn the_console_ring_is_bounded_by_entries_and_by_bytes() {
        let host = Host::default();
        let id = host.register("p", "wt", BLANK, None).unwrap();
        for i in 0..(MAX_CONSOLE_ENTRIES + 50) {
            host.update(&id, |entry| {
                push_console(entry, "log".into(), format!("line {i}"));
            });
        }
        let lines = host.console_of(&id, false).unwrap();
        assert_eq!(lines.len(), MAX_CONSOLE_ENTRIES);
        assert_eq!(lines[0].text, "line 50");

        host.console_of(&id, true);
        host.update(&id, |entry| {
            push_console(entry, "log".into(), "x".repeat(MAX_CONSOLE_BYTES));
            push_console(entry, "log".into(), "after".into());
        });
        let lines = host.console_of(&id, false).unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "after");
    }

    #[test]
    fn the_apps_own_bootstrap_failures_are_kept_out_of_the_pages_console() {
        let host = Host::default();
        let id = host.register("p", "wt", BLANK, None).unwrap();
        host.update(&id, |entry| {
            push_console(
                entry,
                "warn".into(),
                "IPC custom protocol failed, Tauri will now use the postMessage interface instead"
                    .into(),
            );
            push_console(
                entry,
                "error".into(),
                "notification.is_permission_granted not allowed. Permissions associated with this command: …"
                    .into(),
            );
            push_console(entry, "log".into(), "the page said this".into());
        });
        let lines = host.console_of(&id, false).unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "the page said this");
    }

    #[test]
    fn one_agent_drives_a_browser_at_a_time() {
        let host = Host::default();
        let id = host.register("p", "wt", BLANK, None).unwrap();
        assert!(host.begin_driving(&id, "Claude Code").is_ok());
        let busy = host.begin_driving(&id, "Codex").unwrap_err();
        assert!(busy.contains("Claude Code"));
        host.end_driving(&id);
        assert!(host.begin_driving(&id, "Codex").is_ok());
    }

    #[test]
    fn every_runtime_method_rust_calls_is_defined_in_the_runtime_file() {
        // The one check a JS file with no test runner can get: the names Rust dispatches exist.
        for method in [
            "snapshot",
            "pageInfo",
            "getContent",
            "click",
            "hover",
            "type",
            "press",
            "selectOption",
            "scroll",
            "fillForm",
            "waitFor",
            "highlight",
            "setTheme",
            "setCommentMode",
            "renderPins",
        ] {
            assert!(
                RUNTIME_JS.contains(&format!("\n    {method}")),
                "runtime/browser.js does not list `{method}` in its method table"
            );
        }
        assert!(RUNTIME_JS.contains("globalThis.__wtm = Object.freeze({ dispatch"));
        assert!(RUNTIME_JS.contains(&format!("const HANDLER = '{RUNTIME_HANDLER}'")));
        assert!(PAGE_HOOK_JS.contains(&format!("messageHandlers.{PAGE_HANDLER}")));
    }

    #[test]
    fn the_data_store_id_is_stable_and_fully_populated() {
        assert_eq!(data_store_id(), data_store_id());
        assert!(data_store_id().iter().all(|byte| *byte != 0));
    }
}
