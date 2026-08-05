//! An agent session reaches the real CLI, completes a real handshake, and dies with the app.
//!
//! Four properties, none visible to a unit test, each failing in a way that looks like something
//! else.
//!
//! **The handshake must actually complete against the installed binary.** `wtm-agent`'s mapping
//! tests feed the driver recorded lines, which proves the mapping and nothing about whether the
//! argv is right, whether `codex app-server` accepts our `clientInfo`, or whether a reply arrives
//! in the shape the driver expects. Every one of those would present as a pane that says
//! "starting…" forever with no error anywhere.
//!
//! **It must not be waited on.** Opening a session spawns and returns. Anything that blocks until
//! the handshake finishes presents as "the app froze when I opened a chat", with a stack trace
//! pointing at Tauri's thread pool rather than at the line responsible — the same trap
//! `terminals.rs` guards for shells.
//!
//! **A worktree may have several, addressable independently.** The index is keyed by session for
//! that reason; keyed by worktree, a second session would evict the first and the symptom would be
//! a pane whose messages went somewhere else.
//!
//! **It must die with the app.** A piped child gets no `SIGHUP` from a closing tty — there is no
//! tty — and `process_group(0)` puts it in its own group, so nothing about the parent exiting
//! reaches it. A leaked agent CLI holds a model connection open and may still be mid-turn.
//!
//! # No API credit is spent
//!
//! Nothing here sends a turn. `initialize` → `initialized` → `thread/start` is local: it reads
//! `~/.codex/config.toml`, starts the configured MCP servers and opens a thread record. Verified by
//! running it with the network down. A test that spent money on every `just check` would be a test
//! people disable.
//!
//! # Why this skips rather than fails without the CLI
//!
//! `codex` is not a dependency of wtm and CI does not install it. A hard failure there would make
//! the suite red for a reason unrelated to the change under test, and `#[ignore]` would mean it
//! never runs anywhere. So it skips loudly, and the skip names what was missing.

// `Instant::now` is banned so use-cases take the `Clock` port and stay deterministic. Nothing here
// is a use-case: these are wall-clock waits on a real child process, and the whole point is that no
// code path waits on a session, so there is no completion to await and no clock to inject.
//
// `print_stderr` is banned so the app never writes outside its tracing setup. A skip that does not
// say why it skipped is indistinguishable from a pass, which is the one thing a conditionally-run
// test must not be — `real_create.rs` grants itself the same allowance for the same reason.
#![allow(clippy::unwrap_used, clippy::disallowed_methods, clippy::print_stderr)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use wtm_agent::session::AgentSink;
use wtm_core::model::{AgentEvent, ExitOutcome, SessionId};
use wtm_core::ports::pipe::PipeHost;
use wtm_exec::{PipeHostImpl, ResolvedPath};

/// Collects a session's normalized events.
#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<AgentEvent>>,
    ready: Mutex<bool>,
    exit: Mutex<Option<ExitOutcome>>,
}

impl AgentSink for Recorder {
    fn on_event(&self, _session: &SessionId, event: &AgentEvent) {
        self.events.lock().push(event.clone());
    }
    fn on_exit(&self, _session: &SessionId, outcome: &ExitOutcome) {
        self.exit.lock().replace(outcome.clone());
    }
    fn on_ready(&self, _session: &SessionId) {
        *self.ready.lock() = true;
    }
}

impl Recorder {
    /// Whatever the session reported that a failure message should quote.
    fn diagnosis(&self) -> String {
        self.events
            .lock()
            .iter()
            .filter_map(|e| match e {
                AgentEvent::Notice { message, .. } | AgentEvent::Failed { message } => {
                    Some(message.clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" | ")
    }
}

/// Block until `done`, or give up. Polling, because what is being waited for is a sink call on
/// another thread and there is nothing to signal from.
fn settle(done: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    false
}

/// The catalogue entry, if its CLI is installed. Skips loudly otherwise.
fn installed(id: &str) -> Option<(&'static wtm_agent::ProviderEntry, PipeHostImpl)> {
    let entry = wtm_agent::entry(id).expect("the catalogue must contain the id under test");
    let path = ResolvedPath::resolve(None);
    let program = entry.provider.program();
    if path.which(program, &std::env::temp_dir()).is_none() {
        eprintln!(
            "skipping: no `{program}` on the resolved PATH ({})",
            path.value
        );
        return None;
    }
    Some((entry, PipeHostImpl::new(path)))
}

fn request() -> wtm_agent::SessionRequest {
    wtm_agent::SessionRequest {
        // The temp dir rather than a git fixture: `thread/start` only needs a directory that
        // exists, and building a repo would test `GitFixture` rather than the handshake.
        cwd: std::env::temp_dir().to_string_lossy().into_owned(),
        ..wtm_agent::SessionRequest::default()
    }
}

const WEEK_MS: u64 = 7 * 24 * 60 * 60 * 1_000;

#[test]
fn a_codex_session_completes_its_handshake_against_the_real_app_server() {
    let Some((entry, host)) = installed("codex") else {
        return;
    };

    let recorder = Arc::new(Recorder::default());
    let sink: Arc<dyn AgentSink> = Arc::clone(&recorder) as Arc<dyn AgentSink>;
    let host: Arc<dyn PipeHost> = Arc::new(host);

    let opened = Instant::now();
    let session =
        wtm_agent::AgentSession::open(entry.provider, &request(), host, &sink, WEEK_MS, None)
            .expect("the app server should spawn");

    // Spawned and returned, not waited on. Generous, because the assertion is "returns promptly"
    // rather than a benchmark — the handshake itself takes seconds and would blow this away.
    assert!(
        opened.elapsed() < Duration::from_secs(2),
        "opening a session must not wait on its handshake"
    );

    assert!(
        settle(|| *recorder.ready.lock()),
        "the handshake never completed. The session said: {}",
        recorder.diagnosis()
    );

    // Ready means a thread exists and its id came back where the driver looks for it — nested at
    // `result.thread.id`, which is the detail a fixture invented from the schema would get wrong.
    let ready = recorder
        .events
        .lock()
        .iter()
        .find_map(|e| match e {
            AgentEvent::SessionReady {
                provider_session_id,
                ..
            } => Some(provider_session_id.clone()),
            _ => None,
        })
        .expect("a SessionReady event carrying the provider's own thread id");
    assert!(
        !ready.is_empty(),
        "the thread id came back empty, so resume would have nothing to store"
    );

    session.close().expect("close");
}

#[test]
fn two_sessions_in_one_worktree_are_independent() {
    // Keyed by worktree, the second would evict the first and its messages would go somewhere
    // else. This is the shape difference from the terminal dock, where a worktree has one shell.
    let Some((entry, host)) = installed("codex") else {
        return;
    };

    let host: Arc<dyn PipeHost> = Arc::new(host);
    let first = Arc::new(Recorder::default());
    let second = Arc::new(Recorder::default());

    let a = wtm_agent::AgentSession::open(
        entry.provider,
        &request(),
        Arc::clone(&host),
        &(Arc::clone(&first) as Arc<dyn AgentSink>),
        WEEK_MS,
        Some("/tmp/worktree"),
    )
    .expect("spawn the first");
    let b = wtm_agent::AgentSession::open(
        entry.provider,
        &request(),
        Arc::clone(&host),
        &(Arc::clone(&second) as Arc<dyn AgentSink>),
        WEEK_MS,
        Some("/tmp/worktree"),
    )
    .expect("spawn the second");

    assert_ne!(a.id(), b.id(), "each session must have its own id");
    assert_eq!(
        host.sessions().len(),
        2,
        "both sessions must be running at once"
    );

    assert!(
        settle(|| *first.ready.lock() && *second.ready.lock()),
        "both handshakes should complete. First said: {} / second said: {}",
        first.diagnosis(),
        second.diagnosis()
    );

    a.close().expect("close the first");
    assert!(
        settle(|| first.exit.lock().is_some()),
        "the first session should end"
    );
    // The surviving session is untouched, which is the property: closing one pane must not take
    // its neighbour with it.
    assert!(
        second.exit.lock().is_none(),
        "closing one session ended the other"
    );

    b.close().expect("close the second");
}

#[test]
fn closing_a_session_ends_the_cli_rather_than_leaving_it_running() {
    // A piped child gets no SIGHUP from a closing tty — there is no tty — and it is its own
    // process group, so nothing about the parent exiting reaches it. The failure is silent and
    // cumulative: an agent CLI per pane per launch, discovered later in `ps`.
    let Some((entry, host)) = installed("codex") else {
        return;
    };

    let recorder = Arc::new(Recorder::default());
    let host: Arc<dyn PipeHost> = Arc::new(host);
    let session = wtm_agent::AgentSession::open(
        entry.provider,
        &request(),
        Arc::clone(&host),
        &(Arc::clone(&recorder) as Arc<dyn AgentSink>),
        WEEK_MS,
        None,
    )
    .expect("spawn");

    assert!(settle(|| *recorder.ready.lock()), "handshake");
    session.close().expect("close");

    assert!(
        settle(|| recorder.exit.lock().is_some()),
        "the CLI outlived the session that owned it"
    );
    assert!(
        settle(|| host.sessions().is_empty()),
        "a closed session must not still be reported as running"
    );
}
