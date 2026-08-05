//! The Claude Code protocol driver, fed the lines the real CLI actually sends.
//!
//! # Where these came from
//!
//! `claude 2.1.221`, driven with a turn that made a tool call and had it denied, captured verbatim.
//! That matters here more than it sounds: the previous provider's fixtures were written from a
//! *different transport's* serialization of the same events, and every test passed while the mapping
//! was wrong. A fixture in this file that was not captured is a fixture that proves nothing.
//!
//! # The four shapes a guess would get wrong
//!
//!   * a text delta is nested three deep — `stream_event.event.delta.text`, with the *kind* on
//!     `event.delta.type` and only an `index` to say which content block it belongs to;
//!   * thinking and text are told apart by that index, so `content_block_start` has to be tracked;
//!   * `control_request` carries its correlation id at the **top level** (`request_id`), not inside
//!     `request`, which is where every other id on this transport lives;
//!   * `result` reports `total_cost_usd` — real currency, which the other provider has none of.

use pretty_assertions::assert_eq;
use wtm_agent::claude::Claude;
use wtm_agent::provider::{Protocol, Provider, SessionRequest, Step};
use wtm_core::model::{AgentEvent, ApprovalAnswer, ApprovalRequest};

fn driver() -> Box<dyn Protocol> {
    Claude.protocol(&SessionRequest {
        cwd: "/tmp/worktree".to_owned(),
        ..SessionRequest::default()
    })
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
            Step::Write(f) => Some(serde_json::from_str(f).expect("a frame must be JSON")),
            _ => None,
        })
        .collect()
}

#[test]
fn the_argv_carries_the_undocumented_flag_that_makes_approvals_exist() {
    // `--permission-prompt-tool stdio` is in neither `--help` nor the public docs, and without it
    // `can_use_tool` never arrives: the tool is auto-denied and shows up only afterwards in
    // `result.permission_denials`. A session then appears to work while every edit silently fails,
    // which is the most expensive way for this to be wrong.
    let argv = Claude.argv(&SessionRequest {
        cwd: "/tmp/worktree".to_owned(),
        model: Some("opus".to_owned()),
        effort: Some("xhigh".to_owned()),
        mode: Some("plan".to_owned()),
        ..SessionRequest::default()
    });

    let pair = |flag: &str| {
        argv.iter()
            .position(|a| a == flag)
            .and_then(|i| argv.get(i + 1))
            .cloned()
    };

    assert_eq!(pair("--permission-prompt-tool").as_deref(), Some("stdio"));
    // Without this there are no deltas at all — text arrives as whole messages, so a transcript
    // would appear a paragraph at a time.
    assert!(argv.iter().any(|a| a == "--include-partial-messages"));
    assert_eq!(pair("--output-format").as_deref(), Some("stream-json"));
    assert_eq!(pair("--input-format").as_deref(), Some("stream-json"));
    assert_eq!(pair("--model").as_deref(), Some("opus"));
    assert_eq!(pair("--effort").as_deref(), Some("xhigh"));
    assert_eq!(pair("--permission-mode").as_deref(), Some("plan"));
    // The cwd is already the worktree, but a tool that checks an allow-list refuses without this
    // and reports `blocked_path`, which reads as a bug in wtm.
    assert_eq!(pair("--add-dir").as_deref(), Some("/tmp/worktree"));
}

#[test]
fn a_fresh_session_id_is_minted_per_session_and_resume_uses_a_different_flag() {
    // Reusing a `--session-id` is a hard error — "Session ID … is already in use" — and the process
    // exits before saying anything on stdout, which presents as a pane that dies instantly.
    let a = Claude.argv(&SessionRequest::default());
    let b = Claude.argv(&SessionRequest::default());
    let id_of = |argv: &[String]| {
        argv.iter()
            .position(|x| x == "--session-id")
            .and_then(|i| argv.get(i + 1))
            .cloned()
    };
    assert_ne!(id_of(&a), id_of(&b), "each session needs its own id");

    let resumed = Claude.argv(&SessionRequest {
        resume: Some("11111111-2222-4333-8444-555555555555".to_owned()),
        ..SessionRequest::default()
    });
    assert!(
        !resumed.iter().any(|a| a == "--session-id"),
        "resuming must not also claim a new id"
    );
    assert!(resumed.iter().any(|a| a == "--resume"));
}

#[test]
fn the_init_message_makes_the_session_ready_and_reports_the_id_the_cli_chose() {
    let mut d = driver();
    // Ready at once: this CLI emits nothing until it is sent a turn, so gating readiness on `init`
    // would leave a pane saying "starting…" until someone typed into it.
    assert_eq!(d.open(), vec![Step::Ready]);

    let steps = d.on_line(
        r#"{"type":"system","subtype":"init","cwd":"/tmp/worktree","session_id":"e8581508-0000-4000-8000-06a0e8581508","tools":["Bash","Read","Write"],"mcp_servers":[{"name":"pycharm-debugger","status":"failed"}],"model":"claude-haiku-4-5-20251001","permissionMode":"default","slash_commands":["review"]}"#,
    );

    assert!(steps.contains(&Step::Ready));
    match events(&steps).first().expect("SessionReady") {
        AgentEvent::SessionReady {
            provider_session_id,
            model,
            tools,
            ..
        } => {
            assert_eq!(provider_session_id, "e8581508-0000-4000-8000-06a0e8581508");
            assert_eq!(model.as_deref(), Some("claude-haiku-4-5-20251001"));
            assert_eq!(tools.len(), 3);
        }
        other => panic!("expected SessionReady, got {other:?}"),
    }
}

#[test]
fn text_and_thinking_deltas_are_told_apart_by_their_block_index() {
    // The delta itself says only `index`. Without tracking which index was opened as a thinking
    // block, a chain of thought renders as the answer — the single most confusing possible bug in a
    // chat transcript.
    let mut d = driver();

    d.on_line(
        r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}},"session_id":"s"}"#,
    );
    let thinking = d.on_line(
        r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"The user wants me to","estimated_tokens":null}},"session_id":"s"}"#,
    );
    assert_eq!(
        events(&thinking),
        vec![&AgentEvent::ReasoningDelta {
            text: "The user wants me to".to_owned()
        }]
    );

    d.on_line(
        r#"{"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}},"session_id":"s"}"#,
    );
    let text = d.on_line(
        r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"Done."}},"session_id":"s"}"#,
    );
    assert_eq!(
        events(&text),
        vec![&AgentEvent::MessageDelta {
            text: "Done.".to_owned()
        }]
    );
}

#[test]
fn a_tool_call_and_its_result_pair_up_by_id() {
    let mut d = driver();

    let started = d.on_line(
        r#"{"type":"assistant","message":{"model":"claude-haiku-4-5-20251001","id":"msg_1","type":"message","role":"assistant","content":[{"type":"tool_use","id":"toolu_01JhCSgG","name":"Write","input":{"file_path":"/tmp/worktree/probe.txt","content":"hello"},"caller":{"type":"direct"}}],"stop_reason":null,"usage":{"input_tokens":10,"output_tokens":3}},"parent_tool_use_id":null,"session_id":"s","uuid":"u"}"#,
    );
    assert_eq!(
        events(&started),
        vec![&AgentEvent::ToolStarted {
            id: "toolu_01JhCSgG".to_owned(),
            name: "Write".to_owned(),
            title: Some("Write /tmp/worktree/probe.txt".to_owned()),
        }]
    );

    let finished = d.on_line(
        r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"probe","is_error":true,"tool_use_id":"toolu_01JhCSgG"}]},"parent_tool_use_id":null,"session_id":"s","uuid":"u2","tool_use_result":"Error: probe"}"#,
    );
    assert_eq!(
        events(&finished),
        vec![&AgentEvent::ToolFinished {
            id: "toolu_01JhCSgG".to_owned(),
            ok: false,
            output: Some("probe".to_owned()),
        }]
    );
}

#[test]
fn an_assistant_messages_text_block_is_dropped_because_the_deltas_already_carried_it() {
    // Otherwise every reply appears twice: once streamed, once whole. Dropped here rather than in
    // the frontend, so a provider that streams and one that does not both work unchanged.
    let mut d = driver();
    let steps = d.on_line(
        r#"{"type":"assistant","message":{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"DONE"},{"type":"thinking","thinking":"…","signature":"x"}]},"session_id":"s"}"#,
    );
    assert!(steps.is_empty());
}

#[test]
fn thinking_token_counters_draw_nothing() {
    // A real turn sent sixteen of these for one reply. There is a reasoning *stream* for the
    // content; this is a running estimate, and showing it would bury everything else.
    let mut d = driver();
    for _ in 0..3 {
        assert!(
            d.on_line(
                r#"{"type":"system","subtype":"thinking_tokens","estimated_tokens":140,"estimated_tokens_delta":61,"uuid":"u","session_id":"s"}"#
            )
            .is_empty()
        );
    }
    assert!(
        d.on_line(r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed"},"uuid":"u","session_id":"s"}"#)
            .is_empty()
    );
}

#[test]
fn a_can_use_tool_request_becomes_an_approval_answered_on_the_top_level_request_id() {
    // The correlation id is at the top level, not inside `request` — unlike every other id on this
    // transport. Replying with the wrong one leaves the CLI blocked forever.
    let mut d = driver();
    let steps = d.on_line(
        r#"{"type":"control_request","request_id":"c7579cb3-d345-4b0d-97c5-9a324eb871c5","request":{"subtype":"can_use_tool","tool_name":"Bash","display_name":"Bash","input":{"command":"echo hello > probe.txt","description":"Create probe.txt"},"description":"Create probe.txt","permission_suggestions":[{"type":"addRules","rules":[{"toolName":"Bash","ruleContent":"echo hello *"}],"behavior":"allow","destination":"localSettings"}],"tool_use_id":"toolu_01EwB2fr"}}"#,
    );

    let id = match events(&steps).first().expect("an approval") {
        AgentEvent::ApprovalRequested {
            id,
            blocking,
            request,
        } => {
            assert!(*blocking);
            assert_eq!(
                *request,
                ApprovalRequest::Command {
                    command: "echo hello > probe.txt".to_owned(),
                    cwd: None,
                    reason: Some("Create probe.txt".to_owned()),
                }
            );
            id.clone()
        }
        other => panic!("expected ApprovalRequested, got {other:?}"),
    };
    assert_eq!(id, "c7579cb3-d345-4b0d-97c5-9a324eb871c5");

    let allowed = d.answer(&id, &ApprovalAnswer::Allow);
    let frames = writes(&allowed);
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0]["type"], "control_response");
    assert_eq!(frames[0]["response"]["subtype"], "success");
    assert_eq!(frames[0]["response"]["request_id"], id);
    assert_eq!(frames[0]["response"]["response"]["behavior"], "allow");
    // `updatedInput` echoes what arrived. It was *required* before 2.1.207, and sending the
    // original back is the correct no-op on every version.
    assert_eq!(
        frames[0]["response"]["response"]["updatedInput"]["command"],
        "echo hello > probe.txt"
    );

    // The first answer wins, same as the other provider.
    assert!(d.answer(&id, &ApprovalAnswer::Allow).is_empty());
}

#[test]
fn always_this_session_passes_back_the_clis_own_suggestion() {
    // The CLI proposes what "always" should mean — a pattern rule for `Bash`, a mode change for
    // `Write`. It knows better than we do, and passing its suggestion back is what makes the button
    // grant the right thing rather than the broadest thing.
    let mut d = driver();
    let steps = d.on_line(
        r#"{"type":"control_request","request_id":"r1","request":{"subtype":"can_use_tool","tool_name":"Write","display_name":"Write","input":{"file_path":"/tmp/worktree/probe.txt","content":"hello"},"description":"probe.txt","permission_suggestions":[{"type":"setMode","mode":"acceptEdits","destination":"session"}],"tool_use_id":"toolu_1"}}"#,
    );
    let AgentEvent::ApprovalRequested { id, request, .. } = events(&steps)[0].clone() else {
        panic!("expected an approval");
    };
    // A `Write` is a file change, not a command, and its "diff" is the payload it really is —
    // synthesising a unified diff here would mean inventing line numbers.
    assert!(matches!(request, ApprovalRequest::FileChange { .. }));

    let frames = writes(&d.answer(&id, &ApprovalAnswer::AllowForSession));
    assert_eq!(
        frames[0]["response"]["response"]["updatedPermissions"][0]["mode"],
        "acceptEdits"
    );
}

#[test]
fn an_edited_allow_is_honoured_here_because_this_provider_has_the_verb_for_it() {
    // The asymmetry the `ApprovalAnswer` union exists for. Codex refuses this answer; Claude Code's
    // allow can carry a replacement payload, so the edited input is what runs.
    let mut d = driver();
    let steps = d.on_line(
        r#"{"type":"control_request","request_id":"r2","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"rm -rf /"},"tool_use_id":"t"}}"#,
    );
    let AgentEvent::ApprovalRequested { id, .. } = events(&steps)[0].clone() else {
        panic!("expected an approval");
    };

    let frames = writes(&d.answer(
        &id,
        &ApprovalAnswer::AllowWithEdits {
            input: serde_json::json!({ "command": "rm -rf ./build" }),
        },
    ));
    assert_eq!(frames[0]["response"]["response"]["behavior"], "allow");
    assert_eq!(
        frames[0]["response"]["response"]["updatedInput"]["command"], "rm -rf ./build",
        "the edited command is what runs"
    );
}

#[test]
fn exit_plan_mode_becomes_a_plan_review_rather_than_an_ordinary_tool_approval() {
    // The whole reason a GUI can intercept plan approval: it arrives as a tool permission, carrying
    // the plan as markdown and the path the CLI wrote it to.
    let mut d = driver();
    let steps = d.on_line(
        // `r##` rather than `r#`: the plan's markdown heading makes the JSON contain `"#`,
        // which terminates a single-hash raw string early.
        r##"{"type":"control_request","request_id":"r3","request":{"subtype":"can_use_tool","tool_name":"ExitPlanMode","display_name":"ExitPlanMode","input":{"allowedPrompts":[],"plan":"# Add a comment\n\n## Plan\n1. Read it\n2. Write it\n","planFilePath":"/home/.claude/plans/luminous-wall.md"},"tool_use_id":"toolu_01XUbNV","requires_user_interaction":true}}"##,
    );

    match events(&steps).first().expect("an approval") {
        AgentEvent::ApprovalRequested { request, .. } => match request {
            ApprovalRequest::PlanReview { markdown, path } => {
                assert!(markdown.starts_with("# Add a comment"));
                assert_eq!(
                    path.as_deref(),
                    Some("/home/.claude/plans/luminous-wall.md")
                );
            }
            other => panic!("expected PlanReview, got {other:?}"),
        },
        other => panic!("expected ApprovalRequested, got {other:?}"),
    }
}

#[test]
fn a_control_request_this_build_does_not_handle_is_still_declinable() {
    // An unanswered control request blocks the CLI exactly as an unanswered approval does, so an
    // unknown subtype is surfaced *and* kept.
    let mut d = driver();
    let steps = d.on_line(
        r#"{"type":"control_request","request_id":"r9","request":{"subtype":"hook_callback","callback_id":"x"}}"#,
    );
    assert!(matches!(
        events(&steps).first(),
        Some(AgentEvent::Raw { .. })
    ));

    let frames = writes(&d.abandon());
    assert_eq!(frames.len(), 1);
    assert_eq!(frames[0]["response"]["request_id"], "r9");
    assert_eq!(frames[0]["response"]["response"]["behavior"], "deny");
}

#[test]
fn a_turn_is_announced_before_the_frame_goes_out_and_reports_cost_when_it_ends() {
    // Claude announces no turn start of its own, so the provider is where one exists — emitted on
    // send rather than inferred from the first delta, so the composer can show "working…" during
    // the seconds before any token arrives.
    let mut d = driver();
    let sent = d.send_turn("write probe.txt");

    assert_eq!(
        events(&sent),
        vec![
            &AgentEvent::UserEcho {
                text: "write probe.txt".to_owned()
            },
            &AgentEvent::TurnStarted {
                turn: "1".to_owned()
            },
        ]
    );
    let frames = writes(&sent);
    assert_eq!(frames[0]["type"], "user");
    assert_eq!(frames[0]["message"]["content"], "write probe.txt");

    let finished = d.on_line(
        r#"{"type":"result","subtype":"success","is_error":false,"duration_api_ms":13243,"num_turns":3,"stop_reason":"end_turn","session_id":"s","total_cost_usd":0.0332116,"usage":{"input_tokens":26,"cache_creation_input_tokens":9897,"cache_read_input_tokens":81276,"output_tokens":931},"modelUsage":{"claude-haiku-4-5-20251001":{"inputTokens":560,"outputTokens":946,"contextWindow":200000,"maxOutputTokens":32000}}}"#,
    );
    match events(&finished).first().expect("TurnFinished") {
        AgentEvent::TurnFinished {
            turn,
            usage,
            cost_usd,
        } => {
            assert_eq!(turn, "1", "the same turn the send announced");
            assert_eq!(usage.tokens_in, 26);
            assert_eq!(usage.tokens_out, 931);
            assert_eq!(usage.cached, 81276);
            // Nested under the model's own name, which is not known in advance.
            assert_eq!(usage.context_window, Some(200_000));
            // Real currency. Codex reports none at all, which is why the field is an `Option`
            // rather than a number this app computes.
            assert_eq!(*cost_usd, Some(0.033_211_6));
        }
        other => panic!("expected TurnFinished, got {other:?}"),
    }
}

#[test]
fn an_unknown_message_type_becomes_a_raw_row_rather_than_being_dropped() {
    let mut d = driver();
    let steps =
        d.on_line(r#"{"type":"prompt_suggestion","suggestion":"try this next","session_id":"s"}"#);
    match events(&steps).first().expect("a Raw event") {
        AgentEvent::Raw {
            provider, event, ..
        } => {
            assert_eq!(provider, "claude");
            assert_eq!(event, "prompt_suggestion");
        }
        other => panic!("expected Raw, got {other:?}"),
    }
}

#[test]
fn output_that_is_not_json_is_surfaced_as_a_notice() {
    let mut d = driver();
    let steps = d.on_line("Error: Session ID 1111 is already in use.");
    assert!(matches!(
        events(&steps).first(),
        Some(AgentEvent::Notice { .. })
    ));
}

#[test]
fn a_real_turns_events_are_the_conversation_and_nothing_else() {
    // The regression test for the noise, taken from a real turn that wrote a file after an approval
    // was allowed. Before the suppression list this sequence produced three `Raw` rows around a
    // two-word answer — a `status` either side of the tool call and a `post_turn_summary` at the
    // end. Each is the CLI talking to its own UI.
    const WIRE: &[&str] = &[
        r#"{"type":"system","subtype":"init","session_id":"04892eee","tools":["Bash"],"model":"claude-haiku-4-5-20251001","permissionMode":"default"}"#,
        r#"{"type":"system","subtype":"status","status":{"phase":"thinking"},"session_id":"s"}"#,
        r#"{"type":"system","subtype":"thinking_tokens","estimated_tokens":140,"estimated_tokens_delta":61,"session_id":"s"}"#,
        r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}},"session_id":"s"}"#,
        r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"DONE"}},"session_id":"s"}"#,
        r#"{"type":"system","subtype":"post_turn_summary","summarizes_uuid":"u","status_category":"review_ready","needs_action":false,"session_id":"s"}"#,
        r#"{"type":"result","subtype":"success","is_error":false,"session_id":"s","total_cost_usd":0.0232163,"usage":{"input_tokens":18,"output_tokens":263,"cache_read_input_tokens":51413},"modelUsage":{"claude-haiku-4-5-20251001":{"contextWindow":200000}}}"#,
    ];

    let mut d = driver();
    d.open();
    let produced: Vec<AgentEvent> = WIRE
        .iter()
        .flat_map(|l| d.on_line(l))
        .filter_map(|s| match s {
            Step::Emit(e) => Some(e),
            _ => None,
        })
        .collect();

    let raw: Vec<&AgentEvent> = produced
        .iter()
        .filter(|e| matches!(e, AgentEvent::Raw { .. }))
        .collect();
    assert!(
        raw.is_empty(),
        "these lines are all recognised, got {raw:?}"
    );

    assert!(matches!(produced[0], AgentEvent::SessionReady { .. }));
    assert_eq!(
        produced[1],
        AgentEvent::MessageDelta {
            text: "DONE".to_owned()
        }
    );
    match &produced[2] {
        AgentEvent::TurnFinished {
            cost_usd, usage, ..
        } => {
            // Real currency, which the other provider reports none of.
            assert_eq!(*cost_usd, Some(0.023_216_3));
            assert_eq!(usage.cached, 51413);
        }
        other => panic!("expected TurnFinished, got {other:?}"),
    }
    assert_eq!(produced.len(), 3, "the conversation and nothing else");
}
