//! The macOS arm: `UNUserNotificationCenter`, a delegate, and the payload round trip.
//!
//! # Where the payload rides
//!
//! The click payload is the notification request's *identifier*, not `userInfo` — deliberately.
//! An identifier round-trips to the delegate exactly like `userInfo` does, but it is a plain
//! string rather than an `NSDictionary` of property-list objects, and macOS **replaces** a
//! pending notification that reuses one — so a pane that needs the user twice shows one banner,
//! which is the one-toast-per-pane policy `attention` already chose for the in-app route.
//!
//! # Why `unsafe` appears here and nowhere else in the workspace
//!
//! The 0.3 objc2 bindings make the center's own methods safe; what cannot be safe is defining
//! an Objective-C class (`define_class!` is a contract with the runtime), sending `init` to a
//! superclass, dereferencing the pointer a completion block is handed, and reading a framework's
//! `extern` constant. Each site carries its own SAFETY comment; the crate-level lint is `deny`,
//! loosened for this module alone.
#![allow(unsafe_code)]

use std::ptr::NonNull;
use std::sync::mpsc;
use std::time::Duration;

use block2::{DynBlock, RcBlock};
use objc2::rc::Retained;
use objc2::runtime::{Bool, ProtocolObject};
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_foundation::{NSBundle, NSError, NSObject, NSObjectProtocol, NSString};
use objc2_user_notifications::{
    UNAuthorizationOptions, UNAuthorizationStatus, UNErrorCode, UNErrorDomain,
    UNMutableNotificationContent, UNNotificationRequest, UNNotificationResponse,
    UNNotificationSettings, UNUserNotificationCenter, UNUserNotificationCenterDelegate,
};

use crate::{ClickPayload, Error, OnClick, Permission};

/// How long a settings query may take. It involves no user and answers promptly; the bound
/// only exists so a wedged notification daemon cannot park a command thread forever.
const SETTINGS_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the user may sit on the OS permission prompt before silence reads as "no".
/// Generous, because the completion genuinely waits for a human.
const PROMPT_TIMEOUT: Duration = Duration::from_secs(300);

pub(crate) struct Center {
    center: Retained<UNUserNotificationCenter>,
    /// `setDelegate:` is a weak property; this field is the strong reference that keeps the
    /// delegate alive for as long as clicks can arrive.
    _delegate: Retained<Delegate>,
}

// SAFETY: Apple documents UNUserNotificationCenter as usable from any thread, and the
// delegate's only state is the `Send + Sync` closure it calls. Nothing here has interior
// mutability of its own.
unsafe impl Send for Center {}
// SAFETY: as above — every method takes `&self` and forwards to a thread-safe API.
unsafe impl Sync for Center {}

pub(crate) fn attach(on_click: OnClick) -> Option<Center> {
    // Guard FIRST. A bare binary — `tauri dev`, a test runner — has no bundle identifier, and
    // `currentNotificationCenter` raises `NSInternalInconsistencyException` from one; an ObjC
    // exception unwinding into Rust aborts the process. `NSBundle` is the safe probe.
    NSBundle::mainBundle().bundleIdentifier()?;

    let delegate = Delegate::new(on_click);
    let center = UNUserNotificationCenter::currentNotificationCenter();
    center.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    Some(Center {
        center,
        _delegate: delegate,
    })
}

impl Center {
    pub(crate) fn permission(&self) -> Permission {
        let (tx, rx) = mpsc::channel();
        let block = RcBlock::new(move |settings: NonNull<UNNotificationSettings>| {
            // SAFETY: the pointer UserNotifications hands its completion block is valid for
            // the duration of the call, which is the only place it is read.
            let status = unsafe { settings.as_ref() }.authorizationStatus();
            let _ = tx.send(status);
        });
        self.center
            .getNotificationSettingsWithCompletionHandler(&block);

        // A bounded wait on a channel, not a poll — the sync-ports shape the architecture
        // mandates. Timing out reads as "unknown", the state that asks rather than fails.
        match rx.recv_timeout(SETTINGS_TIMEOUT) {
            Ok(UNAuthorizationStatus::Authorized | UNAuthorizationStatus::Provisional) => {
                Permission::Granted
            }
            Ok(UNAuthorizationStatus::Denied) => Permission::Denied,
            _ => Permission::Undetermined,
        }
    }

    pub(crate) fn request_permission(&self) -> Result<bool, Error> {
        let (tx, rx) = mpsc::channel();
        let block = RcBlock::new(move |granted: Bool, error: *mut NSError| {
            // SAFETY: the error pointer is null or valid for the duration of the call, as with the
            // settings block above, and it is only read here.
            let refused = unsafe { error.as_ref() }.is_some_and(not_allowed);
            let _ = tx.send((granted.as_bool(), refused));
        });
        self.center
            .requestAuthorizationWithOptions_completionHandler(
                UNAuthorizationOptions::Alert | UNAuthorizationOptions::Sound,
                &block,
            );
        // The error used to be dropped, which made this indistinguishable from the user clicking
        // Don't Allow — and made the frontend send them to System Settings, where an unsigned build
        // is not listed at all. Answered in about ten milliseconds, measured against a probe app
        // signed the way `tauri build` signs wtm; the timeout is for a human at a real prompt.
        match rx.recv_timeout(PROMPT_TIMEOUT) {
            Ok((_, true)) => Err(Error::NotAllowed),
            Ok((granted, false)) => Ok(granted),
            Err(_) => Ok(false),
        }
    }

    pub(crate) fn post(
        &self,
        title: &str,
        body: &str,
        payload: &ClickPayload,
    ) -> Result<(), Error> {
        // Checked per post rather than cached: the user can flip the switch in System
        // Settings at any moment, and a stale "granted" would swallow notifications silently
        // — the exact failure `attention.blocked` exists to surface.
        if self.permission() != Permission::Granted {
            return Err(Error::Denied);
        }

        let content = UNMutableNotificationContent::new();
        content.setTitle(&NSString::from_str(title));
        content.setBody(&NSString::from_str(body));

        // The payload is the identifier — see the module docs for why.
        let identifier = NSString::from_str(&serde_json::to_string(payload)?);
        let request = UNNotificationRequest::requestWithIdentifier_content_trigger(
            &identifier,
            &content,
            None,
        );
        // Fire and forget, like the plugin path this replaces: a post that fails is not worth
        // a banner over the thing it was reporting, and `Denied` above already covers the case
        // where none of them will ever arrive.
        self.center
            .addNotificationRequest_withCompletionHandler(&request, None);
        Ok(())
    }
}

/// Whether `error` is the daemon refusing to deal with this app at all.
///
/// `UNErrorCodeNotificationsNotAllowed` is what `requestAuthorization` answers when
/// `usernotificationsd` turns the connection away — logged on its side as "Entitlement
/// 'com.apple.private.usernotifications.bundle-identifiers' required to request user
/// notifications", because an app signed under one name asking for another bundle is what that
/// entitlement exists for.
fn not_allowed(error: &NSError) -> bool {
    // SAFETY: an immutable framework constant, read once per answer.
    let domain = unsafe { UNErrorDomain };
    domain.is_some_and(|domain| *error.domain() == *domain)
        && error.code() == UNErrorCode::NotificationsNotAllowed.0
}

struct Ivars {
    on_click: OnClick,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and `Delegate` does not implement Drop.
    #[unsafe(super(NSObject))]
    #[name = "WtmNotificationDelegate"]
    #[ivars = Ivars]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    // SAFETY: the one method implemented matches the protocol's declared signature exactly;
    // the others are optional and deliberately absent — in particular `willPresentNotification`,
    // whose default (suppress while the app is frontmost) matches the store's own gate, which
    // posts nothing while the window is in front anyway.
    unsafe impl UNUserNotificationCenterDelegate for Delegate {
        /// The click — the "default action" — including the one that relaunched the app.
        #[unsafe(method(userNotificationCenter:didReceiveNotificationResponse:withCompletionHandler:))]
        fn user_notification_center_did_receive_notification_response(
            &self,
            _center: &UNUserNotificationCenter,
            response: &UNNotificationResponse,
            completion_handler: &DynBlock<dyn Fn()>,
        ) {
            let identifier = response.notification().request().identifier().to_string();
            // A notification this build did not post parses as nothing and is ignored — the
            // click still activates the app, which is all it ever did before this crate.
            if let Ok(payload) = serde_json::from_str::<ClickPayload>(&identifier) {
                (self.ivars().on_click)(payload);
            }
            completion_handler.call(());
        }
    }
);

impl Delegate {
    fn new(on_click: OnClick) -> Retained<Self> {
        let this = Self::alloc().set_ivars(Ivars { on_click });
        // SAFETY: `init` is NSObject's documented designated initializer, sent exactly once to
        // a freshly allocated instance.
        unsafe { msg_send![super(this), init] }
    }
}
