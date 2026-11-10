//! A package review must bind activation to bytes, survive restart, and never execute on discovery.
//!
//! Also the registry's lifecycle around that review: what an update keeps for rollback, what
//! uninstalling forgets and keeps, which changes a running call holds off and which it may not,
//! and that safe mode hides a configuration without ever saving the hidden copy over it.
#![allow(clippy::unwrap_used)]
use std::sync::Arc;
use wtm_plugins::contributions::{Binding, Slot};
use wtm_plugins::{Store, manifest::Manifest};
use wtm_testkit::FakeRunner;

const MANIFEST: &str = "schema = 1\napi = 1\nid = 'example.tools'\nname = 'Tools'\nversion = '1.0.0'\n[[commands]]\nid = 'status'\ntitle = 'Status'\ncommand = ['git', 'status']\n";
const VIEW: &str = "\n[[views]]\nid='workbench'\ntitle='My workbench'\nfile='view.html'\nslots=['workbench','code']\n";
fn source() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("wtm-plugin.toml"), MANIFEST).unwrap();
    dir
}
fn with_view(manifest: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("view.html"), "<p>My workbench</p>").unwrap();
    std::fs::write(
        dir.path().join("wtm-plugin.toml"),
        format!("{manifest}{VIEW}"),
    )
    .unwrap();
    dir
}
fn install(store: &Store, source: &std::path::Path) {
    let review = store
        .prepare(source.to_str().unwrap(), "local", "")
        .unwrap();
    store.install(&review.token).unwrap();
}
fn version(store: &Store) -> String {
    store.snapshot().registry.installed["example.tools"]
        .current
        .manifest
        .version
        .clone()
}
fn workbench() -> Binding {
    Binding::Plugin {
        plugin: "example.tools".into(),
        view: "workbench".into(),
    }
}
#[test]
fn discovery_never_executes_and_cancelled_reviews_cannot_be_installed() {
    let root = tempfile::tempdir().unwrap();
    let src = source();
    let runner = Arc::new(FakeRunner::new());
    let store = Store::new(root.path().into(), runner.clone(), false);
    let review = store
        .prepare(src.path().to_str().unwrap(), "local", "")
        .unwrap();
    assert!(runner.calls().is_empty());
    assert!(store.snapshot().registry.installed.is_empty());
    store.cancel(&review.token).unwrap();
    assert!(store.install(&review.token).is_err());
}
#[test]
fn reviewed_snapshots_ignore_source_edits_and_changed_bytes_fail_the_next_activation() {
    let root = tempfile::tempdir().unwrap();
    let src = source();
    let runner = Arc::new(FakeRunner::new());
    let store = Store::new(root.path().into(), runner.clone(), false);
    let review = store
        .prepare(src.path().to_str().unwrap(), "local", "")
        .unwrap();
    std::fs::write(src.path().join("wtm-plugin.toml"), "broken").unwrap();
    store.install(&review.token).unwrap();
    assert!(
        store
            .invoke("example.tools", "status", false, "{}", src.path())
            .is_ok()
    );
    assert_eq!(runner.calls().len(), 1);
    std::fs::write(
        root.path()
            .join("packages")
            .join(review.package.directory)
            .join("wtm-plugin.toml"),
        "broken",
    )
    .unwrap();
    // A request trusts this process's last hash; the next launch and enabling hash again.
    let reopened = Store::new(root.path().into(), runner.clone(), false);
    assert!(
        reopened
            .invoke("example.tools", "status", false, "{}", src.path())
            .is_err()
    );
    assert_eq!(runner.calls().len(), 1);
    store.set_enabled("example.tools", false).unwrap();
    assert!(store.set_enabled("example.tools", true).is_err());
}
#[test]
fn updates_roll_back_and_disabled_packages_stay_disabled_after_restart() {
    let root = tempfile::tempdir().unwrap();
    let src = source();
    let runner = Arc::new(FakeRunner::new());
    let store = Store::new(root.path().into(), runner.clone(), false);
    install(&store, src.path());
    std::fs::write(
        src.path().join("wtm-plugin.toml"),
        MANIFEST.replace("1.0.0", "1.1.0"),
    )
    .unwrap();
    install(&store, src.path());
    assert_eq!(version(&store), "1.1.0");
    store.rollback("example.tools").unwrap();
    assert_eq!(version(&store), "1.0.0");
    store.set_enabled("example.tools", false).unwrap();
    let reopened = Store::new(root.path().into(), runner.clone(), false);
    assert!(!reopened.enabled("example.tools"));
    assert!(
        reopened
            .invoke("example.tools", "status", false, "{}", src.path())
            .is_err()
    );
    assert!(runner.calls().is_empty());
}
#[test]
fn reinstalling_the_installed_bytes_is_refused_and_keeps_the_real_rollback_target() {
    let root = tempfile::tempdir().unwrap();
    let src = source();
    let store = Store::new(root.path().into(), Arc::new(FakeRunner::new()), false);
    install(&store, src.path());
    std::fs::write(
        src.path().join("wtm-plugin.toml"),
        MANIFEST.replace("1.0.0", "1.1.0"),
    )
    .unwrap();
    install(&store, src.path());
    let again = store
        .prepare(src.path().to_str().unwrap(), "local", "")
        .unwrap();
    assert_eq!(again.replacing.as_ref(), Some(&again.package.digest));
    let refused = store.install(&again.token).unwrap_err();
    assert_eq!(refused, "This revision is already installed");
    let installed = &store.snapshot().registry.installed["example.tools"];
    assert_eq!(
        installed.previous.as_ref().unwrap().manifest.version,
        "1.0.0"
    );
    store.rollback("example.tools").unwrap();
    assert_eq!(version(&store), "1.0.0");
}
#[test]
fn unsupported_manifests_and_escaping_panel_paths_are_rejected() {
    for text in [
        MANIFEST.replace("api = 1", "api = 99"),
        MANIFEST.replace("example.tools", "wtm.tools"),
        format!("{MANIFEST}\n[[panels]]\nid='panel'\ntitle='Panel'\nfile='../private.html'"),
        format!("{MANIFEST}\n[[panels]]\nid='panel'\ntitle='Panel'\nfile='.git/panel.html'"),
        format!("{MANIFEST}\n[[panels]]\nid='panel'\ntitle='Panel'\nfile='ui/.git/panel.html'"),
        MANIFEST.replace("['git', 'status']", "['./.git/hooks/status']"),
        format!("{MANIFEST}\n[[commands]]\nid='status'\ntitle='Duplicate'\ncommand=['echo']"),
    ] {
        assert!(Manifest::parse(&text).is_err(), "accepted {text}");
    }
}
#[test]
fn recovery_preserves_a_corrupt_registry_and_does_not_execute_it() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("registry.json"), "broken").unwrap();
    let store = Store::new(root.path().into(), Arc::new(FakeRunner::new()), false);
    assert!(store.snapshot().problem.is_some());
    assert!(store.snapshot().safe_mode);
    assert!(store.set_enabled("wtm.home", false).is_err());
    let recovered = store.recover().unwrap();
    assert!(recovered.problem.is_none());
    assert!(recovered.registry.safe_mode);
    assert!(std::fs::read_dir(root.path()).unwrap().any(|p| {
        p.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("registry-corrupt-")
    }));
}
#[test]
fn a_review_cannot_overwrite_a_version_installed_since_the_review_started() {
    let root = tempfile::tempdir().unwrap();
    let src = source();
    let store = Store::new(root.path().into(), Arc::new(FakeRunner::new()), false);
    let a = store
        .prepare(src.path().to_str().unwrap(), "local", "")
        .unwrap();
    let b = store
        .prepare(src.path().to_str().unwrap(), "local", "")
        .unwrap();
    store.install(&a.token).unwrap();
    assert!(store.install(&b.token).is_err());
}
#[test]
fn home_and_chat_dependencies_prevent_an_unusable_default() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::new(root.path().into(), Arc::new(FakeRunner::new()), false);
    assert!(store.set_enabled("wtm.chat", false).is_err());
    store.experience("wtm.native-cli").unwrap();
    assert!(store.set_enabled("wtm.chat", false).is_err());
    store.set_enabled("wtm.home", false).unwrap();
    store.set_enabled("wtm.chat", false).unwrap();
    assert!(store.set_enabled("wtm.home", true).is_err());
    assert!(store.experience("wtm.chat").is_err());
}
#[test]
fn mismatched_rpc_responses_and_backend_errors_are_refused() {
    assert!(
        wtm_plugins::session::decode(r#"{"jsonrpc":"2.0","id":"other","result":{}}"#, "request")
            .is_err()
    );
    let error =
        r#"{"jsonrpc":"2.0","id":"request","error":{"message":"Control lease is held elsewhere"}}"#;
    assert!(
        wtm_plugins::session::decode(error, "request")
            .unwrap_err()
            .contains("Control lease")
    );
}

#[test]
fn views_replace_only_explicitly_selected_regions_and_revocation_restores_the_builtin() {
    let root = tempfile::tempdir().unwrap();
    let src = with_view(MANIFEST);
    let store = Store::new(root.path().into(), Arc::new(FakeRunner::new()), false);
    install(&store, src.path());
    assert_eq!(store.binding(Slot::Workbench), Binding::Builtin);
    assert!(store.bind(Slot::Sidebar, workbench()).is_err());
    assert!(
        store
            .view(Slot::Workbench, "example.tools", "workbench")
            .is_err()
    );
    store.bind(Slot::Workbench, workbench()).unwrap();
    assert_eq!(store.binding(Slot::Workbench), workbench());
    assert!(
        store
            .view(Slot::Workbench, "example.tools", "workbench")
            .unwrap()
            .contains("My workbench")
    );
    assert!(
        store
            .view_selected(Slot::Workbench, "example.tools", "workbench")
            .is_ok()
    );
    assert_eq!(store.binding(Slot::Code), Binding::Builtin);
    // Disabling is how a user escapes a package, so a call it is running cannot hold it off,
    // and the view's next request fails its revalidation.
    let activation = store.activation("example.tools");
    store.set_enabled("example.tools", false).unwrap();
    assert!(
        store
            .view_selected(Slot::Workbench, "example.tools", "workbench")
            .is_err()
    );
    drop(activation);
    assert_eq!(store.binding(Slot::Workbench), Binding::Builtin);
    store.set_enabled("example.tools", true).unwrap();
    assert_eq!(store.binding(Slot::Workbench), Binding::Builtin);
}

#[test]
fn a_running_call_holds_off_replacing_the_code_but_not_turning_it_off() {
    let root = tempfile::tempdir().unwrap();
    let src = source();
    let store = Store::new(root.path().into(), Arc::new(FakeRunner::new()), false);
    install(&store, src.path());
    std::fs::write(
        src.path().join("wtm-plugin.toml"),
        MANIFEST.replace("1.0.0", "1.1.0"),
    )
    .unwrap();
    install(&store, src.path());
    let running = store.activation("example.tools");
    assert!(
        store
            .rollback("example.tools")
            .unwrap_err()
            .contains("installing or rolling back")
    );
    let preference = serde_json::json!({"kind": "order", "order": []});
    store
        .extension_configure("example.tools", &preference)
        .unwrap();
    store.set_enabled("example.tools", false).unwrap();
    drop(running);
    store.rollback("example.tools").unwrap();
    assert_eq!(version(&store), "1.0.0");
}

#[test]
fn uninstalling_forgets_the_package_and_its_regions_but_keeps_snapshots_and_data() {
    let root = tempfile::tempdir().unwrap();
    let src = with_view(MANIFEST);
    let runner = Arc::new(FakeRunner::new());
    let store = Store::new(root.path().into(), runner.clone(), false);
    let review = store
        .prepare(src.path().to_str().unwrap(), "local", "")
        .unwrap();
    store.install(&review.token).unwrap();
    store.bind(Slot::Workbench, workbench()).unwrap();
    store
        .invoke("example.tools", "status", false, "{}", src.path())
        .unwrap();
    let running = store.activation("example.tools");
    let snapshot = store.uninstall("example.tools").unwrap();
    drop(running);
    assert!(snapshot.registry.installed.is_empty());
    assert!(snapshot.registry.bindings.is_empty());
    assert_eq!(store.binding(Slot::Workbench), Binding::Builtin);
    assert!(
        root.path()
            .join("packages")
            .join(&review.package.directory)
            .join("view.html")
            .is_file()
    );
    assert!(root.path().join("data").join("example.tools").is_dir());
    assert!(
        store
            .invoke("example.tools", "status", false, "{}", src.path())
            .is_err()
    );
    assert!(store.uninstall("example.tools").is_err());
    let reopened = Store::new(root.path().into(), runner, false);
    assert!(reopened.snapshot().registry.installed.is_empty());
}

#[test]
fn recovery_restores_hidden_regions_and_never_implicitly_reenables_another_package() {
    let root = tempfile::tempdir().unwrap();
    let src = source();
    let store = Store::new(root.path().into(), Arc::new(FakeRunner::new()), false);
    install(&store, src.path());
    std::fs::write(
        src.path().join("wtm-plugin.toml"),
        MANIFEST.replace("example.tools", "example.other"),
    )
    .unwrap();
    install(&store, src.path());
    store.bind(Slot::Workbench, Binding::Hidden).unwrap();
    store.set_enabled("wtm.titlebar", false).unwrap();
    let restored = Store::new(root.path().into(), Arc::new(FakeRunner::new()), true);
    assert_eq!(restored.binding(Slot::Workbench), Binding::Builtin);
    assert_eq!(restored.binding(Slot::Titlebar), Binding::Builtin);
    restored.set_enabled("example.tools", true).unwrap();
    assert!(restored.enabled("example.tools"));
    assert!(!restored.enabled("example.other"));
}

#[test]
fn a_change_made_in_safe_mode_is_saved_without_wiping_what_safe_mode_hid() {
    let root = tempfile::tempdir().unwrap();
    let tools = with_view(MANIFEST);
    let other = source();
    std::fs::write(
        other.path().join("wtm-plugin.toml"),
        MANIFEST.replace("example.tools", "example.other"),
    )
    .unwrap();
    let runner = Arc::new(FakeRunner::new());
    let store = Store::new(root.path().into(), runner.clone(), false);
    install(&store, tools.path());
    install(&store, other.path());
    store.bind(Slot::Workbench, workbench()).unwrap();
    store.set_enabled("wtm.titlebar", false).unwrap();
    store.experience("wtm.native-cli").unwrap();

    let safe = Store::new(root.path().into(), runner.clone(), true);
    let seen = safe.snapshot();
    assert!(seen.safe_mode && seen.registry.safe_mode);
    assert!(!seen.registry.installed["example.tools"].enabled);
    assert!(seen.registry.bindings.is_empty());
    assert!(seen.registry.disabled.is_empty());
    assert_eq!(seen.registry.experience, "wtm.chat");
    assert_eq!(safe.binding(Slot::Workbench), Binding::Builtin);
    assert!(!safe.enabled("example.tools"));
    assert!(safe.active("example.tools").is_err());
    // The user turns the misbehaving package off, which is the change safe mode exists for.
    let after = safe.set_enabled("example.other", false).unwrap();
    assert!(!after.registry.installed["example.tools"].enabled);
    let saved = std::fs::read_to_string(root.path().join("registry.json")).unwrap();
    assert!(saved.contains("\"safe_mode\": false"));

    let normal = Store::new(root.path().into(), runner, false);
    let registry = normal.snapshot().registry;
    assert!(!registry.safe_mode);
    assert!(!registry.installed["example.other"].enabled);
    assert!(registry.installed["example.tools"].enabled);
    assert_eq!(registry.experience, "wtm.native-cli");
    assert_eq!(normal.binding(Slot::Workbench), workbench());
    assert_eq!(normal.binding(Slot::Titlebar), Binding::Hidden);
}

#[test]
fn safe_mode_checks_a_change_against_what_the_user_sees_as_well_as_what_is_saved() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::new(root.path().into(), Arc::new(FakeRunner::new()), false);
    store.set_enabled("wtm.home", false).unwrap();
    let safe = Store::new(root.path().into(), Arc::new(FakeRunner::new()), true);
    // Saved, Home is already off; shown, it is on, so WTM Chat cannot be turned off under it.
    assert!(safe.enabled("wtm.home"));
    assert!(safe.set_enabled("wtm.chat", false).is_err());
    safe.set_enabled("wtm.home", false).unwrap();
    assert!(!safe.enabled("wtm.home"));
}

#[test]
fn terminal_profiles_are_reviewed_and_resolve_package_programs_without_shell_expansion() {
    let root = tempfile::tempdir().unwrap();
    let src = source();
    std::fs::write(src.path().join("launcher"), "an example executable").unwrap();
    let profile =
        "[[terminals]]\nid='terminal'\ntitle='My CLI'\ncommand=['./launcher', 'literal;argument']";
    std::fs::write(
        src.path().join("wtm-plugin.toml"),
        format!("{MANIFEST}\n{profile}\n"),
    )
    .unwrap();
    let store = Store::new(root.path().into(), Arc::new(FakeRunner::new()), false);
    let review = store
        .prepare(src.path().to_str().unwrap(), "local", "")
        .unwrap();
    assert!(store.terminal("example.tools", "terminal").is_err());
    store.install(&review.token).unwrap();
    let argv = store.terminal("example.tools", "terminal").unwrap();
    assert!(argv[0].starts_with(root.path().to_str().unwrap()));
    assert_eq!(argv[1], "literal;argument");
    let worktree = tempfile::tempdir().unwrap();
    let terminal = store
        .terminal_invocation("example.tools", "terminal", worktree.path())
        .unwrap();
    assert_eq!(terminal.cwd, worktree.path());
    let data = root.path().join("data").join("example.tools");
    assert_eq!(terminal.env["WTM_PLUGIN_DATA"], data.to_str().unwrap());
    assert!(terminal.env["WTM_PLUGIN_ROOT"].starts_with(root.path().to_str().unwrap()));
    store.set_enabled("example.tools", false).unwrap();
    assert!(store.terminal("example.tools", "terminal").is_err());
}

#[test]
fn only_a_dot_slash_program_is_a_package_path_for_every_entry_point() {
    let dir = std::path::Path::new("/packages/one");
    let argv = |program: &str| vec![program.to_owned(), "--flag".to_owned()];
    assert_eq!(
        wtm_plugins::resolve_argv(dir, &argv("./bin/tool")).unwrap()[0],
        "/packages/one/./bin/tool"
    );
    for unchanged in ["python3", ".hidden", "/usr/bin/env"] {
        let resolved = wtm_plugins::resolve_argv(dir, &argv(unchanged)).unwrap();
        assert_eq!(resolved, argv(unchanged));
    }
    assert!(wtm_plugins::resolve_argv(dir, &argv("./../escape")).is_err());
    assert!(wtm_plugins::resolve_argv(dir, &argv("./.git/hooks/tool")).is_err());
    assert!(wtm_plugins::resolve_argv(dir, &[]).is_err());
}
