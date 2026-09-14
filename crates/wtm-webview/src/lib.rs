//! Native WebKit calls the browser pane needs and Tauri does not expose.
//!
//! # Why this crate exists
//!
//! An agent drives a browser pane through JavaScript injected into the page. Tauri can inject
//! scripts and evaluate them, but only in the **page world** — the same global scope the page's own
//! scripts run in. A page that wanted to lie to an agent could redefine `document.querySelectorAll`
//! or wrap `postMessage`, and a page with a strict `connect-src` would block any channel the runtime
//! tried to open back to the app. WebKit's answer to both is a `WKContentWorld`: a separate
//! JavaScript scope that shares the DOM but nothing else, with its own `webkit.messageHandlers`
//! that CSP does not govern. Reaching it needs four calls wry does not make — add a user script in
//! a world, add a message handler in a world, evaluate in a world, and take a snapshot — and this
//! crate is those four calls.
//!
//! # The shape: a borrowed handle, and a receiver that leaves
//!
//! Tauri hands the platform webview to a closure on the main thread and takes it back when the
//! closure returns. [`Handle`] is built inside that closure from the two raw pointers Tauri
//! provides, is `!Send`, and lives no longer than the closure. Nothing native is ever stored in the
//! app's state; the only thing that crosses back is a [`Reply`] — a channel receiver the caller
//! waits on with a bounded timeout, which is the synchronous-ports shape ARCHITECTURE §3 asks for.
//!
//! # Two arms, one seam
//!
//! Same layout as `wtm-notify`: `mac.rs` behind `cfg(target_os = "macos")`, and a no-op arm whose
//! [`Handle`] is uninhabited, so [`Handle::attach`] answering `None` is the portable spelling of
//! "there is no WebKit here". `lib.rs` is the only file with the seam and `platform_seams.rs` in
//! `src-tauri` lists it by name. It is also the second crate in the workspace allowed to contain
//! `unsafe`; the `Cargo.toml` header says why.

#![cfg_attr(test, allow(clippy::unwrap_used))]

#[cfg(target_os = "macos")]
mod mac;
#[cfg(target_os = "macos")]
use mac as imp;
#[cfg(not(target_os = "macos"))]
mod noop;
#[cfg(not(target_os = "macos"))]
use noop as imp;

use std::ffi::c_void;
use std::sync::mpsc;
use std::time::Duration;

/// Which JavaScript scope a script or a handler belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum World {
    /// A named world the page cannot see. The runtime lives here.
    Isolated(String),
    /// The page's own scope. Only the console hook lives here, and it is treated as untrusted.
    Page,
}

/// A message a script posted to one of this crate's handlers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// The handler name the script posted to.
    pub name: String,
    /// The body, which every script this app installs posts as a JSON string.
    pub body: String,
    /// Whether the posting frame is the page's main frame.
    pub main_frame: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The page threw, or the script did not parse. Carries WebKit's description.
    #[error("the page's JavaScript failed: {0}")]
    Js(String),
    /// The completion never came inside the caller's deadline.
    #[error("the page did not answer in time")]
    Timeout,
    /// The webview went away before it could answer.
    #[error("the browser closed before it could answer")]
    Detached,
    /// WebKit could not produce a picture — a hidden view, most often.
    #[error("the page could not be captured: {0}")]
    Snapshot(String),
    /// The result was not the string this app's scripts always post.
    #[error("the page answered with something that was not a string")]
    NotAString,
}

/// The callback a message lands on. Boxed once at the facade so both arms share a spelling.
pub(crate) type OnMessage = Box<dyn Fn(Message) + Send + Sync>;

/// The answer to an evaluation or a snapshot, delivered later.
///
/// A receiver rather than a value because the completion arrives on the main thread some time
/// after the closure that asked has returned. `Send`, so it can leave the closure; the caller
/// waits on it from a worker thread with [`Reply::wait`].
#[derive(Debug)]
pub struct Reply<T>(mpsc::Receiver<Result<T, Error>>);

impl<T> Reply<T> {
    /// Wait for the answer, or give up after `timeout`.
    ///
    /// A channel wait with a duration, not a poll: `recv_timeout` blocks the worker until the main
    /// thread sends, and consults no clock this code owns. A disconnect — the block dropped
    /// without sending — reads as the webview having gone.
    pub fn wait(self, timeout: Duration) -> Result<T, Error> {
        match self.0.recv_timeout(timeout) {
            Ok(answer) => answer,
            Err(mpsc::RecvTimeoutError::Timeout) => Err(Error::Timeout),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(Error::Detached),
        }
    }
}

/// A borrowed webview, alive only inside one `with_webview` closure on the main thread.
pub struct Handle(imp::Handle);

impl std::fmt::Debug for Handle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The inner value is an FFI handle with nothing legible inside it.
        f.write_str("Handle")
    }
}

impl Handle {
    /// Borrow the webview behind Tauri's two platform pointers.
    ///
    /// `webview` and `controller` are `PlatformWebview::inner()` and `::controller()` — a
    /// `WKWebView` and its `WKUserContentController`. `None` off the main thread, for a null
    /// pointer, for an object that is not a `WKWebView`, or on a platform with no WebKit at all.
    /// Safe to call because it checks all four before it trusts either pointer; the precondition it
    /// cannot check — that the pointers are live for the duration of the closure — is the one
    /// Tauri's `with_webview` documents and guarantees.
    #[must_use]
    pub fn attach(webview: *mut c_void, controller: *mut c_void) -> Option<Self> {
        imp::attach(webview, controller).map(Self)
    }

    /// Add a script that runs at document start on every page this webview loads.
    pub fn install_script(&self, world: &World, source: &str, main_frame_only: bool) {
        self.0.install_script(world, source, main_frame_only);
    }

    /// Register `name` in `world`, so `webkit.messageHandlers.<name>.postMessage(s)` reaches `on`.
    ///
    /// The controller retains the handler for as long as the webview lives; nothing here needs to.
    pub fn add_message_handler(
        &self,
        world: &World,
        name: &str,
        on: impl Fn(Message) + Send + Sync + 'static,
    ) {
        self.0.add_message_handler(world, name, Box::new(on));
    }

    /// Run `js` in `world` and hand back what it returned, which must be a string.
    pub fn evaluate(&self, world: &World, js: &str) -> Reply<String> {
        self.0.evaluate(world, js)
    }

    /// A PNG of the visible page, or of `rect` within it (in the view's own points).
    pub fn snapshot(&self, rect: Option<(f64, f64, f64, f64)>) -> Reply<Vec<u8>> {
        self.0.snapshot(rect)
    }

    /// The webview's own word on its history, which nothing else can supply.
    #[must_use]
    pub fn history(&self) -> (bool, bool) {
        self.0.history()
    }
}

/// Whether this thread is the platform's main thread, where every WebKit call must happen.
///
/// `false` on platforms with no WebKit, so a debug assertion built on it — "never wait for a reply
/// here" — cannot misfire there.
#[must_use]
pub fn on_main_thread() -> bool {
    imp::on_main_thread()
}

/// Whether this build can reach WebKit at all. The browser pane's runtime features need it.
#[must_use]
pub fn available() -> bool {
    imp::available()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reply_nobody_answers_reads_as_detached_rather_than_hanging() {
        let (tx, rx) = mpsc::channel::<Result<String, Error>>();
        drop(tx);
        let reply = Reply(rx);
        assert!(matches!(
            reply.wait(Duration::from_millis(50)),
            Err(Error::Detached)
        ));
    }

    #[test]
    fn a_reply_that_is_late_times_out_against_the_callers_deadline() {
        let (_tx, rx) = mpsc::channel::<Result<String, Error>>();
        let reply = Reply(rx);
        assert!(matches!(
            reply.wait(Duration::from_millis(20)),
            Err(Error::Timeout)
        ));
    }

    #[test]
    fn a_reply_that_arrives_is_handed_back_as_sent() {
        let (tx, rx) = mpsc::channel::<Result<String, Error>>();
        tx.send(Ok("\"hello\"".to_owned())).unwrap();
        let reply = Reply(rx);
        assert_eq!(reply.wait(Duration::from_secs(1)).unwrap(), "\"hello\"");
    }

    #[test]
    fn the_noop_arm_and_a_null_pointer_both_attach_to_nothing() {
        // On macOS this exercises the null check; elsewhere the arm itself. Either way: no handle.
        assert!(Handle::attach(std::ptr::null_mut(), std::ptr::null_mut()).is_none());
    }
}
