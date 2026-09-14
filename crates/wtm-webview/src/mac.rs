//! The macOS arm: `WKContentWorld`, a script message handler, world-scoped evaluation, snapshots.
//!
//! # Why `unsafe` appears here
//!
//! Every objc2-web-kit method is `unsafe fn` in the 0.3 bindings — WebKit's API has thread and
//! lifetime preconditions the bindings cannot express — so this module is a sequence of one-line
//! SAFETY comments, each citing the same two facts: the call is on the main thread (proved by the
//! `MainThreadMarker` the handle carries) and the references are live (they are `Retained`, held
//! by the handle for the closure's duration). The two genuinely delicate sites are [`attach`],
//! which trusts pointers Tauri produced, and the completion blocks, which read a pointer WebKit
//! hands them; both say what they rely on.
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
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{NSBitmapImageFileType, NSBitmapImageRep, NSImage};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSDictionary, NSError, NSObject, NSObjectProtocol, NSString};
use objc2_web_kit::{
    WKContentWorld, WKScriptMessage, WKScriptMessageHandler, WKSnapshotConfiguration,
    WKUserContentController, WKUserScript, WKUserScriptInjectionTime, WKWebView,
};

use crate::{Error, Message, OnMessage, Reply, World};

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
