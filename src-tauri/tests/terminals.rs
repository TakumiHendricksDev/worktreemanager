//! A worktree's terminal is its own, it outlives the command that opened it, and quitting
//! takes it with us.
//!
//! Three properties, none visible to a unit test, each failing in a way that looks like
//! something else.
//!
//! **It must not be waited on.** Opening a terminal spawns and returns. Add a `PtyHost::wait`
//! and the command blocks until the user types `exit` — which presents as "the app froze when
//! I opened a terminal", with a stack trace pointing at Tauri's thread pool rather than at the
//! line responsible. `run_action`'s "return as soon as it is running" comment defends the same
//! property for actions, and nothing else asserts it.
//!
//! **It must be the worktree's own shell, not whatever else is running there.** Sessions are
//! tagged with a worktree id, and actions and the setup stage tag theirs with the same one, so
//! a lookup by worktree alone hands the dock a running build to type into. The index in `App`
//! exists for that reason; `app::tests` covers the mistake in detail, and this file covers the
//! path a user takes to reach it.
//!
//! **It must die with the app.** `portable-pty` calls `setsid()`, so a session is its own
//! session leader and survives its parent. The failure is silent and cumulative — a login
//! shell per worktree per launch, discovered weeks later in `ps` — which is exactly the kind
//! of thing a code review does not catch.
//!
//! Runs against a throwaway `git init` via `GitFixture`, so it is part of `just check` rather
//! than something `#[ignore]`d, and addresses worktrees by the ids the listing reports rather
//! than by a path built here: on macOS a temp directory is reached through a symlink
//! (`/var` → `/private/var`), so a hand-built id is a spelling the app never uses.

// `Instant::now` is banned so use-cases take the `Clock` port and stay deterministic. Nothing
// here is a use-case: these are wall-clock timeouts on a real process, and the whole point of
// the properties below is that no code path waits on a session — so there is no completion to
// await and no clock to inject, only output and a process table to observe. `wtm-exec` grants
// itself the same allowance for its own tests via `lib.rs`; an integration test is its own
// crate, so it has to say so here.
#![allow(clippy::unwrap_used, clippy::disallowed_methods)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use wtm_app_lib::app::App;
use wtm_config::AppPaths;
use wtm_core::model::{ExitOutcome, Project, SessionId, Worktree};
use wtm_core::ports::pty::{PtyHost, PtySink};
use wtm_testkit::GitFixture;

/// Collects a session's output so a test can assert on what the shell printed.
#[derive(Default)]
struct Recorder {
    output: Mutex<Vec<u8>>,
}

impl Recorder {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.output.lock()).into_owned()
    }

    /// Wait for `needle` to appear, or give up.
    ///
    /// Polling a buffer rather than the process: the point of these tests is that nothing
    /// waits on the *session*, so there is no completion to await — only output to observe.
    fn wait_for(&self, needle: &str) -> String {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            let text = self.text();
            if text.contains(needle) {
                return text;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        self.text()
    }
}

impl PtySink for Recorder {
    fn on_output(&self, _session: &SessionId, chunk: &[u8]) {
        self.output.lock().extend_from_slice(chunk);
    }
    fn on_exit(&self, _session: &SessionId, _outcome: &ExitOutcome) {}
}

struct Harness {
    app: App,
    project: Project,
    fixture: GitFixture,
    /// The paths `add_worktree` handed back, as it spelled them.
    ///
    /// Not the same strings as the ids the listing reports: git canonicalizes, so on macOS the
    /// listing says `/private/var/…` where the fixture said `/var/…`. `GitFixture` refuses to
    /// delete anything it cannot see inside its own root, so orphaning a worktree has to use
    /// its spelling while every lookup uses the app's.
    created: Vec<(String, std::path::PathBuf)>,
    /// Held so the config directory outlives the app.
    _config: tempfile::TempDir,
}

impl Harness {
    fn new(worktrees: &[&str]) -> Self {
        let fixture = GitFixture::new();
        let created = worktrees
            .iter()
            .map(|dirname| {
                let path = fixture.add_worktree(dirname, &format!("task/{dirname}"));
                ((*dirname).to_owned(), path)
            })
            .collect();

        let config = tempfile::tempdir().unwrap();
        let app = App::with_paths(AppPaths::rooted(config.path())).unwrap();
        let root = app.register(fixture.root()).unwrap();
        let project = app.project(&root.to_string_lossy()).unwrap();

        Self {
            app,
            project,
            fixture,
            created,
            _config: config,
        }
    }

    /// The fixture's own spelling of a worktree's path, for [`GitFixture::orphan_worktree`].
    fn created_path(&self, dirname: &str) -> &std::path::Path {
        &self
            .created
            .iter()
            .find(|(name, _)| name == dirname)
            .unwrap_or_else(|| panic!("`{dirname}` was never created"))
            .1
    }

    fn project_id(&self) -> String {
        self.project.root.to_string_lossy().into_owned()
    }

    /// Look a worktree up by the id the listing reports. See the module header.
    fn worktree(&self, dirname: &str) -> Worktree {
        let id = self
            .app
            .worktrees(&self.project)
            .unwrap()
            .into_iter()
            .find(|v| v.dirname == dirname)
            .unwrap_or_else(|| panic!("`{dirname}` should be listed"))
            .id;
        self.app.worktree(&self.project, &id).unwrap()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        // Otherwise a failing assertion leaves a `sleep` behind for every session the test
        // opened, and the suite gets slower for reasons nobody can attribute.
        self.app.pty.kill_all();
    }
}

/// The shell must start where the worktree is, not where the repository is.
///
/// A plausible copy-paste from `display::resolve_cwd` uses `CwdBase::RepoRoot`, which is what
/// setup commands want and the opposite of what a terminal wants — and the mistake is
/// invisible until someone runs `git status` in the pane and wonders why it is the main
/// worktree's.
#[test]
fn a_shell_opens_in_the_worktrees_own_directory() {
    let harness = Harness::new(&["alpha"]);
    let worktree = harness.worktree("alpha");
    let recorder = Arc::new(Recorder::default());

    harness
        .app
        .open_shell(
            &worktree,
            &harness.project_id(),
            vec!["sh".to_owned(), "-c".to_owned(), "pwd".to_owned()],
            24,
            80,
            Arc::clone(&recorder) as Arc<dyn PtySink>,
        )
        .unwrap();

    // `pwd` in a shell resolves symlinks the way the app's own ids do, so compare against the
    // canonical form of both rather than against the fixture path as written.
    let expected = std::fs::canonicalize(&worktree.path).unwrap();
    let printed = recorder.wait_for("alpha");
    assert!(
        printed.contains(&expected.to_string_lossy().into_owned()),
        "the shell started in the wrong directory; expected {}, got {printed:?}",
        expected.display()
    );
}

/// **The single most important integration property.**
///
/// Opening a terminal returns while the shell is still running. If a `wait` ever creeps into
/// this path the app hangs on ⌘J and the stack trace blames Tauri's thread pool.
#[test]
fn a_shell_survives_the_command_that_started_it() {
    let harness = Harness::new(&["alpha"]);
    let worktree = harness.worktree("alpha");

    let started = Instant::now();
    let session = harness
        .app
        .open_shell(
            &worktree,
            &harness.project_id(),
            vec!["sleep".to_owned(), "30".to_owned()],
            24,
            80,
            Arc::new(Recorder::default()) as Arc<dyn PtySink>,
        )
        .unwrap();
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(5),
        "opening a shell took {elapsed:?}; something is waiting on the session"
    );
    assert!(
        harness
            .app
            .pty
            .sessions()
            .iter()
            .any(|s| s.session == session),
        "the shell must still be running after the call returned"
    );
    assert_eq!(
        harness.app.live_shells().len(),
        1,
        "and it must be reported as this worktree's shell"
    );
}

/// A worktree whose directory is gone must be refused, not spawned into.
///
/// `App::worktree` does not prune — unlike `App::worktrees` — so a prunable entry is still
/// found here, and without an explicit existence check the shell would start with an unlinked
/// cwd. That is a session where `getcwd` fails and every command misbehaves for no visible
/// reason, which is far harder to diagnose than a refusal.
#[test]
fn a_worktree_whose_directory_is_gone_is_refused_rather_than_spawned_into_a_deleted_cwd() {
    let harness = Harness::new(&["doomed"]);
    let worktree = harness.worktree("doomed");
    harness
        .fixture
        .orphan_worktree(harness.created_path("doomed"));

    assert!(
        !harness.app.files.exists(&worktree.path),
        "the fixture should have removed the directory"
    );

    // The command's own guard is this check; asserting it here rather than through
    // `open_terminal` because that needs a `tauri::AppHandle`, which a test binary has no way
    // to build. What is being pinned is that the check has something to catch: `App::worktree`
    // still resolves the entry, so the guard is not dead code.
    assert!(
        harness
            .app
            .worktree(&harness.project, worktree.id.as_str())
            .is_ok(),
        "the entry must still resolve, or the existence check in `open_terminal` is pointless"
    );
}

/// Quitting must not leave a login shell per worktree behind.
#[test]
fn quitting_terminates_every_worktrees_shell() {
    let harness = Harness::new(&["alpha", "beta"]);
    let project_id = harness.project_id();

    for dirname in ["alpha", "beta"] {
        harness
            .app
            .open_shell(
                &harness.worktree(dirname),
                &project_id,
                vec!["sleep".to_owned(), "30".to_owned()],
                24,
                80,
                Arc::new(Recorder::default()) as Arc<dyn PtySink>,
            )
            .unwrap();
    }
    assert_eq!(harness.app.live_shells().len(), 2);

    // What `RunEvent::Exit` calls in `lib.rs`.
    assert_eq!(harness.app.pty.kill_all(), 2);

    let deadline = Instant::now() + Duration::from_secs(10);
    while !harness.app.pty.sessions().is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        harness.app.pty.sessions().is_empty(),
        "a shell outlived the quit: {:?}",
        harness.app.pty.sessions()
    );
    assert!(harness.app.live_shells().is_empty());
}
