//! The public extension catalog must describe the frontend exactly: every surface it offers is
//! mounted where it says, every surface the frontend mounts is in it, and every IPC route says
//! whether a plugin may call or intercept it.
//!
//! Surfaces are semantic only. A component boundary is not a public contract, so components are
//! no longer classified one by one; what is checked is the short list of named surfaces, which is
//! what a manifest can target. Nothing else would notice a surface offered and never mounted, or a
//! mount whose target the catalog does not know: either renders nothing and raises no error.
#![allow(clippy::unwrap_used)]
use regex::Regex;
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
fn surfaces() -> Vec<(String, BTreeSet<String>, String)> {
    catalog()["surfaces"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["id"].as_str().unwrap().to_owned(),
                s["kinds"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|k| k.as_str().unwrap().to_owned())
                    .collect(),
                s["source"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}
/// Every `.svelte` and `.ts` file under `src`, with its contents.
fn frontend(path: &Path, out: &mut Vec<(PathBuf, String)>) {
    for entry in std::fs::read_dir(path).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            frontend(&path, out);
        } else if path.extension().is_some_and(|e| e == "svelte" || e == "ts") {
            let source = std::fs::read_to_string(&path).unwrap();
            out.push((path, source));
        }
    }
}
fn sources() -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    frontend(&root().join("src"), &mut out);
    out
}
/// Targets written out where they are mounted: an `ExtensionPoint` or `ExtensionActions` with a
/// literal `target`, an `extensionPart('…')` attachment, or an `extensions.targets('…')` guard.
fn literal_mounts(source: &str) -> Vec<(&'static str, String)> {
    let point = Regex::new(r#"(?s)<ExtensionPoint\b[^>]*?\btarget="([^"]+)""#).unwrap();
    let actions = Regex::new(r#"(?s)<ExtensionActions\b[^>]*?\btarget="([^"]+)""#).unwrap();
    let part = Regex::new(r#"extensionPart\(\s*['"]([^'"]+)['"]"#).unwrap();
    let guard = Regex::new(r#"extensions\.targets\(\s*['"]([^'"]+)['"]"#).unwrap();
    let mut out = Vec::new();
    for (kind, pattern) in [
        ("point", &point),
        ("actions", &actions),
        ("style", &part),
        ("guard", &guard),
    ] {
        for capture in pattern.captures_iter(source) {
            out.push((kind, capture[1].to_owned()));
        }
    }
    out
}
/// The members of `Surface` in `src/lib/extensions.ts`, the type every mount's target has.
fn surface_type() -> BTreeSet<String> {
    let source = std::fs::read_to_string(root().join("src/lib/extensions.ts")).unwrap();
    let union = Regex::new(r"(?s)export type Surface =([^;]+);")
        .unwrap()
        .captures(&source)
        .expect("src/lib/extensions.ts declares the Surface union")[1]
        .to_owned();
    Regex::new(r"'([^']+)'")
        .unwrap()
        .captures_iter(&union)
        .map(|c| c[1].to_owned())
        .collect()
}

#[test]
fn every_surface_that_takes_contributions_is_mounted_by_the_component_the_catalog_names() {
    let root = root();
    for (id, kinds, source) in surfaces() {
        if kinds.is_empty() {
            continue;
        }
        let text = std::fs::read_to_string(root.join(&source))
            .unwrap_or_else(|_| panic!("{id} names {source}, which does not exist"));
        let mounts = literal_mounts(&text);
        let at = |kind: &str| mounts.iter().any(|(k, t)| *k == kind && *t == id);
        // A target chosen by an expression (Settings picks its section's) is still spelt out in
        // the file, as a literal of the `Surface` type, beside a point that can take it.
        let spelt = text.contains(&format!("'{id}'")) && text.contains("<ExtensionPoint");
        for kind in &kinds {
            let mounted = match kind.as_str() {
                "style" => at("style"),
                "actions" => at("actions") || at("point") || spelt,
                "content" | "settings" => at("point") || spelt,
                other => panic!("{id} declares the unknown kind {other}"),
            };
            assert!(mounted, "{id} takes {kind}, and {source} does not mount it");
        }
    }
}

#[test]
fn every_surface_the_frontend_mounts_is_one_the_catalog_offers() {
    let offered: BTreeSet<String> = surfaces()
        .into_iter()
        .filter(|(_, kinds, _)| !kinds.is_empty())
        .map(|(id, _, _)| id)
        .collect();
    let mut seen = 0;
    for (path, source) in sources() {
        for (_, target) in literal_mounts(&source) {
            seen += 1;
            assert!(
                offered.contains(&target),
                "{} mounts {target}, which the catalog does not offer",
                path.display()
            );
        }
    }
    assert!(seen > 20, "the mount scanner stopped finding mounts");
    assert_eq!(
        surface_type(),
        offered,
        "the Surface type and the catalog's contributable surfaces must be the same list"
    );
}

#[test]
fn a_surface_that_takes_no_contributions_is_mounted_nowhere() {
    for (id, kinds, _) in surfaces() {
        if !kinds.is_empty() {
            continue;
        }
        for (path, source) in sources() {
            assert!(
                !source.contains(&format!("'{id}'")) && !source.contains(&format!("\"{id}\"")),
                "{} names {id}, which takes no contributions",
                path.display()
            );
        }
    }
}

#[test]
fn every_surface_and_operation_id_is_unique() {
    let catalog = catalog();
    for list in ["surfaces", "operations", "events"] {
        let ids: Vec<_> = catalog[list]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["id"].as_str().unwrap())
            .collect();
        assert_eq!(
            ids.len(),
            ids.iter().collect::<BTreeSet<_>>().len(),
            "duplicate {list} id"
        );
    }
}

#[test]
fn every_frontend_command_declares_whether_plugins_may_call_or_intercept_it() {
    let catalog = catalog();
    let operations = catalog["operations"].as_array().unwrap();
    let source = std::fs::read_to_string(root().join("src/lib/ipc/commands.ts")).unwrap();
    let calls = Regex::new(r"(?s)\binvoke(?:<[^;]*?>)?\(\s*'([^']+)'").unwrap();
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
