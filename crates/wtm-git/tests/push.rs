//! Pushing a local fix, against a real `git` and a bare repository standing in for the forge.
//!
//! What matters is what the outbox relies on: a push reaches exactly the branch its refspec names;
//! a push that is not a fast-forward is refused by the remote and changes nothing, with no way to
//! ask for force; the remote branch can be read back before and after; and a `pre-push` hook planted
//! in the repository does not run, which a plain `git push` from the same checkout proves it would.

#![allow(clippy::unwrap_used)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use wtm_core::error::GitError;
use wtm_exec::Runner;
use wtm_git::push::{self, DiffStyle, Pushed};
use wtm_testkit::GitFixture;

fn runner() -> Runner {
    Runner::with_probed_path(None)
}

/// A bare repository beside the fixture, with the fixture's `main` in it, as `origin`.
fn forge(fixture: &GitFixture) -> PathBuf {
    let bare = fixture.parent().join("forge.git");
    fixture.git_in(fixture.parent(), &["init", "-q", "--bare", "forge.git"]);
    fixture.git(&["remote", "set-url", "origin", &bare.display().to_string()]);
    fixture.git(&["push", "-q", "origin", "main"]);
    bare
}

fn head(fixture: &GitFixture) -> String {
    fixture.git(&["rev-parse", "HEAD"]).trim().to_owned()
}

fn remote(root: &Path, reference: &str) -> Option<String> {
    push::remote_head(&runner(), root, "origin", reference)
        .unwrap()
        .map(|c| c.as_str().to_owned())
}

#[test]
fn a_fix_is_pushed_to_exactly_the_branch_its_refspec_names_and_read_back() {
    let fixture = GitFixture::new();
    forge(&fixture);
    fixture.commit("src/export.rs", "fn export() {}\n", "Fix the export");
    let fix = head(&fixture);
    assert_eq!(remote(fixture.root(), "refs/heads/wtm/pr-7-fix"), None);

    let pushed = push::push(
        &runner(),
        fixture.root(),
        "origin",
        &format!("{fix}:refs/heads/wtm/pr-7-fix"),
    )
    .unwrap();
    assert_eq!(pushed, Pushed::Updated);
    assert_eq!(
        remote(fixture.root(), "refs/heads/wtm/pr-7-fix").as_deref(),
        Some(fix.as_str())
    );
    assert_ne!(
        remote(fixture.root(), "refs/heads/main").as_deref(),
        Some(fix.as_str()),
        "nothing else moved"
    );
    assert_eq!(
        push::push(
            &runner(),
            fixture.root(),
            "origin",
            &format!("{fix}:refs/heads/wtm/pr-7-fix")
        )
        .unwrap(),
        Pushed::UpToDate
    );
}

#[test]
fn a_push_that_is_not_a_fast_forward_is_refused_by_the_remote_and_changes_nothing() {
    let fixture = GitFixture::new();
    forge(&fixture);
    let base = head(&fixture);
    // The author pushes on top of the base meanwhile.
    fixture.commit("src/theirs.rs", "theirs\n", "Their new commit");
    let theirs = head(&fixture);
    fixture.git(&["push", "-q", "origin", "HEAD:refs/heads/feature"]);
    // Your fix was made from the base, before their push.
    fixture.git(&["reset", "-q", "--hard", &base]);
    fixture.commit("src/export.rs", "fn export() {}\n", "Fix the export");
    let fix = head(&fixture);

    let refused = push::push(
        &runner(),
        fixture.root(),
        "origin",
        &format!("{fix}:refs/heads/feature"),
    )
    .unwrap();
    assert!(
        matches!(&refused, Pushed::Rejected { reason } if reason.contains("non-fast-forward")),
        "{refused:?}"
    );
    assert_eq!(
        remote(fixture.root(), "refs/heads/feature").as_deref(),
        Some(theirs.as_str())
    );
}

#[test]
fn a_refspec_that_could_force_or_push_a_moving_ref_is_refused_before_git_runs() {
    let fixture = GitFixture::new();
    forge(&fixture);
    let fix = head(&fixture);
    for refspec in [
        format!("+{fix}:refs/heads/feature"),
        "HEAD:refs/heads/feature".to_owned(),
        format!("{fix}:refs/heads/*"),
    ] {
        assert!(
            matches!(
                push::push(&runner(), fixture.root(), "origin", &refspec),
                Err(GitError::BadRef(_))
            ),
            "{refspec}"
        );
    }
    assert_eq!(remote(fixture.root(), "refs/heads/feature"), None);
}

#[test]
fn a_planted_pre_push_hook_runs_on_a_plain_push_and_not_on_the_apps() {
    let fixture = GitFixture::new();
    forge(&fixture);
    let marker = fixture.parent().join("pre-push-ran");
    fixture.write(
        ".githooks/pre-push",
        &format!("#!/bin/sh\ntouch '{}'\nexit 1\n", marker.display()),
    );
    std::fs::set_permissions(
        fixture.root().join(".githooks/pre-push"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    fixture.git(&["add", "."]);
    fixture.git(&["commit", "-q", "-m", "Ship a hook"]);
    fixture.git(&["config", "core.hooksPath", ".githooks"]);
    let fix = head(&fixture);

    let (code, _) = fixture.git_try(&["push", "origin", &format!("{fix}:refs/heads/plain")]);
    assert_ne!(code, 0, "the planted hook should stop a plain push");
    assert!(
        marker.exists(),
        "the test is only worth something if the hook can run"
    );
    std::fs::remove_file(&marker).unwrap();

    let pushed = push::push(
        &runner(),
        fixture.root(),
        "origin",
        &format!("{fix}:refs/heads/apps"),
    )
    .unwrap();
    assert_eq!(pushed, Pushed::Updated);
    assert!(
        !marker.exists(),
        "the app's push must not run the repository's hooks"
    );
}

#[test]
fn the_fixs_diff_and_its_files_are_read_between_two_commits() {
    let fixture = GitFixture::new();
    fixture.commit("src/export.rs", "a\nb\nc\n", "Theirs");
    let base = head(&fixture);
    fixture.commit("src/export.rs", "a\nB\nc\n", "Fix");
    let fix = head(&fixture);

    let text = push::diff(
        &runner(),
        fixture.root(),
        &base,
        &fix,
        DiffStyle::Suggestions,
    )
    .unwrap();
    assert!(text.contains("@@ -2 +2 @@"), "{text}");
    assert!(
        push::diff(&runner(), fixture.root(), &base, &fix, DiffStyle::Binary)
            .unwrap()
            .contains("-b\n+B")
    );
    assert_eq!(
        push::numstat(&runner(), fixture.root(), &base, &fix).unwrap(),
        [("src/export.rs".to_owned(), Some(1), Some(1))]
    );
    assert_eq!(
        push::line_at(&runner(), fixture.root(), &base, "src/export.rs", 2)
            .unwrap()
            .as_deref(),
        Some("b")
    );
    assert_eq!(
        push::line_at(&runner(), fixture.root(), &base, "nope.rs", 1).unwrap(),
        None
    );
}
