//! The step editor's skill list comes from each CLI's own answer, not only from disk.
//!
//! A plugin's skills live in no directory wtm reads, so a recipe editor that listed only the disk
//! seed could never offer one — which is how a plugin's `/implement` went missing from it. The
//! probe opens a session the way a pane does, takes the list the CLI reports without a turn, and
//! closes it. What only a real binary can prove is that the second list really arrives, and that it
//! is the CLI's and not the seed again: the seed has no built-in commands, so a built-in in the
//! answer is the CLI speaking.
//!
//! # No API credit is spent
//!
//! No message is sent. Claude answers `initialize` and Codex `skills/list` before any turn.
//!
//! Skips loudly for a CLI that is not installed, for the reason `agent_capability.rs` gives.

#![allow(clippy::unwrap_used, clippy::print_stderr)]

use std::sync::Arc;

use wtm_app_lib::app::App;

fn app() -> (tempfile::TempDir, Arc<App>) {
    let dir = tempfile::tempdir().unwrap();
    let app = Arc::new(App::with_paths(wtm_config::AppPaths::rooted(dir.path())).unwrap());
    (dir, app)
}

fn installed(app: &App, provider: &str) -> bool {
    let entry = wtm_agent::entry(provider).unwrap();
    if app.agent_executable(entry).is_none() {
        eprintln!("skipping: wtm cannot find {}'s CLI here", entry.label);
        return false;
    }
    true
}

#[test]
fn claude_lists_a_projects_skill_and_its_own_built_ins_without_a_turn() {
    let (_dir, app) = app();
    if !installed(&app, "claude") {
        return;
    }
    let project = tempfile::tempdir().unwrap();
    let skill = project.path().join(".claude/skills/fixture-probe-skill");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: fixture-probe-skill\ndescription: A skill only this test declares\n---\nSay hi.\n",
    )
    .unwrap();

    let listed = wtm_app_lib::recipes::probe_skills(&app, "claude", project.path())
        .unwrap()
        .expect("Claude answered initialize with its commands");
    let names: Vec<&str> = listed.iter().map(|s| s.name.as_str()).collect();
    assert!(
        names.contains(&"fixture-probe-skill"),
        "the project's own skill is in the CLI's list: {names:?}"
    );
    assert!(
        listed
            .iter()
            .any(|s| s.scope.as_deref() == Some("built-in")),
        "a built-in means this is the CLI's answer, not the disk seed again: {names:?}"
    );
}

#[test]
fn codex_lists_its_skills_without_a_turn() {
    let (_dir, app) = app();
    if !installed(&app, "codex") {
        return;
    }
    let project = tempfile::tempdir().unwrap();
    let listed = wtm_app_lib::recipes::probe_skills(&app, "codex", project.path()).unwrap();
    assert!(
        listed.is_some(),
        "Codex answered skills/list once its thread was open"
    );
}
