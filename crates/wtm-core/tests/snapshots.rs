//! Snapshots against a real `git`.
//!
//! Everything a snapshot promises is a claim about git's own behaviour: that the tree holds the
//! untracked files and not the ignored ones, that the patch rebuilds the tree, that a hook in the
//! repository does not run, and above all that **no ref is written** and the worktree being
//! reviewed is left exactly as it was. A fake can only agree with whatever it was told, so these
//! run the real adapters, through the production runner, against a [`GitFixture`].
//!
//! The adapters run with your own git config, the way the app does, which is the point: a
//! signing setup or a global hook must not turn a snapshot into a prompt or a side effect.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use wtm_config::RealFileStore;
use wtm_core::flow::review::{FindingClass, Location, Severity};
use wtm_core::flow::{
    Finding, FindingId, FlowDir, FlowId, ScratchLabel, Snapshot, TreeCheck, Workspace,
};
use wtm_core::ports::exec::CommandRunner;
use wtm_core::usecase::{SnapshotError, Snapshots};
use wtm_exec::Runner;
use wtm_git::GitCli;
use wtm_testkit::GitFixture;

/// A task worktree with one commit of its own, and a flow directory to snapshot it into.
struct Review {
    fixture: GitFixture,
    worktree: PathBuf,
    data: tempfile::TempDir,
    dir: FlowDir,
    snapshots: Snapshots,
}

impl Review {
    fn new() -> Self {
        let fixture = GitFixture::new();
        fixture.commit(".gitignore", "*.log\n", "ignore logs");
        let worktree = fixture.add_worktree("ACME-1-export", "task/ACME-1-export");

        let review = {
            let data = tempfile::tempdir().unwrap();
            let dir = FlowDir::new(&data.path().join("flow"), FlowId(1));
            let runner: Arc<dyn CommandRunner> = Arc::new(Runner::with_probed_path(None));
            Self {
                fixture,
                worktree,
                data,
                dir,
                snapshots: Snapshots {
                    git: Arc::new(GitCli::new(runner)),
                    files: Arc::new(RealFileStore::new()),
                },
            }
        };
        review.write("app/export.py", "def export():\n    return 1\n");
        review.git(&["add", "app/export.py"]);
        review.git(&["commit", "-m", "Add the export"]);
        review
    }

    fn write(&self, relative: &str, contents: impl AsRef<[u8]>) {
        let path = self.worktree.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    fn git(&self, args: &[&str]) -> String {
        self.fixture.git_in(&self.worktree, args)
    }

    fn label(name: &str) -> ScratchLabel {
        ScratchLabel::new(name).unwrap()
    }

    fn take(&self) -> Snapshot {
        self.snapshots
            .take(&self.worktree, "main", &self.dir, &Self::label("take"))
            .unwrap()
    }

    fn check(&self, snapshot: &Snapshot) -> TreeCheck {
        self.snapshots
            .check(&self.worktree, snapshot, &self.dir, &Self::label("check"))
            .unwrap()
    }

    /// Paths in a tree object, recursively.
    fn tree_paths(&self, tree: &str) -> BTreeSet<String> {
        self.git(&["ls-tree", "-r", "--name-only", tree])
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// Everything a snapshot must not change, as one comparable value.
    fn untouchable_state(&self) -> Vec<String> {
        let index = self.git(&["rev-parse", "--git-path", "index"]);
        let index = self.worktree.join(index.trim());
        vec![
            // Every ref, the stash included, in both repositories' views.
            self.git(&["for-each-ref", "--format=%(refname) %(objectname)"]),
            self.fixture
                .git(&["for-each-ref", "--format=%(refname) %(objectname)"]),
            self.git(&["rev-parse", "HEAD"]),
            self.git(&["symbolic-ref", "HEAD"]),
            self.git(&["reflog", "--format=%H %gs"]),
            self.git(&["stash", "list"]),
            // The worktree's own index, byte for byte, and what is on disk.
            format!("{:?}", std::fs::read(&index).unwrap()),
            self.git(&["status", "--porcelain=v1", "-z", "--untracked-files=all"]),
            self.fixture.git(&["worktree", "list", "--porcelain"]),
        ]
    }

    fn finding(id: i64, path: &str) -> Finding {
        Finding {
            id: FindingId(id),
            flow: FlowId(1),
            class: FindingClass::Suggestion,
            severity: Severity::Minor,
            location: Some(Location {
                path: path.to_owned(),
                start_line: 1,
                end_line: 2,
            }),
            title: "t".to_owned(),
            rationale: "r".to_owned(),
            requirement: None,
            tree_sha: String::new(),
            dismissed: None,
        }
    }
}

#[test]
fn a_snapshot_holds_untracked_and_unstaged_work_and_leaves_ignored_files_out() {
    let review = Review::new();
    review.write("app/export.py", "def export():\n    return 2\n");
    review.write("notes/plan.md", "# untracked\n");
    review.write("debug.log", "ignored\n");
    review.write("app/staged.py", "STAGED = True\n");
    review.git(&["add", "app/staged.py"]);

    let snapshot = review.take();

    let paths = review.tree_paths(&snapshot.tree_sha);
    assert!(paths.contains("notes/plan.md"), "{paths:?}");
    assert!(paths.contains("app/staged.py"), "{paths:?}");
    assert!(!paths.contains("debug.log"), "{paths:?}");
    let export = review.git(&["show", &format!("{}:app/export.py", snapshot.tree_sha)]);
    assert!(export.contains("return 2"), "the unstaged edit: {export}");
}

#[test]
fn a_snapshot_writes_no_ref_and_leaves_the_reviewed_worktree_and_its_index_alone() {
    let review = Review::new();
    review.write("app/export.py", "def export():\n    return 2\n");
    review.write("notes/plan.md", "# untracked\n");
    review.write("app/staged.py", "STAGED = True\n");
    review.git(&["add", "app/staged.py"]);
    let before = review.untouchable_state();

    // The whole life of a review: take, check out, edit nothing, check, close.
    let snapshot = review.take();
    let checkout = review
        .snapshots
        .open_checkout(&review.worktree, &snapshot, &review.dir)
        .unwrap();
    assert_eq!(review.check(&snapshot), TreeCheck::Unchanged);
    review
        .snapshots
        .close_checkout(&review.worktree, &checkout)
        .unwrap();

    assert_eq!(review.untouchable_state(), before);
    // Objects were written, which is the relaxation ARCHITECTURE §5 states, and nothing points
    // at them: the snapshot commit is on no branch, tag or other ref.
    assert_eq!(
        review.git(&["cat-file", "-t", &snapshot.tree_sha]).trim(),
        "tree"
    );
    assert_eq!(
        review.git(&[
            "for-each-ref",
            "--contains",
            checkout.commit.as_str(),
            "--format=%(refname)"
        ]),
        ""
    );
}

#[test]
fn the_patch_rebuilds_the_snapshot_tree_from_the_base_binary_files_included() {
    let review = Review::new();
    review.write("assets/logo.bin", [0_u8, 159, 146, 150, 255, 0, 1]);
    // Latin-1, not UTF-8: a patch captured as text would come back corrupted.
    review.write("legacy/readme.txt", [b'c', b'a', b'f', 0xE9, b'\n']);
    review.write("notes/plan.md", "# untracked\n");

    let snapshot = review.take();
    let patch = review.dir.resolve(&snapshot.patch.path);
    let bytes = std::fs::read(&patch).unwrap();
    assert_eq!(snapshot.patch.bytes, bytes.len() as u64);
    assert_eq!(snapshot.patch.sha256.len(), 64);

    // Apply it where the change started, and the same tree comes out.
    let rebuilt = review.fixture.parent().join("rebuilt");
    review.fixture.git(&[
        "worktree",
        "add",
        "--detach",
        &rebuilt.to_string_lossy(),
        &snapshot.base_sha,
    ]);
    review.fixture.git_in(
        &rebuilt,
        &["apply", "--index", "--binary", &patch.to_string_lossy()],
    );
    assert_eq!(
        review.fixture.git_in(&rebuilt, &["write-tree"]).trim(),
        snapshot.tree_sha
    );
}

#[test]
fn a_snapshot_is_measured_from_where_the_branch_left_its_base() {
    let review = Review::new();
    let fork = review.git(&["merge-base", "HEAD", "main"]);
    review.fixture.commit(
        "main-only.txt",
        "landed after the branch was cut\n",
        "Moves main on",
    );

    let snapshot = review.take();

    assert_eq!(snapshot.base_sha, fork.trim());
    assert_eq!(snapshot.head_sha, review.git(&["rev-parse", "HEAD"]).trim());
    let patch = std::fs::read_to_string(review.dir.resolve(&snapshot.patch.path)).unwrap();
    assert!(patch.contains("app/export.py"), "the branch's own commit");
    assert!(
        !patch.contains("main-only.txt"),
        "main's later commit is not this change"
    );
}

#[test]
fn editing_a_file_during_a_review_flips_its_results_to_stale() {
    let review = Review::new();
    let snapshot = review.take();
    let checkout = review
        .snapshots
        .open_checkout(&review.worktree, &snapshot, &review.dir)
        .unwrap();

    // You keep working while the reviewers read the checkout.
    review.write("app/export.py", "def export():\n    return 3\n");

    let check = review.check(&snapshot);
    assert!(check.is_stale());
    assert_eq!(check.changed_paths(), vec!["app/export.py"]);

    let findings = [
        Review::finding(1, "app/export.py"),
        Review::finding(2, "app/other.py"),
    ];
    assert_eq!(
        check.stale_findings(Workspace::Snapshot, &findings),
        vec![FindingId(1)],
        "a role that read the snapshot is stale only where the code moved"
    );
    assert_eq!(
        check.stale_findings(Workspace::Worktree, &findings),
        vec![FindingId(1), FindingId(2)],
        "a role that read the worktree while it moved is stale throughout"
    );
    // And the checkout the static roles were reading did not move with it.
    let read = std::fs::read_to_string(checkout.path.join("app/export.py")).unwrap();
    assert!(read.contains("return 1"), "{read}");
}

#[test]
fn a_file_created_during_a_review_is_a_change_even_untracked() {
    let review = Review::new();
    let snapshot = review.take();

    review.write("app/new_helper.py", "HELPER = 1\n");

    assert_eq!(
        review.check(&snapshot).changed_paths(),
        vec!["app/new_helper.py"]
    );
}

#[test]
fn staging_what_was_already_on_disk_does_not_make_a_review_stale() {
    // A snapshot records content, not what happened to be staged, so `git add` of an edit the
    // review already read changes nothing it said.
    let review = Review::new();
    review.write("app/export.py", "def export():\n    return 2\n");
    let snapshot = review.take();

    review.git(&["add", "-A"]);

    assert_eq!(review.check(&snapshot), TreeCheck::Unchanged);
}

#[test]
fn a_committed_review_tree_is_still_current_because_the_content_did_not_change() {
    let review = Review::new();
    review.write("app/export.py", "def export():\n    return 2\n");
    let snapshot = review.take();

    review.git(&["commit", "-am", "Return two"]);

    assert_eq!(review.check(&snapshot), TreeCheck::Unchanged);
}

#[test]
fn the_snapshot_checkout_holds_the_snapshot_and_is_gone_once_closed() {
    let review = Review::new();
    review.write("notes/plan.md", "# untracked\n");
    let snapshot = review.take();

    let checkout = review
        .snapshots
        .open_checkout(&review.worktree, &snapshot, &review.dir)
        .unwrap();
    assert!(checkout.path.starts_with(review.data.path()));
    assert_eq!(
        std::fs::read_to_string(checkout.path.join("notes/plan.md")).unwrap(),
        "# untracked\n"
    );
    assert_eq!(
        review
            .fixture
            .git_in(&checkout.path, &["rev-parse", "HEAD^{tree}"])
            .trim(),
        snapshot.tree_sha
    );
    // A static reviewer can still ask git what changed, which is why this is a checkout and not
    // a copy of the files.
    let diff = review.fixture.git_in(
        &checkout.path,
        &["diff", "--name-only", &snapshot.base_sha, "HEAD"],
    );
    assert!(diff.contains("notes/plan.md"), "{diff}");

    review
        .snapshots
        .close_checkout(&review.worktree, &checkout)
        .unwrap();
    assert!(!checkout.path.exists());
    assert!(
        !review
            .fixture
            .git(&["worktree", "list", "--porcelain"])
            .contains(&*checkout.path.to_string_lossy())
    );
}

#[test]
fn closing_a_checkout_somebody_already_deleted_still_clears_it_from_git() {
    let review = Review::new();
    let snapshot = review.take();
    let checkout = review
        .snapshots
        .open_checkout(&review.worktree, &snapshot, &review.dir)
        .unwrap();

    std::fs::remove_dir_all(&checkout.path).unwrap();
    review
        .snapshots
        .close_checkout(&review.worktree, &checkout)
        .unwrap();

    let listing = review.fixture.git(&["worktree", "list", "--porcelain"]);
    assert!(!listing.contains("checkouts"), "{listing}");
}

#[test]
fn a_hook_planted_in_the_repository_does_not_run_when_the_checkout_is_made() {
    let review = Review::new();
    let marker = review.fixture.parent().join("hook-ran");
    review.write(
        ".githooks/post-checkout",
        format!("#!/bin/sh\ntouch '{}'\n", marker.display()),
    );
    let hook = review.worktree.join(".githooks/post-checkout");
    set_executable(&hook);
    review.git(&["config", "core.hooksPath", ".githooks"]);
    let snapshot = review.take();

    review
        .snapshots
        .open_checkout(&review.worktree, &snapshot, &review.dir)
        .unwrap();

    assert!(!marker.exists(), "the repository's post-checkout hook ran");
}

#[test]
fn opening_the_same_snapshot_twice_makes_the_same_commit() {
    let review = Review::new();
    review.write("notes/plan.md", "# untracked\n");
    let snapshot = review.take();

    let first = review
        .snapshots
        .open_checkout(&review.worktree, &snapshot, &review.dir)
        .unwrap();
    // No close in between: the second open finds the first one's checkout, as it would after a
    // crash mid-stage, and replaces it.
    let second = review
        .snapshots
        .open_checkout(&review.worktree, &snapshot, &review.dir)
        .unwrap();

    assert_eq!(first.commit, second.commit);
    assert!(second.path.exists());
}

#[test]
fn no_scratch_index_is_left_behind() {
    let review = Review::new();
    review.write("notes/plan.md", "# untracked\n");
    let snapshot = review.take();
    let _ = review.check(&snapshot);

    let scratch = review.dir.path().join("scratch");
    let left: Vec<PathBuf> = std::fs::read_dir(&scratch)
        .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
        .unwrap_or_default();
    assert!(left.is_empty(), "{left:?}");
}

#[test]
fn a_base_the_branch_shares_no_history_with_is_refused() {
    let review = Review::new();
    let error = review
        .snapshots
        .take(
            &review.worktree,
            "no-such-branch",
            &review.dir,
            &Review::label("take"),
        )
        .unwrap_err();
    assert!(matches!(error, SnapshotError::NoBase { .. }), "{error:?}");
}

fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}
