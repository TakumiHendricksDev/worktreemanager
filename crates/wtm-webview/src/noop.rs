//! The arm for platforms with no WebKit.
//!
//! [`Handle`] is an *uninhabited* enum rather than a unit struct with `unreachable!` bodies:
//! [`attach`] is the only constructor and it always answers `None`, so the type system itself
//! proves the methods below can never run — `match *self {}` compiles precisely because there is
//! no value to match. Same construction as `wtm-notify`'s no-op arm, for the same reason.

use std::ffi::c_void;

use crate::{OnMessage, Reply, World};

pub(crate) enum Handle {}

pub(crate) fn attach(_webview: *mut c_void, _controller: *mut c_void) -> Option<Handle> {
    None
}

pub(crate) fn on_main_thread() -> bool {
    false
}

pub(crate) fn available() -> bool {
    false
}

impl Handle {
    pub(crate) fn install_script(&self, _world: &World, _source: &str, _main_frame_only: bool) {
        match *self {}
    }

    pub(crate) fn add_message_handler(&self, _world: &World, _name: &str, _on: OnMessage) {
        match *self {}
    }

    pub(crate) fn evaluate(&self, _world: &World, _js: &str) -> Reply<String> {
        match *self {}
    }

    pub(crate) fn snapshot(&self, _rect: Option<(f64, f64, f64, f64)>) -> Reply<Vec<u8>> {
        match *self {}
    }

    pub(crate) fn history(&self) -> (bool, bool) {
        match *self {}
    }
}
