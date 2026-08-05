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
    let fake = Arc::new(FakePipe::new());
    let recorder = Arc::new(Recorder::default());
    let sink: Arc<dyn AgentSink> = Arc::clone(&recorder) as Arc<dyn AgentSink>;
    let host: Arc<dyn PipeHost> = Arc::clone(&fake) as Arc<dyn PipeHost>;

    let session = AgentSession::open(
        &Codex,
        &SessionRequest {
            cwd: "/tmp/worktree".to_owned(),
            ..SessionRequest::default()
        },
        host,
        &sink,
        60_000,
        Some("/tmp/worktree"),
    )
    .expect("the fake always spawns");

    (session, fake, recorder)
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
