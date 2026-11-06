//! Draft creation must preserve user files and survive restart without executing or trusting
//! generated code. These tests use inert provider binaries and the real Git/config adapters.
#![allow(clippy::unwrap_used)]
use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};
use wtm_app_lib::{app::App, plugin_authoring};
use wtm_config::AppPaths;

struct Harness {
    app: App,
    config: tempfile::TempDir,
    root: tempfile::TempDir,
}
impl Harness {
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
        Self { app, config, root }
    }
    fn folder(&self) -> PathBuf {
        self.root.path().canonicalize().unwrap().join("my-plugin")
    }
}

#[test]
fn preparing_a_draft_is_idempotent_and_survives_restart_without_starting_an_agent() {
    let h = Harness::new();
    let id = uuid::Uuid::new_v4().to_string();
    let path = h.folder();
    let prepare = || {
        plugin_authoring::prepare(
            &h.app,
            &id,
            "codex",
            "Build a custom view",
            path.to_str().unwrap(),
        )
        .unwrap()
    };
    let first = prepare();
    assert_eq!(first.folder, path.to_str().unwrap());
    assert_eq!(prepare().id, first.id);
    assert!(h.app.live_agents().is_empty());
    assert!(h.app.plugins.snapshot().registry.installed.is_empty());
    assert!(h.app.project(&first.folder).is_ok());
    let project = h.app.project(&first.folder).unwrap();
    assert!(h.app.worktree(&project, &first.folder).is_ok());
    assert!(
        std::fs::read_to_string(path.join(".git/wtm-plugin-guide.md"))
            .unwrap()
            .contains("wtm.request")
    );
    let restarted = App::with_paths(AppPaths::rooted(h.config.path())).unwrap();
    let jobs = restarted.plugin_jobs.list().unwrap();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].brief, "Build a custom view");
    assert!(!jobs[0].submitted);
    assert!(restarted.live_agents().is_empty());
    assert!(
        plugin_authoring::prepare(
            &h.app,
            &id,
            "claude",
            "Different request",
            path.to_str().unwrap()
        )
        .is_err()
    );
}

#[test]
fn creating_a_plugin_never_overwrites_an_existing_folder_or_follows_a_symlink() {
    let h = Harness::new();
    let path = h.folder();
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("keep.txt"), "user content").unwrap();
    let create = |p: &Path| {
        plugin_authoring::prepare(
            &h.app,
            &uuid::Uuid::new_v4().to_string(),
            "claude",
            "Add a command",
            p.to_str().unwrap(),
        )
    };
    assert!(create(&path).is_err());
    assert_eq!(
        std::fs::read_to_string(path.join("keep.txt")).unwrap(),
        "user content"
    );
    assert!(!path.join(".git").exists());
    let link = h.root.path().canonicalize().unwrap().join("linked");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    assert!(create(&link.join("child")).is_err());
    assert!(!path.join("child").exists());
    assert!(h.app.plugin_jobs.list().unwrap().is_empty());
}

#[test]
fn disabling_chat_refuses_creation_before_making_a_workspace() {
    let h = Harness::new();
    h.app.plugins.set_enabled("wtm.home", false).unwrap();
    h.app.plugins.experience("wtm.native-cli").unwrap();
    h.app.plugins.set_enabled("wtm.chat", false).unwrap();
    let result = plugin_authoring::prepare(
        &h.app,
        &uuid::Uuid::new_v4().to_string(),
        "codex",
        "New panel",
        h.folder().to_str().unwrap(),
    );
    assert!(result.unwrap_err().message.contains("Enable WTM Chat"));
    assert!(!h.folder().exists());
}

#[test]
fn invalid_output_is_not_installed_and_the_guide_is_not_packaged() {
    let h = Harness::new();
    let job = plugin_authoring::prepare(
        &h.app,
        &uuid::Uuid::new_v4().to_string(),
        "codex",
        "New panel",
        h.folder().to_str().unwrap(),
    )
    .unwrap();
    assert!(h.app.plugins.prepare(&job.folder, "local", "").is_err());
    std::fs::write(h.folder().join("wtm-plugin.toml"), "schema=1\napi=1\nid='example.generated'\nname='Generated'\nversion='0.1.0'\n[[panels]]\nid='view'\ntitle='View'\nfile='missing.html'\n").unwrap();
    assert!(h.app.plugins.prepare(&job.folder, "local", "").is_err());
    std::fs::write(
        h.folder().join("missing.html"),
        "<!doctype html><p>Generated view</p>",
    )
    .unwrap();
    let review = h.app.plugins.prepare(&job.folder, "local", "").unwrap();
    assert!(!Path::new(&review.package.directory).join(".git").exists());
    assert!(h.app.plugins.snapshot().registry.installed.is_empty());
    h.app.plugins.install(&review.token).unwrap();
    assert_eq!(
        plugin_authoring::list(&h.app).unwrap()[0]
            .installed
            .as_deref(),
        Some("example.generated")
    );
}

#[test]
fn damaged_draft_records_are_reported_without_overwriting_them() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("drafts.json");
    std::fs::write(&path, "broken").unwrap();
    let jobs = plugin_authoring::Jobs::new(path.clone());
    assert!(jobs.list().is_err());
    assert_eq!(std::fs::read_to_string(path).unwrap(), "broken");
}
