//! The app's checkpoint commit, against a real `git`, in a worktree a writer has been loose in.
//!
//! The property that matters is the one F2's hooks demo found broken: a hook planted inside the
//! worktree must not run when the app commits. The test plants one the way an agent would — a
//! repository pointing `core.hooksPath` at an in-tree directory, and a `pre-commit` there that both
//! leaves a mark and fails — and proves a plain `git commit` runs it before proving the checkpoint
//! does not. Without the first half the second would pass on a machine where hooks never run.

#![allow(clippy::unwrap_used)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use wtm_exec::Runner;
use wtm_git::checkpoint::{self, CheckpointError};
use wtm_testkit::GitFixture;

fn runner() -> Runner {
    Runner::with_probed_path(None)
}

fn refuse(path: &str) -> Option<&'static str> {
    path.starts_with(".githooks/").then_some(".githooks/")
}

/// A worktree whose repository runs hooks from `.githooks`, with a `pre-commit` that marks
/// `marker` and fails. The hook is committed, as it would be in a repository that ships one.
fn hooked(fixture: &GitFixture, marker: &Path) -> std::path::PathBuf {
    fixture.write(
        ".githooks/pre-commit",
        &format!("#!/bin/sh\ntouch '{}'\nexit 1\n", marker.display()),
    );
    let hook = fixture.root().join(".githooks/pre-commit");
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    fixture.git(&["add", "."]);
    fixture.git(&["-c", "core.hooksPath=", "commit", "-m", "ship a hook"]);
    fixture.git(&["config", "core.hooksPath", ".githooks"]);
    fixture.add_worktree("acme-wt", "task/ACME-1")
}

#[test]
fn a_planted_hook_runs_on_an_ordinary_commit_and_not_on_a_checkpoint() {
    let fixture = GitFixture::new();
    let marker = fixture.parent().join("hook-ran");
    let worktree = hooked(&fixture, &marker);

    std::fs::write(worktree.join("feature.txt"), "one\n").unwrap();
    fixture.git_in(&worktree, &["add", "feature.txt"]);
    let ordinary = ordinary_commit(&worktree);
    assert!(
        !ordinary.status.success(),
        "the planted hook should fail an ordinary commit"
    );
    assert!(
        marker.exists(),
        "the test is only worth something if the hook can run"
    );
    std::fs::remove_file(&marker).unwrap();

    let made = checkpoint::commit(&runner(), &worktree, "checkpoint: feature", |_| None).unwrap();
    assert!(
        !marker.exists(),
        "a checkpoint must not run the worktree's hooks"
    );
    assert_eq!(made.paths, ["feature.txt"]);
    let head = fixture.git_in(&worktree, &["rev-parse", "HEAD"]);
    assert_eq!(made.commit.as_str(), head.trim());
    let subject = fixture.git_in(&worktree, &["log", "-1", "--format=%s"]);
    assert_eq!(subject.trim(), "checkpoint: feature");
}

/// `git commit` as anything else would run it, so the hook gets its chance.
fn ordinary_commit(worktree: &Path) -> std::process::Output {
    // A test is a sanctioned place to spawn directly, as `GitFixture` itself does: the point is a
    // commit that does not go through the code under test.
    #[allow(clippy::disallowed_methods)]
    std::process::Command::new("git")
        .args(["commit", "-m", "ordinary"])
        .current_dir(worktree)
        .output()
        .unwrap()
}

#[test]
fn a_change_to_a_protected_path_refuses_the_whole_checkpoint_and_stages_nothing() {
    let fixture = GitFixture::new();
    let marker = fixture.parent().join("hook-ran");
    let worktree = hooked(&fixture, &marker);

    std::fs::write(worktree.join("feature.txt"), "one\n").unwrap();
    std::fs::write(worktree.join(".githooks/pre-commit"), "#!/bin/sh\nexit 0\n").unwrap();

    let refused = checkpoint::commit(&runner(), &worktree, "checkpoint", refuse);
    assert!(
        matches!(&refused, Err(CheckpointError::Protected { path, .. }) if path == ".githooks/pre-commit"),
        "{refused:?}"
    );
    let staged = fixture.git_in(&worktree, &["diff", "--cached", "--name-only"]);
    assert_eq!(staged, "", "a refusal leaves the index as it was");
}

#[test]
fn a_renamed_protected_file_is_refused_by_its_old_name_too() {
    let fixture = GitFixture::new();
    let marker = fixture.parent().join("hook-ran");
    let worktree = hooked(&fixture, &marker);
    fixture.git_in(&worktree, &["mv", ".githooks/pre-commit", "harmless.sh"]);

    let refused = checkpoint::commit(&runner(), &worktree, "checkpoint", refuse);
    assert!(
        matches!(refused, Err(CheckpointError::Protected { .. })),
        "{refused:?}"
    );
}

#[test]
fn a_checkpoint_with_nothing_changed_says_so_rather_than_making_an_empty_commit() {
    let fixture = GitFixture::new();
    let refused = checkpoint::commit(&runner(), fixture.root(), "checkpoint", |_| None);
    assert!(
        matches!(refused, Err(CheckpointError::NothingToCommit)),
        "{refused:?}"
    );
}

#[test]
fn a_linked_worktrees_git_dirs_are_its_common_dir_its_gitdir_and_its_pointer_file() {
    let fixture = GitFixture::new();
    let worktree = fixture.add_worktree("acme-wt", "task/ACME-1");
    let dirs = checkpoint::git_dirs(&runner(), &worktree).unwrap();

    let common = fixture.root().join(".git").canonicalize().unwrap();
    let canonical: Vec<_> = dirs
        .iter()
        .map(|dir| dir.canonicalize().unwrap_or_else(|_| dir.clone()))
        .collect();
    assert_eq!(canonical.len(), 3, "{dirs:?}");
    assert_eq!(canonical[0], common);
    assert_eq!(canonical[1], common.join("worktrees/acme-wt"));
    assert_eq!(dirs[2], worktree.join(".git"));
    assert!(dirs[2].is_file(), "a linked worktree's `.git` is a file");
}

#[test]
fn a_main_checkouts_git_dirs_name_its_one_directory_once() {
    let fixture = GitFixture::new();
    let dirs = checkpoint::git_dirs(&runner(), fixture.root()).unwrap();
    assert_eq!(dirs.len(), 1, "{dirs:?}");
    assert_eq!(
        dirs[0].canonicalize().unwrap(),
        fixture.root().join(".git").canonicalize().unwrap()
    );
}
