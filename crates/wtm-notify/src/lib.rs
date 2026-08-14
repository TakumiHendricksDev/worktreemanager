//! Native notifications whose clicks carry a payload back to the app.
//!
//! # Why this crate exists
//!
//! The Tauri notification plugin can post, but a click on what it posts is invisible: its
//! action listeners are mobile-only, and its macOS backend (`notify-rust` over the deprecated
//! `NSUserNotificationCenter`) cannot observe a click on a plain notification at all. Landing
//! the user on the worktree a notification is about therefore needs the modern framework —
//! `UNUserNotificationCenter`, a delegate, and a payload that survives the round trip.
//!
//! That framework only exists on macOS and only answers to a bundled app, so this crate is a
//! facade with two arms: [`mac`](self) behind `cfg(target_os = "macos")`, and a no-op arm whose
//! [`Center`] is uninhabited — [`Center::attach`] returning `None` is the portable spelling of
//! "there is nothing to attach to", and the caller falls back to the plugin path it already had.
//! `lib.rs` is deliberately the only file with a platform seam; `platform_seams.rs` in
//! `src-tauri` lists it by name.
//!
//! It is also the one crate in the workspace allowed to contain `unsafe` — see the header of
//! its `Cargo.toml` for the carve-out, and `mac.rs` for the sites.

#![cfg_attr(test, allow(clippy::unwrap_used))]

#[cfg(target_os = "macos")]
mod mac;
#[cfg(target_os = "macos")]
use mac as imp;
#[cfg(not(target_os = "macos"))]
mod noop;
#[cfg(not(target_os = "macos"))]
use noop as imp;

use serde::{Deserialize, Serialize};

/// What a clicked notification navigates to.
///
/// `pane_id` is process-local (`pane-<n>`) and best-effort: a notification can outlive the pane
/// it was about. `(project_id, worktree_id)` is the durable key — the frontend treats a stale
/// pane as "arrive at the worktree, skip the focus".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
// The shared `_id` postfix is the point: these are the frontend's own `projectId` /
// `worktreeId` / `paneId` spellings, which the serde rename derives from the field names.
#[allow(clippy::struct_field_names)]
pub struct ClickPayload {
    pub project_id: String,
    pub worktree_id: String,
    pub pane_id: String,
}

/// Whether the OS will deliver, in the three states macOS actually has.
///
/// Not a boolean because `Undetermined` is real and actionable: it means the OS has never been
/// asked, which is exactly when asking is the right move rather than reporting a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    Granted,
    Denied,
    Undetermined,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The OS is refusing to deliver. The fix is in System Settings, where the app cannot
    /// reach — surfaced rather than swallowed, so the frontend can say so.
    #[error("macOS is not delivering notifications for this app")]
    Denied,
    /// The payload could not be encoded. Unreachable for the struct above, but lying about
    /// that with an `unwrap` would make the one impossible case an abort.
    #[error("the click payload could not be encoded: {0}")]
    Payload(#[from] serde_json::Error),
}

/// The callback a click lands on. Boxed once at the facade so both arms share a spelling.
pub(crate) type OnClick = Box<dyn Fn(ClickPayload) + Send + Sync>;

/// A handle on the OS notification center, holding the delegate that receives clicks.
pub struct Center(imp::Center);

impl std::fmt::Debug for Center {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The inner value is an FFI handle with nothing legible inside it.
        f.write_str("Center")
    }
}

impl Center {
    /// Attach to the OS notification center, or `None` where there is none to attach to: a
    /// non-mac platform, or a bare binary with no bundle identifier (`tauri dev`, tests). The
    /// caller falls back to the plugin path in either case, so this cannot fail loudly.
    pub fn attach(on_click: impl Fn(ClickPayload) + Send + Sync + 'static) -> Option<Self> {
        imp::attach(Box::new(on_click)).map(Self)
    }

    /// What the OS currently says about delivery.
    #[must_use]
    pub fn permission(&self) -> Permission {
        self.0.permission()
    }

    /// Ask the OS for permission, blocking until the user answers the prompt (or a bounded
    /// timeout, read as a refusal). Called from a worker thread, never an event loop.
    pub fn request_permission(&self) -> bool {
        self.0.request_permission()
    }

    /// Post a notification whose click will deliver `payload` to the `attach` callback.
    ///
    /// # Errors
    ///
    /// [`Error::Denied`] when the OS is refusing delivery — the signal the frontend's
    /// "notifications are blocked" warning keys off.
    pub fn post(&self, title: &str, body: &str, payload: &ClickPayload) -> Result<(), Error> {
        self.0.post(title, body, payload)
    }
}

#[cfg(test)]
mod tests {
    use super::ClickPayload;

    /// Pins the exact JSON that rides the notification and the `notification:clicked` event.
    /// The frontend's `NotificationClick` mirror in `types.ts` is hand-written against this
    /// string, so a serde rename drifting here must fail a test rather than break a click.
    #[test]
    fn a_click_payload_round_trips_as_camel_case_json() {
        let payload = ClickPayload {
            project_id: "proj".to_owned(),
            worktree_id: "/repos/thing/wt".to_owned(),
            pane_id: "pane-3".to_owned(),
        };
        let json = serde_json::to_string(&payload).unwrap();
        assert_eq!(
            json,
            r#"{"projectId":"proj","worktreeId":"/repos/thing/wt","paneId":"pane-3"}"#
        );
        let back: ClickPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(back, payload);
    }
}
