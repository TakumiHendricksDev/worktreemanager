//! Native tracking enters a nested `AppKit` loop which can service another webview IPC.
//!
//! Tauri 2.11.6's menu plugin holds the webview resource table while waiting for that
//! loop to return. A nested resource request then deadlocks the UI against the popup
//! worker. Own the menu before dispatching, and never keep a lock across native UI.

use std::ops::Deref;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Deserialize;
use tauri::menu::{ContextMenu, Menu, Submenu};
use tauri::{LogicalPosition, Manager, Resource, ResourceId, ResourceTable, Webview, Window};

use crate::commands::{Reply, blocking};
use crate::view::ErrorView;

static TRACKING: PopupGate = PopupGate(AtomicBool::new(false));

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MenuKind {
    Menu,
    Submenu,
}

#[derive(Debug, Deserialize)]
pub struct MenuPoint {
    x: f64,
    y: f64,
}

struct PopupGate(AtomicBool);

impl PopupGate {
    fn enter(&self) -> Reply<Tracking<'_>> {
        self.0
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| Tracking(self))
            .map_err(|_| ErrorView::new("menu_busy", "Another native menu is open."))
    }
}

struct Tracking<'a>(&'a PopupGate);

impl Drop for Tracking<'_> {
    fn drop(&mut self) {
        self.0.0.store(false, Ordering::Release);
    }
}

/// Taking ownership of the guard makes releasing it part of this tested boundary.
fn with_resource<T: Resource, U>(
    table: impl Deref<Target = ResourceTable>,
    rid: ResourceId,
    use_resource: impl FnOnce(Arc<T>) -> tauri::Result<U>,
) -> tauri::Result<U> {
    let resource = table.get::<T>(rid)?;
    drop(table);
    use_resource(resource)
}

fn show(menu: &impl ContextMenu, window: Window, at: Option<MenuPoint>) -> tauri::Result<()> {
    match at {
        Some(point) => menu.popup_at(window, LogicalPosition::new(point.x, point.y)),
        None => menu.popup(window),
    }
}

#[tauri::command]
pub async fn popup_native_menu(
    webview: Webview,
    window: Window,
    rid: ResourceId,
    kind: MenuKind,
    at: Option<MenuPoint>,
) -> Reply<()> {
    blocking(move || {
        let _tracking = TRACKING.enter()?;
        if at
            .as_ref()
            .is_some_and(|p| !p.x.is_finite() || !p.y.is_finite())
        {
            return Err(ErrorView::new("menu", "Menu coordinates must be finite."));
        }
        // Both arms go through the same lock-release boundary. The window comes from
        // the caller's IPC context; the request cannot target another window by name.
        let result = match kind {
            MenuKind::Menu => {
                with_resource::<Menu<tauri::Wry>, _>(webview.resources_table(), rid, |menu| {
                    show(menu.as_ref(), window, at)
                })
            }
            MenuKind::Submenu => {
                with_resource::<Submenu<tauri::Wry>, _>(webview.resources_table(), rid, |menu| {
                    show(menu.as_ref(), window, at)
                })
            }
        };
        result.map_err(|e| ErrorView::new("menu", e.to_string()))
    })
    .await
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use parking_lot::Mutex;

    use super::*;

    struct TestMenu;
    impl Resource for TestMenu {}
    struct TestSubmenu;
    impl Resource for TestSubmenu {}

    fn nested_request<T: Resource>(resource: T) {
        let table = Mutex::new(ResourceTable::default());
        let rid = table.lock().add(resource);
        with_resource::<T, _>(table.lock(), rid, |menu| {
            let mut nested = table
                .try_lock()
                .expect("native callback must be able to service IPC");
            nested.close(rid)?;
            // A resource close during tracking must not destroy the owned menu.
            assert_eq!(Arc::strong_count(&menu), 1);
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn native_tracking_can_reenter_the_resource_table_for_both_menu_kinds() {
        nested_request(TestMenu);
        nested_request(TestSubmenu);
    }

    #[test]
    fn missing_or_wrong_resources_never_enter_native_tracking() {
        let table = Mutex::new(ResourceTable::default());
        let rid = table.lock().add(TestMenu);
        assert!(
            with_resource::<TestSubmenu, ()>(table.lock(), rid, |_| panic!("wrong type")).is_err()
        );
        table.lock().close(rid).unwrap();
        assert!(
            with_resource::<TestMenu, ()>(table.lock(), rid, |_| panic!("missing menu")).is_err()
        );
        assert!(table.try_lock().is_some());
    }

    #[test]
    fn popup_admission_recovers_after_errors_and_dismissal() {
        let gate = PopupGate(AtomicBool::new(false));
        let fail = || -> Reply<()> {
            let _tracking = gate.enter()?;
            assert_eq!(gate.enter().err().unwrap().kind, "menu_busy");
            Err(ErrorView::new("menu", "native popup failed"))
        };
        assert!(fail().is_err());
        drop(gate.enter().unwrap());
        assert!(gate.enter().is_ok());
    }
}
