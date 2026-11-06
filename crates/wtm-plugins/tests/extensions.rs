//! API 2 must reject undeclared authority and retain user preferences when code is removed.
#![allow(clippy::unwrap_used)]
use serde_json::json;
use std::sync::Arc;
use wtm_plugins::{Store, manifest::Manifest};
use wtm_testkit::FakeRunner;

const PACKAGE: &str = r"
schema=1
api=2
id='example.workbench'
name='Workbench Toolkit'
version='1.0.0'
[extensions]
access=['ui.workspace.rename']
[[extensions.styles]]
id='links'
title='Readable links'
target='chat.markdown.link'
[extensions.styles.properties]
color='var(--accent)'
text-decoration='underline'
[[extensions.settings]]
id='label'
title='Workspace label'
section='settings.appearance'
kind='text'
default='My workspace'
[[extensions.actions]]
id='rename'
title='Rename workspace'
target='workspace.actions'
operation='ui.workspace.rename'
[extensions.actions.arguments]
name={'$setting'='label'}
";

#[test]
fn the_shipped_workbench_toolkit_uses_only_installable_public_contracts() {
    Manifest::parse(include_str!(
        "../../../examples/plugins/workbench-toolkit/wtm-plugin.toml"
    ))
    .unwrap();
}

#[test]
fn only_supported_public_contracts_and_declared_operations_can_be_installed() {
    assert!(Manifest::parse(PACKAGE).is_ok());
    for invalid in [
        PACKAGE.replace("api=2", "api=1"),
        PACKAGE.replace("chat.markdown.link", "private.dom.selector"),
        PACKAGE.replace("chat.markdown.link", "component.approval-card"),
        PACKAGE.replace("access=['ui.workspace.rename']", "access=[]"),
        PACKAGE.replace("ui.workspace.rename", "answer_approval"),
        PACKAGE.replace(
            "color='var(--accent)'",
            "color='url(https://example.invalid)'",
        ),
        PACKAGE.replace("text-decoration='underline'", "position='fixed'"),
        PACKAGE.replace("kind='text'", "kind='boolean'"),
        PACKAGE.replace("id='rename'", "id='links'"),
    ] {
        assert!(Manifest::parse(&invalid).is_err(), "accepted {invalid}");
    }
}

#[test]
fn scoped_preferences_survive_disable_uninstall_and_reinstall() {
    let source = tempfile::tempdir().unwrap();
    let root = tempfile::tempdir().unwrap();
    std::fs::write(source.path().join("wtm-plugin.toml"), PACKAGE).unwrap();
    let store = Store::new(root.path().into(), Arc::new(FakeRunner::new()), false);
    let review = store
        .prepare(source.path().to_str().unwrap(), "local", "")
        .unwrap();
    store.install(&review.token).unwrap();
    store.extension_configure("example.workbench",&json!({"kind":"setting","id":"label","value":"Review workspace","worktree":"/workspace"})).unwrap();
    store
        .extension_configure(
            "example.workbench",
            &json!({"kind":"scope","worktrees":["/workspace"]}),
        )
        .unwrap();
    store
        .extension_configure(
            "example.workbench",
            &json!({"kind":"choice","target":"chat.markdown.link","id":"builtin"}),
        )
        .unwrap();
    assert!(
        store
            .extension_allowed("example.workbench", "ui.workspace.rename", "/workspace")
            .is_ok()
    );
    assert!(
        store
            .extension_allowed("example.workbench", "ui.workspace.rename", "/another")
            .is_err()
    );
    assert!(
        store
            .extension_allowed("example.workbench", "answer_approval", "/workspace")
            .is_err()
    );
    store.set_enabled("example.workbench", false).unwrap();
    assert!(
        store
            .extension_allowed("example.workbench", "ui.workspace.rename", "/workspace")
            .is_err()
    );
    store.uninstall("example.workbench").unwrap();
    let review = store
        .prepare(source.path().to_str().unwrap(), "local", "")
        .unwrap();
    store.install(&review.token).unwrap();
    assert_eq!(
        store.snapshot().registry.extensions["example.workbench"].values["/workspace"]["label"],
        "Review workspace"
    );
    assert!(
        store
            .extension_configure(
                "example.workbench",
                &json!({"kind":"setting","id":"label","value":false})
            )
            .is_err()
    );
}

#[test]
fn untrusted_runtime_paths_and_recursive_content_are_rejected() {
    assert!(
        Manifest::parse(&PACKAGE.replace("[extensions]", "[extensions]\nruntime=['../runner']"))
            .is_err()
    );
    assert!(
        Manifest::parse(&PACKAGE.replace("[extensions]", "[extensions]\nevents=['chat.event']"))
            .is_err()
    );
    let recursive = format!(
        "{PACKAGE}\n[[extensions.content]]\nid='note'\ntitle='Note'\ntarget='chat.message'\nbody=[{{kind='action',action='missing'}}]\n"
    );
    assert!(Manifest::parse(&recursive).is_err());
}
