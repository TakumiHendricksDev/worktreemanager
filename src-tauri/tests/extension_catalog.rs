//! New surfaces and IPC routes must explicitly join the public registry or explain their host boundary.
#![allow(clippy::unwrap_used)]
use serde_json::Value;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}
fn catalog() -> Value {
    serde_json::from_str(include_str!(
        "../../crates/wtm-plugins/contracts/catalog.json"
    ))
    .unwrap()
}
fn components(path: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(path).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files.extend(components(&path));
        } else if path.extension().is_some_and(|e| e == "svelte") {
            files.push(path);
        }
    }
    files
}
#[test]
fn every_component_has_a_public_contract_or_an_explicit_host_classification() {
    let root = root();
    let catalog = catalog();
    let surfaces = catalog["surfaces"].as_array().unwrap();
    for file in components(&root.join("src/lib/components")) {
        let relative = file.strip_prefix(&root).unwrap().to_str().unwrap();
        let entries: Vec<_> = surfaces
            .iter()
            .filter(|s| s["source"] == relative)
            .collect();
        assert!(
            !entries.is_empty(),
            "classify {relative} before introducing a new surface"
        );
        let source = std::fs::read_to_string(&file).unwrap();
        for entry in entries {
            assert!(!entry["reason"].as_str().unwrap().is_empty());
            if !entry["kinds"].as_array().unwrap().is_empty() {
                let id = entry["id"].as_str().unwrap();
                assert!(
                    source.contains(id) || id.starts_with("settings."),
                    "declared {id} is not mounted by {relative}"
                );
                assert!(
                    source.contains("ExtensionPoint") || source.contains("extensionPart"),
                    "{relative} must consume the shared registry"
                );
            }
        }
    }
    let ids: Vec<_> = surfaces.iter().map(|s| s["id"].as_str().unwrap()).collect();
    assert_eq!(ids.len(), ids.iter().collect::<BTreeSet<_>>().len());
}

#[test]
fn every_frontend_command_declares_whether_plugins_may_call_or_intercept_it() {
    let catalog = catalog();
    let operations = catalog["operations"].as_array().unwrap();
    let source = std::fs::read_to_string(root().join("src/lib/ipc/commands.ts")).unwrap();
    let calls = regex::Regex::new(r"(?s)\binvoke(?:<[^;]*?>)?\(\s*'([^']+)'").unwrap();
    let mut count = 0;
    for capture in calls.captures_iter(&source) {
        count += 1;
        let id = &capture[1];
        assert!(
            operations.iter().any(|o| o["id"] == id),
            "classify {id} before exposing an IPC operation"
        );
    }
    assert!(
        count > 100,
        "command scanner stopped covering the IPC module"
    );
    let ids: Vec<_> = operations
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids.len(), ids.iter().collect::<BTreeSet<_>>().len());
    for protected in [
        "answer_approval",
        "set_config_trust",
        "extension_reply",
        "plugin_change",
        "chat_edit",
    ] {
        assert!(
            operations
                .iter()
                .any(|o| o["id"] == protected && o["exposure"] == "protected")
        );
    }
}
