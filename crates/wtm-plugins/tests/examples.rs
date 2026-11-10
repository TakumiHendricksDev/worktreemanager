//! The packages under `examples/plugins/` are what authors copy and what the authoring agent is
//! handed, so each one must pass the same review a user's package does.
//!
//! A manifest field the parser no longer accepts, a target the catalog dropped, or a panel that
//! picked up an external reference would otherwise ship as an example that cannot be installed,
//! and the first to find out would be someone following it. Each package is prepared from its
//! folder by the real [`Store`], installed, and its panels opened through the activation path, so
//! the digest, the manifest and the document rule are all the ones the app applies.
//!
//! The guide's own Hello, world is held to the same review, because it is the first thing a new
//! author, or an authoring agent, copies.
#![allow(clippy::unwrap_used)]
use std::path::{Path, PathBuf};
use std::sync::Arc;
use wtm_plugins::{Store, document};
use wtm_testkit::FakeRunner;

fn examples() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/plugins");
    let mut packages: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.join("wtm-plugin.toml").is_file())
        .collect();
    packages.sort();
    packages
}

#[test]
fn every_example_package_is_found() {
    // A moved directory would otherwise make the next test pass by checking nothing.
    let names: Vec<_> = examples()
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    for expected in ["project-tools", "session-demo", "workbench-toolkit"] {
        assert!(
            names.iter().any(|n| n == expected),
            "{expected} in {names:?}"
        );
    }
}

#[test]
fn every_example_package_passes_review_and_installs_without_running_anything() {
    for source in examples() {
        let root = tempfile::tempdir().unwrap();
        let runner = Arc::new(FakeRunner::default());
        let store = Store::new(root.path().into(), runner.clone(), false);
        let review = store
            .prepare(source.to_str().unwrap(), "local", "")
            .unwrap_or_else(|e| panic!("{} was refused: {e}", source.display()));
        assert!(
            runner.calls().is_empty(),
            "{} ran a program while it was prepared",
            source.display()
        );
        let manifest = review.package.manifest.clone();
        store.install(&review.token).unwrap();
        for panel in &manifest.panels {
            store
                .panel(&manifest.id, &panel.id)
                .unwrap_or_else(|e| panic!("{} panel {}: {e}", manifest.id, panel.id));
        }
        assert!(runner.calls().is_empty(), "{} ran a program", manifest.id);
    }
}

#[test]
fn every_example_panel_and_view_is_a_self_contained_document() {
    for source in examples() {
        let manifest = wtm_plugins::manifest::Manifest::parse(
            &std::fs::read_to_string(source.join("wtm-plugin.toml")).unwrap(),
        )
        .unwrap();
        let files = manifest
            .panels
            .iter()
            .map(|p| &p.file)
            .chain(manifest.views.iter().map(|v| &v.file));
        for file in files {
            let html = std::fs::read_to_string(source.join(file))
                .unwrap_or_else(|e| panic!("{}: {file}: {e}", manifest.id));
            assert!(
                document::self_contained(&html),
                "{}: {file} references something outside itself",
                manifest.id
            );
        }
    }
}

/// The body of the first fenced block tagged `lang` after `heading` in the plugin guide.
fn guide_block(heading: &str, lang: &str) -> String {
    let guide = include_str!("../../../docs/plugins.md");
    let section = &guide[guide.find(heading).unwrap()..];
    let open = format!("```{lang}\n");
    let start = section.find(&open).unwrap() + open.len();
    let end = start + section[start..].find("\n```").unwrap();
    section[start..end].to_owned()
}

#[test]
fn the_guides_hello_world_passes_review_as_written() {
    let source = tempfile::tempdir().unwrap();
    let manifest = guide_block("## Hello, world", "toml");
    std::fs::write(source.path().join("wtm-plugin.toml"), &manifest).unwrap();
    let parsed = wtm_plugins::manifest::Manifest::parse(&manifest).unwrap();
    let html = guide_block("## Hello, world", "html");
    assert!(document::self_contained(&html));
    std::fs::write(source.path().join(&parsed.views[0].file), html).unwrap();
    let root = tempfile::tempdir().unwrap();
    let store = Store::new(root.path().into(), Arc::new(FakeRunner::default()), false);
    let review = store
        .prepare(source.path().to_str().unwrap(), "local", "")
        .unwrap();
    // It is the guide's example of a plugin that runs nothing, so it must not carry the warning.
    assert!(!review.executable);
    store.install(&review.token).unwrap();
}
