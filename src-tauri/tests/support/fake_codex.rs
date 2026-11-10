//! A real `App` driving a fake `codex app-server`, with no window.
//!
//! Shared by the authoring and chat-edit tests, whose promises — a brief is never sent twice, an
//! edit is never submitted twice, a retry returns its first outcome — are only worth something if
//! they hold against a real provider process, real Git and the real journal on disk. The provider
//! is `examples/fixtures/plugin_codex.py`, which speaks just enough of the app-server protocol and
//! keeps its threads in the session folder, so a resume or a fork reads what an earlier process
//! wrote. Nothing reaches a model, a network or the user's own Codex state.
//!
//! The window is [`Headless`]: it records what would have been announced, and its sink does the
//! app-state half of the real sink through `agent_bridge::keep`, so the replay, the provider's
//! conversation id and the draft records are exactly what the app itself would hold.

// Each test binary that includes this uses part of it. `pub(crate)` throughout, because the
// module is private to each binary and `unreachable_pub` would flag a bare `pub`.
//
// `Instant::now` is banned so use-cases take the `Clock` port. Nothing here is a use-case: these
// are wall-clock waits on a real child process, the allowance `agent_sessions.rs` makes too.
#![allow(dead_code, clippy::disallowed_methods, clippy::unwrap_used)]

use std::{
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use wtm_agent::session::AgentSink;
use wtm_app_lib::{
    agent_bridge::{self, SpawnedSession, Window},
    app::App,
};
use wtm_config::AppPaths;
use wtm_core::model::{AgentEvent, ExitOutcome, SessionId};

/// The longest any wait here may take. The fixture answers in milliseconds; this only bounds a
/// failure, so that a broken test fails with a message instead of hanging the suite.
const PATIENCE: Duration = Duration::from_secs(20);

pub(crate) struct Harness {
    pub(crate) app: Arc<App>,
    pub(crate) window: Headless,
    pub(crate) bin: PathBuf,
    pub(crate) config: tempfile::TempDir,
    pub(crate) root: tempfile::TempDir,
}

impl Harness {
    /// An app whose PATH finds the fake `codex` first, then the machine's own tools — `git`, and
    /// the `python3` the fixture runs on.
    pub(crate) fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().canonicalize().unwrap().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let codex = bin.join("codex");
        std::fs::write(
            &codex,
            include_str!("../../examples/fixtures/plugin_codex.py"),
        )
        .unwrap();
        std::fs::set_permissions(&codex, std::fs::Permissions::from_mode(0o755)).unwrap();
        let config = tempfile::tempdir().unwrap();
        let path = format!(
            "{}:{}",
            bin.display(),
            wtm_exec::ResolvedPath::resolve(None).value
        );
        std::fs::write(
            config.path().join("config.toml"),
            format!("[exec]\npath = {path:?}\n"),
        )
        .unwrap();
        let app = Arc::new(App::with_paths(AppPaths::rooted(config.path())).unwrap());
        Self {
            window: Headless::new(&app),
            app,
            bin,
            config,
            root,
        }
    }

    /// A folder for a draft, not yet created. Named `generated`, the fixture plays an authoring
    /// agent: a fixed conversation id, and a valid package written on every turn.
    pub(crate) fn folder(&self, name: &str) -> PathBuf {
        self.root.path().canonicalize().unwrap().join(name)
    }

    /// Every `turn/start` the provider received in `folder`, across every process there.
    pub(crate) fn turns_sent(folder: &Path) -> usize {
        std::fs::read_to_string(folder.join(".git/author-turns.jsonl"))
            .map_or(0, |text| text.lines().count())
    }

    /// How many times the schema generator ran.
    pub(crate) fn probes(&self) -> usize {
        std::fs::read_to_string(self.bin.join("probes.log")).map_or(0, |text| text.lines().count())
    }
}

/// Wait for `ready`, or fail saying what was being waited for.
pub(crate) fn wait(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + PATIENCE;
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// A window that records instead of drawing.
pub(crate) struct Headless {
    app: Arc<App>,
    pub(crate) announced: Mutex<Vec<String>>,
    pub(crate) changed: Mutex<Vec<String>>,
}

impl Headless {
    fn new(app: &Arc<App>) -> Self {
        Self {
            app: Arc::clone(app),
            announced: Mutex::new(Vec::new()),
            changed: Mutex::new(Vec::new()),
        }
    }
}

impl Window for Headless {
    fn sink(&self) -> Arc<dyn AgentSink> {
        Arc::new(Sink {
            app: Arc::clone(&self.app),
        })
    }

    fn announce(&self, spawned: &SpawnedSession) {
        self.announced.lock().push(spawned.session.clone());
    }

    fn changed(&self, event: &str) {
        self.changed.lock().push(event.to_owned());
    }
}

/// The app-state half of `AgentEventSink`, in the same order: bookkeeping before anything else,
/// draft records after.
struct Sink {
    app: Arc<App>,
}

impl AgentSink for Sink {
    fn on_event(&self, session: &SessionId, event: &AgentEvent) {
        if matches!(event, AgentEvent::LimitsUpdated(_)) {
            return;
        }
        agent_bridge::keep(&self.app, session, event);
        self.app
            .plugin_jobs
            .observed(&self.app, session.as_str(), event);
    }

    fn on_exit(&self, _session: &SessionId, _outcome: &ExitOutcome) {}

    fn on_ready(&self, _session: &SessionId) {}
}
