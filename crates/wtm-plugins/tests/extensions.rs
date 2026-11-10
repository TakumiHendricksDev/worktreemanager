//! API 2 must reject undeclared authority and retain user preferences when code is removed.
//!
//! It also pins the style allowlist, whose job is that a plugin can restyle a part but never
//! unhide, resize or move a host region, and the rule that declared host access is native trust.
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
        PACKAGE.replace("chat.markdown.link", "settings.plugins"),
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
    let label = json!({
        "kind": "setting",
        "id": "label",
        "value": "Review workspace",
        "worktree": "/workspace",
    });
    store
        .extension_configure("example.workbench", &label)
        .unwrap();
    store
        .extension_configure(
            "example.workbench",
            &json!({"kind":"scope","worktrees":["/workspace"]}),
        )
        .unwrap();
    let choose = |target: &str, id: &str| {
        let change = json!({"kind": "choice", "target": target, "id": id});
        store.extension_configure("example.workbench", &change)
    };
    choose("style:chat.markdown.link", "links").unwrap();
    // A style is chosen under its part's style key, never a bare id or another part's key.
    assert!(choose("chat.markdown.link", "links").is_err());
    assert!(choose("style:chat.markdown.code", "links").is_err());
    assert!(choose("ui.workspace.rename", "links").is_err());
    choose("style:chat.markdown.link", "builtin").unwrap();
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
fn untrusted_runtime_paths_unknown_events_and_undeclared_widget_references_are_rejected() {
    assert!(
        Manifest::parse(&PACKAGE.replace(
            "[extensions]",
            "[extensions]
runtime=['../runner']"
        ))
        .is_err()
    );
    assert!(
        Manifest::parse(&PACKAGE.replace(
            "[extensions]",
            "[extensions]
runtime=['./.git/run']"
        ))
        .is_err()
    );
    assert!(
        Manifest::parse(&PACKAGE.replace(
            "[extensions]",
            "[extensions]
events=['chat.event']"
        ))
        .is_err()
    );
    let undeclared = format!(
        "{PACKAGE}
[[extensions.content]]
id='note'
title='Note'
target='chat.message'
body=[{{kind='action',action='missing'}}]
"
    );
    assert!(Manifest::parse(&undeclared).is_err());
}

/// Content nested `depth` groups deep, with one text widget at the bottom.
fn nested(depth: usize) -> String {
    let mut body = "{kind='text',text='Deepest'}".to_owned();
    for _ in 0..depth {
        body = format!("{{kind='group',title='Group',children=[{body}]}}");
    }
    format!(
        "{PACKAGE}
[[extensions.content]]
id='note'
title='Note'
target='chat.message'
body=[{body}]
"
    )
}

#[test]
fn content_can_only_be_added_beside_a_surface_never_replace_it() {
    const NOTE: &str = "[[extensions.content]]\nid='note'\ntitle='Note'\ntarget='chat.message'";
    let content = |position: &str| {
        format!("{PACKAGE}\n{NOTE}\nposition='{position}'\nbody=[{{kind='text',text='Note'}}]\n")
    };
    assert!(Manifest::parse(&content("before")).is_ok());
    assert!(Manifest::parse(&content("after")).is_ok());
    assert!(Manifest::parse(&content("replace")).is_err());
}

#[test]
fn nested_content_is_accepted_to_eight_groups_deep_and_refused_beyond() {
    assert!(Manifest::parse(&nested(8)).is_ok());
    assert!(Manifest::parse(&nested(9)).unwrap_err().contains("nesting"));
}

#[test]
fn styles_may_not_set_a_boxs_size_visibility_or_place_and_only_real_urls_are_refused() {
    use wtm_plugins::extensions::validate_style;
    for property in [
        "display",
        "width",
        "min-width",
        "max-height",
        "height",
        "grid-template-columns",
        "margin",
        "margin-inline",
        "position",
        "visibility",
    ] {
        assert!(validate_style(property, "0").is_err(), "allowed {property}");
    }
    assert!(validate_style("font-family", "\"Curlz MT\", serif").is_ok());
    assert!(validate_style("font-family", "Expressway").is_ok());
    for value in [
        "url(https://example.invalid/x.png)",
        "URL(x)",
        "expression(alert(1))",
    ] {
        assert!(validate_style("color", value).is_err(), "allowed {value}");
    }
}

#[test]
fn declared_host_access_alone_makes_a_package_native_code_for_review() {
    let declarative = PACKAGE
        .replace("access=['ui.workspace.rename']", "access=[]")
        .split("[[extensions.actions]]")
        .next()
        .unwrap()
        .to_owned();
    assert!(!Manifest::parse(&declarative).unwrap().executable());
    assert!(Manifest::parse(PACKAGE).unwrap().executable());
}
