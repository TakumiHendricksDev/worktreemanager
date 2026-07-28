//! `GitCli` against a real `git` binary.
//!
//! The unit tests in `cli.rs` prove the adapter builds the right argv. These prove
//! that the argv actually does what we think, and that the parsers handle what git
//! really prints — including the awkward shapes that exist in the reference repo and
//! are reconstructed here on purpose.
//!
//! Everything runs through the production [`Runner`], so these also exercise the
//! real timeout, `PATH` resolution and process-group handling end to end.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use wtm_core::model::{BranchRef, Checkout, TrackMode};
use wtm_core::ports::exec::CommandRunner;
use wtm_core::ports::git::{AddOptions, BranchFilter, Git};
use wtm_exec::Runner;
use wtm_git::GitCli;
use wtm_testkit::GitFixture;

fn git_cli() -> GitCli {
    GitCli::new(Arc::new(Runner::with_probed_path(None)) as Arc<dyn CommandRunner>)
}

#[test]
fn lists_the_main_worktree_first() {
    let fixture = GitFixture::new();
    let git = git_cli();

    let worktrees = git.list_worktrees(fixture.root()).unwrap();
    assert_eq!(worktrees.len(), 1);
    assert!(
        worktrees[0].is_main,
        "git documents the main worktree as the first record"
    );
    assert_eq!(worktrees[0].branch().map(BranchRef::as_str), Some("main"));
    assert!(worktrees[0].head.is_some());
}

#[test]
fn lists_multiple_worktrees_with_the_main_one_still_first() {
    let fixture = GitFixture::new();
    fixture.add_worktree("ACME-1-alpha", "task/ACME-1-alpha");
    fixture.add_worktree("ACME-2-beta", "bug/ACME-2-beta");
    let git = git_cli();

    let worktrees = git.list_worktrees(fixture.root()).unwrap();
    assert_eq!(worktrees.len(), 3);
    assert!(worktrees[0].is_main);
    assert!(worktrees[1..].iter().all(|w| !w.is_main));

    let branches: Vec<&str> = worktrees
        .iter()
        .filter_map(|w| w.branch().map(BranchRef::as_str))
        .collect();
    assert!(branches.contains(&"task/ACME-1-alpha"));
    assert!(branches.contains(&"bug/ACME-2-beta"));
}

/// The single most important real-world case: never infer a branch from a directory.
#[test]
fn reports_the_actual_branch_when_it_disagrees_with_the_directory_name() {
    let fixture = GitFixture::new();
    fixture.add_worktree(
        "ACME-4567-move-account-settings",
        "experiment/ACME-0000-something-else",
    );
    let git = git_cli();

    let odd = git
        .list_worktrees(fixture.root())
        .unwrap()
        .into_iter()
        .find(|w| w.dirname() == "ACME-4567-move-account-settings")
        .expect("worktree should be listed");

    assert_eq!(
        odd.branch().map(BranchRef::as_str),
        Some("experiment/ACME-0000-something-else"),
        "the branch must come from git, not from the directory name"
    );
}

#[test]
fn a_detached_worktree_reports_no_branch() {
    let fixture = GitFixture::new();
    fixture.add_detached_worktree("detached-one");
    let git = git_cli();

    let detached = git
        .list_worktrees(fixture.root())
        .unwrap()
        .into_iter()
        .find(|w| w.dirname() == "detached-one")
        .expect("worktree should be listed");

    assert_eq!(detached.checkout, Checkout::Detached);
    assert!(detached.branch().is_none());
    assert!(detached.head.is_some(), "detached still has a HEAD commit");
}

#[test]
fn handles_a_worktree_whose_path_contains_spaces() {
    // Exactly what the human-readable `git worktree list` cannot express.
    let fixture = GitFixture::new();
    fixture.add_worktree("dir with spaces", "task/spaces");
    let git = git_cli();

    let found = git
        .list_worktrees(fixture.root())
        .unwrap()
        .into_iter()
        .find(|w| w.dirname() == "dir with spaces");
    assert!(found.is_some(), "a path with spaces must round-trip");
    assert_eq!(
        found.unwrap().branch().map(BranchRef::as_str),
        Some("task/spaces")
    );
}

#[test]
fn a_deleted_worktree_directory_is_reported_prunable_then_pruned_away() {
    let fixture = GitFixture::new();
    let path = fixture.add_worktree("doomed", "task/doomed");
    fixture.orphan_worktree(&path);
    let git = git_cli();

    let before = git.list_worktrees(fixture.root()).unwrap();
    let doomed = before
        .iter()
        .find(|w| w.dirname() == "doomed")
        .expect("still listed");
    assert!(
        doomed.prunable.is_some(),
        "git keeps the admin entry until pruned"
    );

    git.prune_worktrees(fixture.root()).unwrap();

    let after = git.list_worktrees(fixture.root()).unwrap();
    assert!(
        after.iter().all(|w| w.dirname() != "doomed"),
        "prune should drop the stale entry so it cannot pollute pickers"
    );
}

#[test]
fn repo_root_and_git_common_dir_are_stable_from_inside_a_linked_worktree() {
    // The reason `git_common_dir` exists: inside a linked worktree `.git` is a file
    // and `--git-dir` points at a per-worktree subdirectory, so config keyed on it
    // would silently differ depending on which worktree the app was opened from.
    let fixture = GitFixture::new();
    let linked = fixture.add_worktree("ACME-3-gamma", "task/ACME-3-gamma");
    let git = git_cli();

    let common_from_main = git.git_common_dir(fixture.root()).unwrap();
    let common_from_linked = git.git_common_dir(&linked).unwrap();
    assert_eq!(
        std::fs::canonicalize(&common_from_main).unwrap(),
        std::fs::canonicalize(&common_from_linked).unwrap(),
        "every worktree must resolve to the same common dir"
    );
    assert!(common_from_main.is_absolute());

    // And the linked worktree's own root is itself, not the main checkout.
    assert_eq!(
        std::fs::canonicalize(git.repo_root(&linked).unwrap()).unwrap(),
        std::fs::canonicalize(&linked).unwrap()
    );
}

#[test]
fn branches_separates_local_from_remote_and_dedupes() {
    let fixture = GitFixture::new();
    fixture.branch("develop");
    fixture.add_remote_ref("develop", "HEAD");
    fixture.add_remote_ref("release", "HEAD");
    let git = git_cli();

    let local: Vec<String> = git
        .branches(fixture.root(), BranchFilter::Local)
        .unwrap()
        .iter()
        .map(|b| b.as_str().to_owned())
        .collect();
    assert!(local.contains(&"main".to_owned()));
    assert!(local.contains(&"develop".to_owned()));
    assert!(
        !local.contains(&"release".to_owned()),
        "release exists only on the remote"
    );

    let both: Vec<String> = git
        .branches(fixture.root(), BranchFilter::Both)
        .unwrap()
        .iter()
        .map(|b| b.as_str().to_owned())
        .collect();
    assert!(both.contains(&"release".to_owned()));
    assert_eq!(
        both.iter().filter(|b| *b == "develop").count(),
        1,
        "develop exists locally and remotely; it must be offered once"
    );
}

#[test]
fn rev_parse_resolves_a_real_ref_and_declines_a_missing_one() {
    let fixture = GitFixture::new();
    let git = git_cli();

    let head = git.rev_parse(fixture.root(), "HEAD").unwrap();
    assert!(head.is_some());
    assert_eq!(head.unwrap().as_str().len(), 40, "expected a full sha");

    assert_eq!(
        git.rev_parse(fixture.root(), "origin/does-not-exist")
            .unwrap(),
        None,
        "a missing ref is an expected answer during planning, not an error"
    );
}

#[test]
fn rev_parse_peels_an_annotated_tag_to_its_commit() {
    let fixture = GitFixture::new();
    fixture.git(&["tag", "-a", "v1.0", "-m", "release"]);
    let git = git_cli();

    let commit = git
        .rev_parse(fixture.root(), "v1.0")
        .unwrap()
        .expect("tag should resolve");
    let head = fixture.git(&["rev-parse", "HEAD"]);
    assert_eq!(
        commit.as_str(),
        head.trim(),
        "must peel to the commit, not the tag object"
    );
}

#[test]
fn status_distinguishes_staged_dirty_and_untracked() {
    let fixture = GitFixture::new();
    let git = git_cli();

    assert!(git.status(fixture.root()).unwrap().is_clean());

    fixture.write("untracked.txt", "new\n");
    let status = git.status(fixture.root()).unwrap();
    assert_eq!(status.untracked, 1);
    assert!(
        !status.dirty_tracked,
        "an untracked file is not a tracked modification"
    );
    assert!(!status.is_clean());

    fixture.write("README.md", "# changed\n");
    let status = git.status(fixture.root()).unwrap();
    assert!(
        status.dirty_tracked,
        "a modified tracked file must set dirty_tracked"
    );

    fixture.git(&["add", "README.md"]);
    let status = git.status(fixture.root()).unwrap();
    assert_eq!(status.staged, 1);
}

#[test]
fn ahead_behind_reports_divergence_in_the_right_direction() {
    let fixture = GitFixture::new();
    // A base with one extra commit, and a branch with two of its own.
    fixture.git(&["checkout", "-b", "feature"]);
    fixture.commit("a.txt", "a\n", "feature 1");
    fixture.commit("b.txt", "b\n", "feature 2");
    fixture.git(&["checkout", "main"]);
    fixture.commit("c.txt", "c\n", "main 1");

    let git = git_cli();
    let (ahead, behind) = git
        .ahead_behind(fixture.root(), &BranchRef::new("feature"), "main")
        .unwrap();
    assert_eq!(
        (ahead, behind),
        (2, 1),
        "feature is 2 ahead of and 1 behind main"
    );
}

#[test]
fn add_worktree_creates_a_new_branch_without_an_upstream() {
    // `--no-track` is deliberate: a branch cut from a shared integration branch must
    // not inherit it as upstream, or a reflexive `git push` targets that branch.
    let fixture = GitFixture::new();
    fixture.add_remote_ref("develop", "HEAD");
    let git = git_cli();

    let target = fixture.parent().join("ACME-9-delta");
    let created = git
        .add_worktree(
            fixture.root(),
            &AddOptions {
                path: target.clone(),
                branch: Some(BranchRef::new("task/ACME-9-delta")),
                start_point: "origin/develop".to_owned(),
                track: TrackMode::NoTrack,
                create_branch: true,
            },
        )
        .unwrap();

    assert_eq!(
        created.branch().map(BranchRef::as_str),
        Some("task/ACME-9-delta")
    );
    assert!(target.join(".git").exists());
    assert!(!created.is_main);

    let (code, upstream) =
        fixture.git_try(&["rev-parse", "--abbrev-ref", "task/ACME-9-delta@{upstream}"]);
    assert_ne!(
        code, 0,
        "--no-track must leave no upstream, got {upstream:?}"
    );
}

#[test]
fn add_worktree_can_adopt_a_remote_only_branch_with_tracking() {
    let fixture = GitFixture::new();
    fixture.add_remote_ref("feature-x", "HEAD");
    let git = git_cli();

    let target = fixture.parent().join("feature-x");
    let created = git
        .add_worktree(
            fixture.root(),
            &AddOptions {
                path: target.clone(),
                branch: Some(BranchRef::new("feature-x")),
                start_point: "origin/feature-x".to_owned(),
                track: TrackMode::Track,
                create_branch: true,
            },
        )
        .unwrap();

    assert_eq!(created.branch().map(BranchRef::as_str), Some("feature-x"));
    let upstream = fixture.git(&["rev-parse", "--abbrev-ref", "feature-x@{upstream}"]);
    assert_eq!(
        upstream.trim(),
        "origin/feature-x",
        "adoption should set the upstream"
    );
}

#[test]
fn add_worktree_can_check_out_an_existing_local_branch() {
    let fixture = GitFixture::new();
    fixture.branch("existing");
    let git = git_cli();

    let target = fixture.parent().join("existing");
    let created = git
        .add_worktree(
            fixture.root(),
            &AddOptions {
                path: target,
                branch: Some(BranchRef::new("existing")),
                start_point: "existing".to_owned(),
                track: TrackMode::Detach,
                create_branch: false,
            },
        )
        .unwrap();
    assert_eq!(created.branch().map(BranchRef::as_str), Some("existing"));
}

#[test]
fn add_worktree_can_create_a_detached_worktree() {
    let fixture = GitFixture::new();
    let git = git_cli();

    let created = git
        .add_worktree(
            fixture.root(),
            &AddOptions {
                path: fixture.parent().join("scratch"),
                branch: None,
                start_point: "HEAD".to_owned(),
                track: TrackMode::Detach,
                create_branch: false,
            },
        )
        .unwrap();
    assert_eq!(created.checkout, Checkout::Detached);
}

#[test]
fn add_worktree_fails_when_the_branch_is_already_checked_out_elsewhere() {
    // git refuses this outright, which is why preflight checks for it before
    // anything is created rather than letting stage 8 blow up.
    let fixture = GitFixture::new();
    fixture.add_worktree("first", "task/shared");
    let git = git_cli();

    let err = git
        .add_worktree(
            fixture.root(),
            &AddOptions {
                path: fixture.parent().join("second"),
                branch: Some(BranchRef::new("task/shared")),
                start_point: "task/shared".to_owned(),
                track: TrackMode::Detach,
                create_branch: false,
            },
        )
        .unwrap_err();
    let message = err.to_string();
    assert!(
        message.contains("already") || message.contains("used by worktree"),
        "expected git's already-checked-out message, got: {message}"
    );
}

#[test]
fn add_worktree_fails_on_a_non_empty_existing_directory() {
    let fixture = GitFixture::new();
    let occupied = fixture.parent().join("occupied");
    std::fs::create_dir_all(&occupied).unwrap();
    std::fs::write(occupied.join("file.txt"), "x").unwrap();
    let git = git_cli();

    assert!(
        git.add_worktree(
            fixture.root(),
            &AddOptions {
                path: occupied,
                branch: Some(BranchRef::new("task/occupied")),
                start_point: "main".to_owned(),
                track: TrackMode::NoTrack,
                create_branch: true,
            },
        )
        .is_err(),
        "a populated target must be rejected"
    );
}

#[test]
fn remove_worktree_then_delete_branch() {
    let fixture = GitFixture::new();
    let path = fixture.add_worktree("ACME-4-epsilon", "task/ACME-4-epsilon");
    let git = git_cli();
    let branch = BranchRef::new("task/ACME-4-epsilon");

    git.remove_worktree(fixture.root(), &path, false).unwrap();
    assert!(!path.exists(), "the directory should be gone");
    assert!(
        git.list_worktrees(fixture.root())
            .unwrap()
            .iter()
            .all(|w| w.path != path),
        "and it should be unlisted"
    );

    // The branch survives removal until explicitly deleted — the app turns that into
    // a checkbox rather than a stdin prompt.
    assert!(
        git.branches(fixture.root(), BranchFilter::Local)
            .unwrap()
            .contains(&branch)
    );
    git.delete_branch(fixture.root(), &branch, true).unwrap();
    assert!(
        !git.branches(fixture.root(), BranchFilter::Local)
            .unwrap()
            .contains(&branch)
    );
}

#[test]
fn remove_worktree_refuses_a_dirty_worktree_unless_forced() {
    let fixture = GitFixture::new();
    let path = fixture.add_worktree("ACME-5-zeta", "task/ACME-5-zeta");
    std::fs::write(path.join("README.md"), "# modified\n").unwrap();
    let git = git_cli();

    assert!(
        git.remove_worktree(fixture.root(), &path, false).is_err(),
        "git protects unsaved work"
    );
    assert!(path.exists());

    git.remove_worktree(fixture.root(), &path, true).unwrap();
    assert!(!path.exists(), "force should override");
}

#[test]
fn delete_branch_without_force_refuses_unmerged_work() {
    let fixture = GitFixture::new();
    let path = fixture.add_worktree("ACME-6-eta", "task/ACME-6-eta");
    fixture.git_in(&path, &["commit", "--allow-empty", "-m", "unique work"]);
    fixture.git(&["worktree", "remove", &path.to_string_lossy()]);

    let git = git_cli();
    let branch = BranchRef::new("task/ACME-6-eta");

    assert!(
        !git.is_merged(fixture.root(), &branch, "main").unwrap(),
        "it has unique commits"
    );
    assert!(
        git.delete_branch(fixture.root(), &branch, false).is_err(),
        "-d must refuse unmerged work"
    );
    git.delete_branch(fixture.root(), &branch, true).unwrap();
}

#[test]
fn is_merged_is_true_for_a_branch_with_no_unique_commits() {
    let fixture = GitFixture::new();
    fixture.branch("no-work");
    let git = git_cli();
    assert!(
        git.is_merged(fixture.root(), &BranchRef::new("no-work"), "main")
            .unwrap()
    );
}

#[test]
fn repo_root_rejects_a_directory_that_is_not_a_repository() {
    let dir = tempfile::tempdir().unwrap();
    let git = git_cli();
    assert!(git.repo_root(dir.path()).is_err());
}
