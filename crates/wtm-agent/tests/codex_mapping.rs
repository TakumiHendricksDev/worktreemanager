//! The Codex protocol driver, fed the lines the real app server actually sends.
//!
//! # Why this file exists
//!
//! The mapping from `codex app-server` notifications to domain events is the one part of this
//! feature with no compiler holding it up: every field is read out of a `serde_json::Value` by
//! string key, so a renamed key, a `camelCase`/`snake_case` slip or a nesting change is a silently
//! empty event rather than a build failure. This is the only mechanism that notices.
//!
//! # Where these lines came from, and why it matters more than it sounds
//!
//! Every fixture below is a line captured from `codex-cli 0.144.6` on a real machine, pasted
//! verbatim. That is not fastidiousness: the first version of this file used fixtures taken from
//! a `codex exec --json` capture, and **`exec` and the app server serialize the same items
//! differently** — `agent_message` against `agentMessage`. So every test passed against a spelling
//! the app server never sends, and the bug only surfaced when a real turn showed
//! `item/started:agentMessage` falling through to `Raw`.
//!
//! Four details a fixture invented from the schema would have got wrong, each verified on the wire:
//!
//!   * a reply is `{"id":1,"result":{…}}` with **no `jsonrpc` field**;
//!   * `thread/start`'s id is nested at `result.thread.id`, not `result.id`;
//!   * `turn/started` and `turn/completed` nest the id at `params.turn.id`, and the flat
//!     `params.turnId` that `item/*` notifications carry does not exist on them;
//!   * `turn/completed` carries **no usage at all** — token counts come separately on
//!     `thread/tokenUsage/updated`.
//!
//! If a fixture here is ever edited, capture the replacement rather than writing it.
//!
//! # No process, no host, no timing
//!
//! `Protocol` is a pure state machine — lines in, steps out — so these are synchronous
//! assertions with nothing spawned. That is the property the design exists for, and it is why
//! there is no `FakePipe` here.

// `unwrap_used` is banned in the app so a failure carries a message. In an assertion it adds noise
// without adding information — a panic is the failure report either way — which is the allowance
// `wtm-exec` grants its own tests via `lib.rs`. An integration test is its own crate, so it has to
// say so here.
#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;

use pretty_assertions::assert_eq;
use serde_json::json;
use wtm_agent::codex::Codex;
use wtm_agent::provider::{Protocol, Provider, SessionRequest, Step};
use wtm_core::model::{
    AgendaStatus, AgentAttachment, AgentEvent, ApprovalAnswer, ApprovalRequest, NoticeLevel, Usage,
};

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
        mode: Some("auto".to_owned()),
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
    // One wtm mode, both protocol fields. Sending `approvalPolicy` alone — which is what this did
    // before — left the sandbox at whatever `~/.codex/config.toml` said, so two sessions wtm
    // believed were configured the same could have different filesystem reach.
    assert_eq!(frames[1]["params"]["approvalPolicy"], "on-request");
    assert_eq!(frames[1]["params"]["sandbox"], "workspace-write");

    // Nothing is ready yet: `thread/start` is a second round trip, which is why `Step::Ready`
    // exists apart from the `SessionReady` event.
    assert!(!after_init.contains(&Step::Ready));
}

#[test]
fn a_side_session_is_an_ephemeral_fork_not_a_prompt_named_btw() {
    let mut driver = Codex.protocol(&SessionRequest {
        cwd: "/tmp/worktree".to_owned(),
        fork: Some("parent-thread".to_owned()),
        ephemeral: true,
        ..SessionRequest::default()
    });

    driver.open();
    let frames = writes(&driver.on_line(r#"{"id":1,"result":{"userAgent":"wtm"}}"#));

    assert_eq!(frames[1]["method"], "thread/fork");
    assert_eq!(frames[1]["params"]["threadId"], "parent-thread");
    assert_eq!(frames[1]["params"]["ephemeral"], true);
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

    let early = driver.send_turn("review the plan", &[]);
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
    let frames = writes(&driver.send_turn("go", &[]));
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
        r#"{"method":"item/started","params":{"item":{"id":"item_1","type":"commandExecution","command":"/bin/zsh -lc 'echo hi'","status":"in_progress"},"threadId":"019fd3ce","turnId":"019fd3ce-c271"}}"#,
    );
    assert_eq!(
        events(&started),
        vec![&AgentEvent::CommandStarted {
            id: "item_1".to_owned(),
            command: "/bin/zsh -lc 'echo hi'".to_owned(),
            cwd: None,
        }]
    );

    // `exitCode`. A command item has not been observed on this transport — it needs a writable
    // sandbox — so this is the `ThreadItem` convention every other type was just corrected to,
    // with the snake_case spelling an `exec --json` capture showed kept as a fallback.
    let finished = driver.on_line(
        r#"{"method":"item/completed","params":{"item":{"id":"item_1","type":"commandExecution","exitCode":0,"status":"completed"},"threadId":"019fd3ce","turnId":"019fd3ce-c271"}}"#,
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
    // Captured verbatim. `agentMessage`, not `agent_message` — the spelling that made this file's
    // first version pass while the code was wrong.
    let steps = driver.on_line(
        r#"{"method":"item/started","params":{"item":{"id":"msg_08d2497a","memoryCitation":null,"phase":"final_answer","text":"","type":"agentMessage"},"startedAtMs":1785964972195,"threadId":"019fd3ce","turnId":"019fd3ce-c271"}}"#,
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
fn a_finished_turn_reports_its_nested_id_and_the_last_usage_seen() {
    // Two corrections a real turn forced, in one test because they arrive together.
    let mut driver = ready_driver();

    // Usage first, because `turn/completed` has none of its own — the driver reports the last one
    // it saw, and without that cache a finished turn shows a row of zeros.
    driver.on_line(
        r#"{"method":"thread/tokenUsage/updated","params":{"threadId":"019fd3d8-481e","turnId":"019fd3d8-4df8","tokenUsage":{"total":{"totalTokens":364000,"inputTokens":361000,"cachedInputTokens":310000,"outputTokens":3000,"reasoningOutputTokens":900},"last":{"totalTokens":12545,"inputTokens":12529,"cachedInputTokens":6016,"outputTokens":16,"reasoningOutputTokens":9},"modelContextWindow":258400}}}"#,
    );
    let completed = driver.on_line(
        r#"{"method":"turn/completed","params":{"threadId":"019fd3ce-bc97-7b33-90c2-31db4160c47d","turn":{"completedAt":1785964972,"durationMs":1715,"error":null,"id":"019fd3ce-c271-73c2-b986-156b4d998338","items":[],"itemsView":"notLoaded","startedAt":1785964970,"status":"completed"}}}"#,
    );
    match events(&completed).first().expect("a TurnFinished event") {
        AgentEvent::TurnFinished {
            usage, cost_usd, ..
        } => {
            // `last` is the current context footprint. `total` above is deliberately greater than
            // the model window: it is lifetime billing usage and displaying it was the 100%-in-a-
            // few-seconds bug this test protects against.
            assert_eq!(usage.tokens_in, 12529);
            assert_eq!(usage.cached, 6016);
            assert_eq!(usage.tokens_out, 16);
            assert_eq!(usage.context_used, 12_545);
            // The denominator for "how full is my context".
            assert_eq!(usage.context_window, Some(258_400));
            // Codex reports no currency. A number here would have to be invented.
            assert_eq!(*cost_usd, None);
        }
        other => panic!("expected TurnFinished, got {other:?}"),
    }
}

#[test]
fn a_resumed_thread_rehydrates_its_visible_messages() {
    let mut driver = Codex.protocol(&SessionRequest {
        cwd: "/tmp/worktree".to_owned(),
        resume: Some("019fd37c-f1e4-7a22-81e7-02200fd6d127".to_owned()),
        ..SessionRequest::default()
    });
    driver.open();
    driver.on_line(r#"{"id":1,"result":{}}"#);
    let steps = driver.on_line(
        r#"{"id":2,"result":{"thread":{"id":"019fd37c-f1e4-7a22-81e7-02200fd6d127","turns":[{"id":"turn-1","status":"completed","items":[{"id":"u1","type":"userMessage","content":[{"type":"text","text":"Fix the parser"}]},{"id":"r1","type":"reasoning","summary":[],"content":[]},{"id":"a1","type":"agentMessage","text":"I fixed it.","phase":"final_answer"}]}]}}}"#,
    );
    let history = events(&steps);
    assert!(matches!(history[0], AgentEvent::SessionReady { .. }));
    assert_eq!(
        history[1..],
        [
            &AgentEvent::UserEcho {
                text: "Fix the parser".to_owned()
            },
            &AgentEvent::Message {
                text: "I fixed it.".to_owned()
            }
        ]
    );
}

#[test]
fn a_resumed_thread_replays_its_commands_diffs_and_reasoning_and_not_only_its_prose() {
    // The complaint this fixes: the same conversation looked like two different things before and
    // after a restart, because a resumed thread rendered prose while a live one rendered cards.
    let mut driver = Codex.protocol(&SessionRequest {
        cwd: "/tmp/worktree".to_owned(),
        resume: Some("019fd37c-f1e4-7a22-81e7-02200fd6d127".to_owned()),
        ..SessionRequest::default()
    });
    driver.open();
    driver.on_line(r#"{"id":1,"result":{}}"#);
    let steps = driver.on_line(
        r#"{"id":2,"result":{"thread":{"id":"019fd37c-f1e4-7a22-81e7-02200fd6d127","turns":[{"id":"turn-1","status":"completed","items":[
            {"id":"r1","type":"reasoning","summary":[{"type":"text","text":"weighing it"}]},
            {"id":"c1","type":"commandExecution","command":"cargo test","cwd":"/tmp/worktree","aggregatedOutput":"ok","exitCode":0},
            {"id":"f1","type":"fileChange","diff":"--- a\n+++ b\n"},
            {"id":"m1","type":"mcpToolCall","title":"search","status":"completed","output":"found"},
            {"id":"a1","type":"agentMessage","text":"I fixed it.","phase":"final_answer"}
        ]}]}}}"#,
    );
    let history = events(&steps);
    assert!(matches!(history[0], AgentEvent::SessionReady { .. }));
    assert_eq!(
        history[1..],
        [
            &AgentEvent::ReasoningDelta {
                text: "weighing it".to_owned()
            },
            &AgentEvent::CommandStarted {
                id: "c1".to_owned(),
                command: "cargo test".to_owned(),
                cwd: Some("/tmp/worktree".to_owned()),
            },
            &AgentEvent::CommandOutput {
                id: "c1".to_owned(),
                chunk: "ok".to_owned(),
            },
            &AgentEvent::CommandFinished {
                id: "c1".to_owned(),
                exit_code: Some(0),
            },
            &AgentEvent::Patch {
                id: "f1".to_owned(),
                unified_diff: "--- a\n+++ b\n".to_owned(),
            },
            &AgentEvent::ToolStarted {
                id: "m1".to_owned(),
                name: "mcpToolCall".to_owned(),
                title: Some("search".to_owned()),
            },
            &AgentEvent::ToolFinished {
                id: "m1".to_owned(),
                ok: true,
                output: Some("found".to_owned()),
            },
            &AgentEvent::Message {
                text: "I fixed it.".to_owned()
            },
        ]
    );
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
fn interrupting_an_idle_ready_session_writes_nothing() {
    let mut driver = ready_driver();
    assert!(driver.interrupt().is_empty());
}

#[test]
fn interrupting_an_active_turn_targets_its_thread_and_turn() {
    let mut driver = ready_driver();
    driver.on_line(
        r#"{"method":"turn/started","params":{"turn":{"id":"turn-7","status":"inProgress"}}}"#,
    );

    let frames = writes(&driver.interrupt());
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0]["method"], "turn/interrupt");
    assert_eq!(
        frames[0]["params"]["threadId"],
        "019fd37c-f1e4-7a22-81e7-02200fd6d127"
    );
    assert_eq!(frames[0]["params"]["turnId"], "turn-7");
}

#[test]
fn a_completed_turn_is_no_longer_interruptible() {
    let mut driver = ready_driver();
    driver.on_line(
        r#"{"method":"turn/started","params":{"turn":{"id":"turn-7","status":"inProgress"}}}"#,
    );
    driver.on_line(
        r#"{"method":"turn/completed","params":{"turn":{"id":"turn-7","status":"completed"}}}"#,
    );

    assert!(driver.interrupt().is_empty());
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
    let second = driver.answer(&id, &ApprovalAnswer::Deny { message: None });
    assert!(
        writes(&second).is_empty(),
        "a second answer must not write to the server"
    );
    assert!(
        events(&second)
            .iter()
            .any(|event| matches!(event, AgentEvent::Notice { .. })),
        "a stale answer is a notice rather than a silent no-op: {second:?}"
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
fn request_user_input_becomes_choices_and_replies_with_the_schema_answer_map() {
    let mut driver = ready_driver();
    let steps = driver.on_line(
        r#"{"jsonrpc":"2.0","id":42,"method":"item/tool/requestUserInput","params":{"threadId":"t","turnId":"turn","itemId":"item","isBlocking":true,"questions":[{"id":"scope","header":"Scope","question":"Which packages?","options":[{"label":"UI","description":"Frontend only"},{"label":"Core","description":"Rust crates"}]},{"id":"tests","header":"Tests","question":"Which checks?","options":[{"label":"Fast","description":"Focused tests"}],"multiSelect":true}]}}"#,
    );
    let AgentEvent::ApprovalRequested { id, request, .. } = events(&steps)[0].clone() else {
        panic!("expected a user-input request");
    };
    let ApprovalRequest::UserInput { questions } = request else {
        panic!("expected structured questions");
    };
    assert_eq!(questions.len(), 2);
    assert_eq!(questions[0].id, "scope");
    assert_eq!(questions[0].options[0].label, "UI");
    assert!(questions[1].multiple);

    let frames = writes(&driver.answer(
        &id,
        &ApprovalAnswer::UserInput {
            answers: BTreeMap::from([
                ("scope".to_owned(), vec!["UI".to_owned()]),
                (
                    "tests".to_owned(),
                    vec!["Fast".to_owned(), "Core too".to_owned()],
                ),
            ]),
            notes: Some("Keep the API stable".to_owned()),
        },
    ));
    assert_eq!(frames[0]["id"], 42);
    assert_eq!(frames[0]["result"]["answers"]["scope"]["answers"][0], "UI");
    assert_eq!(
        frames[0]["result"]["answers"]["tests"]["answers"][2],
        "Notes: Keep the API stable"
    );
}

#[test]
fn image_attachments_are_visible_and_sent_as_local_images() {
    let mut driver = ready_driver();
    let attachment = AgentAttachment {
        name: "screen.png".to_owned(),
        path: "/tmp/screen.png".to_owned(),
        mime: "image/png".to_owned(),
        size: 3,
        data_base64: "YWJj".to_owned(),
    };
    let steps = driver.send_turn("What is wrong here?", std::slice::from_ref(&attachment));
    assert!(matches!(
        events(&steps).first(),
        Some(AgentEvent::Attachments { attachments }) if attachments == &vec![attachment]
    ));
    let frames = writes(&steps);
    assert_eq!(frames[0]["params"]["input"][1]["type"], "localImage");
    assert_eq!(frames[0]["params"]["input"][1]["path"], "/tmp/screen.png");
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

#[test]
fn a_real_turns_lines_produce_a_transcript_and_not_a_wall_of_raw_rows() {
    // The regression test for the whole class of bug a real turn exposed. These are the lines a
    // genuine `turn/start` produced, in order, pasted verbatim — including the lifecycle chatter,
    // because the point is that most of it draws *nothing*.
    //
    // Before the fix this sequence produced eleven `Raw` rows and no message: every item type was
    // matched in snake_case, and every status notification became a collapsed row burying the
    // reply. A reviewer who reintroduces either fails here.
    const WIRE: &[&str] = &[
        r#"{"method":"remoteControl/status/changed","params":{"status":"disabled"}}"#,
        r#"{"method":"thread/started","params":{"thread":{"id":"019fd3ce-bc97"}}}"#,
        r#"{"method":"mcpServer/startupStatus/updated","params":{"name":"node_repl","status":"starting"}}"#,
        r#"{"method":"mcpServer/startupStatus/updated","params":{"name":"node_repl","status":"ready"}}"#,
        r#"{"method":"thread/status/changed","params":{"threadId":"019fd3ce-bc97"}}"#,
        r#"{"method":"turn/started","params":{"threadId":"019fd3ce-bc97","turn":{"id":"019fd3ce-c271","status":"inProgress"}}}"#,
        r#"{"method":"item/started","params":{"item":{"content":[{"text":"hi","type":"text"}],"id":"019fd3ce-c322","type":"userMessage"},"threadId":"019fd3ce-bc97","turnId":"019fd3ce-c271"}}"#,
        r#"{"method":"item/completed","params":{"item":{"content":[{"text":"hi","type":"text"}],"id":"019fd3ce-c322","type":"userMessage"},"threadId":"019fd3ce-bc97","turnId":"019fd3ce-c271"}}"#,
        r#"{"method":"item/started","params":{"item":{"content":[],"id":"rs_08d2","summary":[],"type":"reasoning"},"threadId":"019fd3ce-bc97","turnId":"019fd3ce-c271"}}"#,
        r#"{"method":"item/completed","params":{"item":{"content":[],"id":"rs_08d2","summary":[],"type":"reasoning"},"threadId":"019fd3ce-bc97","turnId":"019fd3ce-c271"}}"#,
        r#"{"method":"item/started","params":{"item":{"id":"msg_08d2","phase":"final_answer","text":"","type":"agentMessage"},"threadId":"019fd3ce-bc97","turnId":"019fd3ce-c271"}}"#,
        r#"{"method":"item/agentMessage/delta","params":{"delta":"OK","itemId":"msg_08d2","threadId":"019fd3ce-bc97","turnId":"019fd3ce-c271"}}"#,
        r#"{"method":"item/completed","params":{"item":{"id":"msg_08d2","phase":"final_answer","text":"OK","type":"agentMessage"},"threadId":"019fd3ce-bc97","turnId":"019fd3ce-c271"}}"#,
        r#"{"method":"account/rateLimits/updated","params":{}}"#,
        r#"{"method":"thread/status/changed","params":{"threadId":"019fd3ce-bc97"}}"#,
        r#"{"method":"turn/completed","params":{"threadId":"019fd3ce-bc97","turn":{"completedAt":1785964972,"id":"019fd3ce-c271","status":"completed"}}}"#,
    ];

    let mut driver = ready_driver();
    let produced: Vec<AgentEvent> = WIRE
        .iter()
        .flat_map(|line| driver.on_line(line))
        .filter_map(|step| match step {
            Step::Emit(event) => Some(event),
            _ => None,
        })
        .collect();

    // Not one `Raw`. Every line here is either drawn or deliberately silent.
    let raw: Vec<&AgentEvent> = produced
        .iter()
        .filter(|e| matches!(e, AgentEvent::Raw { .. }))
        .collect();
    assert!(
        raw.is_empty(),
        "these lines are all recognised, got {raw:?}"
    );

    assert_eq!(
        produced,
        vec![
            AgentEvent::TurnStarted {
                turn: "019fd3ce-c271".to_owned()
            },
            AgentEvent::MessageDelta {
                text: "OK".to_owned()
            },
            AgentEvent::Message {
                text: "OK".to_owned()
            },
            AgentEvent::TurnFinished {
                turn: "019fd3ce-c271".to_owned(),
                usage: Usage::default(),
                cost_usd: None,
            },
        ],
        "the reply, its turn, and nothing else"
    );
}

#[test]
fn lifecycle_chatter_draws_nothing_while_an_unknown_method_still_does() {
    // The distinction the two arms encode: "we recognise this and there is nothing to say" versus
    // "we do not recognise this". Collapsing them would make a genuinely new event vanish, which
    // is the one failure `Raw` exists to prevent.
    let mut driver = ready_driver();

    for quiet in [
        r#"{"method":"mcpServer/startupStatus/updated","params":{}}"#,
        r#"{"method":"thread/status/changed","params":{}}"#,
        r#"{"method":"account/rateLimits/updated","params":{}}"#,
        r#"{"method":"serverRequest/resolved","params":{}}"#,
    ] {
        assert!(
            driver.on_line(quiet).is_empty(),
            "{quiet} should draw nothing"
        );
    }

    assert!(matches!(
        events(&driver.on_line(r#"{"method":"some/brand/newThing","params":{}}"#)).first(),
        Some(AgentEvent::Raw { .. })
    ));
}

#[test]
fn a_rate_limits_update_with_an_exhausted_window_reports_limit_reached() {
    // `account/rateLimits/updated` used to be in the silent list outright. It still is for every
    // payload that has room left — see the test below — but a window at 100% is the one thing this
    // notification can say that a reader needs.
    let mut driver = ready_driver();
    let steps = driver.on_line(
        r#"{"method":"account/rateLimits/updated","params":{"rateLimits":{"primary":{"usedPercent":100.0,"windowMinutes":300,"resetsInSeconds":12600},"secondary":{"usedPercent":41.2,"windowMinutes":10080,"resetsInSeconds":400000}}}}"#,
    );
    match events(&steps).first().expect("a LimitReached") {
        AgentEvent::LimitReached { message, resets_at } => {
            assert!(
                message.contains("210 minute"),
                "the wait belongs in the sentence, got {message:?}"
            );
            assert_eq!(
                *resets_at, None,
                "Codex states a duration, and a protocol has no clock to turn it into an instant"
            );
        }
        other => panic!("expected LimitReached, got {other:?}"),
    }
}

#[test]
fn a_rate_limits_update_below_the_ceiling_still_draws_nothing() {
    // This notification fires on a schedule. A row every few minutes reporting how much quota is
    // *left* is how a transcript teaches its reader to skip the one that eventually matters.
    let mut driver = ready_driver();
    for quiet in [
        r#"{"method":"account/rateLimits/updated","params":{}}"#,
        r#"{"method":"account/rateLimits/updated","params":{"rateLimits":{}}}"#,
        r#"{"method":"account/rateLimits/updated","params":{"rateLimits":{"primary":{"usedPercent":99.4,"resetsInSeconds":60}}}}"#,
    ] {
        assert!(
            driver.on_line(quiet).is_empty(),
            "{quiet} should draw nothing"
        );
    }
}

#[test]
fn an_error_notification_naming_a_usage_limit_becomes_limit_reached_rather_than_a_plain_failure() {
    let mut driver = ready_driver();
    let steps = driver.on_line(
        r#"{"method":"error","params":{"message":"You have exceeded your quota for this billing period."}}"#,
    );
    assert!(matches!(
        events(&steps).first(),
        Some(AgentEvent::LimitReached { .. })
    ));

    // And the other half: an ordinary server error stays a failure, because carrying the
    // conversation to Claude would not fix it.
    let ordinary =
        driver.on_line(r#"{"method":"error","params":{"message":"sandbox denied write to /etc"}}"#);
    assert!(matches!(
        events(&ordinary).first(),
        Some(AgentEvent::Failed { .. })
    ));
}

#[test]
fn a_nested_provider_error_keeps_the_actionable_message() {
    let mut driver = ready_driver();
    let steps = driver.on_line(
        r#"{"method":"error","params":{"error":{"message":"{\"type\":\"error\",\"status\":400,\"error\":{\"type\":\"invalid_request_error\",\"message\":\"The model id is not supported.\"}}"}}}"#,
    );

    assert!(matches!(
        events(&steps).as_slice(),
        [AgentEvent::Failed { message }] if message == "The model id is not supported."
    ));
}

#[test]
fn a_failed_turn_completion_keeps_its_error() {
    let mut driver = ready_driver();
    let steps = driver.on_line(
        r#"{"method":"turn/completed","params":{"turn":{"id":"turn-7","status":"failed","error":{"message":"Please sign in again."}}}}"#,
    );

    assert!(matches!(
        events(&steps).as_slice(),
        [AgentEvent::Failed { message }] if message == "Please sign in again."
    ));
}

#[test]
fn the_model_list_reply_yields_per_model_effort_ladders() {
    // The fact the whole capability query exists for: the ladders differ *within* one provider.
    // Captured from a real `model/list` — `gpt-5.6-sol` reaches `ultra`, `gpt-5.5` stops at `xhigh`.
    // A picker built on a single provider-wide list would offer rungs the selected model rejects.
    let reply: serde_json::Value = serde_json::from_str(
        r#"{"id":3,"result":{"data":[
          {"id":"gpt-5.6-sol","model":"gpt-5.6-sol","displayName":"GPT-5.6-Sol","description":"","hidden":false,"isDefault":true,"defaultReasoningEffort":"medium","supportedReasoningEfforts":[{"reasoningEffort":"low","description":"Fast"},{"reasoningEffort":"medium","description":"Balanced"},{"reasoningEffort":"high","description":"Deeper"},{"reasoningEffort":"xhigh","description":"Extra"},{"reasoningEffort":"max","description":"Maximum"},{"reasoningEffort":"ultra","description":"Maximum reasoning with automatic task delegation"}]},
          {"id":"gpt-5.5","model":"gpt-5.5","displayName":"GPT-5.5","description":"","hidden":false,"isDefault":false,"defaultReasoningEffort":"xhigh","supportedReasoningEfforts":[{"reasoningEffort":"low","description":""},{"reasoningEffort":"medium","description":""},{"reasoningEffort":"high","description":""},{"reasoningEffort":"xhigh","description":""}]},
          {"id":"gpt-5.6-sol-wm","model":"gpt-5.6-sol-wm","displayName":"GPT-5.6-Sol-WM","description":"","hidden":true,"isDefault":false,"defaultReasoningEffort":"low","supportedReasoningEfforts":[]}
        ],"nextCursor":null}}"#,
    )
    .unwrap();

    let models = wtm_agent::codex::parse_models(&reply);

    // The hidden one is dropped: the server marks what it does not want in a picker, and offering it
    // anyway would present something the user has no way to understand.
    assert_eq!(models.len(), 2, "the hidden model must not be offered");

    let sol = &models[0];
    assert_eq!(sol.id, "gpt-5.6-sol");
    assert_eq!(sol.label, "GPT-5.6-Sol");
    assert!(sol.is_default);
    // The server's own word, faithfully. `prefer_effort` overrides this afterwards and on purpose —
    // see `capability::prefer_effort` for why the override is a second pass rather than part of the
    // parse. This assertion is what makes a CLI that changes its own defaults visible.
    assert_eq!(sol.default_effort.as_deref(), Some("medium"));
    let ladder: Vec<&str> = sol.efforts.iter().map(|e| e.effort.as_str()).collect();
    assert_eq!(ladder, ["low", "medium", "high", "xhigh", "max", "ultra"]);
    assert_eq!(
        sol.efforts[5].description.as_deref(),
        Some("Maximum reasoning with automatic task delegation"),
        "the description is what tells a user what `ultra` costs"
    );

    // Four rungs, not six. This is the assertion a hardcoded ladder would fail.
    assert_eq!(models[1].efforts.len(), 4);
    assert!(!models[1].efforts.iter().any(|e| e.effort == "ultra"));
}

#[test]
fn preferring_a_rung_leaves_a_model_whose_ladder_lacks_it_on_its_own_default() {
    // The reason `prefer_effort` checks per model rather than assigning one constant: these ladders
    // differ *within* the provider, so an unconditional override would seed a model on a rung its
    // own protocol rejects. `gpt-5.6-sol` has `xhigh` and moves onto it; the synthetic model below
    // does not and keeps what the server advertised.
    let reply: serde_json::Value = serde_json::from_str(
        r#"{"id":3,"result":{"data":[
          {"id":"gpt-5.6-sol","model":"gpt-5.6-sol","displayName":"GPT-5.6-Sol","description":"","hidden":false,"isDefault":true,"defaultReasoningEffort":"medium","supportedReasoningEfforts":[{"reasoningEffort":"low","description":""},{"reasoningEffort":"medium","description":""},{"reasoningEffort":"high","description":""},{"reasoningEffort":"xhigh","description":""}]},
          {"id":"gpt-tiny","model":"gpt-tiny","displayName":"GPT-Tiny","description":"","hidden":false,"isDefault":false,"defaultReasoningEffort":"low","supportedReasoningEfforts":[{"reasoningEffort":"low","description":""},{"reasoningEffort":"medium","description":""}]}
        ],"nextCursor":null}}"#,
    )
    .unwrap();

    let mut capability = wtm_core::model::AgentCapability {
        models: wtm_agent::codex::parse_models(&reply),
        modes: wtm_agent::codex_modes(),
        models_are_live: true,
        supports_fast: false,
    };
    wtm_agent::prefer_effort(&mut capability);

    assert_eq!(
        capability.models[0].default_effort.as_deref(),
        Some(wtm_agent::capability::PREFERRED_EFFORT),
        "a model whose ladder has the preferred rung should be seeded on it"
    );
    assert_eq!(
        capability.models[1].default_effort.as_deref(),
        Some("low"),
        "a model without that rung keeps the default it advertised"
    );

    // The property that matters more than either specific value: whatever the seed ends up being, it
    // has to be a rung the model will accept.
    for model in &capability.models {
        let seed = model.default_effort.as_deref().unwrap();
        assert!(
            model.efforts.iter().any(|e| e.effort == seed),
            "{} was seeded on `{seed}`, which is not on its own ladder",
            model.id
        );
    }
}

#[test]
fn a_model_list_that_did_not_answer_yields_nothing_rather_than_a_guess() {
    // An error reply, or a reply to something else. Returning an empty list lets the caller say "this
    // agent reported no models — it may not be logged in", which is actionable; inventing a default
    // would offer a model the CLI may reject.
    for line in [
        r#"{"id":3,"error":{"code":-32000,"message":"not logged in"}}"#,
        r#"{"id":3,"result":{}}"#,
        r#"{"method":"thread/started","params":{}}"#,
    ] {
        let value: serde_json::Value = serde_json::from_str(line).unwrap();
        assert!(wtm_agent::codex::parse_models(&value).is_empty(), "{line}");
    }
}

#[test]
fn the_capability_frames_ask_without_the_experimental_capability() {
    // Verified against the real server: `model/list` needs no `experimentalApi`, and asking for one
    // defensively would be a claim this build cannot support.
    let frames: Vec<serde_json::Value> = wtm_agent::codex::model_list_frames()
        .iter()
        .map(|f| serde_json::from_str(f).unwrap())
        .collect();
    assert_eq!(frames.len(), 3);
    assert_eq!(frames[0]["method"], "initialize");
    assert!(frames[0]["params"]["capabilities"].is_null());
    // `initialized` before anything else, or the server refuses the request that follows.
    assert_eq!(frames[1]["method"], "initialized");
    assert_eq!(frames[2]["method"], "model/list");
    assert_eq!(frames[2]["id"], wtm_agent::codex::MODEL_LIST_ID);
}

#[test]
fn each_mode_preset_expands_to_both_protocol_axes() {
    // The protocol has two independent fields and Codex's own TUI offers three named combinations
    // of them. wtm offers the same three, so this is the table that has to stay true — a preset
    // that sent the wrong sandbox would be a control whose label does not describe what it does.
    //
    // `thread/start` spells the sandbox as a **string**. `turn/start` spells the same setting as a
    // tagged **object** under a different key, which is why the two are asserted separately below
    // rather than sharing one expectation. Both spellings are copied from
    // `codex app-server generate-json-schema` — `SandboxMode` and `SandboxPolicy`.
    for (mode, approval, sandbox, policy) in [
        ("read-only", "on-request", "read-only", "readOnly"),
        ("auto", "on-request", "workspace-write", "workspaceWrite"),
        (
            "full-access",
            "never",
            "danger-full-access",
            "dangerFullAccess",
        ),
    ] {
        let mut driver = Codex.protocol(&SessionRequest {
            cwd: "/tmp/worktree".to_owned(),
            mode: Some(mode.to_owned()),
            ..SessionRequest::default()
        });
        driver.open();
        let frames = writes(&driver.on_line(r#"{"id":1,"result":{"userAgent":"wtm"}}"#));
        let open = &frames[1]["params"];
        assert_eq!(open["approvalPolicy"], approval, "{mode} approval");
        assert_eq!(open["sandbox"], sandbox, "{mode} sandbox");
        assert!(
            open["sandbox"].is_string(),
            "thread/start takes SandboxMode, a string"
        );

        driver.on_line(r#"{"id":2,"result":{"thread":{"id":"t","cwd":"/tmp/worktree"}}}"#);
        let turn = writes(&driver.send_turn("go", &[]));
        let params = &turn[0]["params"];
        assert_eq!(params["approvalPolicy"], approval, "{mode} turn approval");
        // The object form, and the assertion that would have caught the bug that shipped: a plain
        // string here is rejected by the server, and a rejected `turn/start` produces no answer at
        // all — a session that takes a message, says `0 in 0 out`, and never replies.
        assert!(
            params["sandboxPolicy"].is_object(),
            "turn/start takes SandboxPolicy, a tagged object — got {}",
            params["sandboxPolicy"]
        );
        assert_eq!(
            params["sandboxPolicy"]["type"], policy,
            "{mode} turn policy"
        );
    }
}

#[test]
fn a_mode_this_build_does_not_know_falls_back_to_the_cautious_preset() {
    // A `wtm.toml` written against a later version, or a renamed preset. The unsafe direction is
    // silent: opening the sandbox because a string did not match is how unreviewed writes happen.
    // Falling back to the middle preset costs the user some approval prompts and nothing else.
    let mut driver = Codex.protocol(&SessionRequest {
        cwd: "/tmp/worktree".to_owned(),
        mode: Some("yolo-mode-9000".to_owned()),
        ..SessionRequest::default()
    });
    driver.open();
    let frames = writes(&driver.on_line(r#"{"id":1,"result":{"userAgent":"wtm"}}"#));
    assert_eq!(frames[1]["params"]["approvalPolicy"], "on-request");
    assert_eq!(frames[1]["params"]["sandbox"], "workspace-write");
}

#[test]
fn a_turn_start_the_server_rejects_is_surfaced_rather_than_looking_like_silence() {
    // How the `sandboxPolicy` bug presented, and the reason it took a report to find rather than a
    // glance: a rejected `turn/start` is an error reply, the turn simply never happens, and the
    // pane shows a usage row of zeros with nothing above it. This asserts the error reaches the
    // transcript, so the next protocol mistake reads as a failure instead of as a quiet agent.
    let mut driver = ready_driver();
    let turn = writes(&driver.send_turn("go", &[]));
    // The id `turn/start` actually got, rather than a literal. `ready_driver` leaves a
    // `skills/list` outstanding, and hard-coding an id that happened to be that one instead is how
    // this test used to pass while asserting nothing about `turn/start` at all.
    let id = turn[0]["id"].as_i64().expect("turn/start carries an id");

    let steps = driver.on_line(&format!(
        r#"{{"id":{id},"error":{{"code":-32602,"message":"invalid type: string \"workspace-write\", expected internally tagged enum SandboxPolicy"}}}}"#,
    ));
    match events(&steps).first().expect("a Failed event") {
        AgentEvent::Failed { message } => assert!(message.contains("SandboxPolicy"), "{message}"),
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[test]
fn a_turn_re_sends_the_mode_which_is_why_this_provider_needs_no_restart() {
    // The counterpart to Claude's control requests. Codex has no "change the mode" method at all —
    // it re-reads these off every `turn/start` — so `reconfigure` writes nothing and the change
    // rides the next turn instead.
    let mut driver = ready_driver();
    driver.reconfigure(Some("gpt-5.6-luna"), Some("max"), Some("full-access"), None);

    let frames = writes(&driver.send_turn("go", &[]));
    let params = &frames[0]["params"];
    assert_eq!(frames[0]["method"], "turn/start");
    assert_eq!(params["model"], "gpt-5.6-luna");
    assert_eq!(params["effort"], "max");
    assert_eq!(params["approvalPolicy"], "never");
    // `{"type":"dangerFullAccess"}`, not the `"danger-full-access"` string `thread/start` takes.
    // This line asserted the string until a real session found it: the server rejected every turn
    // and the pane answered nothing. Different key, different casing, different JSON type — see
    // `expand_mode`.
    assert_eq!(
        params["sandboxPolicy"],
        json!({ "type": "dangerFullAccess" })
    );
}

#[test]
fn reconfiguring_codex_writes_nothing_by_itself() {
    // Deliberate, and the reason it has its own test: a frame here would be a second way to say
    // the same thing, and a turn already in flight cannot be re-approved anyway. The change is
    // recorded and lands on the next turn — see the method's docs.
    let mut driver = ready_driver();
    assert!(
        driver
            .reconfigure(Some("gpt-5.5"), Some("high"), Some("read-only"), None)
            .is_empty()
    );
}

#[test]
fn the_skills_list_is_asked_for_after_the_session_is_usable_and_scoped_to_its_worktree() {
    // Nobody is blocked on the skill list, so gating `Ready` behind it would trade a pane that
    // opens late for a list the user has not asked to see. And the `cwds` scoping is what stops one
    // worktree's repo-scoped skills being offered in another's composer.
    let mut driver = Codex.protocol(&SessionRequest {
        cwd: "/tmp/worktree".to_owned(),
        ..SessionRequest::default()
    });
    driver.open();
    driver.on_line(r#"{"id":1,"result":{"userAgent":"wtm"}}"#);

    let opened = driver.on_line(
        r#"{"id":2,"result":{"thread":{"id":"019fd37c-f1e4-7a22-81e7-02200fd6d127","cwd":"/tmp/worktree"}}}"#,
    );
    assert!(opened.contains(&Step::Ready), "ready first, then ask");

    let ask = writes(&opened);
    assert_eq!(ask.len(), 1);
    assert_eq!(ask[0]["method"], "skills/list");
    assert_eq!(ask[0]["params"]["cwds"][0], "/tmp/worktree");

    // The reply is grouped by cwd because the method takes several. Disabled entries are in the
    // answer so a settings UI can grey them out; offering one here would insert a name that does
    // nothing when sent.
    let listed = driver.on_line(
        r#"{"id":3,"result":{"data":[{"cwd":"/tmp/worktree","errors":[],"skills":[
          {"name":"review","description":"A long model-facing prompt","shortDescription":"Review a diff","enabled":true,"path":"/s/review","scope":"repo"},
          {"name":"deploy","description":"Ship it","enabled":false,"path":"/s/deploy","scope":"user"},
          {"name":"plan","description":"Plan the work","enabled":true,"path":"/s/plan","scope":"user"}
        ]}]}}"#,
    );

    match events(&listed).first().expect("SkillsListed") {
        AgentEvent::SkillsListed { skills } => {
            let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
            assert_eq!(names, ["review", "plan"], "the disabled one is dropped");
            // `shortDescription` wins where there is one: the schema says the long `description` is
            // the model-facing prompt, which is the wrong thing to put in a picker.
            assert_eq!(skills[0].description.as_deref(), Some("Review a diff"));
            assert_eq!(skills[1].description.as_deref(), Some("Plan the work"));
            assert_eq!(skills[0].scope.as_deref(), Some("repo"));
        }
        other => panic!("expected SkillsListed, got {other:?}"),
    }
}

#[test]
fn a_skills_reply_that_says_nothing_useful_yields_an_empty_list_rather_than_a_failure() {
    let mut driver = skills_driver();

    let steps = driver.on_line(r#"{"id":3,"result":{"data":[]}}"#);
    match events(&steps).first().expect("SkillsListed") {
        AgentEvent::SkillsListed { skills } => assert!(skills.is_empty()),
        other => panic!("expected SkillsListed, got {other:?}"),
    }
}

#[test]
fn a_server_too_old_to_know_skills_list_does_not_take_the_session_down_with_it() {
    // This is what the test above claimed to cover and did not: it fed a *success* reply, so the
    // error path — which returned `Failed` for any rejected request, whatever it was for — was
    // never exercised. The composer simply has no `/` list, which is the same state Claude is in
    // before its init line arrives, and is not worth a red row in a working session's transcript.
    let mut driver = skills_driver();

    let steps = driver
        .on_line(r#"{"id":3,"error":{"code":-32601,"message":"Method not found: skills/list"}}"#);
    assert!(
        events(&steps).is_empty(),
        "a refused skills list must draw nothing: {:?}",
        events(&steps)
    );

    // And the id must have been released, or the next reply carrying it is read as a skills list.
    let after = driver.on_line(r#"{"id":3,"error":{"code":-32602,"message":"turn rejected"}}"#);
    assert!(
        matches!(events(&after).first(), Some(AgentEvent::Failed { .. })),
        "a later error on a released id is an ordinary failure: {:?}",
        events(&after)
    );
}

/// A driver at the point where `skills/list` is outstanding as id 3.
fn skills_driver() -> Box<dyn Protocol> {
    let mut driver = Codex.protocol(&SessionRequest {
        cwd: "/tmp/worktree".to_owned(),
        ..SessionRequest::default()
    });
    driver.open();
    driver.on_line(r#"{"id":1,"result":{"userAgent":"wtm"}}"#);
    driver.on_line(r#"{"id":2,"result":{"thread":{"id":"t","cwd":"/tmp/worktree"}}}"#);
    driver
}
