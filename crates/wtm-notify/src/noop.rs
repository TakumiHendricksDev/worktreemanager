//! The arm for platforms with no `UNUserNotificationCenter`.
//!
//! [`Center`] is an *uninhabited* enum rather than a unit struct with `unreachable!` bodies:
//! [`attach`] is the only constructor and it always answers `None`, so the type system itself
//! proves the methods below can never run — `match *self {}` compiles precisely because there
//! is no value to match.

use crate::{ClickPayload, Error, OnClick, Permission};

pub(crate) enum Center {}

pub(crate) fn attach(_on_click: OnClick) -> Option<Center> {
    None
}

impl Center {
    pub(crate) fn permission(&self) -> Permission {
        match *self {}
    }

    pub(crate) fn request_permission(&self) -> bool {
        match *self {}
    }

    pub(crate) fn post(
        &self,
        _title: &str,
        _body: &str,
        _payload: &ClickPayload,
    ) -> Result<(), Error> {
        match *self {}
    }
}
