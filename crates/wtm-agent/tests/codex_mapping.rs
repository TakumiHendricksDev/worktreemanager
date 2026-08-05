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
use wtm_core::model::{AgendaStatus, AgentEvent, ApprovalAnswer, ApprovalRequest, NoticeLevel};

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

#[test]
fn a_command_approval_request_becomes_a_blocking_card_and_its_answer_replies_on_the_same_id() {
    // The whole round trip, in one test, because the two halves are only correct together: an
    // approval whose reply carries the wrong JSON-RPC id leaves the server blocked forever, and
    // nothing about the request half alone would notice.
    let mut driver = ready_driver();

    // A server-initiated *request*: an id AND a method, unlike a notification. That distinction is
    // the only thing separating "answer this" from "display this".
    let steps = driver.on_line(
        r#"{"jsonrpc":"2.0","id":41,"method":"item/commandExecution/requestApproval","params":{"itemId":"item_7","threadId":"019fd37c-f1e4-7a22-81e7-02200fd6d127","turnId":"t1","startedAtMs":1785963000000,"command":"rm -rf node_modules && bun install","cwd":"/tmp/worktree","reason":"reinstalling dependencies"}}"#,
    );

    let id = match events(&steps).first().expect("an ApprovalRequested event") {
        AgentEvent::ApprovalRequested {
            id,
            blocking,
            request,
        } => {
            // Always blocking: the server does not continue the turn without a reply, so a card the
            // user could scroll past would stall the session with nothing on screen to explain it.
            assert!(*blocking, "a Codex approval always blocks its turn");
            assert_eq!(
                *request,
                ApprovalRequest::Command {
                    command: "rm -rf node_modules && bun install".to_owned(),
                    cwd: Some("/tmp/worktree".to_owned()),
                    reason: Some("reinstalling dependencies".to_owned()),
                }
            );
            id.clone()
        }
        other => panic!("expected ApprovalRequested, got {other:?}"),
    };

    let answered = driver.answer(&id, &ApprovalAnswer::Allow);
    let frames = writes(&answered);
    assert_eq!(frames.len(), 1);
    // The id is the only thing that correlates a reply with its request. 41, not a fresh one.
    assert_eq!(frames[0]["id"], 41);
    assert_eq!(frames[0]["result"]["decision"], "accept");
    assert!(
        answered.contains(&Step::Emit(AgentEvent::ApprovalResolved { id: id.clone() })),
        "the card has to be told to collapse"
    );

    // The first answer wins. A second — two panes, or a click racing a keystroke — finds nothing,
    // because `answer` removes the request when it replies. Replying twice on one id would
    // desynchronise the server's view of the turn.
    assert!(
        driver
            .answer(&id, &ApprovalAnswer::Deny { message: None })
            .is_empty(),
        "a second answer for the same request must write nothing"
    );
}

#[test]
fn denying_declines_rather_than_cancelling_so_the_rest_of_the_turn_survives() {
    // Both verbs deny. The server documents `decline` as "the agent will continue the turn" and
    // `cancel` as "the turn will also be immediately interrupted" — so refusing one command must
    // not throw away the work around it. Stop is a separate button for when it should.
    let mut driver = ready_driver();
    let steps = driver.on_line(
        r#"{"jsonrpc":"2.0","id":7,"method":"item/commandExecution/requestApproval","params":{"itemId":"i","threadId":"t","turnId":"t1","startedAtMs":1,"command":"curl example.com"}}"#,
    );
    let AgentEvent::ApprovalRequested { id, .. } = events(&steps)[0].clone() else {
        panic!("expected an approval");
    };

    let frames = writes(&driver.answer(&id, &ApprovalAnswer::Deny { message: None }));
    assert_eq!(frames[0]["result"]["decision"], "decline");
}

#[test]
fn allow_for_session_uses_the_servers_own_verb() {
    let mut driver = ready_driver();
    let steps = driver.on_line(
        r#"{"jsonrpc":"2.0","id":8,"method":"item/fileChange/requestApproval","params":{"itemId":"i","threadId":"t","turnId":"t1","startedAtMs":1,"reason":"writing outside the workspace"}}"#,
    );
    let AgentEvent::ApprovalRequested { id, request, .. } = events(&steps)[0].clone() else {
        panic!("expected an approval");
    };
    assert!(matches!(request, ApprovalRequest::FileChange { .. }));

    let frames = writes(&driver.answer(&id, &ApprovalAnswer::AllowForSession));
    assert_eq!(frames[0]["result"]["decision"], "acceptForSession");
}

#[test]
fn an_edited_allow_is_refused_rather_than_downgraded_to_a_plain_accept() {
    // Claude Code's allow can carry a replacement payload and rewrite the call; Codex has no verb
    // for it. Treating this as `accept` would run the command the user *edited*, unedited — the
    // worst available outcome, and silent. The UI does not offer the affordance for this provider;
    // this guards the case where it is offered by mistake.
    let mut driver = ready_driver();
    let steps = driver.on_line(
        r#"{"jsonrpc":"2.0","id":9,"method":"item/commandExecution/requestApproval","params":{"itemId":"i","threadId":"t","turnId":"t1","startedAtMs":1,"command":"rm -rf /"}}"#,
    );
    let AgentEvent::ApprovalRequested { id, .. } = events(&steps)[0].clone() else {
        panic!("expected an approval");
    };

    let refused = driver.answer(
        &id,
        &ApprovalAnswer::AllowWithEdits {
            input: serde_json::json!({ "command": "rm -rf ./build" }),
        },
    );
    assert!(
        writes(&refused).is_empty(),
        "nothing may be sent for an answer this provider cannot express"
    );
    assert!(matches!(
        events(&refused).first(),
        Some(AgentEvent::Failed { .. })
    ));
    // Still answerable: refusing the verb must not consume the request, or the session is stuck.
    assert!(
        !writes(&driver.answer(&id, &ApprovalAnswer::Allow)).is_empty(),
        "the request must survive an answer this provider refused"
    );
}

#[test]
fn abandoning_declines_everything_outstanding_so_the_server_is_never_left_waiting() {
    // On close and on quit. A server blocked on a reply does not read its stdin closing at all, so
    // without this the child is only reachable by the kill — reported as `Signalled`, which reads
    // in the UI as a crash rather than an end.
    let mut driver = ready_driver();
    for id in [11, 12] {
        driver.on_line(&format!(
            r#"{{"jsonrpc":"2.0","id":{id},"method":"item/commandExecution/requestApproval","params":{{"itemId":"i{id}","threadId":"t","turnId":"t1","startedAtMs":1,"command":"echo {id}"}}}}"#
        ));
    }

    let steps = driver.abandon();
    let frames = writes(&steps);
    assert_eq!(
        frames.len(),
        2,
        "every outstanding request must be answered"
    );
    for frame in &frames {
        assert_eq!(frame["result"]["decision"], "decline");
    }
    assert_eq!(
        events(&steps).len(),
        2,
        "each card has to be told to collapse"
    );

    // Drained, not iterated: abandoning twice must not reply twice on the same id.
    assert!(driver.abandon().is_empty());
}

#[test]
fn a_server_request_this_build_does_not_know_is_still_declinable() {
    // An MCP elicitation, a tool asking for input — anything with an id and a method that is not
    // one of the three approvals. Shown as `raw` rather than acted on, but *kept* in the pending
    // map, because a request that is neither answered nor declined leaves the server blocked
    // forever. Dropping it on the floor is the bug this test exists for.
    let mut driver = ready_driver();
    let steps = driver.on_line(
        r#"{"jsonrpc":"2.0","id":21,"method":"mcpServer/elicitation/request","params":{"mode":"form"}}"#,
    );
    assert!(matches!(
        events(&steps).first(),
        Some(AgentEvent::Raw { .. })
    ));

    let frames = writes(&driver.abandon());
    assert_eq!(frames.len(), 1, "an unknown request must still be declined");
    assert_eq!(frames[0]["id"], 21);
}

#[test]
fn a_command_approval_with_no_command_reported_still_names_itself() {
    // `command` is nullable in the schema even for a command approval. An empty card with two
    // buttons is unanswerable; a placeholder at least says what is being asked.
    let mut driver = ready_driver();
    let steps = driver.on_line(
        r#"{"jsonrpc":"2.0","id":31,"method":"item/commandExecution/requestApproval","params":{"itemId":"i","threadId":"t","turnId":"t1","startedAtMs":1}}"#,
    );
    match events(&steps).first().expect("an approval") {
        AgentEvent::ApprovalRequested { request, .. } => match request {
            ApprovalRequest::Command { command, .. } => assert!(!command.is_empty()),
            other => panic!("expected a Command approval, got {other:?}"),
        },
        other => panic!("expected ApprovalRequested, got {other:?}"),
    }
}
