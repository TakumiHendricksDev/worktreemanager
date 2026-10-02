//! The macOS arm: `WKContentWorld`, a script message handler, world-scoped evaluation, snapshots.
//!
//! # Why `unsafe` appears here
//!
//! Every objc2-web-kit method is `unsafe fn` in the 0.3 bindings — WebKit's API has thread and
//! lifetime preconditions the bindings cannot express — so this module is a sequence of one-line
//! SAFETY comments, each citing the same two facts: the call is on the main thread (proved by the
//! `MainThreadMarker` the handle carries) and the references are live (they are `Retained`, held
//! by the handle for the closure's duration). The genuinely delicate sites are [`attach`], which
//! trusts pointers Tauri produced, the completion blocks, which read a pointer WebKit hands them,
//! and [`NavigationObserver`], which stands in for another library's delegate; each says what it
//! relies on.
//!
//! # Why the handle retains
//!
//! Tauri gives `with_webview` raw pointers into objects it keeps alive for the closure. Retaining
//! them here costs a retain/release pair and buys a `Retained<WKWebView>` the rest of the module can
//! call methods on without a second `unsafe` deref at every site. The handle never leaves the
//! closure, so the retain is released before Tauri's own reference could be.
#![allow(unsafe_code)]

use std::ffi::c_void;
use std::sync::mpsc;

use block2::RcBlock;
use objc2::rc::{Retained, Weak};
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::{
    ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send, sel,
};
use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSImage};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSDictionary, NSError, NSObject, NSObjectProtocol, NSString, NSURL};
use objc2_web_kit::{
    WKContentWorld, WKNavigation, WKNavigationDelegate, WKScriptMessage, WKScriptMessageHandler,
    WKSnapshotConfiguration, WKUserContentController, WKUserScript, WKUserScriptInjectionTime,
    WKWebView,
};

use crate::{Error, LoadFailure, Message, Navigation, OnMessage, OnNavigation, Reply, World};

/// The key a [`NavigationObserver`] is attached under. Only its address matters.
static OBSERVER_KEY: u8 = 0;

pub(crate) struct Handle {
    view: Retained<WKWebView>,
    controller: Retained<WKUserContentController>,
    mtm: MainThreadMarker,
}

pub(crate) fn on_main_thread() -> bool {
    MainThreadMarker::new().is_some()
}

pub(crate) fn available() -> bool {
    true
}

pub(crate) fn attach(webview: *mut c_void, controller: *mut c_void) -> Option<Handle> {
    // Main thread first: WKWebView is main-thread-only, and a marker is also what every call
    // below needs to construct a content world.
    let mtm = MainThreadMarker::new()?;
    if webview.is_null() || controller.is_null() {
        return None;
    }
    // SAFETY: both pointers come from Tauri's `PlatformWebview`, which documents them as the
    // live `WKWebView` and `WKUserContentController` for the duration of the `with_webview`
    // closure this is called from. They are checked non-null above and checked for class below
    // before anything is sent to them; `retain` adds a reference this handle releases on drop.
    let (view, controller) = unsafe {
        let object: &AnyObject = &*webview.cast::<AnyObject>();
        let is_view: bool = msg_send![object, isKindOfClass: WKWebView::class()];
        if !is_view {
            return None;
        }
        let manager: &AnyObject = &*controller.cast::<AnyObject>();
        let is_controller: bool =
            msg_send![manager, isKindOfClass: WKUserContentController::class()];
        if !is_controller {
            return None;
        }
        (
            Retained::retain(webview.cast::<WKWebView>())?,
            Retained::retain(controller.cast::<WKUserContentController>())?,
        )
    };
    Some(Handle {
        view,
        controller,
        mtm,
    })
}

impl Handle {
    fn world(&self, world: &World) -> Retained<WKContentWorld> {
        // SAFETY: both constructors only require the main thread, which `mtm` proves.
        unsafe {
            match world {
                World::Isolated(name) => {
                    WKContentWorld::worldWithName(&NSString::from_str(name), self.mtm)
                }
                World::Page => WKContentWorld::pageWorld(self.mtm),
            }
        }
    }

    pub(crate) fn install_script(&self, world: &World, source: &str, main_frame_only: bool) {
        let world = self.world(world);
        // SAFETY: `WKUserScript`'s designated initializer on a fresh allocation, then a method on a
        // live controller, both on the main thread.
        unsafe {
            let script = WKUserScript::initWithSource_injectionTime_forMainFrameOnly_inContentWorld(
                WKUserScript::alloc(self.mtm),
                &NSString::from_str(source),
                WKUserScriptInjectionTime::AtDocumentStart,
                main_frame_only,
                &world,
            );
            self.controller.addUserScript(&script);
        }
    }

    pub(crate) fn add_message_handler(&self, world: &World, name: &str, on: OnMessage) {
        let world = self.world(world);
        let handler = Handler::new(self.mtm, on);
        // SAFETY: a method on a live controller, on the main thread. The controller retains the
        // handler, which is why dropping our `Retained` at the end of this function is fine.
        unsafe {
            self.controller.addScriptMessageHandler_contentWorld_name(
                ProtocolObject::from_ref(&*handler),
                &world,
                &NSString::from_str(name),
            );
        }
    }

    pub(crate) fn evaluate(&self, world: &World, js: &str) -> Reply<String> {
        let world = self.world(world);
        let (tx, rx) = mpsc::channel();
        let block = RcBlock::new(move |value: *mut AnyObject, error: *mut NSError| {
            let answer = if !error.is_null() {
                // SAFETY: WebKit hands its completion block a live NSError when non-null; it is
                // read only inside the block.
                Err(Error::Js(
                    unsafe { &*error }.localizedDescription().to_string(),
                ))
            } else if value.is_null() {
                Err(Error::NotAString)
            } else {
                // SAFETY: as above, a live object for the duration of the block. `retain` because
                // `downcast` needs an owned pointer, and the extra reference is released with it.
                match unsafe { Retained::retain(value) } {
                    Some(object) => object
                        .downcast::<NSString>()
                        .map(|text| text.to_string())
                        .map_err(|_| Error::NotAString),
                    None => Err(Error::NotAString),
                }
            };
            let _ = tx.send(answer);
        });
        // SAFETY: a method on the live view, on the main thread; `None` for the frame means the
        // main frame, which is where the runtime is installed.
        unsafe {
            self.view
                .evaluateJavaScript_inFrame_inContentWorld_completionHandler(
                    &NSString::from_str(js),
                    None,
                    &world,
                    Some(&block),
                );
        }
        Reply(rx)
    }

    pub(crate) fn snapshot(&self, rect: Option<(f64, f64, f64, f64)>) -> Reply<Vec<u8>> {
        let (tx, rx) = mpsc::channel();
        // SAFETY: a fresh configuration on the main thread; setters on the object just made.
        let configuration = unsafe {
            let configuration = WKSnapshotConfiguration::new(self.mtm);
            configuration.setAfterScreenUpdates(true);
            if let Some((x, y, w, h)) = rect {
                configuration.setRect(CGRect {
                    origin: CGPoint { x, y },
                    size: CGSize {
                        width: w,
                        height: h,
                    },
                });
            }
            configuration
        };
        let block = RcBlock::new(move |image: *mut NSImage, error: *mut NSError| {
            let answer = if !error.is_null() {
                // SAFETY: a live NSError for the duration of the block, read only here.
                Err(Error::Snapshot(
                    unsafe { &*error }.localizedDescription().to_string(),
                ))
            } else if image.is_null() {
                Err(Error::Snapshot("no image was produced".to_owned()))
            } else {
                // SAFETY: a live NSImage for the duration of the block, read only here.
                png_of(unsafe { &*image })
            };
            let _ = tx.send(answer);
        });
        // SAFETY: a method on the live view, on the main thread.
        unsafe {
            self.view
                .takeSnapshotWithConfiguration_completionHandler(Some(&configuration), &block);
        }
        Reply(rx)
    }

    pub(crate) fn history(&self) -> (bool, bool) {
        // SAFETY: two getters on the live view, on the main thread.
        unsafe { (self.view.canGoBack(), self.view.canGoForward()) }
    }

    pub(crate) fn observe_navigation(&self, on: OnNavigation) {
        // SAFETY: a getter on the live view, on the main thread.
        let inner = unsafe { self.view.navigationDelegate() };
        let observer = NavigationObserver::new(self.mtm, inner.as_ref(), on);
        // `navigationDelegate` is weak, so something has to own the observer, and the choice is
        // the whole of its memory safety. It is attached to the delegate it forwards to: wry's,
        // which wry owns for as long as the view lives. That ties the two lifetimes together, so
        // WebKit's weak reference empties at the moment the forwarding target goes, and never
        // points at an observer with nothing behind it. Attaching it to the *view* instead would
        // be a cycle — wry's delegate holds the view strongly, and the observer would hold the
        // delegate — and every closed browser pane would leak its WebContent process.
        let owner: &AnyObject = match &inner {
            Some(inner) => inner.as_ref(),
            None => self.view.as_ref(),
        };
        // SAFETY: both objects are live; the key is a static whose address is unique to this
        // crate; the policy retains the observer, which the owner releases when it deallocates.
        // `setNavigationDelegate` is a setter on the live view, on the main thread.
        unsafe {
            objc2::ffi::objc_setAssociatedObject(
                std::ptr::from_ref(owner).cast_mut(),
                std::ptr::from_ref(&OBSERVER_KEY).cast(),
                Retained::as_ptr(&observer).cast_mut().cast(),
                objc2::ffi::OBJC_ASSOCIATION_RETAIN_NONATOMIC,
            );
            self.view
                .setNavigationDelegate(Some(ProtocolObject::from_ref(&*observer)));
        }
    }
}

/// What a failed navigation's `NSError` says, as plain data.
fn load_failure(view: &WKWebView, error: &NSError, committed: bool) -> LoadFailure {
    // The `NSURL` under `NSErrorFailingURLKey`, not the string under `…URLStringKey`: WebKit's
    // navigation errors carry only the former, observed on macOS 26 for both a refused connection
    // and a Stop. Spelled as the key's value rather than the `NSURLErrorFailingURLErrorKey` symbol,
    // which would need a Foundation feature for one string; the value is documented and stable.
    let url = error
        .userInfo()
        .objectForKey(&NSString::from_str("NSErrorFailingURLKey"))
        .and_then(|value| value.downcast::<NSURL>().ok())
        .and_then(|url| url.absoluteString())
        .map(|url| url.to_string());
    LoadFailure {
        domain: error.domain().to_string(),
        code: i64::try_from(error.code()).unwrap_or_default(),
        description: error.localizedDescription().to_string(),
        url,
        committed,
        // SAFETY: a getter on the live view WebKit is calling about, on the main thread.
        loading: unsafe { view.isLoading() },
    }
}

struct ObserverIvars {
    /// The delegate this one stands in front of. Weak, because that delegate owns this observer;
    /// see [`Handle::observe_navigation`].
    inner: Option<Weak<ProtocolObject<dyn WKNavigationDelegate>>>,
    on_navigation: OnNavigation,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and `NavigationObserver` does not
    // implement Drop.
    //
    // # How it stands in for wry's delegate
    //
    // A WKWebView has one navigation delegate, wry's is it, and wry implements commit, finish and
    // policy but not the two failure callbacks. So this becomes the delegate, implements those two
    // and the process-termination callback, and hands every other message to wry's through
    // `forwardingTargetForSelector:` — the runtime's fast forwarding, which re-sends the message to
    // that object unchanged, whatever its signature. WebKit asks `respondsToSelector:` for each
    // optional method once, when the delegate is set, so this answers for itself *and* for wry's;
    // a method neither implements is never sent, which is what keeps forwarding from ever reaching
    // a target that cannot answer.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "WtmBrowserNavigationObserver"]
    #[ivars = ObserverIvars]
    struct NavigationObserver;

    // SAFETY: both override NSObject methods with NSObject's own signatures: a selector in, a BOOL
    // out; a selector in, an unretained object (or nil) out.
    impl NavigationObserver {
        #[unsafe(method(respondsToSelector:))]
        fn responds_to_selector(&self, selector: Sel) -> bool {
            // SAFETY: NSObject's own implementation, answering for this class's methods.
            let own: bool = unsafe { msg_send![super(self), respondsToSelector: selector] };
            own || self
                .inner()
                .is_some_and(|inner| inner.respondsToSelector(selector))
        }

        #[unsafe(method(forwardingTargetForSelector:))]
        fn forwarding_target_for_selector(&self, selector: Sel) -> *mut AnyObject {
            match self.inner() {
                // Unretained, as the method's contract is: the delegate is owned by wry for as long
                // as this observer exists, so the pointer outlives the temporary dropped here.
                Some(inner) if inner.respondsToSelector(selector) => {
                    Retained::as_ptr(&inner).cast_mut().cast()
                }
                _ => std::ptr::null_mut(),
            }
        }
    }

    unsafe impl NSObjectProtocol for NavigationObserver {}

    // SAFETY: each method matches the protocol's declared signature. Each also passes the call on
    // to the inner delegate when it implements the method itself, so a later wry that starts
    // handling failures is not silently shadowed.
    unsafe impl WKNavigationDelegate for NavigationObserver {
        #[unsafe(method(webView:didFailProvisionalNavigation:withError:))]
        fn did_fail_provisional_navigation(
            &self,
            view: &WKWebView,
            navigation: Option<&WKNavigation>,
            error: &NSError,
        ) {
            (self.ivars().on_navigation)(Navigation::Failed(load_failure(view, error, false)));
            if let Some(inner) = self.inner()
                && inner.respondsToSelector(sel!(webView:didFailProvisionalNavigation:withError:))
            {
                // SAFETY: the inner delegate says it implements this; the arguments are WebKit's.
                unsafe {
                    inner.webView_didFailProvisionalNavigation_withError(view, navigation, error);
                }
            }
        }

        #[unsafe(method(webView:didFailNavigation:withError:))]
        fn did_fail_navigation(
            &self,
            view: &WKWebView,
            navigation: Option<&WKNavigation>,
            error: &NSError,
        ) {
            (self.ivars().on_navigation)(Navigation::Failed(load_failure(view, error, true)));
            if let Some(inner) = self.inner()
                && inner.respondsToSelector(sel!(webView:didFailNavigation:withError:))
            {
                // SAFETY: as above.
                unsafe { inner.webView_didFailNavigation_withError(view, navigation, error) };
            }
        }

        #[unsafe(method(webViewWebContentProcessDidTerminate:))]
        fn web_content_process_did_terminate(&self, view: &WKWebView) {
            (self.ivars().on_navigation)(Navigation::ContentProcessTerminated);
            if let Some(inner) = self.inner()
                && inner.respondsToSelector(sel!(webViewWebContentProcessDidTerminate:))
            {
                // SAFETY: as above. wry implements this one, for Tauri's app-wide hook.
                unsafe { inner.webViewWebContentProcessDidTerminate(view) };
            }
        }
    }
);

impl NavigationObserver {
    fn new(
        mtm: MainThreadMarker,
        inner: Option<&Retained<ProtocolObject<dyn WKNavigationDelegate>>>,
        on_navigation: OnNavigation,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ObserverIvars {
            inner: inner.map(Weak::from_retained),
            on_navigation,
        });
        // SAFETY: `init` is NSObject's documented designated initializer, sent exactly once to
        // a freshly allocated instance.
        unsafe { msg_send![super(this), init] }
    }

    fn inner(&self) -> Option<Retained<ProtocolObject<dyn WKNavigationDelegate>>> {
        self.ivars().inner.as_ref().and_then(Weak::load)
    }
}

/// An `NSImage` as PNG bytes, by way of the TIFF representation every `NSImage` can produce.
fn png_of(image: &NSImage) -> Result<Vec<u8>, Error> {
    let tiff = image
        .TIFFRepresentation()
        .ok_or_else(|| Error::Snapshot("the image had no bitmap representation".to_owned()))?;
    let bitmap = NSBitmapImageRep::imageRepWithData(&tiff)
        .ok_or_else(|| Error::Snapshot("the image could not be read back".to_owned()))?;
    // SAFETY: a method on the object just made, with an empty property dictionary — the
    // defaults, which for PNG are lossless and need nothing set.
    let png = unsafe {
        bitmap.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new())
    }
    .ok_or_else(|| Error::Snapshot("the image could not be encoded as PNG".to_owned()))?;
    Ok(png.to_vec())
}

struct Ivars {
    on_message: OnMessage,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements, and `Handler` does not implement Drop.
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "WtmBrowserMessageHandler"]
    #[ivars = Ivars]
    struct Handler;

    unsafe impl NSObjectProtocol for Handler {}

    // SAFETY: the one method matches the protocol's declared signature exactly; it is the
    // protocol's only method.
    unsafe impl WKScriptMessageHandler for Handler {
        #[unsafe(method(userContentController:didReceiveScriptMessage:))]
        fn user_content_controller_did_receive_script_message(
            &self,
            _controller: &WKUserContentController,
            message: &WKScriptMessage,
        ) {
            // SAFETY: getters on the live message WebKit is delivering, on the main thread.
            let (body, name, main_frame) = unsafe {
                (
                    message.body(),
                    message.name().to_string(),
                    message.frameInfo().isMainFrame(),
                )
            };
            // Every script this app installs posts a JSON *string*. Anything else is a page that
            // found the handler and is poking it, and the right answer to that is silence.
            let Ok(text) = body.downcast::<NSString>() else {
                return;
            };
            (self.ivars().on_message)(Message {
                name,
                body: text.to_string(),
                main_frame,
            });
        }
    }
);

impl Handler {
    fn new(mtm: MainThreadMarker, on_message: OnMessage) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(Ivars { on_message });
        // SAFETY: `init` is NSObject's documented designated initializer, sent exactly once to
        // a freshly allocated instance.
        unsafe { msg_send![super(this), init] }
    }
}
