//! Exercise a real external backend through the public package and RPC contract, not a UI mock.
#![allow(clippy::unwrap_used)]
use serde_json::{Value, json};
use std::sync::Arc;
use wtm_plugins::{
    Store,
    session::{Method, Request},
};

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
    let source =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples/plugins/session-demo");
    let review = store
        .prepare(source.to_str().unwrap(), "local", "")
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
    assert!(call(Method::Input, json!({"session":"demo-1", "client":"b", "lease":lease, "text":"wrong", "input_id":"wrong"})).is_err());
    for _ in 0..2 {
        call(Method::Input, json!({"session":"demo-1", "client":"a", "lease":lease, "text":"hello", "input_id":"one"})).unwrap();
    }
    assert!(store.set_enabled("example.session-demo", false).is_err());
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
    let source =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples/plugins/session-demo");
    let review = store
        .prepare(source.to_str().unwrap(), "local", "")
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
    assert!(store.set_enabled("example.session-demo", false).is_err());
    store.release_client("view");
    store.set_enabled("example.session-demo", false).unwrap();
}
