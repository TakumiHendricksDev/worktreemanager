//! The `Git` implementation, over the `git` binary.
//!
//! # It takes a `CommandRunner`, it does not spawn
//!
//! [`GitCli`] holds an `Arc<dyn CommandRunner>` rather than calling
//! `std::process::Command` itself. Three things follow:
//!
//! - every `git` invocation inherits the deadline, resolved `PATH`, sanitized
//!   environment and process-group kill that the runner guarantees;
//! - this crate's dependency list proves it cannot spawn anything, which is the
//!   dependency-inversion rule made checkable;
//! - and its logic is unit-testable against a fake runner, while the parsing is
//!   tested separately in [`crate::porcelain`].
//!
//! # Timeouts
//!
//! Read-only queries get [`QUERY_TIMEOUT`]. `fetch` gets much longer, because it is
//! a network operation on a repository that may be large. Nothing here is unbounded:
//! `git` can prompt (for credentials, for a passphrase), and a prompt with no
//! deadline is a hang.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use wtm_core::error::{ExecError, GitError};
use wtm_core::model::{
    BranchRef, Checkout, CommitId, TrackMode, WorkingTreeStatus, Worktree, WorktreeId,
};
use wtm_core::ports::exec::{CancelToken, CommandRunner, Invocation};
use wtm_core::ports::git::{AddOptions, BranchFilter, Git};

use crate::porcelain;

/// Deadline for local read-only queries. Generous for a very large repository,
/// short enough that a credential prompt cannot hang the UI.
pub const QUERY_TIMEOUT: u64 = 30_000;

/// Deadline for local mutations (`worktree add`, `branch -D`). Longer than a query
/// because `worktree add` writes a full checkout, and hooks may run.
pub const MUTATE_TIMEOUT: u64 = 300_000;

/// Deadline for network operations.
pub const FETCH_TIMEOUT: u64 = 300_000;

/// `Git` over the git CLI.
pub struct GitCli {
    runner: Arc<dyn CommandRunner>,
    /// Remote used when listing remote branches and adopting remote-only ones.
    remote: String,
}

impl std::fmt::Debug for GitCli {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitCli")
            .field("remote", &self.remote)
            .finish_non_exhaustive()
    }
}

impl GitCli {
    #[must_use]
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self {
            runner,
            remote: "origin".to_owned(),
        }
    }

    #[must_use]
    pub fn with_remote(runner: Arc<dyn CommandRunner>, remote: impl Into<String>) -> Self {
        Self {
            runner,
            remote: remote.into(),
        }
    }

    #[must_use]
    pub fn remote(&self) -> &str {
        &self.remote
    }

    /// Run `git` in `cwd`, requiring success.
    fn git(&self, cwd: &Path, args: &[&str], timeout_ms: u64) -> Result<String, GitError> {
        let out = self.run(cwd, args, timeout_ms, false)?;
        Ok(out.stdout)
    }

    /// Run `git` in `cwd`, tolerating a non-zero exit.
    fn git_status_only(&self, cwd: &Path, args: &[&str], timeout_ms: u64) -> Result<i32, GitError> {
        Ok(self.run(cwd, args, timeout_ms, true)?.code)
    }

    fn run(
        &self,
        cwd: &Path,
        args: &[&str],
        timeout_ms: u64,
        allow_failure: bool,
    ) -> Result<wtm_core::ports::exec::Output, GitError> {
        let mut command = Vec::with_capacity(args.len() + 1);
        command.push("git".to_owned());
        command.extend(args.iter().map(|a| (*a).to_owned()));

        let inv = Invocation::new(command, cwd, timeout_ms);
        let cancel = CancelToken::new();

        let result = if allow_failure {
            self.runner.run_allow_failure(&inv, &cancel)
        } else {
            self.runner.run(&inv, &cancel)
        };

        result.map_err(|e| Self::to_git_error(&inv, e))
    }

    /// Translate an exec failure into a git-shaped one, keeping git's own stderr.
    ///
    /// git's messages are better than anything we would paraphrase — "fatal: a
    /// branch named 'x' already exists" tells the user exactly what to do.
    fn to_git_error(inv: &Invocation, err: ExecError) -> GitError {
        match err {
            ExecError::NonZeroExit { code, stderr, .. } => GitError::Failed {
                argv: inv.display(),
                code,
                stderr: stderr.trim().to_owned(),
            },
            other => GitError::Failed {
                argv: inv.display(),
                code: -1,
                stderr: other.to_string(),
            },
        }
    }
}

impl Git for GitCli {
    fn repo_root(&self, any_path: &Path) -> Result<PathBuf, GitError> {
        let out = self
            .git(any_path, &["rev-parse", "--show-toplevel"], QUERY_TIMEOUT)
            .map_err(|_| GitError::NotARepository(any_path.to_path_buf()))?;
        let trimmed = out.trim();
        if trimmed.is_empty() {
            // A bare repository has no working tree, so --show-toplevel is empty.
            return Err(GitError::NotARepository(any_path.to_path_buf()));
        }
        Ok(PathBuf::from(trimmed))
    }

    fn git_common_dir(&self, repo_root: &Path) -> Result<PathBuf, GitError> {
        let out = self.git(repo_root, &["rev-parse", "--git-common-dir"], QUERY_TIMEOUT)?;
        let raw = PathBuf::from(out.trim());
        // git may answer relatively (typically the literal `.git`). Resolve against
        // the repo root so callers get a usable absolute path.
        Ok(if raw.is_absolute() {
            raw
        } else {
            repo_root.join(raw)
        })
    }

    fn list_worktrees(&self, repo_root: &Path) -> Result<Vec<Worktree>, GitError> {
        let out = self.git(
            repo_root,
            &["worktree", "list", "--porcelain", "-z"],
            QUERY_TIMEOUT,
        )?;
        porcelain::parse_worktree_list(&out)
    }

    fn prune_worktrees(&self, repo_root: &Path) -> Result<(), GitError> {
        self.git(repo_root, &["worktree", "prune"], QUERY_TIMEOUT)?;
        Ok(())
    }

    fn branches(&self, repo_root: &Path, filter: BranchFilter) -> Result<Vec<BranchRef>, GitError> {
        let mut branches = Vec::new();

        if matches!(filter, BranchFilter::Local | BranchFilter::Both) {
            let out = self.git(
                repo_root,
                &["for-each-ref", "--format=%(refname:short)", "refs/heads"],
                QUERY_TIMEOUT,
            )?;
            branches.extend(porcelain::parse_branch_lines(&out, None));
        }

        if matches!(filter, BranchFilter::Remote | BranchFilter::Both) {
            let refs = format!("refs/remotes/{}", self.remote);
            let out = self.git(
                repo_root,
                &["for-each-ref", "--format=%(refname:short)", &refs],
                QUERY_TIMEOUT,
            )?;
            let remote = porcelain::parse_branch_lines(&out, Some(&self.remote));
            // Local wins on a name collision — it is the one actually checked out,
            // and offering the same name twice in a picker is confusing.
            for branch in remote {
                if !branches.contains(&branch) {
                    branches.push(branch);
                }
            }
        }

        Ok(branches)
    }

    fn remotes(&self, repo_root: &Path) -> Result<Vec<String>, GitError> {
        let out = self.git(repo_root, &["remote"], QUERY_TIMEOUT)?;
        Ok(out
            .lines()
            .map(str::trim)
            .filter(|remote| !remote.is_empty())
            .map(str::to_owned)
            .collect())
    }

    fn rev_parse(&self, repo_root: &Path, rev: &str) -> Result<Option<CommitId>, GitError> {
        // `--verify --quiet` exits non-zero for an unknown ref without writing to
        // stderr, which is what makes "does not exist" an expected answer here
        // rather than an error. Planning legitimately asks about refs that may not
        // exist yet.
        let spec = format!("{rev}^{{commit}}");
        let out = self.run(
            repo_root,
            &["rev-parse", "--verify", "--quiet", &spec],
            QUERY_TIMEOUT,
            true,
        )?;
        if out.code != 0 {
            return Ok(None);
        }
        let sha = out.stdout.trim();
        Ok(if sha.is_empty() {
            None
        } else {
            Some(CommitId::new(sha))
        })
    }

    fn status(&self, worktree_path: &Path) -> Result<WorkingTreeStatus, GitError> {
        let out = self.git(
            worktree_path,
            &["status", "--porcelain=v1", "-z", "--untracked-files=normal"],
            QUERY_TIMEOUT,
        )?;
        Ok(porcelain::parse_status(&out))
    }

    fn ahead_behind(
        &self,
        repo_root: &Path,
        branch: &BranchRef,
        base: &str,
    ) -> Result<(u32, u32), GitError> {
        // Base first so left = behind, right = ahead. See `parse_ahead_behind`.
        let range = format!("{base}...{}", branch.as_str());
        let out = self.run(
            repo_root,
            &["rev-list", "--left-right", "--count", &range],
            QUERY_TIMEOUT,
            true,
        )?;
        if out.code != 0 {
            // An unresolvable base (no upstream, a fresh repo) is normal, not an
            // error worth surfacing — the UI simply shows no divergence.
            tracing::debug!(range, "ahead/behind unavailable");
            return Ok((0, 0));
        }
        porcelain::parse_ahead_behind(&out.stdout)
    }

    fn fetch(&self, repo_root: &Path, remote: &str, refspec: &str) -> Result<(), GitError> {
        self.git(repo_root, &["fetch", "--", remote, refspec], FETCH_TIMEOUT)?;
        Ok(())
    }

    fn add_worktree(&self, repo_root: &Path, opts: &AddOptions) -> Result<Worktree, GitError> {
        let argv = self.add_worktree_argv(opts);
        // The trait's default implementation builds the argv and the review screen
        // shows it, so it must also be what actually runs — hence reusing it here
        // rather than reconstructing the flags. One source of truth, no drift
        // between preview and execution.
        let tail: Vec<&str> = argv.iter().skip(1).map(String::as_str).collect();
        self.git(repo_root, &tail, MUTATE_TIMEOUT)?;

        // Prefer git's record because it enriches the result with the resolved path
        // and HEAD. A successful mutation remains authoritative when that best-effort
        // lookup cannot match: reporting failure here would strand a live worktree and
        // any branch that `git worktree add` just created.
        let created = match self.list_worktrees(repo_root) {
            Ok(worktrees) => worktrees
                .into_iter()
                .find(|worktree| paths_equal(&worktree.path, &opts.path)),
            Err(error) => {
                // The mutation already succeeded. Relisting enriches the answer, but making its
                // failure authoritative would report that no worktree was created while leaving
                // both the checkout and possibly its new branch on disk.
                tracing::debug!(%error, "could not enrich a newly created worktree from the relist");
                None
            }
        };

        Ok(created.unwrap_or_else(|| Worktree {
            id: WorktreeId::from_path(&opts.path),
            path: opts.path.clone(),
            head: None,
            checkout: opts
                .branch
                .clone()
                .map_or(Checkout::Detached, |branch| Checkout::Branch { branch }),
            is_main: false,
            is_bare: false,
            locked: None,
            prunable: None,
        }))
    }

    fn remove_worktree(
        &self,
        repo_root: &Path,
        worktree_path: &Path,
        force: bool,
    ) -> Result<(), GitError> {
        let path = worktree_path.to_string_lossy();
        let mut args = vec!["worktree", "remove"];
        if force {
            args.push("--force");
        }
        args.push("--");
        args.push(&path);
        self.git(repo_root, &args, MUTATE_TIMEOUT)?;
        Ok(())
    }

    fn delete_branch(
        &self,
        repo_root: &Path,
        branch: &BranchRef,
        force: bool,
    ) -> Result<(), GitError> {
        // -d refuses to delete unmerged work; -D does not. The choice is the
        // caller's, made explicit in the UI, never silently forced.
        let flag = if force { "-D" } else { "-d" };
        self.git(
            repo_root,
            &["branch", flag, branch.as_str()],
            MUTATE_TIMEOUT,
        )?;
        Ok(())
    }

    fn is_merged(
        &self,
        repo_root: &Path,
        branch: &BranchRef,
        base: &str,
    ) -> Result<bool, GitError> {
        let code = self.git_status_only(
            repo_root,
            &["merge-base", "--is-ancestor", branch.as_str(), base],
            QUERY_TIMEOUT,
        )?;
        // 0 = ancestor, 1 = not. Anything else means the refs could not be
        // resolved, in which case "not merged" is the safe answer: it makes the UI
        // warn before deleting rather than stay quiet.
        Ok(code == 0)
    }
}

/// Compare paths tolerantly.
///
/// git may record a path that differs textually from the one we asked for — a
/// resolved symlink (`/var` → `/private/var` on macOS, which every temp directory
/// goes through) or a stripped trailing separator. Comparing the canonical forms
/// when both exist avoids a spurious "git said success but the worktree is missing".
fn paths_equal(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Map a [`TrackMode`] onto the flag `git worktree add` expects.
///
/// Kept as a named function so the mapping is greppable from the config side.
#[must_use]
pub const fn track_flag(mode: TrackMode) -> Option<&'static str> {
    match mode {
        TrackMode::NoTrack => Some("--no-track"),
        TrackMode::Track => Some("--track"),
        TrackMode::Detach => None,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use wtm_testkit::FakeRunner;

    use super::*;

    /// Build a `GitCli` over a scripted fake runner.
    ///
    /// The fake lives in `wtm-testkit` rather than here so `wtm-config` and the
    /// pipeline tests script `CommandRunner` the same way — one fake, one set of
    /// semantics.
    fn git_with(
        responses: Vec<Result<wtm_core::ports::exec::Output, ExecError>>,
    ) -> (GitCli, Arc<FakeRunner>) {
        let runner = Arc::new(FakeRunner::scripted(responses));
        (
            GitCli::new(Arc::clone(&runner) as Arc<dyn CommandRunner>),
            runner,
        )
    }

    fn repo() -> PathBuf {
        PathBuf::from("/repo")
    }

    #[test]
    fn list_worktrees_asks_for_porcelain_z() {
        // The whole point of the parser: never the human-readable form.
        let (git, runner) = git_with(vec![FakeRunner::ok(
            "worktree /repo\0HEAD abc\0branch refs/heads/main\0\0",
        )]);

        assert_eq!(git.list_worktrees(&repo()).unwrap().len(), 1);
        assert_eq!(
            runner.argvs()[0],
            vec!["git", "worktree", "list", "--porcelain", "-z"],
            "must request the machine-readable form"
        );
    }

    #[test]
    fn rev_parse_returns_none_for_an_unknown_ref_rather_than_erroring() {
        // Planning asks about refs that may not exist; that is not a failure.
        let (git, _) = git_with(vec![FakeRunner::failed(128, "")]);
        assert_eq!(git.rev_parse(&repo(), "origin/nope").unwrap(), None);
    }

    #[test]
    fn rev_parse_peels_to_a_commit_so_an_annotated_tag_resolves_correctly() {
        let (git, runner) = git_with(vec![FakeRunner::ok("d2e33557e6\n")]);
        assert_eq!(
            git.rev_parse(&repo(), "v1.0")
                .unwrap()
                .map(|c| c.as_str().to_owned()),
            Some("d2e33557e6".to_owned())
        );
        assert!(
            runner.argvs()[0].iter().any(|a| a == "v1.0^{commit}"),
            "without ^{{commit}} an annotated tag yields the tag object's sha: {:?}",
            runner.argvs()[0]
        );
    }

    #[test]
    fn branches_dedupes_with_local_winning() {
        let (git, _) = git_with(vec![
            FakeRunner::ok("main\ndevelop\n"),
            FakeRunner::ok("origin/main\norigin/feature\norigin/HEAD\n"),
        ]);
        let names: Vec<String> = git
            .branches(&repo(), BranchFilter::Both)
            .unwrap()
            .iter()
            .map(|b| b.as_str().to_owned())
            .collect();
        assert_eq!(
            names,
            vec!["main", "develop", "feature"],
            "main must appear once"
        );
    }

    #[test]
    fn branches_respects_a_non_default_remote() {
        let runner = Arc::new(FakeRunner::scripted(vec![FakeRunner::ok(
            "upstream/main\n",
        )]));
        let git = GitCli::with_remote(Arc::clone(&runner) as Arc<dyn CommandRunner>, "upstream");
        let names: Vec<String> = git
            .branches(&repo(), BranchFilter::Remote)
            .unwrap()
            .iter()
            .map(|b| b.as_str().to_owned())
            .collect();
        assert_eq!(names, vec!["main"]);
        assert!(
            runner.argvs()[0]
                .iter()
                .any(|a| a == "refs/remotes/upstream")
        );
    }

    #[test]
    fn git_stderr_is_preserved_verbatim() {
        // git's own message is more useful than any paraphrase we could write.
        let (git, _) = git_with(vec![FakeRunner::failed(
            128,
            "fatal: a branch named 'task/x' already exists\n",
        )]);
        match git.prune_worktrees(&repo()).unwrap_err() {
            GitError::Failed { code, stderr, .. } => {
                assert_eq!(code, 128);
                assert_eq!(stderr, "fatal: a branch named 'task/x' already exists");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn ahead_behind_puts_the_base_on_the_left() {
        let (git, runner) = git_with(vec![FakeRunner::ok("3\t7\n")]);
        assert_eq!(
            git.ahead_behind(&repo(), &BranchRef::new("mine"), "origin/develop")
                .unwrap(),
            (7, 3),
            "7 ahead, 3 behind"
        );
        assert!(
            runner.argvs()[0]
                .iter()
                .any(|a| a == "origin/develop...mine"),
            "base must be the left side: {:?}",
            runner.argvs()[0]
        );
    }

    #[test]
    fn ahead_behind_degrades_to_zero_when_the_base_is_unresolvable() {
        let (git, _) = git_with(vec![FakeRunner::failed(128, "unknown revision")]);
        assert_eq!(
            git.ahead_behind(&repo(), &BranchRef::new("x"), "origin/nope")
                .unwrap(),
            (0, 0)
        );
    }

    #[test]
    fn delete_branch_uses_capital_d_only_when_forced() {
        let (git, runner) = git_with(vec![FakeRunner::ok(""), FakeRunner::ok("")]);
        let branch = BranchRef::new("task/x");
        git.delete_branch(&repo(), &branch, false).unwrap();
        git.delete_branch(&repo(), &branch, true).unwrap();

        let calls = runner.argvs();
        assert!(
            calls[0].contains(&"-d".to_owned()),
            "unforced must be -d: {:?}",
            calls[0]
        );
        assert!(
            calls[1].contains(&"-D".to_owned()),
            "forced must be -D: {:?}",
            calls[1]
        );
    }

    #[test]
    fn remove_worktree_only_forces_when_asked() {
        let (git, runner) = git_with(vec![FakeRunner::ok(""), FakeRunner::ok("")]);
        git.remove_worktree(&repo(), Path::new("/wt/a"), false)
            .unwrap();
        git.remove_worktree(&repo(), Path::new("/wt/a"), true)
            .unwrap();

        let calls = runner.argvs();
        assert!(!calls[0].contains(&"--force".to_owned()));
        assert!(calls[1].contains(&"--force".to_owned()));
        assert!(
            calls[0].windows(2).any(|w| w == ["--", "/wt/a"]),
            "the path must follow `--` so a dash-prefixed worktree cannot be parsed as a flag: {:?}",
            calls[0]
        );
    }

    #[test]
    fn is_merged_treats_an_unresolvable_ref_as_not_merged() {
        // The safe direction: the UI warns before deleting instead of staying quiet.
        let (git, _) = git_with(vec![FakeRunner::failed(128, "bad ref")]);
        assert!(
            !git.is_merged(&repo(), &BranchRef::new("x"), "nope")
                .unwrap()
        );
    }

    #[test]
    fn git_common_dir_is_resolved_against_the_repo_root_when_relative() {
        // git usually answers the bare string `.git`, which is useless as a path.
        let (git, _) = git_with(vec![FakeRunner::ok(".git\n")]);
        assert_eq!(
            git.git_common_dir(&repo()).unwrap(),
            PathBuf::from("/repo/.git")
        );
    }

    #[test]
    fn git_common_dir_keeps_an_absolute_answer() {
        let (git, _) = git_with(vec![FakeRunner::ok("/elsewhere/.git\n")]);
        assert_eq!(
            git.git_common_dir(&repo()).unwrap(),
            PathBuf::from("/elsewhere/.git")
        );
    }

    #[test]
    fn a_bare_repo_reports_not_a_repository_for_repo_root() {
        // --show-toplevel is empty for a bare repo; an empty PathBuf would be worse
        // than an error.
        let (git, _) = git_with(vec![FakeRunner::ok("\n")]);
        assert!(matches!(
            git.repo_root(Path::new("/bare.git")).unwrap_err(),
            GitError::NotARepository(_)
        ));
    }

    #[test]
    fn add_worktree_runs_exactly_the_previewed_argv() {
        // The review screen promises an argv; this is the test that the promise is
        // kept, because both come from `Git::add_worktree_argv`.
        let (git, runner) = git_with(vec![
            FakeRunner::ok(""),
            FakeRunner::ok(
                "worktree /repo\0HEAD a\0branch refs/heads/main\0\0\
                 worktree /wt/new\0HEAD b\0branch refs/heads/task/x\0\0",
            ),
        ]);

        let opts = AddOptions {
            path: PathBuf::from("/wt/new"),
            branch: Some(BranchRef::new("task/x")),
            start_point: "origin/develop".to_owned(),
            track: TrackMode::NoTrack,
            create_branch: true,
        };
        let previewed = git.add_worktree_argv(&opts);
        let created = git.add_worktree(&repo(), &opts).unwrap();

        assert_eq!(created.path, PathBuf::from("/wt/new"));
        assert!(!created.is_main);
        assert_eq!(
            runner.argvs()[0],
            previewed,
            "executed argv must equal the previewed argv"
        );
    }

    #[test]
    fn a_created_worktree_is_reported_even_when_the_relist_cannot_match_its_path() {
        // A mismatch after exit zero must not turn creation into a reported failure:
        // cleanup cannot undo a live worktree or the branch git created for it.
        let (git, _) = git_with(vec![
            FakeRunner::ok(""),
            FakeRunner::ok("worktree /repo\0HEAD a\0branch refs/heads/main\0\0"),
        ]);
        let created = git
            .add_worktree(
                &repo(),
                &AddOptions {
                    path: PathBuf::from("/wt/missing"),
                    branch: Some(BranchRef::new("task/missing")),
                    start_point: "HEAD".to_owned(),
                    track: TrackMode::NoTrack,
                    create_branch: true,
                },
            )
            .unwrap();

        assert_eq!(created.path, PathBuf::from("/wt/missing"));
        assert_eq!(created.id, WorktreeId::from_path(Path::new("/wt/missing")));
        assert_eq!(created.head, None);
        assert_eq!(created.branch(), Some(&BranchRef::new("task/missing")));
        assert!(!created.is_main);
    }

    #[test]
    fn a_created_worktree_is_reported_even_when_the_relist_fails() {
        let (git, _) = git_with(vec![
            FakeRunner::ok(""),
            FakeRunner::failed(128, "could not read worktree metadata"),
        ]);
        let created = git
            .add_worktree(
                &repo(),
                &AddOptions {
                    path: PathBuf::from("/wt/created"),
                    branch: Some(BranchRef::new("task/created")),
                    start_point: "HEAD".to_owned(),
                    track: TrackMode::NoTrack,
                    create_branch: true,
                },
            )
            .unwrap();

        assert_eq!(created.path, PathBuf::from("/wt/created"));
        assert_eq!(created.branch(), Some(&BranchRef::new("task/created")));
    }

    #[test]
    fn track_flag_mapping() {
        assert_eq!(track_flag(TrackMode::NoTrack), Some("--no-track"));
        assert_eq!(track_flag(TrackMode::Track), Some("--track"));
        assert_eq!(track_flag(TrackMode::Detach), None);
    }
}
