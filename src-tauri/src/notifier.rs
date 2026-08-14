//! Posting notifications, and receiving the clicks they get.
//!
//! # Two paths, one of which can hear a click
//!
//! On a bundled macOS build, [`wtm_notify::Center`] posts through `UNUserNotificationCenter`
//! with the navigation payload riding along, and its delegate hands a click back here to be
//! emitted at the webview as [`NOTIFICATION_CLICKED_EVENT`]. Everywhere that center cannot
//! exist — Linux, or a `tauri dev` binary with no bundle identifier — posting falls back to
//! the notification plugin, which is exactly the pre-click behaviour this module replaced:
//! the notification appears, clicking it activates the window, and nothing navigates. The
//! fallback is the floor, not a regression.
//!
//! # Why the decision to *send* stays in the webview
//!
//! Unchanged from before this module existed: `attention.svelte.ts` knows which worktree is
//! selected and whether the window is in front, and Rust does not. This side only posts what
//! it is told to and reports what the OS says about delivery.

use std::sync::Arc;

use tauri::Emitter;
use tauri_plugin_notification::NotificationExt;

use crate::view::ErrorView;
use wtm_notify::{Center, ClickPayload, Permission};

/// The event a notification click arrives on. `App.svelte` listens for it; the payload is a
/// [`ClickPayload`], whose JSON shape `wtm-notify`'s round-trip test pins.
pub const NOTIFICATION_CLICKED_EVENT: &str = "notification:clicked";

/// Managed state: the native center when there is one, else the plugin fallback.
#[derive(Debug)]
pub struct Notifier {
    center: Option<Center>,
}

impl Notifier {
    /// Attach to the OS notification center and route its clicks at the webview.
    pub fn attach(handle: &tauri::AppHandle) -> Self {
        let emitter = handle.clone();
        let center = Center::attach(move |payload| {
            // A failed emit means the webview is gone. A click can also arrive before the
            // frontend's listener is up (the app was *relaunched* by it) — the frontend's
            // `booted` guard drops that one, and the click still activated the window.
            let _ = emitter.emit(NOTIFICATION_CLICKED_EVENT, &payload);
        });
        if center.is_none() {
            tracing::info!(
                "no native notification center (unbundled or non-mac); clicks will not navigate"
            );
        }
        Self { center }
    }

    /// Post a notification. On the native path its click will navigate; on the fallback path
    /// it merely appears.
    ///
    /// # Errors
    ///
    /// When the OS refuses delivery — the signal `attention.blocked` keys off.
    pub fn post(
        &self,
        handle: &tauri::AppHandle,
        title: &str,
        body: &str,
        payload: &ClickPayload,
    ) -> Result<(), ErrorView> {
        match &self.center {
            Some(center) => center
                .post(title, body, payload)
                .map_err(|e| ErrorView::new("notification", e.to_string())),
            None => handle
                .notification()
                .builder()
                .title(title)
                .body(body)
                .show()
                .map_err(|e| ErrorView::new("notification", e.to_string())),
        }
    }

    /// What the OS says about delivery, in the vocabulary the web permission API taught
    /// everyone: `granted`, `denied` or `prompt`.
    ///
    /// The fallback path reports `granted` because the plugin's desktop backends have no
    /// permission model to consult — a lie in no direction anything can act on.
    #[must_use]
    pub fn permission(&self) -> &'static str {
        match &self.center {
            Some(center) => match center.permission() {
                Permission::Granted => "granted",
                Permission::Denied => "denied",
                Permission::Undetermined => "prompt",
            },
            None => "granted",
        }
    }

    /// Ask the OS for permission. Blocks until the user answers, so callers go through
    /// `blocking()`. Trivially true on the fallback path, where there is nothing to ask.
    #[must_use]
    pub fn request_permission(&self) -> bool {
        match &self.center {
            Some(center) => center.request_permission(),
            None => true,
        }
    }
}

/// Install the notifier as managed state. Called from `setup`, which is the first moment an
/// [`tauri::AppHandle`] exists to emit clicks with.
pub fn install(handle: &tauri::AppHandle) {
    use tauri::Manager;
    let notifier = Notifier::attach(handle);
    handle.manage(Arc::new(notifier));
}
