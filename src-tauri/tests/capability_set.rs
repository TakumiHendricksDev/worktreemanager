//! The capability file must stay a list a person can read.
//!
//! `capabilities/default.json` is the whole of what the frontend may reach outside this app's own
//! `#[tauri::command]` functions, and its `description` is an argued document rather than a label —
//! every entry names what it grants and why the narrower form was chosen over the plugin's default
//! set. That argument is only worth anything if the file keeps matching it.
//!
//! # Why a `*:default` is the thing being tested for
//!
//! Because a plugin's default permission set is **whatever its next release decides to include**. It
//! is a moving target defined in someone else's repository, so granting one means the app's real
//! privileges can grow on a `cargo update` with no diff in this tree to review. Named permissions
//! cannot do that. `dialog:allow-open` rather than `dialog:default` was the first instance of the
//! rule, and the since-removed notification entries were the second; this makes it a lint rather
//! than a habit.
//!
//! The two exceptions are Tauri's own `core:*`, which are not a plugin and whose contents are the
//! framework version already pinned in `Cargo.lock`.
//!
//! A lint, not a promise — the same shape as `platform_config.rs` and `repo_hygiene.rs`.

#![allow(clippy::unwrap_used)]

use std::path::Path;

use serde_json::Value;

/// `*:default` grants that are allowed, and why.
///
/// Both are Tauri's own rather than a plugin's, so their contents move only with the framework
/// version in `Cargo.lock` — which is reviewed like any other dependency bump.
const ALLOWED_DEFAULTS: &[&str] = &["core:default", "core:event:default"];

fn permissions() -> Vec<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("capabilities/default.json");
    let text = std::fs::read_to_string(&path).unwrap();
    let config: Value = serde_json::from_str(&text).unwrap();
    config["permissions"]
        .as_array()
        .expect("`permissions` must be an array")
        .iter()
        .map(|p| p.as_str().expect("a permission is a string").to_owned())
        .collect()
}

#[test]
fn no_plugin_default_permission_set_is_granted() {
    for permission in permissions() {
        if !permission.ends_with(":default") || ALLOWED_DEFAULTS.contains(&permission.as_str()) {
            continue;
        }
        panic!(
            "`{permission}` grants whatever that plugin's next release puts in its default set. \
             Name the permissions this app actually uses instead, and say why in the file's \
             description — `dialog:allow-open` is the precedent."
        );
    }
}

#[test]
fn the_webview_holds_no_notification_permission_at_all() {
    // Notifications went native (see `notifier.rs` and `wtm-notify`): the webview posts through
    // this app's own `post_notification` command so the payload can carry a navigation target,
    // and the permission calls go through commands for the same reason. The plugin is still a
    // Rust-side posting fallback, but nothing in the webview may address it — a grant here
    // would widen the surface back for an API nothing uses.
    let granted = permissions();
    assert!(
        !granted.iter().any(|p| p.starts_with("notification:")),
        "a `notification:*` grant reappeared; the webview reaches notifications only through \
         `post_notification` and friends, so this can only be surface with no caller"
    );
}

#[test]
fn every_permission_is_described_in_the_files_own_description() {
    /*
     * The property that keeps the description honest.
     *
     * That text is the only explanation anywhere of why this set is what it is, and a permission
     * added without a sentence about it is exactly how a minimal capability set stops being one —
     * silently, and with the file still claiming otherwise.
     *
     * Matched on the permission's own name appearing verbatim, which is the weakest useful check: it
     * cannot tell whether the sentence is any good, only that someone wrote the name down.
     */
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("capabilities/default.json");
    let text = std::fs::read_to_string(&path).unwrap();
    let config: Value = serde_json::from_str(&text).unwrap();
    let description = config["description"].as_str().unwrap();

    for permission in permissions() {
        assert!(
            description.contains(&permission),
            "`{permission}` is granted but never mentioned in the capability file's description. \
             Say what it is for, in the voice the rest of that text uses."
        );
    }
}
