//! Draft creation must preserve user files and survive restart without executing or trusting
//! generated code, and a draft's conversation must hear its brief exactly once.
//!
//! The first half uses inert provider binaries and the real Git/config adapters: nothing here
//! needs an agent to prove where a draft may live or how its record is kept. The second half
//! drives a fake `codex app-server` (see `support/fake_codex.rs`), because "the brief is never
//! sent twice" is a claim about a real process receiving real frames, and the only test that can
//! fail it is one that counts what the process received.
#![allow(clippy::unwrap_used)]

#[path = "support/fake_codex.rs"]
mod fake_codex;

use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};
use wtm_app_lib::{
    app::{AgentStatus, App, CHAT_DISABLED, HOME_DISABLED, PLUGIN_DRAFTS_FILE, SessionScope},
    plugin_authoring,
};
use wtm_config::AppPaths;
use wtm_core::ports::config::ConfigStore;

struct Harness {
    app: App,
    bin: PathBuf,
    config: tempfile::TempDir,
    root: tempfile::TempDir,
}
impl Harness {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        std::os::unix::fs::symlink(real_git(), bin.join("git")).unwrap();
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
        Self {
            app,
            bin,
            config,
            root,
        }
    }
    fn folder(&self) -> PathBuf {
        self.root.path().canonicalize().unwrap().join("my-plugin")
    }
    fn prepare(&self, folder: &Path) -> Result<plugin_authoring::Job, String> {
        plugin_authoring::prepare(
            &self.app,
            &uuid::Uuid::new_v4().to_string(),
            "codex",
            "Add a command",
            folder.to_str().unwrap(),
        )
        .map_err(|e| e.message)
    }
}

fn real_git() -> PathBuf {
    wtm_exec::ResolvedPath::resolve(None)
        .which("git", Path::new("."))
        .unwrap()
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
fn the_drafts_file_is_versioned_and_readable_only_by_its_owner() {
    let h = Harness::new();
    h.prepare(&h.folder()).unwrap();
    let file = h.config.path().join(PLUGIN_DRAFTS_FILE);
    // A brief is whatever the user typed, and the record names their conversation.
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(saved["version"], 1);
    assert_eq!(saved["drafts"].as_object().unwrap().len(), 1);
    // Nothing half-written is left beside it.
    let strays: Vec<_> = std::fs::read_dir(h.config.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(strays.is_empty(), "{strays:?}");
}

#[test]
fn a_braced_or_urn_spelling_of_an_id_names_the_same_draft_as_its_canonical_form() {
    let h = Harness::new();
    let id = uuid::Uuid::new_v4();
    let path = h.folder();
    let prepare = |spelling: String| {
        plugin_authoring::prepare(&h.app, &spelling, "codex", "Panel", path.to_str().unwrap())
            .unwrap()
    };
    let first = prepare(format!("{{{}}}", id.to_string().to_uppercase()));
    assert_eq!(first.id, id.to_string());
    let again = prepare(id.urn().to_string());
    assert_eq!(again.id, first.id);
    assert_eq!(h.app.plugin_jobs.list().unwrap().len(), 1);
}

#[test]
fn creating_a_plugin_never_overwrites_an_existing_folder() {
    let h = Harness::new();
    let path = h.folder();
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("keep.txt"), "user content").unwrap();
    let refusal = h.prepare(&path).unwrap_err();
    assert!(refusal.contains("new or empty folder"), "{refusal}");
    assert_eq!(
        std::fs::read_to_string(path.join("keep.txt")).unwrap(),
        "user content"
    );
    assert!(!path.join(".git").exists());
    assert!(h.app.plugin_jobs.list().unwrap().is_empty());
}

#[test]
fn a_folder_holding_only_finder_metadata_is_still_empty() {
    let h = Harness::new();
    let path = h.folder();
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join(".DS_Store"), "finder").unwrap();
    h.prepare(&path).unwrap();
    assert_eq!(
        std::fs::read_to_string(path.join(".DS_Store")).unwrap(),
        "finder"
    );
}

#[test]
fn a_new_folder_is_refused_inside_any_project_even_through_a_link() {
    let h = Harness::new();
    // A draft's own folder is a registered project, which makes it the nearest one to hand.
    let project = h.prepare(&h.folder()).unwrap();
    let project = PathBuf::from(project.folder);
    let inside = h.prepare(&project.join("nested")).unwrap_err();
    assert!(inside.contains("outside your projects"), "{inside}");

    let link = h.root.path().canonicalize().unwrap().join("linked");
    std::os::unix::fs::symlink(&project, &link).unwrap();
    let through = h.prepare(&link.join("child")).unwrap_err();
    assert!(through.contains("outside your projects"), "{through}");
    assert!(!project.join("nested").exists());
    assert!(!project.join("child").exists());

    // A link as the folder itself is refused too: the draft would live somewhere other than
    // the path the user chose.
    let empty = h.root.path().canonicalize().unwrap().join("empty");
    std::fs::create_dir(&empty).unwrap();
    let alias = h.root.path().canonicalize().unwrap().join("alias");
    std::os::unix::fs::symlink(&empty, &alias).unwrap();
    let named = h.prepare(&alias).unwrap_err();
    assert!(named.contains("symbolic link"), "{named}");
    assert_eq!(h.app.plugin_jobs.list().unwrap().len(), 1);
}

#[test]
fn a_folder_reached_through_a_linked_parent_is_accepted_as_the_folder_it_really_is() {
    // `/tmp` is a link to `/private/tmp` on macOS, and the first thing anyone tries.
    let h = Harness::new();
    let real = h.root.path().canonicalize().unwrap().join("real");
    std::fs::create_dir(&real).unwrap();
    let alias = h.root.path().canonicalize().unwrap().join("tmp");
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    let job = h.prepare(&alias.join("plugin")).unwrap();
    assert_eq!(Path::new(&job.folder), real.join("plugin"));
    assert!(real.join("plugin/.git").is_dir());
}

#[test]
fn each_refusal_says_which_rule_the_request_broke() {
    let h = Harness::new();
    let folder = h.folder();
    let folder = folder.to_str().unwrap();
    let id = uuid::Uuid::new_v4().to_string();
    let refuse = |id: &str, provider: &str, brief: &str, folder: &str| {
        plugin_authoring::prepare(&h.app, id, provider, brief, folder)
            .unwrap_err()
            .message
    };
    let file = h.root.path().canonicalize().unwrap().join("a-file");
    std::fs::write(&file, "").unwrap();
    let refusals = [
        refuse("not-a-uuid", "codex", "Panel", folder),
        refuse(&id, "gemini", "Panel", folder),
        refuse(&id, "codex", "  ", folder),
        refuse(&id, "codex", &"x".repeat(32 * 1024 + 1), folder),
        refuse(&id, "codex", "Panel", "relative/plugin"),
        refuse(&id, "codex", "Panel", "/tmp/../plugin"),
        refuse(&id, "codex", "Panel", file.to_str().unwrap()),
    ];
    let distinct: std::collections::BTreeSet<_> = refusals.iter().collect();
    assert_eq!(distinct.len(), refusals.len(), "{refusals:#?}");
    assert!(refusals[0].contains("id"), "{}", refusals[0]);
    assert!(refusals[6].contains("file"), "{}", refusals[6]);
}

#[test]
fn a_failed_prepare_takes_back_what_it_made_so_the_same_request_can_retry() {
    let h = Harness::new();
    // Git that initialises but cannot answer `rev-parse`, so registering is what fails: after
    // the folder, its `.git` and the guide all exist. Shell builtins only, because the app's PATH
    // here is the fixture's `bin` and nothing else.
    let git = h.bin.join("git");
    std::fs::remove_file(&git).unwrap();
    std::fs::write(
        &git,
        format!(
            "#!/bin/sh\nif [ -e \"${{0%/*}}/refuse\" ]; then case \" $* \" in \
             *' rev-parse '*) exit 1;; esac; fi\nexec {:?} \"$@\"\n",
            real_git()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(h.bin.join("refuse"), "").unwrap();

    let id = uuid::Uuid::new_v4().to_string();
    let new = h
        .root
        .path()
        .canonicalize()
        .unwrap()
        .join("deep/new-plugin");
    let empty = h.folder();
    std::fs::create_dir(&empty).unwrap();
    let prepare = |folder: &Path| {
        plugin_authoring::prepare(&h.app, &id, "codex", "Panel", folder.to_str().unwrap())
    };
    assert!(prepare(&new).is_err());
    // Every directory it created is gone, the parent it created on the way included.
    assert!(!h.root.path().join("deep").exists());
    assert!(prepare(&empty).is_err());
    // A folder that was already there stays, as empty as it was.
    assert_eq!(std::fs::read_dir(&empty).unwrap().count(), 0);
    assert!(h.app.plugin_jobs.list().unwrap().is_empty());
    assert!(h.app.config.projects().unwrap().is_empty());

    std::fs::remove_file(h.bin.join("refuse")).unwrap();
    let job = prepare(&empty).unwrap();
    assert_eq!(Path::new(&job.folder), empty);
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
fn a_damaged_drafts_file_is_set_aside_so_authoring_keeps_working() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("drafts.json");
    std::fs::write(&path, "broken").unwrap();
    let jobs = plugin_authoring::Jobs::new(path.clone());
    // Not a refusal: before this, one bad byte blocked every draft until someone found the file.
    assert!(jobs.list().unwrap().is_empty());
    let notice = jobs.notice().unwrap();
    assert!(!path.exists());
    let aside: Vec<_> = std::fs::read_dir(root.path())
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect();
    assert_eq!(aside.len(), 1);
    assert!(notice.contains(aside[0].to_str().unwrap()), "{notice}");
    // Moved, never rewritten: whatever was salvageable in it still is.
    assert_eq!(std::fs::read_to_string(&aside[0]).unwrap(), "broken");
}

#[test]
fn drafts_from_another_version_are_refused_and_left_untouched() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("drafts.json");
    let newer = r#"{"version":2,"drafts":{},"tags":[]}"#;
    std::fs::write(&path, newer).unwrap();
    let jobs = plugin_authoring::Jobs::new(path.clone());
    assert!(jobs.list().unwrap_err().message.contains("version 2"));
    assert!(jobs.notice().is_none());
    assert_eq!(std::fs::read_to_string(path).unwrap(), newer);
}

// ── With a provider process ───────────────────────────────────────────────────────────────────

fn draft(h: &fake_codex::Harness, folder: &Path) -> plugin_authoring::Job {
    plugin_authoring::prepare(
        &h.app,
        &uuid::Uuid::new_v4().to_string(),
        "codex",
        "Create a panel",
        folder.to_str().unwrap(),
    )
    .unwrap()
}

#[test]
fn opening_a_draft_sends_its_brief_once_and_returning_to_it_never_sends_it_again() {
    let h = fake_codex::Harness::new();
    let folder = h.folder("generated");
    let job = draft(&h, &folder);

    let first = plugin_authoring::open(&h.window, &h.app, &job.id, false).unwrap();
    assert!(first.submitted);
    let session = first.session.clone().unwrap();
    fake_codex::wait("the conversation id and the generated package", || {
        let saved = h.app.plugin_jobs.get(&job.id).unwrap();
        saved.conversation.as_deref() == Some("native-author-thread")
            && folder.join("wtm-plugin.toml").exists()
    });

    // Returning to a running conversation reuses it.
    let again = plugin_authoring::open(&h.window, &h.app, &job.id, false).unwrap();
    assert_eq!(again.session.as_deref(), Some(session.as_str()));
    assert_eq!(h.app.live_agents().len(), 1);

    // Returning after it closed resumes the same conversation, and sends nothing.
    h.app.close_agent(&session);
    let resumed = plugin_authoring::open(&h.window, &h.app, &job.id, false).unwrap();
    let resumed = resumed.session.unwrap();
    assert_ne!(resumed, session);
    fake_codex::wait("the resumed conversation to be ready", || {
        h.app
            .overview_of(&resumed)
            .is_some_and(|s| s.status == AgentStatus::Idle)
    });
    assert_eq!(
        h.app.agent_fork_source(&resumed).unwrap().provider_session,
        "native-author-thread"
    );
    // A replayed brief would have been written by the time the thread opened; the grace is for
    // the fixture to log what it read.
    std::thread::sleep(std::time::Duration::from_millis(500));
    assert_eq!(fake_codex::Harness::turns_sent(&folder), 1);
    // Every open puts the conversation on screen, the resumed one included.
    assert_eq!(h.window.announced.lock().len(), 3);
    h.app.close_agent(&resumed);
}

#[test]
fn a_failed_turn_is_recorded_on_the_draft_and_cleared_by_the_next_one() {
    let h = fake_codex::Harness::new();
    let folder = h.folder("generated");
    let job = draft(&h, &folder);
    std::fs::write(folder.join(".git/fixture-fail"), "").unwrap();

    let opened = plugin_authoring::open(&h.window, &h.app, &job.id, false).unwrap();
    let session = opened.session.unwrap();
    fake_codex::wait("the failure on the draft", || {
        h.app.plugin_jobs.get(&job.id).unwrap().problem.as_deref()
            == Some("The fixture failed this turn")
    });
    let listed = plugin_authoring::list(&h.app).unwrap();
    assert_eq!(listed[0].status, "error");

    std::fs::remove_file(folder.join(".git/fixture-fail")).unwrap();
    h.app.send_agent_turn(&session, "Try again", &[]).unwrap();
    fake_codex::wait("the next turn to clear the failure", || {
        h.app.plugin_jobs.get(&job.id).unwrap().problem.is_none()
    });
    h.app.close_agent(&session);
}

#[test]
fn review_waits_for_a_busy_agent_and_never_installs_what_it_validates() {
    let h = fake_codex::Harness::new();
    let folder = h.folder("generated");
    let job = draft(&h, &folder);
    std::fs::write(folder.join(".git/fixture-hold"), "").unwrap();

    let session = plugin_authoring::open(&h.window, &h.app, &job.id, false)
        .unwrap()
        .session
        .unwrap();
    fake_codex::wait("the held turn to be under way", || {
        folder.join("wtm-plugin.toml").exists()
            && h.app
                .overview_of(&session)
                .is_some_and(|s| s.status == AgentStatus::Working)
    });
    let refusal = plugin_authoring::review(&h.app, &job.id).unwrap_err();
    assert!(
        refusal.message.contains("Wait for the agent"),
        "{}",
        refusal.message
    );

    h.app.close_agent(&session);
    let review = plugin_authoring::review(&h.app, &job.id).unwrap();
    assert_eq!(review.package.manifest.id, "example.generated-check");
    assert!(h.app.plugins.snapshot().registry.installed.is_empty());
    h.app.plugins.cancel(&review.token).unwrap();

    std::fs::remove_file(folder.join("panel.html")).unwrap();
    assert!(plugin_authoring::review(&h.app, &job.id).is_err());
    // Kept on the draft, which is what **Ask agent to fix** sends.
    assert!(
        h.app
            .plugin_jobs
            .get(&job.id)
            .unwrap()
            .validation_error
            .is_some()
    );
}

#[test]
fn deleting_a_draft_waits_for_its_conversation_and_keeps_its_folder() {
    let h = fake_codex::Harness::new();
    let folder = h.folder("generated");
    let job = draft(&h, &folder);
    let session = plugin_authoring::open(&h.window, &h.app, &job.id, false)
        .unwrap()
        .session
        .unwrap();
    fake_codex::wait("the agent to write its package", || {
        folder.join("wtm-plugin.toml").exists()
    });

    let refusal = plugin_authoring::delete(&h.app, &job.id).unwrap_err();
    assert!(refusal.message.contains("Close"), "{}", refusal.message);
    assert_eq!(h.app.plugin_jobs.list().unwrap().len(), 1);

    h.app.close_agent(&session);
    plugin_authoring::delete(&h.app, &job.id).unwrap();
    assert!(h.app.plugin_jobs.list().unwrap().is_empty());
    // The user's files, and their place in the sidebar, are theirs.
    assert!(folder.join(".git").is_dir());
    assert!(folder.join("wtm-plugin.toml").exists());
    assert!(h.app.config.projects().unwrap().contains(&folder));
    let restarted = App::with_paths(AppPaths::rooted(h.config.path())).unwrap();
    assert!(restarted.plugin_jobs.list().unwrap().is_empty());
}

#[test]
fn turning_chat_off_closes_chat_panes_but_not_the_engine_recipes_and_handoff_run_on() {
    let h = fake_codex::Harness::new();
    let folder = h.folder("generated");
    let job = draft(&h, &folder);
    h.app.plugins.set_enabled("wtm.home", false).unwrap();
    h.app.plugins.experience("wtm.native-cli").unwrap();
    h.app.plugins.set_enabled("wtm.chat", false).unwrap();

    // The routes a person uses to open a chat are refused...
    let worktree = SessionScope::worktree(&job.folder, &job.folder);
    assert_eq!(h.app.chat_pane_refusal(&worktree), Some(CHAT_DISABLED));
    assert_eq!(
        h.app.chat_pane_refusal(&SessionScope::Home),
        Some(HOME_DISABLED)
    );
    let refusal = plugin_authoring::open(&h.window, &h.app, &job.id, false).unwrap_err();
    assert!(
        refusal.message.contains("Enable WTM Chat"),
        "{}",
        refusal.message
    );
    assert!(h.app.live_agents().is_empty());

    // ...and the session a recipe step or an `ask_agent` call opens is not. Before, this was
    // refused too, so "Native CLI, with WTM Chat off" could run no recipe and delegate nothing.
    let entry = wtm_agent::entry("codex").unwrap();
    let request = wtm_agent::SessionRequest {
        cwd: job.folder.clone(),
        ..wtm_agent::SessionRequest::default()
    };
    let sink = wtm_app_lib::agent_bridge::Window::sink(&h.window);
    let session = h.app.open_agent(entry, &request, worktree, &sink).unwrap();
    fake_codex::wait("the engine's session to be ready", || {
        h.app.agent_fork_source(session.as_str()).is_some()
    });
    h.app.close_agent(session.as_str());
}
