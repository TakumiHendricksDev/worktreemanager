//! A package review must bind activation to bytes, survive restart, and never execute on discovery.
#![allow(clippy::unwrap_used)]
use std::sync::Arc;
use wtm_plugins::contributions::{Binding, Slot};
use wtm_plugins::{Store, manifest::Manifest};
use wtm_testkit::FakeRunner;

const MANIFEST: &str = "schema = 1\napi = 1\nid = 'example.tools'\nname = 'Tools'\nversion = '1.0.0'\n[[commands]]\nid = 'status'\ntitle = 'Status'\ncommand = ['git', 'status']\n";
fn source() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("wtm-plugin.toml"), MANIFEST).unwrap();
    dir
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
fn reviewed_snapshots_ignore_source_edits_but_refuse_changed_installed_bytes() {
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
    assert!(
        store
            .invoke("example.tools", "status", false, "{}", src.path())
            .is_err()
    );
    assert_eq!(runner.calls().len(), 1);
}
#[test]
fn updates_roll_back_and_disabled_packages_stay_disabled_after_restart() {
    let root = tempfile::tempdir().unwrap();
    let src = source();
    let runner = Arc::new(FakeRunner::new());
    let store = Store::new(root.path().into(), runner.clone(), false);
    let first = store
        .prepare(src.path().to_str().unwrap(), "local", "")
        .unwrap();
    store.install(&first.token).unwrap();
    std::fs::write(
        src.path().join("wtm-plugin.toml"),
        MANIFEST.replace("1.0.0", "1.1.0"),
    )
    .unwrap();
    let second = store
        .prepare(src.path().to_str().unwrap(), "local", "")
        .unwrap();
    store.install(&second.token).unwrap();
    assert_eq!(
        store.snapshot().registry.installed["example.tools"]
            .current
            .manifest
            .version,
        "1.1.0"
    );
    store.rollback("example.tools").unwrap();
    assert_eq!(
        store.snapshot().registry.installed["example.tools"]
            .current
            .manifest
            .version,
        "1.0.0"
    );
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
fn unsupported_manifests_and_escaping_panel_paths_are_rejected() {
    for text in [
        MANIFEST.replace("api = 1", "api = 99"),
        MANIFEST.replace("example.tools", "wtm.tools"),
        format!("{MANIFEST}\n[[panels]]\nid='panel'\ntitle='Panel'\nfile='../private.html'"),
        format!("{MANIFEST}\n[[commands]]\nid='status'\ntitle='Duplicate'\ncommand=['echo']"),
    ] {
        assert!(Manifest::parse(&text).is_err());
    }
}
#[test]
fn recovery_preserves_a_corrupt_registry_and_does_not_execute_it() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("registry.json"), "broken").unwrap();
    let store = Store::new(root.path().into(), Arc::new(FakeRunner::new()), false);
    assert!(store.snapshot().problem.is_some());
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
    assert!(wtm_plugins::session::decode(r#"{"jsonrpc":"2.0","id":"request","error":{"message":"Control lease is held elsewhere"}}"#, "request").unwrap_err().contains("Control lease"));
}

#[test]
fn views_replace_only_explicitly_selected_regions_and_revocation_restores_the_builtin() {
    let root = tempfile::tempdir().unwrap();
    let src = source();
    std::fs::write(src.path().join("view.html"), "<p>My workbench</p>").unwrap();
    std::fs::write(src.path().join("wtm-plugin.toml"), format!("{MANIFEST}\n[[views]]\nid='workbench'\ntitle='My workbench'\nfile='view.html'\nslots=['workbench','code']\n")).unwrap();
    let store = Store::new(root.path().into(), Arc::new(FakeRunner::new()), false);
    let review = store
        .prepare(src.path().to_str().unwrap(), "local", "")
        .unwrap();
    store.install(&review.token).unwrap();
    assert_eq!(store.binding(Slot::Workbench), Binding::Builtin);
    let choice = Binding::Plugin {
        plugin: "example.tools".into(),
        view: "workbench".into(),
    };
    assert!(store.bind(Slot::Sidebar, choice.clone()).is_err());
    assert!(
        store
            .view(Slot::Workbench, "example.tools", "workbench")
            .is_err()
    );
    store.bind(Slot::Workbench, choice.clone()).unwrap();
    assert_eq!(store.binding(Slot::Workbench), choice);
    assert!(
        store
            .view(Slot::Workbench, "example.tools", "workbench")
            .unwrap()
            .contains("My workbench")
    );
    assert_eq!(store.binding(Slot::Code), Binding::Builtin);
    let activation = store.activation("example.tools");
    assert!(store.set_enabled("example.tools", false).is_err());
    drop(activation);
    store.set_enabled("example.tools", false).unwrap();
    assert_eq!(store.binding(Slot::Workbench), Binding::Builtin);
    store.set_enabled("example.tools", true).unwrap();
    assert_eq!(store.binding(Slot::Workbench), Binding::Builtin);
}

#[test]
fn recovery_restores_hidden_regions_and_never_implicitly_reenables_another_package() {
    let root = tempfile::tempdir().unwrap();
    let src = source();
    let store = Store::new(root.path().into(), Arc::new(FakeRunner::new()), false);
    let first = store
        .prepare(src.path().to_str().unwrap(), "local", "")
        .unwrap();
    store.install(&first.token).unwrap();
    std::fs::write(
        src.path().join("wtm-plugin.toml"),
        MANIFEST.replace("example.tools", "example.other"),
    )
    .unwrap();
    let second = store
        .prepare(src.path().to_str().unwrap(), "local", "")
        .unwrap();
    store.install(&second.token).unwrap();
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
fn terminal_profiles_are_reviewed_and_resolve_package_programs_without_shell_expansion() {
    let root = tempfile::tempdir().unwrap();
    let src = source();
    std::fs::write(src.path().join("launcher"), "an example executable").unwrap();
    std::fs::write(src.path().join("wtm-plugin.toml"), format!("{MANIFEST}\n[[terminals]]\nid='terminal'\ntitle='My CLI'\ncommand=['./launcher', 'literal;argument']\n")).unwrap();
    let store = Store::new(root.path().into(), Arc::new(FakeRunner::new()), false);
    let review = store
        .prepare(src.path().to_str().unwrap(), "local", "")
        .unwrap();
    assert!(store.terminal("example.tools", "terminal").is_err());
    store.install(&review.token).unwrap();
    let argv = store.terminal("example.tools", "terminal").unwrap();
    assert!(argv[0].starts_with(root.path().to_str().unwrap()));
    assert_eq!(argv[1], "literal;argument");
    store.set_enabled("example.tools", false).unwrap();
    assert!(store.terminal("example.tools", "terminal").is_err());
}
