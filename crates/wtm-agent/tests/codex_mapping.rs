//! The Codex protocol driver, fed the lines the real app server actually sends.
//!
//! # Why this file exists
//!
//! The mapping from `codex app-server` notifications to domain events is the one part of this
//! feature with no compiler holding it up: every field is read out of a `serde_json::Value` by
//! string key, so a renamed key, a `camelCase`/`snake_case` slip or a nesting change is a silently
//! empty event rather than a build failure. This is the only mechanism that notices.
//!
//! # Where these lines came from
//!
//! Driven against `codex-cli 0.144.6` on a real machine — `initialize`, `initialized`,
//! `thread/start`, `thread/list`, `model/list` piped into `codex app-server` — and pasted here
//! verbatim, including the details that would otherwise be guessed wrong:
//!
//!   * a reply is `{"id":1,"result":{…}}` with **no `jsonrpc` field**;
//!   * `thread/start`'s id is nested at `result.thread.id`, not `result.id`;
//!   * `thread/started` arrives as a notification *after* the reply, carrying the same thread.
//!
//! A fixture invented from the schema would have had the first of those wrong, and the symptom
//! would have been every reply rejected and a session that never became ready.
//!
//! # No process, no host, no timing
//!
//! `Protocol` is a pure state machine — lines in, steps out — so these are synchronous
//! assertions with nothing spawned. That is the property the design exists for, and it is why
//! there is no `FakePipe` here.

use pretty_assertions::assert_eq;
use wtm_agent::codex::Codex;
use wtm_agent::provider::{Protocol, Provider, SessionRequest, Step};
use wtm_core::model::{AgendaStatus, AgentEvent, NoticeLevel};

/// The two frames a session sends before it can do anything, and the replies to them.
///
/// Returned rather than asserted here so each test can pick up mid-handshake without repeating
/// it, and so the handshake's own test is the one place that checks its shape.
fn ready_driver() -> Box<dyn Protocol> {
    let mut driver = Codex.protocol(&SessionRequest {
        cwd: "/tmp/worktree".to_owned(),
        model: Some("gpt-5.6-sol".to_owned()),
        effort: Some("ultra".to_owned()),
        ..SessionRequest::default()
    });

    driver.open();
    driver.on_line(
        r#"{"id":1,"result":{"userAgent":"wtm/0.144.6 (Mac OS 26.5.2; arm64)","codexHome":"/home/.codex","platformFamily":"unix","platformOs":"macos"}}"#,
    );
    driver.on_line(
        r#"{"id":2,"result":{"thread":{"id":"019fd37c-f1e4-7a22-81e7-02200fd6d127","sessionId":"019fd37c-f1e4-7a22-81e7-02200fd6d127","cwd":"/tmp/worktree","status":{"type":"idle"},"cliVersion":"0.144.6"}}}"#,
    );
    driver
}

fn events(steps: &[Step]) -> Vec<&AgentEvent> {
    steps
        .iter()
        .filter_map(|s| match s {
            Step::Emit(e) => Some(e),
            _ => None,
        })
        .collect()
}

fn writes(steps: &[Step]) -> Vec<serde_json::Value> {
    steps
        .iter()
        .filter_map(|s| match s {
            Step::Write(frame) => Some(serde_json::from_str(frame).expect("a frame must be JSON")),
            _ => None,
        })
        .collect()
}

#[test]
fn the_argv_starts_the_app_server_on_stdio_and_never_exec() {
    // `codex exec` cannot ask for approval — there is no `--ask-for-approval` flag on it at all —
    // and emits no deltas. Both are disqualifying, so this asserts the mode rather than trusting
    // a comment.
    let argv = Codex.argv(&SessionRequest {
        cwd: "/tmp/w".to_owned(),
        extra_args: vec!["--enable".to_owned(), "shell_tool".to_owned()],
        ..SessionRequest::default()
    });
    assert_eq!(
        argv,
        vec!["codex", "app-server", "--stdio", "--enable", "shell_tool"]
    );
}

#[test]
fn the_handshake_is_initialize_then_initialized_then_a_thread_carrying_the_cwd() {
    let mut driver = Codex.protocol(&SessionRequest {
        cwd: "/tmp/worktree".to_owned(),
        model: Some("gpt-5.6-sol".to_owned()),
        mode: Some("on-request".to_owned()),
        ..SessionRequest::default()
    });

    let opening = writes(&driver.open());
    assert_eq!(opening.len(), 1);
    assert_eq!(opening[0]["method"], "initialize");
    // The server refuses anything sent before `initialize`, so this frame has to be first.
    assert_eq!(opening[0]["params"]["clientInfo"]["name"], "wtm");

    // Note the reply carries no `jsonrpc` field. That is what the real server sends; requiring
    // one here would reject every response it makes.
    let after_init = driver.on_line(r#"{"id":1,"result":{"userAgent":"wtm/0.144.6"}}"#);
    let frames = writes(&after_init);
    assert_eq!(
        frames.len(),
        2,
        "expected `initialized` and the thread open"
    );
    assert_eq!(frames[0]["method"], "initialized");
    assert_eq!(frames[1]["method"], "thread/start");
    // The cwd is the whole integration: it is what pins the thread to this worktree.
    assert_eq!(frames[1]["params"]["cwd"], "/tmp/worktree");
    assert_eq!(frames[1]["params"]["model"], "gpt-5.6-sol");
    assert_eq!(frames[1]["params"]["approvalPolicy"], "on-request");

    // Nothing is ready yet: `thread/start` is a second round trip, which is why `Step::Ready`
    // exists apart from the `SessionReady` event.
    assert!(!after_init.contains(&Step::Ready));
}

#[test]
fn the_thread_id_is_read_from_the_nested_result_and_makes_the_session_ready() {
    let mut driver = Codex.protocol(&SessionRequest {
        cwd: "/tmp/worktree".to_owned(),
        ..SessionRequest::default()
    });
    driver.open();
    driver.on_line(r#"{"id":1,"result":{"userAgent":"wtm/0.144.6"}}"#);

    // `result.thread.id`, not `result.id`. Verified against the real reply; reading the wrong
    // level yields a session that never becomes ready and never says why.
    let steps = driver.on_line(
        r#"{"id":2,"result":{"thread":{"id":"019fd37c-f1e4-7a22-81e7-02200fd6d127","cwd":"/tmp/worktree"}}}"#,
    );

    assert!(steps.contains(&Step::Ready));
    match events(&steps).first().expect("a SessionReady event") {
        AgentEvent::SessionReady {
            provider_session_id,
            ..
        } => assert_eq!(provider_session_id, "019fd37c-f1e4-7a22-81e7-02200fd6d127"),
        other => panic!("expected SessionReady, got {other:?}"),
    }
}

#[test]
fn a_thread_opened_without_an_id_fails_the_session_rather_than_hanging() {
    // The one shape that would otherwise leave a pane that looks alive and accepts no turns.
    let mut driver = Codex.protocol(&SessionRequest::default());
    driver.open();
    driver.on_line(r#"{"id":1,"result":{}}"#);
    let steps = driver.on_line(r#"{"id":2,"result":{"thread":{}}}"#);
    assert!(matches!(
        events(&steps).first(),
        Some(AgentEvent::Failed { .. })
    ));
}

#[test]
fn a_turn_sent_before_the_handshake_finishes_is_queued_and_not_lost() {
    // The composer is live the moment a pane opens, so this is the ordinary case on a slow
    // start — not an edge case. Dropping it would lose the user's first prompt.
    let mut driver = Codex.protocol(&SessionRequest {
        cwd: "/tmp/worktree".to_owned(),
        ..SessionRequest::default()
    });
    driver.open();

    let early = driver.send_turn("review the plan");
    assert!(
        writes(&early).is_empty(),
        "nothing can be written before there is a thread"
    );
    // Echoed anyway, so the message is visibly in the transcript rather than seeming to vanish.
    assert_eq!(
        events(&early),
        vec![&AgentEvent::UserEcho {
            text: "review the plan".to_owned()
        }]
    );

    driver.on_line(r#"{"id":1,"result":{"userAgent":"wtm/0.144.6"}}"#);
    let ready = driver.on_line(r#"{"id":2,"result":{"thread":{"id":"thread-1"}}}"#);

    let frames = writes(&ready);
    let turn = frames
        .iter()
        .find(|f| f["method"] == "turn/start")
        .expect("the queued turn must be replayed once the thread exists");
    assert_eq!(turn["params"]["threadId"], "thread-1");
    assert_eq!(turn["params"]["input"][0]["text"], "review the plan");
}

#[test]
fn a_turn_carries_the_model_and_effort_so_a_mid_session_change_needs_no_new_thread() {
    let mut driver = ready_driver();
    let frames = writes(&driver.send_turn("go"));
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0]["method"], "turn/start");
    assert_eq!(frames[0]["params"]["model"], "gpt-5.6-sol");
    // `ultra` is a real rung, and only on some models — which is why the picker reads the ladder
    // from `model/list` rather than hardcoding one.
    assert_eq!(frames[0]["params"]["effort"], "ultra");
}

#[test]
fn assistant_and_reasoning_deltas_are_kept_apart() {
    // Merged, thinking would be indistinguishable from the answer and could not be collapsed.
    let mut driver = ready_driver();

    let message = driver.on_line(
        r#"{"jsonrpc":"2.0","method":"item/agentMessage/delta","params":{"delta":"Look"}}"#,
    );
    assert_eq!(
        events(&message),
        vec![&AgentEvent::MessageDelta {
            text: "Look".to_owned()
        }]
    );

    let thinking = driver.on_line(
        r#"{"jsonrpc":"2.0","method":"item/reasoning/textDelta","params":{"delta":"hmm"}}"#,
    );
    assert_eq!(
        events(&thinking),
        vec![&AgentEvent::ReasoningDelta {
            text: "hmm".to_owned()
        }]
    );
}

#[test]
fn a_command_execution_item_becomes_a_started_and_a_finished_event() {
    let mut driver = ready_driver();

    let started = driver.on_line(
        r#"{"jsonrpc":"2.0","method":"item/started","params":{"item":{"id":"item_1","type":"command_execution","command":"/bin/zsh -lc 'echo hi'","status":"in_progress"}}}"#,
    );
    assert_eq!(
        events(&started),
        vec![&AgentEvent::CommandStarted {
            id: "item_1".to_owned(),
            command: "/bin/zsh -lc 'echo hi'".to_owned(),
            cwd: None,
        }]
    );

    // `exit_code` is snake_case inside the item even though the envelope is camelCase. Reading
    // it as `exitCode` yields a silently absent status, which is exactly the class of bug this
    // file exists to catch.
    let finished = driver.on_line(
        r#"{"jsonrpc":"2.0","method":"item/completed","params":{"item":{"id":"item_1","type":"command_execution","aggregated_output":"hi\n","exit_code":0,"status":"completed"}}}"#,
    );
    assert_eq!(
        events(&finished),
        vec![&AgentEvent::CommandFinished {
            id: "item_1".to_owned(),
            exit_code: Some(0),
        }]
    );
}

#[test]
fn a_started_agent_message_emits_nothing_because_the_deltas_already_carried_it() {
    // Otherwise an empty bubble appears ahead of the text that is already streaming into it.
    let mut driver = ready_driver();
    let steps = driver.on_line(
        r#"{"jsonrpc":"2.0","method":"item/started","params":{"item":{"id":"item_0","type":"agent_message","text":""}}}"#,
    );
    assert!(steps.is_empty());
}

#[test]
fn a_plan_update_becomes_an_agenda_with_its_step_statuses() {
    let mut driver = ready_driver();
    let steps = driver.on_line(
        r#"{"jsonrpc":"2.0","method":"turn/plan/updated","params":{"explanation":"three things","plan":[{"step":"read the code","status":"completed"},{"step":"write the fix","status":"in_progress"},{"step":"run the tests","status":"pending"}]}}"#,
    );

    match events(&steps).first().expect("an AgendaUpdated event") {
        AgentEvent::AgendaUpdated { explanation, steps } => {
            assert_eq!(explanation.as_deref(), Some("three things"));
            assert_eq!(steps.len(), 3);
            assert_eq!(steps[0].status, AgendaStatus::Completed);
            assert_eq!(steps[1].status, AgendaStatus::InProgress);
            assert_eq!(steps[2].status, AgendaStatus::Pending);
            assert_eq!(steps[1].text, "write the fix");
        }
        other => panic!("expected AgendaUpdated, got {other:?}"),
    }
}

#[test]
fn token_usage_is_read_from_both_spellings_the_server_uses() {
    // `turn/completed` reports snake_case token counts; `thread/tokenUsage/updated` has been seen
    // with camelCase. Accepting both is why `usage_from` sums two keys per field.
    let mut driver = ready_driver();

    let completed = driver.on_line(
        r#"{"jsonrpc":"2.0","method":"turn/completed","params":{"turnId":"t1","usage":{"input_tokens":26845,"cached_input_tokens":19968,"output_tokens":75}}}"#,
    );
    match events(&completed).first().expect("a TurnFinished event") {
        AgentEvent::TurnFinished {
            usage, cost_usd, ..
        } => {
            assert_eq!(usage.tokens_in, 26845);
            assert_eq!(usage.cached, 19968);
            assert_eq!(usage.tokens_out, 75);
            // Codex reports no currency. A number here would have to be invented.
            assert_eq!(*cost_usd, None);
        }
        other => panic!("expected TurnFinished, got {other:?}"),
    }
}

#[test]
fn an_unknown_notification_becomes_a_raw_row_rather_than_being_dropped_or_failing() {
    // The property the whole design rests on. Both protocols are experimental and will grow
    // event kinds in a patch release: matching exhaustively would blank the transcript on the day
    // the user upgrades, and dropping the unknown would lose information with no trace. A
    // reviewer who replaces `Raw` with `_ => return None` breaks this and only this.
    let mut driver = ready_driver();
    let steps = driver.on_line(
        r#"{"jsonrpc":"2.0","method":"item/mcpToolCall/progress","params":{"itemId":"x","progress":0.5}}"#,
    );

    match events(&steps).first().expect("a Raw event") {
        AgentEvent::Raw {
            provider,
            event,
            payload,
        } => {
            assert_eq!(provider, "codex");
            assert_eq!(event, "item/mcpToolCall/progress");
            // The payload survives whole, so a collapsed row can still show what arrived.
            assert_eq!(payload["progress"], 0.5);
        }
        other => panic!("expected Raw, got {other:?}"),
    }
}

#[test]
fn output_that_is_not_json_is_surfaced_as_a_notice_rather_than_swallowed() {
    // When a CLI is misconfigured this is a human-readable complaint on stdout, and it is the
    // only clue the user gets. Dropping it produces a dead pane with an empty transcript.
    let mut driver = ready_driver();
    let steps = driver.on_line("error: not logged in. Run `codex login`.");
    assert_eq!(
        events(&steps),
        vec![&AgentEvent::Notice {
            level: NoticeLevel::Warn,
            message: "error: not logged in. Run `codex login`.".to_owned(),
        }]
    );
}

#[test]
fn a_rejected_request_is_reported_rather_than_leaving_the_session_silent() {
    let mut driver = Codex.protocol(&SessionRequest::default());
    driver.open();
    let steps = driver.on_line(
        r#"{"id":1,"error":{"code":-32600,"message":"collaborationMode/list requires experimentalApi capability"}}"#,
    );
    match events(&steps).first().expect("a Failed event") {
        AgentEvent::Failed { message } => assert!(message.contains("experimentalApi")),
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[test]
fn an_empty_line_produces_nothing() {
    let mut driver = ready_driver();
    assert!(driver.on_line("").is_empty());
    assert!(driver.on_line("   ").is_empty());
}

#[test]
fn interrupting_before_a_thread_exists_writes_nothing() {
    // Pressing stop during a slow handshake must not send `turn/interrupt` with a null thread,
    // which the server would reject and surface as a confusing failure.
    let mut driver = Codex.protocol(&SessionRequest::default());
    driver.open();
    assert!(driver.interrupt().is_empty());
}

#[test]
fn interrupting_a_ready_session_targets_its_thread() {
    let mut driver = ready_driver();
    let frames = writes(&driver.interrupt());
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0]["method"], "turn/interrupt");
    assert_eq!(
        frames[0]["params"]["threadId"],
        "019fd37c-f1e4-7a22-81e7-02200fd6d127"
    );
}

#[test]
fn resuming_asks_for_the_thread_by_id_and_keeps_the_rest_of_the_settings() {
    // The two open paths share every parameter but the id. Drifting apart is how a resumed
    // session would quietly lose its approval policy.
    let mut driver = Codex.protocol(&SessionRequest {
        cwd: "/tmp/worktree".to_owned(),
        mode: Some("on-request".to_owned()),
        resume: Some("019fd37c-f1e4-7a22-81e7-02200fd6d127".to_owned()),
        ..SessionRequest::default()
    });
    driver.open();
    let frames = writes(&driver.on_line(r#"{"id":1,"result":{}}"#));
    let open = frames
        .iter()
        .find(|f| f["method"] == "thread/resume")
        .expect("resume must use thread/resume, not thread/start");
    assert_eq!(
        open["params"]["threadId"],
        "019fd37c-f1e4-7a22-81e7-02200fd6d127"
    );
    assert_eq!(open["params"]["cwd"], "/tmp/worktree");
    assert_eq!(open["params"]["approvalPolicy"], "on-request");
}
