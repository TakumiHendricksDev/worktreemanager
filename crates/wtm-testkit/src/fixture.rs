//! A real git repository in a temporary directory.
//!
//! # Why a real binary
//!
//! Because the thing being tested is our understanding of what `git` prints and
//! does. Mocking git's output to verify our parsing of git's output tests the mock.
//! Every awkward case the parser claims to handle — a detached worktree, a path with
//! a space, a directory whose name disagrees with its branch — is *constructed here
//! with real git commands*, so the assertion is against reality rather than against
//! a transcript that may have been copied down wrong.
//!
//! # Isolation
//!
//! Each fixture gets its own [`tempfile::TempDir`], its own identity, and a
//! deliberately hermetic environment:
//!
//! - `HOME` points inside the fixture, so the developer's `~/.gitconfig` cannot
//!   change behaviour and the tests do not depend on whose machine they run on;
//! - commit signing is off (this machine signs by default via a 1Password SSH
//!   agent, which would prompt for Touch ID — mid-test-suite);
//! - `core.hooksPath` is emptied, so a global hook cannot interfere;
//! - the default branch is set explicitly rather than inherited.

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

/// A throwaway git repository.
pub struct GitFixture {
    dir: TempDir,
    root: PathBuf,
}

impl std::fmt::Debug for GitFixture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitFixture")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl GitFixture {
    /// Create a repository with one commit on `main`.
    ///
    /// # Panics
    ///
    /// If `git` is unavailable or any setup command fails. A fixture that half-built
    /// itself would produce baffling downstream failures, so it fails loudly here.
    #[must_use]
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("create temp dir");
        // The repo lives in a subdirectory so sibling worktrees (`../{name}`, the
        // common layout) land inside the TempDir and get cleaned up with it.
        let root = dir.path().join("repo");
        std::fs::create_dir_all(&root).expect("create repo dir");
        // A fake HOME, so a real ~/.gitconfig cannot leak in.
        std::fs::create_dir_all(dir.path().join("home")).expect("create fake home");

        let fixture = Self { dir, root };

        fixture.git(&["init", "-b", "main"]);
        fixture.git(&["config", "user.name", "wtm Test"]);
        fixture.git(&["config", "user.email", "test@example.invalid"]);
        // This machine has commit.gpgsign=true with a 1Password SSH signer; leaving
        // it on would pop Touch ID prompts during the test suite.
        fixture.git(&["config", "commit.gpgsign", "false"]);
        fixture.git(&["config", "tag.gpgsign", "false"]);
        fixture.git(&["config", "core.hooksPath", ""]);
        // Keep `worktree add` from inheriting an unexpected default.
        fixture.git(&["config", "init.defaultBranch", "main"]);

        // Configure `origin` as a real remote, even though nothing is ever fetched.
        //
        // Not cosmetic: without `remote.origin.fetch`, git does not regard
        // `refs/remotes/origin/x` as a *branch*, and `worktree add --track -b`
        // fails with "starting point 'origin/x' is not a branch". So a fixture
        // without this cannot exercise the adopt-a-remote-branch path at all. The
        // URL points at the repo itself and is never used.
        fixture.git(&[
            "config",
            "remote.origin.url",
            &fixture.root.to_string_lossy(),
        ]);
        fixture.git(&[
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/origin/*",
        ]);

        fixture.write("README.md", "# fixture\n");
        fixture.git(&["add", "."]);
        fixture.git(&["commit", "-m", "initial"]);

        fixture
    }

    /// Absolute repo root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The directory containing the repo — where sibling worktrees go.
    #[must_use]
    pub fn parent(&self) -> &Path {
        self.dir.path()
    }

    /// Run `git` in the repo root, requiring success.
    ///
    /// # Panics
    ///
    /// If the command fails, including its stderr in the message.
    pub fn git(&self, args: &[&str]) -> String {
        self.git_in(&self.root.clone(), args)
    }

    /// Run `git` in `cwd`, requiring success.
    ///
    /// # Panics
    ///
    /// If the command fails.
    pub fn git_in(&self, cwd: &Path, args: &[&str]) -> String {
        let output = self.command(cwd, args);
        assert!(
            output.status.success(),
            "git {} failed in {}:\n{}",
            args.join(" "),
            cwd.display(),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// Run `git`, returning the exit code and stdout without asserting.
    pub fn git_try(&self, args: &[&str]) -> (i32, String) {
        let output = self.command(&self.root.clone(), args);
        (
            output.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&output.stdout).into_owned(),
        )
    }

    fn command(&self, cwd: &Path, args: &[&str]) -> std::process::Output {
        // A test fixture is one of the two sanctioned places to spawn directly (the
        // other being `wtm-exec` itself); routing fixtures through the production
        // runner would make setup depend on the code under test.
        #[allow(clippy::disallowed_methods)]
        let mut cmd = Command::new("git");
        cmd.args(args)
            .current_dir(cwd)
            .env("HOME", self.dir.path().join("home"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "wtm Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
            .env("GIT_COMMITTER_NAME", "wtm Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
            // Never wait on a credential or passphrase prompt.
            .env("GIT_TERMINAL_PROMPT", "0")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE");
        cmd.output().expect("run git")
    }

    /// Write a file in the repo root, creating parent directories.
    ///
    /// # Panics
    ///
    /// If the write fails.
    pub fn write(&self, relative: &str, contents: &str) {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent dir");
        }
        std::fs::write(path, contents).expect("write file");
    }

    /// Commit a file change.
    pub fn commit(&self, relative: &str, contents: &str, message: &str) {
        self.write(relative, contents);
        self.git(&["add", relative]);
        self.git(&["commit", "-m", message]);
    }

    /// Create a branch without checking it out.
    pub fn branch(&self, name: &str) {
        self.git(&["branch", name]);
    }

    /// Add a worktree as a sibling of the repo root — the `../{name}` layout.
    ///
    /// Returns its absolute path.
    pub fn add_worktree(&self, dirname: &str, branch: &str) -> PathBuf {
        let path = self.dir.path().join(dirname);
        self.git(&[
            "worktree",
            "add",
            "--no-track",
            "-b",
            branch,
            &path.to_string_lossy(),
            "main",
        ]);
        path
    }

    /// Add a worktree on an existing branch.
    pub fn add_worktree_existing(&self, dirname: &str, branch: &str) -> PathBuf {
        let path = self.dir.path().join(dirname);
        self.git(&["worktree", "add", &path.to_string_lossy(), branch]);
        path
    }

    /// Add a detached worktree — one of the real cases the parser must handle.
    pub fn add_detached_worktree(&self, dirname: &str) -> PathBuf {
        let path = self.dir.path().join(dirname);
        self.git(&[
            "worktree",
            "add",
            "--detach",
            &path.to_string_lossy(),
            "HEAD",
        ]);
        path
    }

    /// Simulate a "fake remote" by creating `refs/remotes/origin/<branch>`.
    ///
    /// Cheaper and more reliable than cloning, and enough to exercise
    /// remote-branch listing and the adopt-a-remote-branch path.
    pub fn add_remote_ref(&self, branch: &str, committish: &str) {
        let sha = self.git(&["rev-parse", committish]);
        self.git(&[
            "update-ref",
            &format!("refs/remotes/origin/{branch}"),
            sha.trim(),
        ]);
    }

    /// Delete a worktree's directory without telling git, leaving the admin entry
    /// behind. git then reports it as `prunable`.
    ///
    /// # Panics
    ///
    /// If `path` is outside this fixture, or cannot be removed. The containment
    /// check is not ceremony: this is a recursive delete driven by a test, and a
    /// path built from the wrong base would erase real work.
    pub fn orphan_worktree(&self, path: &Path) {
        assert!(
            path.starts_with(self.dir.path()),
            "refusing to recursively delete {} — it is outside the fixture at {}",
            path.display(),
            self.dir.path().display()
        );
        std::fs::remove_dir_all(path).expect("remove worktree dir");
    }
}

impl Default for GitFixture {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn builds_a_repo_with_one_commit_on_main() {
        let fixture = GitFixture::new();
        assert_eq!(
            fixture.git(&["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
            "main"
        );
        assert_eq!(fixture.git(&["rev-list", "--count", "HEAD"]).trim(), "1");
    }

    #[test]
    fn signing_is_disabled_so_the_suite_never_prompts_for_touch_id() {
        let fixture = GitFixture::new();
        assert_eq!(fixture.git(&["config", "commit.gpgsign"]).trim(), "false");
    }

    #[test]
    fn worktrees_are_siblings_of_the_repo_root() {
        // The `../{name}` layout the app's default naming produces.
        let fixture = GitFixture::new();
        let path = fixture.add_worktree("ACME-1-x", "task/ACME-1-x");
        assert_eq!(path.parent(), Some(fixture.parent()));
        assert!(
            path.join(".git").exists(),
            "linked worktrees have a .git file"
        );
    }

    #[test]
    fn can_construct_a_directory_whose_name_disagrees_with_its_branch() {
        // The real-world case from the reference repo, reproducible on demand.
        let fixture = GitFixture::new();
        fixture.add_worktree(
            "ACME-4567-move-account-settings",
            "experiment/ACME-0000-something-else",
        );
        let listing = fixture.git(&["worktree", "list", "--porcelain"]);
        assert!(listing.contains("ACME-4567-move-account-settings"));
        assert!(listing.contains("refs/heads/experiment/ACME-0000-something-else"));
    }

    #[test]
    fn can_construct_a_detached_worktree() {
        let fixture = GitFixture::new();
        fixture.add_detached_worktree("detached-one");
        assert!(
            fixture
                .git(&["worktree", "list", "--porcelain"])
                .contains("detached")
        );
    }

    #[test]
    fn can_construct_a_prunable_worktree() {
        let fixture = GitFixture::new();
        let path = fixture.add_worktree("doomed", "task/doomed");
        fixture.orphan_worktree(&path);
        assert!(
            fixture
                .git(&["worktree", "list", "--porcelain"])
                .contains("prunable"),
            "deleting the directory should leave a prunable entry"
        );
    }

    #[test]
    fn can_construct_a_remote_only_branch() {
        let fixture = GitFixture::new();
        fixture.add_remote_ref("develop", "HEAD");
        let remotes = fixture.git(&["for-each-ref", "--format=%(refname:short)", "refs/remotes"]);
        assert!(remotes.contains("origin/develop"), "got {remotes:?}");
        // And it must not appear as a local branch.
        let locals = fixture.git(&["for-each-ref", "--format=%(refname:short)", "refs/heads"]);
        assert!(!locals.contains("develop"));
    }

    #[test]
    fn git_try_reports_failure_without_panicking() {
        let fixture = GitFixture::new();
        let (code, _) = fixture.git_try(&["rev-parse", "--verify", "refs/heads/nope"]);
        assert_ne!(code, 0);
    }
}
