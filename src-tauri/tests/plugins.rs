//! A plugin view's document keeps the host's isolation, and a real external backend works through
//! the public package and RPC contract, not a UI mock.
//!
//! The backend tests also pin which lifecycle changes local attachments hold off. Replacing the
//! code waits for them, because a session would otherwise be answered by a backend it never
//! attached to; disabling does not, because it is how a user gets away from a misbehaving package.
#![allow(clippy::unwrap_used)]
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;
use wtm_plugins::{
    Store,
    session::{Method, Request},
};

/// The session demo, copied so a test can review a later version of it.
fn demo() -> tempfile::TempDir {
    let from = Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples/plugins/session-demo");
    let copy = tempfile::tempdir().unwrap();
    for name in ["backend.py", "workbench.html", "wtm-plugin.toml"] {
        std::fs::copy(from.join(name), copy.path().join(name)).unwrap();
    }
    copy
}
/// A reviewed update of the copied demo, ready to install.
fn update(store: &Store, demo: &Path) -> String {
    let manifest = std::fs::read_to_string(demo.join("wtm-plugin.toml")).unwrap();
    std::fs::write(
        demo.join("wtm-plugin.toml"),
        manifest.replace(r#"version = "0.1.0""#, r#"version = "0.2.0""#),
    )
    .unwrap();
    store
        .prepare(demo.to_str().unwrap(), "local", "")
        .unwrap()
        .token
}

#[test]
fn custom_views_can_handle_forms_without_gaining_the_hosts_origin_or_network() {
    let document = wtm_app_lib::plugin_views::document(
        "<script>window.example = true</script><form>My UI</form>",
    );
    assert!(document.contains("sandbox=\"allow-scripts allow-forms\""));
    assert!(!document.contains("allow-same-origin"));
    assert!(document.contains("connect-src 'none'"));
    assert!(document.contains("form-action 'none'"));
    assert!(!document.contains("<script>window.example"));
    assert!(document.contains("&lt;script&gt;window.example"));
    assert!(document.contains("wtm.plugin.request"));
}

#[test]
fn two_clients_reconnect_to_one_session_without_duplicating_input_or_stealing_control() {
    let root = tempfile::tempdir().unwrap();
    let (_, runner, _, _, _) = wtm_exec::adapters(None);
    let store = Store::new(root.path().into(), Arc::new(runner), false);
    let source = demo();
    let review = store
        .prepare(source.path().to_str().unwrap(), "local", "")
        .unwrap();
    store.install(&review.token).unwrap();
    let call = |method, params| {
        store.session_call(
            "example.session-demo",
            "demo",
            &Request { method, params },
            root.path(),
        )
    };
    let first = call(
        Method::Attach,
        json!({"session":"demo-1", "client":"a", "after":0}),
    )
    .unwrap();
    assert_eq!(first["cursor"], 1);
    let lease =
        call(Method::Control, json!({"session":"demo-1", "client":"a"})).unwrap()["lease"].clone();
    assert!(call(Method::Control, json!({"session":"demo-1", "client":"b"})).is_err());
    let input = |client: &str, text: &str, input: &str| {
        let params = json!({
            "session": "demo-1",
            "client": client,
            "lease": lease,
            "text": text,
            "input_id": input,
        });
        call(Method::Input, params)
    };
    assert!(input("b", "wrong", "wrong").is_err());
    for _ in 0..2 {
        input("a", "hello", "one").unwrap();
    }
    let held = update(&store, source.path());
    assert!(store.install(&held).unwrap_err().contains("Detach"));
    call(Method::Detach, json!({"session":"demo-1", "client":"a"})).unwrap();
    let second = call(
        Method::Attach,
        json!({"session":"demo-1", "client":"b", "after":1}),
    )
    .unwrap();
    assert_eq!(second["cursor"], 2);
    assert_eq!(second["events"].as_array().unwrap().len(), 1);
    let sessions = call(Method::List, json!({"client":"b"})).unwrap();
    assert_eq!(sessions["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(sessions["sessions"][0]["id"], "demo-1");
    assert_eq!(second["lease"], Value::Null);
    let lease_b =
        call(Method::Control, json!({"session":"demo-1", "client":"b"})).unwrap()["lease"].clone();
    call(
        Method::Stop,
        json!({"session":"demo-1", "client":"b", "lease":lease_b}),
    )
    .unwrap();
    assert!(
        call(
            Method::Attach,
            json!({"session":"demo-1", "client":"a", "after":2})
        )
        .is_err()
    );
    assert!(
        call(Method::List, json!({"client":"b"})).unwrap()["sessions"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    store.set_enabled("example.session-demo", false).unwrap();
}

#[test]
fn detaching_one_session_keeps_the_same_views_other_attachment_owned() {
    let root = tempfile::tempdir().unwrap();
    let (_, runner, _, _, _) = wtm_exec::adapters(None);
    let store = Store::new(root.path().into(), Arc::new(runner), false);
    let source = demo();
    let review = store
        .prepare(source.path().to_str().unwrap(), "local", "")
        .unwrap();
    store.install(&review.token).unwrap();
    let call = |method, params| {
        store.session_call(
            "example.session-demo",
            "demo",
            &Request { method, params },
            root.path(),
        )
    };
    let created = call(Method::Create, json!({"client":"view"})).unwrap();
    for session in [json!("demo-1"), created["id"].clone()] {
        call(
            Method::Attach,
            json!({"client":"view","session":session,"after":0}),
        )
        .unwrap();
    }
    call(Method::Detach, json!({"client":"view","session":"demo-1"})).unwrap();
    let held = update(&store, source.path());
    assert!(store.install(&held).unwrap_err().contains("Detach"));
    store.release_client("view");
    store.install(&held).unwrap();
}

#[test]
fn disabling_a_package_releases_its_attachments_instead_of_waiting_for_them() {
    let root = tempfile::tempdir().unwrap();
    let (_, runner, _, _, _) = wtm_exec::adapters(None);
    let store = Store::new(root.path().into(), Arc::new(runner), false);
    let source = demo();
    let review = store
        .prepare(source.path().to_str().unwrap(), "local", "")
        .unwrap();
    store.install(&review.token).unwrap();
    let attach = Request {
        method: Method::Attach,
        params: json!({"client":"view","session":"demo-1","after":0}),
    };
    store
        .session_call("example.session-demo", "demo", &attach, root.path())
        .unwrap();
    store.set_enabled("example.session-demo", false).unwrap();
    assert!(
        store
            .session_call("example.session-demo", "demo", &attach, root.path())
            .is_err()
    );
    store.set_enabled("example.session-demo", true).unwrap();
    let next = update(&store, source.path());
    store.install(&next).unwrap();
}
