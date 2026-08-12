//! In-memory implementations of the ports.
//!
//! Each one records what it was asked to do, so a test can assert on *behaviour*
//! ("planning must not mutate", "the executed argv equals the previewed argv")
//! rather than just on return values.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use parking_lot::Mutex;
use wtm_core::error::{ConfigError, ExecError, GitError};
use wtm_core::model::{
    BranchRef, Checkout, CommitId, TrackMode, WorkingTreeStatus, Worktree, WorktreeId,
};
use wtm_core::ports::clock::Clock;
use wtm_core::ports::exec::{CancelToken, CommandRunner, Invocation, Output};
use wtm_core::ports::fs::FileStore;
use wtm_core::ports::git::{AddOptions, BranchFilter, Git};
use wtm_core::ports::progress::{ProgressEvent, ProgressSink};

// ─────────────────────────────── CommandRunner ───────────────────────────────

/// Records every invocation and replays canned responses in order.
///
/// Once the queue is exhausted it returns empty success, so a test only has to
/// script the calls it cares about.
#[derive(Debug, Default)]
pub struct FakeRunner {
    calls: Mutex<Vec<Invocation>>,
    responses: Mutex<Vec<Result<Output, ExecError>>>,
    programs: Mutex<Vec<String>>,
}

impl FakeRunner {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A runner that replays `responses` in order.
    #[must_use]
    pub fn scripted(responses: Vec<Result<Output, ExecError>>) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            responses: Mutex::new(responses),
            programs: Mutex::new(Vec::new()),
        }
    }

    /// Declare which programs [`CommandRunner::which`] should find.
    ///
    /// Empty means "everything resolves"; that is the convenient default, and a
    /// test exercising a missing-program preflight sets it explicitly.
    #[must_use]
    pub fn with_programs(self, programs: &[&str]) -> Self {
        *self.programs.lock() = programs.iter().map(|p| (*p).to_owned()).collect();
        self
    }

    /// A successful response for [`Self::scripted`].
    ///
    /// The `Result` wrapper is the element type of the scripted queue — it is what
    /// lets a test mix successful output with spawn failures and timeouts in one
    /// list — so it is deliberate rather than a needless wrap.
    #[allow(clippy::unnecessary_wraps)]
    pub fn ok(stdout: &str) -> Result<Output, ExecError> {
        Ok(Output {
            code: 0,
            stdout: stdout.to_owned(),
            stderr: String::new(),
            duration_ms: 1,
        })
    }

    /// A non-zero-exit response for [`Self::scripted`]. See [`Self::ok`].
    #[allow(clippy::unnecessary_wraps)]
    pub fn failed(code: i32, stderr: &str) -> Result<Output, ExecError> {
        Ok(Output {
            code,
            stdout: String::new(),
            stderr: stderr.to_owned(),
            duration_ms: 1,
        })
    }

    /// Every invocation, in order.
    #[must_use]
    pub fn calls(&self) -> Vec<Invocation> {
        self.calls.lock().clone()
    }

    /// Every invocation's argv, in order — the usual assertion target.
    #[must_use]
    pub fn argvs(&self) -> Vec<Vec<String>> {
        self.calls.lock().iter().map(|i| i.argv.clone()).collect()
    }

    #[must_use]
    pub fn call_count(&self) -> usize {
        self.calls.lock().len()
    }

    /// Whether any invocation's joined argv contains `needle`.
    #[must_use]
    pub fn ran_containing(&self, needle: &str) -> bool {
        self.calls
            .lock()
            .iter()
            .any(|i| i.display().contains(needle))
    }

    fn record(&self, inv: &Invocation) -> Result<Output, ExecError> {
        self.calls.lock().push(inv.clone());
        let mut responses = self.responses.lock();
        if responses.is_empty() {
            Self::ok("")
        } else {
            responses.remove(0)
        }
    }
}

impl CommandRunner for FakeRunner {
    fn run(&self, inv: &Invocation, _cancel: &CancelToken) -> Result<Output, ExecError> {
        let out = self.record(inv)?;
        if out.is_success() {
            Ok(out)
        } else {
            Err(ExecError::NonZeroExit {
                argv: inv.display(),
                code: out.code,
                stdout: out.stdout,
                stderr: out.stderr,
            })
        }
    }

    fn run_allow_failure(
        &self,
        inv: &Invocation,
        _cancel: &CancelToken,
    ) -> Result<Output, ExecError> {
        self.record(inv)
    }

    fn which(&self, program: &str) -> Option<PathBuf> {
        let programs = self.programs.lock();
        if programs.is_empty() || programs.iter().any(|p| p == program) {
            Some(PathBuf::from("/fake/bin").join(program))
        } else {
            None
        }
    }

    fn resolved_path(&self) -> String {
        "/fake/bin".to_owned()
    }
}

// ─────────────────────────────── Clock ───────────────────────────────

/// A clock that does not move unless told to.
///
/// Cache-TTL behaviour is otherwise untestable without sleeping, and a test that
/// sleeps is a test that is either slow or flaky.
#[derive(Debug)]
pub struct FakeClock {
    unix_ms: Mutex<u64>,
    monotonic_ms: Mutex<u64>,
    date: Mutex<String>,
}

impl Default for FakeClock {
    fn default() -> Self {
        // 2026-07-28T12:00:00Z — a fixed, recognizable instant.
        Self {
            unix_ms: Mutex::new(1_785_931_200_000),
            monotonic_ms: Mutex::new(0),
            date: Mutex::new("2026-07-28".to_owned()),
        }
    }
}

impl FakeClock {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Move both clocks forward.
    pub fn advance_ms(&self, delta: u64) {
        *self.unix_ms.lock() += delta;
        *self.monotonic_ms.lock() += delta;
    }
}

impl Clock for FakeClock {
    fn now_unix_ms(&self) -> u64 {
        *self.unix_ms.lock()
    }

    fn today(&self) -> String {
        self.date.lock().clone()
    }

    fn now_iso(&self) -> String {
        format!("{}T12:00:00Z", self.date.lock())
    }

    fn monotonic_ms(&self) -> u64 {
        *self.monotonic_ms.lock()
    }
}

// ─────────────────────────────── FileStore ───────────────────────────────

/// An in-memory filesystem.
#[derive(Debug, Default)]
pub struct FakeFileStore {
    files: Mutex<BTreeMap<PathBuf, String>>,
    dirs: Mutex<BTreeMap<PathBuf, bool>>,
}

impl FakeFileStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_file(&self, path: impl Into<PathBuf>, contents: impl Into<String>) {
        self.files.lock().insert(path.into(), contents.into());
    }

    /// Register a directory. `empty` drives the "target exists but is empty" case,
    /// which `git worktree add` tolerates while a populated one is fatal.
    pub fn add_dir(&self, path: impl Into<PathBuf>, empty: bool) {
        self.dirs.lock().insert(path.into(), empty);
    }
}

impl FileStore for FakeFileStore {
    fn exists(&self, path: &Path) -> bool {
        self.files.lock().contains_key(path) || self.dirs.lock().contains_key(path)
    }

    fn is_dir(&self, path: &Path) -> bool {
        self.dirs.lock().contains_key(path)
    }

    fn is_dir_empty(&self, path: &Path) -> Result<bool, ConfigError> {
        self.dirs
            .lock()
            .get(path)
            .copied()
            .ok_or_else(|| ConfigError::Io {
                path: path.to_path_buf(),
                message: "no such directory".to_owned(),
            })
    }

    fn read_to_string(&self, path: &Path) -> Result<String, ConfigError> {
        self.files
            .lock()
            .get(path)
            .cloned()
            .ok_or_else(|| ConfigError::Io {
                path: path.to_path_buf(),
                message: "no such file".to_owned(),
            })
    }

    fn read_dotenv(&self, path: &Path) -> Result<BTreeMap<String, String>, ConfigError> {
        let contents = self.read_to_string(path)?;
        Ok(parse_dotenv(&contents))
    }

    fn absolutize(&self, path: &Path) -> Result<PathBuf, ConfigError> {
        Ok(normalize(path))
    }
}

/// Parse `KEY=value` text.
///
/// Shared by the fake and the real adapter so both agree on the semantics: the last
/// assignment wins, one layer of surrounding quotes is stripped, comments and blanks
/// are ignored, and nothing is expanded. Exported because having two
/// implementations of this would be a subtle way for tests to disagree with reality.
#[must_use]
pub fn parse_dotenv(contents: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // `export KEY=value` is common in hand-edited env files.
        let line = line.strip_prefix("export ").unwrap_or(line);
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            if key.is_empty() {
                continue;
            }
            let value = value.trim();
            let value = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
                .unwrap_or(value);
            // Last wins.
            out.insert(key.to_owned(), value.to_owned());
        }
    }
    out
}

/// Lexically normalize a path, resolving `.` and `..` without touching the disk.
///
/// Must not require existence: it runs on the *target* directory during planning,
/// before anything is created, so `canonicalize` is not an option.
#[must_use]
pub fn normalize(path: &Path) -> PathBuf {
    use std::path::Component;

    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match out.components().next_back() {
                // Cancel out a real name.
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                // At the filesystem root, `..` is a no-op — POSIX defines root's
                // parent as root. Pushing it would produce `/..`, which then makes
                // a *following* `..` pop the literal `..` and appear to escape.
                Some(Component::RootDir) => {}
                // A relative path's leading `..` must be preserved; there is nothing
                // to cancel it against yet.
                _ => out.push(".."),
            },
            other => out.push(other.as_os_str()),
        }
    }
    out
}

// ─────────────────────────────── Git ───────────────────────────────

/// An in-memory git.
///
/// Tracks worktrees and branches, and — importantly — records every mutation, so a
/// test can assert the create pipeline's central invariant: **nothing mutates before
/// stage 7**.
#[derive(Debug, Default)]
pub struct FakeGit {
    worktrees: Mutex<Vec<Worktree>>,
    remotes: Mutex<Vec<String>>,
    local_branches: Mutex<Vec<BranchRef>>,
    remote_branches: Mutex<Vec<BranchRef>>,
    revs: Mutex<BTreeMap<String, CommitId>>,
    statuses: Mutex<BTreeMap<PathBuf, WorkingTreeStatus>>,
    /// Names of mutating operations, in order.
    mutations: Mutex<Vec<String>>,
    fail_add: Mutex<Option<String>>,
    merged: Mutex<Option<bool>>,
}

impl FakeGit {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A repo whose main worktree is at `root` on `branch`.
    #[must_use]
    pub fn with_main(root: impl Into<PathBuf>, branch: &str) -> Self {
        let root = root.into();
        let git = Self::new();
        git.worktrees.lock().push(Worktree {
            id: WorktreeId::from_path(&root),
            path: root,
            head: Some(CommitId::new("1111111111111111111111111111111111111111")),
            checkout: Checkout::Branch {
                branch: BranchRef::new(branch),
            },
            is_main: true,
            is_bare: false,
            locked: None,
            prunable: None,
        });
        git.local_branches.lock().push(BranchRef::new(branch));
        git.remotes.lock().push("origin".to_owned());
        git
    }

    #[must_use]
    pub fn with_local_branches(self, branches: &[&str]) -> Self {
        let mut local = self.local_branches.lock();
        for branch in branches {
            let branch = BranchRef::new(*branch);
            if !local.contains(&branch) {
                local.push(branch);
            }
        }
        drop(local);
        self
    }

    #[must_use]
    pub fn with_remote_branches(self, branches: &[&str]) -> Self {
        *self.remote_branches.lock() = branches.iter().map(|b| BranchRef::new(*b)).collect();
        self
    }

    #[must_use]
    pub fn with_remotes(self, remotes: &[&str]) -> Self {
        *self.remotes.lock() = remotes.iter().map(|remote| (*remote).to_owned()).collect();
        self
    }

    /// Make `rev` resolve to `sha`. Unregistered revs resolve to `None`.
    #[must_use]
    pub fn with_rev(self, rev: &str, sha: &str) -> Self {
        self.revs.lock().insert(rev.to_owned(), CommitId::new(sha));
        self
    }

    #[must_use]
    pub fn with_status(self, path: impl Into<PathBuf>, status: WorkingTreeStatus) -> Self {
        self.statuses.lock().insert(path.into(), status);
        self
    }

    /// Add an extra worktree, for "already checked out" and "path in use" cases.
    #[must_use]
    pub fn with_worktree(self, path: impl Into<PathBuf>, branch: Option<&str>) -> Self {
        let path = path.into();
        self.worktrees.lock().push(Worktree {
            id: WorktreeId::from_path(&path),
            path,
            head: Some(CommitId::new("2222222222222222222222222222222222222222")),
            checkout: branch.map_or(Checkout::Detached, |b| Checkout::Branch {
                branch: BranchRef::new(b),
            }),
            is_main: false,
            is_bare: false,
            locked: None,
            prunable: None,
        });
        self
    }

    /// Make `add_worktree` fail, for testing the abort path of stage 8.
    #[must_use]
    pub fn failing_add(self, message: &str) -> Self {
        *self.fail_add.lock() = Some(message.to_owned());
        self
    }

    #[must_use]
    pub fn with_merged(self, merged: bool) -> Self {
        *self.merged.lock() = Some(merged);
        self
    }

    /// Mutating operations performed, in order.
    #[must_use]
    pub fn mutations(&self) -> Vec<String> {
        self.mutations.lock().clone()
    }

    /// The assertion behind the no-mutation-before-stage-7 invariant.
    #[must_use]
    pub fn was_mutated(&self) -> bool {
        !self.mutations.lock().is_empty()
    }

    fn note(&self, what: impl Into<String>) {
        self.mutations.lock().push(what.into());
    }
}

impl Git for FakeGit {
    fn repo_root(&self, _any_path: &Path) -> Result<PathBuf, GitError> {
        self.worktrees
            .lock()
            .first()
            .map(|w| w.path.clone())
            .ok_or_else(|| GitError::NotARepository(PathBuf::from("/")))
    }

    fn git_common_dir(&self, repo_root: &Path) -> Result<PathBuf, GitError> {
        Ok(repo_root.join(".git"))
    }

    fn list_worktrees(&self, _repo_root: &Path) -> Result<Vec<Worktree>, GitError> {
        Ok(self.worktrees.lock().clone())
    }

    fn prune_worktrees(&self, _repo_root: &Path) -> Result<(), GitError> {
        // Deliberately NOT recorded as a mutation: prune only drops admin entries
        // for directories the user already deleted, so planning may call it freely
        // without violating the no-mutation invariant.
        Ok(())
    }

    fn branches(
        &self,
        _repo_root: &Path,
        filter: BranchFilter,
    ) -> Result<Vec<BranchRef>, GitError> {
        let mut out = Vec::new();
        if matches!(filter, BranchFilter::Local | BranchFilter::Both) {
            out.extend(self.local_branches.lock().iter().cloned());
        }
        if matches!(filter, BranchFilter::Remote | BranchFilter::Both) {
            for branch in self.remote_branches.lock().iter() {
                if !out.contains(branch) {
                    out.push(branch.clone());
                }
            }
        }
        Ok(out)
    }

    fn remotes(&self, _repo_root: &Path) -> Result<Vec<String>, GitError> {
        Ok(self.remotes.lock().clone())
    }

    fn rev_parse(&self, _repo_root: &Path, rev: &str) -> Result<Option<CommitId>, GitError> {
        Ok(self.revs.lock().get(rev).cloned())
    }

    fn status(&self, worktree_path: &Path) -> Result<WorkingTreeStatus, GitError> {
        Ok(self
            .statuses
            .lock()
            .get(worktree_path)
            .copied()
            .unwrap_or_default())
    }

    fn ahead_behind(
        &self,
        _repo_root: &Path,
        _branch: &BranchRef,
        _base: &str,
    ) -> Result<(u32, u32), GitError> {
        Ok((0, 0))
    }

    fn fetch(&self, _repo_root: &Path, remote: &str, refspec: &str) -> Result<(), GitError> {
        self.note(format!("fetch {remote} {refspec}"));
        Ok(())
    }

    fn add_worktree(&self, _repo_root: &Path, opts: &AddOptions) -> Result<Worktree, GitError> {
        if let Some(message) = self.fail_add.lock().clone() {
            return Err(GitError::Failed {
                argv: "git worktree add".to_owned(),
                code: 128,
                stderr: message,
            });
        }
        self.note(format!("add_worktree {}", opts.path.display()));

        let worktree = Worktree {
            id: WorktreeId::from_path(&opts.path),
            path: opts.path.clone(),
            head: Some(CommitId::new("3333333333333333333333333333333333333333")),
            checkout: opts
                .branch
                .clone()
                .map_or(Checkout::Detached, |branch| Checkout::Branch { branch }),
            is_main: false,
            is_bare: false,
            locked: None,
            prunable: None,
        };
        self.worktrees.lock().push(worktree.clone());
        if opts.create_branch
            && !matches!(opts.track, TrackMode::Detach)
            && let Some(branch) = &opts.branch
        {
            self.local_branches.lock().push(branch.clone());
        }
        Ok(worktree)
    }

    fn remove_worktree(
        &self,
        _repo_root: &Path,
        worktree_path: &Path,
        force: bool,
    ) -> Result<(), GitError> {
        self.note(format!(
            "remove_worktree {} force={force}",
            worktree_path.display()
        ));
        self.worktrees.lock().retain(|w| w.path != worktree_path);
        Ok(())
    }

    fn delete_branch(
        &self,
        _repo_root: &Path,
        branch: &BranchRef,
        force: bool,
    ) -> Result<(), GitError> {
        self.note(format!("delete_branch {branch} force={force}"));
        self.local_branches.lock().retain(|b| b != branch);
        Ok(())
    }

    fn is_merged(
        &self,
        _repo_root: &Path,
        _branch: &BranchRef,
        _base: &str,
    ) -> Result<bool, GitError> {
        Ok(self.merged.lock().unwrap_or(true))
    }
}

// ─────────────────────────────── ProgressSink ───────────────────────────────

/// Captures progress events so a test can assert the pipeline's stage ordering.
#[derive(Debug, Default)]
pub struct RecordedProgress {
    events: Mutex<Vec<ProgressEvent>>,
}

impl RecordedProgress {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn events(&self) -> Vec<ProgressEvent> {
        self.events.lock().clone()
    }

    /// Stage ids, in the order they were entered.
    #[must_use]
    pub fn stages(&self) -> Vec<String> {
        self.events
            .lock()
            .iter()
            .filter_map(|e| match e {
                ProgressEvent::Stage { id, .. } => Some(id.clone()),
                _ => None,
            })
            .collect()
    }
}

impl ProgressSink for RecordedProgress {
    fn emit(&self, event: ProgressEvent) {
        self.events.lock().push(event);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn dotenv_last_assignment_wins_and_quotes_are_stripped() {
        let env = parse_dotenv(
            "# comment\n\
             HOST_PORT_WEB=8000\n\
             HOST_PORT_WEB=8007\n\
             DOMAIN=\"127.0.0.1:8007\"\n\
             QUOTED='single'\n\
             export EXPORTED=yes\n\
             \n\
             EMPTY=\n",
        );
        assert_eq!(env.get("HOST_PORT_WEB").map(String::as_str), Some("8007"));
        assert_eq!(
            env.get("DOMAIN").map(String::as_str),
            Some("127.0.0.1:8007")
        );
        assert_eq!(env.get("QUOTED").map(String::as_str), Some("single"));
        assert_eq!(env.get("EXPORTED").map(String::as_str), Some("yes"));
        assert_eq!(env.get("EMPTY").map(String::as_str), Some(""));
        assert!(!env.contains_key("# comment"));
    }

    #[test]
    fn dotenv_does_not_expand_variables() {
        // Reading a file is not evaluating it.
        let env = parse_dotenv("A=1\nB=${A}/x\n");
        assert_eq!(env.get("B").map(String::as_str), Some("${A}/x"));
    }

    #[test]
    fn normalize_resolves_dot_dot_without_touching_the_disk() {
        assert_eq!(
            normalize(Path::new("/a/b/../c")),
            PathBuf::from("/a/c"),
            "the ../{{name}} layout depends on this"
        );
        assert_eq!(normalize(Path::new("/a/./b")), PathBuf::from("/a/b"));
        assert_eq!(
            normalize(Path::new("/does/not/exist/../x")),
            PathBuf::from("/does/not/x")
        );
    }

    #[test]
    fn normalize_never_climbs_past_the_root() {
        assert_eq!(normalize(Path::new("/../..")), PathBuf::from("/"));
    }

    #[test]
    fn fake_git_records_mutations_but_not_reads() {
        let git = FakeGit::with_main("/repo", "main").with_local_branches(&["develop"]);

        git.list_worktrees(Path::new("/repo")).unwrap();
        git.branches(Path::new("/repo"), BranchFilter::Both)
            .unwrap();
        git.status(Path::new("/repo")).unwrap();
        git.prune_worktrees(Path::new("/repo")).unwrap();
        assert!(
            !git.was_mutated(),
            "reads and prune must not count as mutations"
        );

        git.add_worktree(
            Path::new("/repo"),
            &AddOptions {
                path: PathBuf::from("/wt/a"),
                branch: Some(BranchRef::new("task/a")),
                start_point: "main".to_owned(),
                track: TrackMode::NoTrack,
                create_branch: true,
            },
        )
        .unwrap();
        assert_eq!(git.mutations(), vec!["add_worktree /wt/a"]);
        assert_eq!(git.list_worktrees(Path::new("/repo")).unwrap().len(), 2);
    }

    #[test]
    fn fake_runner_replays_then_falls_back_to_success() {
        let runner = FakeRunner::scripted(vec![FakeRunner::ok("first")]);
        let cancel = CancelToken::new();
        let inv = Invocation::new(vec!["git".to_owned(), "status".to_owned()], "/repo", 1000);

        assert_eq!(runner.run(&inv, &cancel).unwrap().stdout, "first");
        assert_eq!(runner.run(&inv, &cancel).unwrap().stdout, "");
        assert_eq!(runner.call_count(), 2);
        assert!(runner.ran_containing("git status"));
    }

    #[test]
    fn fake_runner_which_respects_a_declared_program_list() {
        let runner = FakeRunner::new().with_programs(&["git"]);
        assert!(runner.which("git").is_some());
        assert!(
            runner.which("just").is_none(),
            "undeclared programs must not resolve"
        );
        // An empty list means everything resolves.
        assert!(FakeRunner::new().which("anything").is_some());
    }

    #[test]
    fn fake_clock_only_moves_when_told() {
        let clock = FakeClock::new();
        let before = clock.now_unix_ms();
        assert_eq!(clock.now_unix_ms(), before, "must not drift");
        clock.advance_ms(5_000);
        assert_eq!(clock.now_unix_ms(), before + 5_000);
        assert_eq!(clock.today(), "2026-07-28");
    }
}

// ─────────────────────────────── PtyHost ───────────────────────────────

/// An in-memory pty host.
///
/// Records what was spawned and returns a configurable outcome without touching a real
/// terminal, so pipeline tests can cover the setup stage — including its failure paths —
/// without spawning processes.
#[derive(Debug)]
pub struct FakePty {
    spawns: Mutex<Vec<Invocation>>,
    outcome: Mutex<wtm_core::model::ExitOutcome>,
    fail_spawn: Mutex<Option<String>>,
    next_id: Mutex<u32>,
}

impl Default for FakePty {
    fn default() -> Self {
        Self {
            spawns: Mutex::new(Vec::new()),
            outcome: Mutex::new(wtm_core::model::ExitOutcome::Success),
            fail_spawn: Mutex::new(None),
            next_id: Mutex::new(0),
        }
    }
}

impl FakePty {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Make every session finish with `outcome`.
    #[must_use]
    pub fn with_outcome(self, outcome: wtm_core::model::ExitOutcome) -> Self {
        *self.outcome.lock() = outcome;
        self
    }

    /// Make `spawn` itself fail, for the "setup could not even start" path.
    #[must_use]
    pub fn failing_spawn(self, message: &str) -> Self {
        *self.fail_spawn.lock() = Some(message.to_owned());
        self
    }

    /// Every invocation spawned, in order.
    #[must_use]
    pub fn spawns(&self) -> Vec<Invocation> {
        self.spawns.lock().clone()
    }
}

impl wtm_core::ports::pty::PtyHost for FakePty {
    fn spawn(
        &self,
        inv: &Invocation,
        _rows: u16,
        _cols: u16,
        _worktree: Option<&str>,
        sink: std::sync::Arc<dyn wtm_core::ports::pty::PtySink>,
    ) -> Result<wtm_core::ports::pty::Spawned, ExecError> {
        if let Some(message) = self.fail_spawn.lock().clone() {
            return Err(ExecError::Spawn {
                argv: inv.display(),
                message,
            });
        }

        self.spawns.lock().push(inv.clone());

        let mut next = self.next_id.lock();
        *next += 1;
        let session = wtm_core::model::SessionId::new(format!("fake-{next}"));
        drop(next);

        // Emit something, so a test can assert the sink is actually wired.
        sink.on_output(&session, b"fake pty output\n");
        sink.on_exit(&session, &self.outcome.lock().clone());

        Ok(wtm_core::ports::pty::Spawned {
            session,
            argv: inv.argv.clone(),
        })
    }

    fn wait(
        &self,
        _session: &wtm_core::model::SessionId,
        _cancel: &CancelToken,
    ) -> Result<wtm_core::model::ExitOutcome, ExecError> {
        Ok(self.outcome.lock().clone())
    }

    fn write(&self, _session: &wtm_core::model::SessionId, _data: &[u8]) -> Result<(), ExecError> {
        Ok(())
    }

    fn resize(
        &self,
        _session: &wtm_core::model::SessionId,
        _rows: u16,
        _cols: u16,
    ) -> Result<(), ExecError> {
        Ok(())
    }

    fn kill(&self, _session: &wtm_core::model::SessionId) -> Result<(), ExecError> {
        Ok(())
    }

    fn sessions(&self) -> Vec<wtm_core::ports::pty::PtySession> {
        Vec::new()
    }
}

/// A sink that discards everything.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullPtySink;

impl wtm_core::ports::pty::PtySink for NullPtySink {
    fn on_output(&self, _session: &wtm_core::model::SessionId, _chunk: &[u8]) {}
    fn on_exit(
        &self,
        _session: &wtm_core::model::SessionId,
        _outcome: &wtm_core::model::ExitOutcome,
    ) {
    }
}

/// In-memory [`PipeHost`](wtm_core::ports::pipe::PipeHost).
///
/// Records what was spawned and what was written, and hands back canned lines. Unlike
/// [`FakePty`] it does **not** emit on spawn: the protocols this port carries begin with a
/// handshake the caller writes, so a fake that spoke first would let a test pass against a
/// provider adapter that never sent one.
pub struct FakePipe {
    spawns: Mutex<Vec<Invocation>>,
    writes: Mutex<Vec<String>>,
    /// Lines handed to the sink on the next write, in order. Drained as they are used, so a
    /// test can script a conversation turn by turn.
    replies: Mutex<Vec<Vec<String>>>,
    outcome: Mutex<wtm_core::model::ExitOutcome>,
    fail_spawn: Mutex<Option<String>>,
    stdin_closed: Mutex<bool>,
    killed: Mutex<bool>,
    sink: Mutex<Option<std::sync::Arc<dyn wtm_core::ports::pipe::PipeSink>>>,
    session: Mutex<Option<wtm_core::model::SessionId>>,
}

/// Hand-written because a `dyn PipeSink` is not `Debug` and deriving would demand it of every
/// sink a test writes. Reports the counts, which is what a failure message wants anyway.
impl std::fmt::Debug for FakePipe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakePipe")
            .field("spawns", &self.spawns.lock().len())
            .field("writes", &self.writes.lock().len())
            .finish_non_exhaustive()
    }
}

/// Hand-written for the same reason [`FakePty`]'s is: `ExitOutcome` has no `Default`, and
/// picking one here rather than in the domain is right — a fake's happy path is a test-fixture
/// decision, not a property of the type.
impl Default for FakePipe {
    fn default() -> Self {
        Self {
            spawns: Mutex::new(Vec::new()),
            writes: Mutex::new(Vec::new()),
            replies: Mutex::new(Vec::new()),
            outcome: Mutex::new(wtm_core::model::ExitOutcome::Success),
            fail_spawn: Mutex::new(None),
            stdin_closed: Mutex::new(false),
            killed: Mutex::new(false),
            sink: Mutex::new(None),
            session: Mutex::new(None),
        }
    }
}

impl FakePipe {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue the lines the fake emits in response to the next write.
    pub fn reply_with(&self, lines: &[&str]) {
        self.replies
            .lock()
            .push(lines.iter().map(|l| (*l).to_owned()).collect());
    }

    pub fn fail_spawn_with(&self, message: impl Into<String>) {
        self.fail_spawn.lock().replace(message.into());
    }

    #[must_use]
    pub fn spawned(&self) -> Vec<Invocation> {
        self.spawns.lock().clone()
    }

    #[must_use]
    pub fn written(&self) -> Vec<String> {
        self.writes.lock().clone()
    }

    #[must_use]
    pub fn stdin_is_closed(&self) -> bool {
        *self.stdin_closed.lock()
    }

    #[must_use]
    pub fn was_killed(&self) -> bool {
        *self.killed.lock()
    }

    /// Emit `line` as though the child had written it, outside any write.
    ///
    /// For the half of these protocols the caller does not drive: a server-initiated approval
    /// request arrives on its own schedule, not as a reply to anything.
    pub fn emit(&self, line: &str) {
        let sink = self.sink.lock().clone();
        let session = self.session.lock().clone();
        if let (Some(sink), Some(session)) = (sink, session) {
            sink.on_line(&session, line);
        }
    }

    /// End the session, as the real host does when the child exits.
    pub fn finish(&self) {
        let sink = self.sink.lock().clone();
        let session = self.session.lock().clone();
        if let (Some(sink), Some(session)) = (sink, session) {
            sink.on_exit(&session, &self.outcome.lock().clone());
        }
    }
}

impl wtm_core::ports::pipe::PipeHost for FakePipe {
    fn spawn(
        &self,
        inv: &Invocation,
        _worktree: Option<&str>,
        sink: std::sync::Arc<dyn wtm_core::ports::pipe::PipeSink>,
    ) -> Result<wtm_core::ports::pty::Spawned, ExecError> {
        if let Some(message) = self.fail_spawn.lock().clone() {
            return Err(ExecError::Spawn {
                argv: inv.display(),
                message,
            });
        }

        self.spawns.lock().push(inv.clone());
        let session = wtm_core::model::SessionId::new("fake-pipe-1");
        self.sink.lock().replace(sink);
        self.session.lock().replace(session.clone());

        Ok(wtm_core::ports::pty::Spawned {
            session,
            argv: inv.argv.clone(),
        })
    }

    fn write_line(
        &self,
        session: &wtm_core::model::SessionId,
        line: &str,
    ) -> Result<(), ExecError> {
        if *self.stdin_closed.lock() {
            return Err(ExecError::NoSuchSession(session.as_str().to_owned()));
        }
        self.writes.lock().push(line.to_owned());

        let scripted = {
            let mut replies = self.replies.lock();
            if replies.is_empty() {
                None
            } else {
                Some(replies.remove(0))
            }
        };
        // Emitted outside the `replies` lock: a sink is free to call back in, and holding it
        // across that would be a self-deadlock a test could only diagnose by hanging.
        if let Some(lines) = scripted {
            for reply in lines {
                self.emit(&reply);
            }
        }
        Ok(())
    }

    fn close_stdin(&self, _session: &wtm_core::model::SessionId) -> Result<(), ExecError> {
        *self.stdin_closed.lock() = true;
        Ok(())
    }

    fn kill(&self, _session: &wtm_core::model::SessionId) -> Result<(), ExecError> {
        *self.killed.lock() = true;
        Ok(())
    }

    fn sessions(&self) -> Vec<wtm_core::ports::pipe::PipeSession> {
        Vec::new()
    }
}

/// A [`PipeSink`](wtm_core::ports::pipe::PipeSink) that discards everything.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullPipeSink;

impl wtm_core::ports::pipe::PipeSink for NullPipeSink {
    fn on_line(&self, _session: &wtm_core::model::SessionId, _line: &str) {}
    fn on_stderr(&self, _session: &wtm_core::model::SessionId, _line: &str) {}
    fn on_exit(
        &self,
        _session: &wtm_core::model::SessionId,
        _outcome: &wtm_core::model::ExitOutcome,
    ) {
    }
}
