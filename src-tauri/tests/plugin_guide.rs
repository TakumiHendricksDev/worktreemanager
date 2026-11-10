//! The guide an authoring agent reads must work where it is read: in a fresh plugin workspace,
//! with no wtm checkout anywhere.
//!
//! The guide is `docs/plugins.md`, which also lives in this repository and links to the example
//! packages beside it. Copied into a workspace unchanged, those links named files that were not
//! there, and its commands for working on wtm's own source sent the agent to a checkout it did
//! not have. These tests create a real workspace through `plugin_authoring::prepare` and read what
//! it wrote, so a new example file, a new link or a new repository-only command that the writer
//! does not handle fails here instead of in an agent's turn.
#![allow(clippy::unwrap_used)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use wtm_app_lib::{app::App, plugin_authoring};
use wtm_config::AppPaths;

/// A real app over temporary config, with Git and inert provider binaries on its PATH.
struct Workspace {
    folder: PathBuf,
    _config: tempfile::TempDir,
    _root: tempfile::TempDir,
}
impl Workspace {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let git = wtm_exec::ResolvedPath::resolve(None)
            .which("git", Path::new("."))
            .unwrap();
        std::os::unix::fs::symlink(git, bin.join("git")).unwrap();
        for provider in ["claude", "codex"] {
            let path = bin.join(provider);
            std::fs::write(&path, "#!/bin/sh\nexit 97\n").unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let config = tempfile::tempdir().unwrap();
        std::fs::write(
            config.path().join("config.toml"),
            format!("[exec]\npath = {:?}\n", bin.to_string_lossy()),
        )
        .unwrap();
        let app = App::with_paths(AppPaths::rooted(config.path())).unwrap();
        let folder = root.path().canonicalize().unwrap().join("my-plugin");
        plugin_authoring::prepare(
            &app,
            &uuid::Uuid::new_v4().to_string(),
            "codex",
            "Add a panel",
            folder.to_str().unwrap(),
        )
        .unwrap();
        Self {
            folder,
            _config: config,
            _root: root,
        }
    }
    fn git(&self) -> PathBuf {
        self.folder.join(".git")
    }
    fn guide(&self) -> String {
        std::fs::read_to_string(self.git().join("wtm-plugin-guide.md")).unwrap()
    }
}

/// Every `](target)` in the Markdown, without its `#fragment`.
fn links(markdown: &str) -> Vec<String> {
    markdown
        .split("](")
        .skip(1)
        .filter_map(|rest| rest.split_once(')').map(|(target, _)| target))
        .map(|target| target.split('#').next().unwrap_or_default().to_owned())
        .filter(|target| !target.is_empty())
        .collect()
}

#[test]
fn every_relative_link_in_the_bundled_guide_names_a_file_written_beside_it() {
    let workspace = Workspace::new();
    let guide = workspace.guide();
    let relative: Vec<_> = links(&guide)
        .into_iter()
        .filter(|t| !t.starts_with("https://") && !t.starts_with("http://"))
        .collect();
    assert!(!relative.is_empty(), "the guide links to no example at all");
    for target in relative {
        assert!(
            workspace.git().join(&target).exists(),
            "the guide links to {target}, which the workspace does not have"
        );
    }
}

#[test]
fn the_bundled_guide_leaves_out_what_only_a_wtm_checkout_can_run() {
    let guide = Workspace::new().guide();
    for repository_only in [
        "Developing wtm itself",
        "cargo run",
        "cargo nextest",
        "bun scripts/",
        "just check",
        "](../",
    ] {
        assert!(
            !guide.contains(repository_only),
            "the bundled guide still says {repository_only:?}"
        );
    }
    assert!(guide.contains("wtm.request"));
}

#[test]
fn every_example_file_is_bundled_unchanged_with_the_contracts() {
    let workspace = Workspace::new();
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../examples/plugins");
    let mut stack = vec![examples.clone()];
    let mut count = 0;
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let name = path.strip_prefix(&examples).unwrap();
            let bundled = workspace
                .git()
                .join(plugin_authoring::GUIDE_EXAMPLES)
                .join(name);
            assert_eq!(
                std::fs::read_to_string(&bundled).ok(),
                Some(std::fs::read_to_string(&path).unwrap()),
                "{} is not bundled as written; add it to plugin_authoring's EXAMPLES",
                name.display()
            );
            count += 1;
        }
    }
    assert!(count >= 9, "only {count} example files were found");
    for contract in ["wtm-plugin-catalog.json", "wtm-plugin-operations.json"] {
        let text = std::fs::read_to_string(workspace.git().join(contract)).unwrap();
        serde_json::from_str::<serde_json::Value>(&text).unwrap();
    }
}
