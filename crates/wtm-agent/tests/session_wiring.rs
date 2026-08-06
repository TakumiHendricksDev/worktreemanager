//! `AgentSession` applies what a driver returns, and closes a session without leaving it blocked.
//!
//! The mapping tests in `codex_mapping.rs` prove what a driver *says*. This proves that the thin
//! layer bolting it to a `PipeHost` actually writes the frames, in order, and calls `abandon` on the
//! way out — none of which a pure state machine can be asked about.
//!
//! `FakePipe` rather than a real CLI, deliberately: every property here is about wiring, and a real
//! `codex` would add a handshake, a network round trip and a reason to be flaky without proving
//! anything extra. The real binary is covered by `src-tauri/tests/agent_sessions.rs`.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use parking_lot::Mutex;
use pretty_assertions::assert_eq;
use wtm_agent::codex::Codex;
use wtm_agent::provider::SessionRequest;
use wtm_agent::session::{AgentSession, AgentSink};
use wtm_core::model::{AgentEvent, ApprovalAnswer, ExitOutcome, SessionId};
use wtm_core::ports::pipe::PipeHost;
use wtm_testkit::FakePipe;

#[derive(Default)]
struct Recorder {
    events: Mutex<Vec<AgentEvent>>,
    ready: Mutex<bool>,
}

impl AgentSink for Recorder {
    fn on_event(&self, _session: &SessionId, event: &AgentEvent) {
        self.events.lock().push(event.clone());
    }
    fn on_exit(&self, _session: &SessionId, _outcome: &ExitOutcome) {}
    fn on_ready(&self, _session: &SessionId) {
        *self.ready.lock() = true;
    }
}

/// A session on a fake pipe, plus the fake and the recorder to inspect.
fn session() -> (AgentSession, Arc<FakePipe>, Arc<Recorder>) {
    let recorder = Arc::new(Recorder::default());
    let (session, fake) = session_with(&(Arc::clone(&recorder) as Arc<dyn AgentSink>));
    (session, fake, recorder)
}

/// The same, for a test that needs its own sink.
fn session_with(sink: &Arc<dyn AgentSink>) -> (AgentSession, Arc<FakePipe>) {
    let fake = Arc::new(FakePipe::new());
    let host: Arc<dyn PipeHost> = Arc::clone(&fake) as Arc<dyn PipeHost>;

    let session = AgentSession::open(
        &Codex,
        &SessionRequest {
            cwd: "/tmp/worktree".to_owned(),
            ..SessionRequest::default()
        },
        host,
        sink,
        60_000,
        Some("/tmp/worktree"),
    )
    .expect("the fake always spawns");

    (session, fake)
}

/// Walk the fake through Codex's two-step handshake.
fn handshake(fake: &FakePipe) {
    fake.emit(r#"{"id":1,"result":{"userAgent":"wtm/0.144.6"}}"#);
    fake.emit(r#"{"id":2,"result":{"thread":{"id":"thread-1"}}}"#);
}

#[test]
fn the_handshake_is_written_before_open_returns() {
    // The sink is installed before the spawn for exactly this: a provider's first frames go out
    // immediately, and its reply can land before `open` has returned. Attaching afterwards would
    // lose whatever arrived in between — the race `Terminal.svelte` solves by buffering, solved
    // here instead so the frontend never has to.
    let (_session, fake, _rec) = session();
    let written = fake.written();
    assert_eq!(written.len(), 1, "expected exactly `initialize`");
    let frame: serde_json::Value = serde_json::from_str(&written[0]).unwrap();
    assert_eq!(frame["method"], "initialize");
}

#[test]
fn a_line_from_the_child_drives_the_driver_and_its_frames_reach_the_pipe() {
    let (_session, fake, rec) = session();
    handshake(&fake);

    assert!(*rec.ready.lock(), "the session should be ready");
    let methods: Vec<String> = fake
        .written()
        .iter()
        .map(|f| serde_json::from_str::<serde_json::Value>(f).unwrap()["method"].to_string())
        .collect();
    assert_eq!(
        methods,
        ["\"initialize\"", "\"initialized\"", "\"thread/start\""],
        "the handshake's three frames, in order"
    );
}

#[test]
fn a_turn_reaches_the_pipe_and_is_echoed_to_the_transcript() {
    let (session, fake, rec) = session();
    handshake(&fake);

    session.send_turn("do the thing").expect("send");

    let last: serde_json::Value = serde_json::from_str(fake.written().last().unwrap()).unwrap();
    assert_eq!(last["method"], "turn/start");
    assert_eq!(last["params"]["input"][0]["text"], "do the thing");
    assert!(
        rec.events.lock().contains(&AgentEvent::UserEcho {
            text: "do the thing".to_owned()
        }),
        "the turn must appear in the transcript, not only on the wire"
    );
}

/// A sink that reads state its caller might already be holding — the shape the real one has.
///
/// `try_lock` rather than `lock`, and that is not a detail: the point is to *detect* re-entrancy,
/// and a test that reproduced the deadlock faithfully would hang the suite instead of failing it.
/// There is no `.config/nextest.toml`, so there is no per-test timeout to rescue one that did.
#[derive(Default)]
struct Reentrant {
    /// Stands in for `App::agents`: state the sink reads, which a caller may already hold.
    state: Mutex<()>,
    /// Set when `on_event` found that state locked — i.e. the caller's guard was still alive.
    blocked: Mutex<bool>,
}

impl AgentSink for Reentrant {
    fn on_event(&self, _session: &SessionId, _event: &AgentEvent) {
        if self.state.try_lock().is_none() {
            *self.blocked.lock() = true;
        }
    }
    fn on_exit(&self, _session: &SessionId, _outcome: &ExitOutcome) {}
    fn on_ready(&self, _session: &SessionId) {}
}

#[test]
fn a_sink_runs_on_the_calling_thread_so_a_caller_must_not_hold_what_it_reads() {
    // The contract behind a real deadlock, pinned here because nothing else states it.
    //
    // `send_turn` applies its steps inline, and the first step of a turn is `Emit(UserEcho)`. So the
    // sink runs on the caller's thread, *inside* whatever scope the caller is in. `src-tauri`'s sink
    // answers `UserEcho` by calling `App::live_agents`, which locks the agent map; `App::with_agent`
    // used to hold that same map's lock across this call, and `parking_lot::Mutex` is not reentrant.
    // Every Send parked forever, holding the map, and every later agent command queued behind it.
    //
    // Reproduced with `try_lock` so this reports the hazard rather than hanging on it.
    let rec = Arc::new(Reentrant::default());
    let (session, fake) = session_with(&(Arc::clone(&rec) as Arc<dyn AgentSink>));
    handshake(&fake);

    {
        // Exactly what `with_agent` used to do: take the lock, then run the session underneath it.
        let _guard = rec.state.lock();
        session.send_turn("under a held lock").expect("send");
    }
    assert!(
        *rec.blocked.lock(),
        "the sink must run inside the caller's scope — if this fails the hazard is gone and \
         `App::agent_session` can be inlined again"
    );

    // And the shape `App` uses now: look the session up, drop the guard, then run it.
    *rec.blocked.lock() = false;
    session.send_turn("with nothing held").expect("send");
    assert!(
        !*rec.blocked.lock(),
        "with no guard alive the sink reaches its own state, which is what makes Send work"
    );
}

#[test]
fn closing_declines_an_outstanding_approval_before_it_closes_stdin() {
    // The property, and the order is the property. A server blocked on an approval reply does not
    // read its stdin closing at all, so closing first leaves a child only the kill can reach —
    // reported as `Signalled`, which in the UI reads as a crash rather than an end.
    let (session, fake, _rec) = session();
    handshake(&fake);
    fake.emit(
        r#"{"jsonrpc":"2.0","id":9,"method":"item/commandExecution/requestApproval","params":{"itemId":"i","threadId":"t","turnId":"t1","startedAtMs":1,"command":"rm -rf node_modules"}}"#,
    );

    assert!(!fake.stdin_is_closed(), "not yet");
    session.close().expect("close");

    let last: serde_json::Value = serde_json::from_str(fake.written().last().unwrap()).unwrap();
    assert_eq!(last["id"], 9, "the decline must answer the outstanding id");
    assert_eq!(last["result"]["decision"], "decline");
    assert!(fake.stdin_is_closed(), "stdin closes after the decline");
    assert!(fake.was_killed(), "and the kill is the backstop");
}

#[test]
fn answering_through_the_session_writes_the_reply() {
    let (session, fake, rec) = session();
    handshake(&fake);
    fake.emit(
        r#"{"jsonrpc":"2.0","id":5,"method":"item/commandExecution/requestApproval","params":{"itemId":"i","threadId":"t","turnId":"t1","startedAtMs":1,"command":"bun install"}}"#,
    );

    let id = rec
        .events
        .lock()
        .iter()
        .find_map(|e| match e {
            AgentEvent::ApprovalRequested { id, .. } => Some(id.clone()),
            _ => None,
        })
        .expect("an approval reached the transcript");

    session.answer(&id, &ApprovalAnswer::Allow).expect("answer");

    let last: serde_json::Value = serde_json::from_str(fake.written().last().unwrap()).unwrap();
    assert_eq!(last["id"], 5);
    assert_eq!(last["result"]["decision"], "accept");
}

#[test]
fn a_write_that_fails_becomes_a_transcript_event_rather_than_a_lost_error() {
    // Lines arrive on the host's reader thread, where there is no caller to return a `Result` to.
    // A broken pipe there has to become something the user can see, or a dead session presents as
    // one that has simply gone quiet.
    let (_session, fake, rec) = session();
    fake.close_stdin(&SessionId::new("fake-pipe-1")).unwrap();

    // The reply to `initialize` makes the driver want to write `initialized` — which now fails.
    fake.emit(r#"{"id":1,"result":{"userAgent":"wtm/0.144.6"}}"#);

    assert!(
        rec.events
            .lock()
            .iter()
            .any(|e| matches!(e, AgentEvent::Failed { .. })),
        "a failed write must be reported, not swallowed"
    );
}
