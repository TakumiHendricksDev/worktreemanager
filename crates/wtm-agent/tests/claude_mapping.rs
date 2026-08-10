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

// `unwrap_used` is banned in the app so a failure carries a message. In an assertion it adds noise
// without adding information — a panic is the failure report either way — which is the allowance
// `wtm-exec` grants its own tests via `lib.rs`. An integration test is its own crate, so it has to
// say so here.
#![allow(clippy::unwrap_used)]

use pretty_assertions::assert_eq;
use wtm_agent::claude::Claude;
use wtm_agent::provider::{Protocol, Provider, SessionRequest, Step};
use wtm_core::model::{AgentEvent, ApprovalAnswer, ApprovalRequest, ModeRisk};

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
fn an_assistant_messages_thinking_block_is_dropped_because_the_deltas_already_carried_it() {
    // Narrowed from "text and thinking are always dropped", which was too strong and cost a user a
    // silent pane: when nothing streamed, the `assistant` message is the *only* copy of the reply.
    // See `a_turn_the_cli_could_not_run_reports_why_instead_of_an_empty_pane`.
    //
    // Thinking stays unconditional. It has no synthetic case — a refusal or an auth failure carries
    // a `text` block and never a `thinking` one — and re-emitting a completed chain of thought
    // would put the whole thing in the transcript a second time, unstreamed, after the answer.
    let mut d = driver();
    d.on_line(
        r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"DONE"}},"session_id":"s"}"#,
    );
    let steps = d.on_line(
        r#"{"type":"assistant","message":{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"DONE"},{"type":"thinking","thinking":"…","signature":"x"}]},"session_id":"s"}"#,
    );
    assert!(steps.is_empty(), "both blocks already streamed");

    // And with no deltas at all, the thinking block is *still* dropped while the text is not.
    let mut fresh = driver();
    let alone = fresh.on_line(
        r#"{"type":"assistant","message":{"id":"msg_2","type":"message","role":"assistant","content":[{"type":"thinking","thinking":"…","signature":"x"}]},"session_id":"s"}"#,
    );
    assert!(alone.is_empty(), "thinking is never re-emitted whole");
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

#[test]
fn the_compiled_capability_is_honest_about_being_compiled() {
    // There is no `model/list` here — `--model` takes an alias or an id and the CLI validates it at
    // startup, so the only way to enumerate is to know. `models_are_live: false` is what lets the UI
    // say "as of this build" rather than presenting a stale table as the CLI's answer.
    let capability = wtm_agent::claude_capability();
    assert!(
        !capability.models_are_live,
        "a compiled table must not claim to be live"
    );

    // Aliases rather than dated ids: each resolves to the current model of its tier, so this list
    // ages far better than `claude-opus-4-5-20251101` would. The *labels* carry a version and the
    // ids do not, which is the trade the capability's own docs explain.
    let ids: Vec<&str> = capability.models.iter().map(|m| m.id.as_str()).collect();
    assert!(ids.contains(&"opus") && ids.contains(&"sonnet") && ids.contains(&"haiku"));

    // The rungs, the same for every model — the opposite of the other provider, where the ladder is
    // per model. The first five are `--help`'s own list; `ultracode` is last because the CLI's
    // interactive `/effort` puts it last.
    for model in &capability.models {
        let ladder: Vec<&str> = model.efforts.iter().map(|e| e.effort.as_str()).collect();
        assert_eq!(
            ladder,
            ["low", "medium", "high", "xhigh", "max", "ultracode"],
            "{} should carry the documented ladder",
            model.id
        );
        assert!(
            !ladder.contains(&"ultra"),
            "`ultra` is Codex's rung. Claude's is `ultracode` — two names, two things"
        );
    }
}

#[test]
fn every_offered_mode_is_one_the_permission_flag_actually_accepts() {
    // The list this replaced contained `default`, which `--permission-mode` rejects outright, and
    // was missing `auto`. Both failures present as a session that dies at spawn with the CLI's own
    // usage error, which is a long way from the picker the user clicked.
    //
    // Verbatim from `claude --help` on 2.1.221:
    //   --permission-mode <mode>  (choices: "acceptEdits", "auto", "bypassPermissions", "manual",
    //                              "dontAsk", "plan")
    let accepted = [
        "acceptEdits",
        "auto",
        "bypassPermissions",
        "manual",
        "dontAsk",
        "plan",
    ];
    let capability = wtm_agent::claude_capability();

    for mode in &capability.modes {
        assert!(
            accepted.contains(&mode.id.as_str()),
            "`{}` is not a value the flag takes",
            mode.id
        );
        assert!(
            mode.label != mode.id,
            "`{}` needs a written label — a wire value is not a label",
            mode.id
        );
    }

    // The three that act without asking must be rated as such. A picker that renders every mode in
    // the same quiet grey is how someone leaves a worktree in `bypassPermissions` overnight.
    let risk = |id: &str| {
        capability
            .modes
            .iter()
            .find(|m| m.id == id)
            .unwrap_or_else(|| panic!("{id} should be offered"))
            .risk
    };
    assert_eq!(risk("manual"), ModeRisk::Normal);
    assert_eq!(risk("plan"), ModeRisk::Normal);
    assert_eq!(risk("acceptEdits"), ModeRisk::Elevated);
    assert_eq!(risk("auto"), ModeRisk::Elevated);
    assert_eq!(risk("dontAsk"), ModeRisk::Elevated);
    assert_eq!(risk("bypassPermissions"), ModeRisk::Unsandboxed);

    // None marked default, unlike Codex. wtm passes no `--permission-mode` for this provider so
    // that `~/.claude/settings.json` decides, and marking one here would make the picker send it on
    // every session and silently override that setting. The pane learns the real mode from `init`.
    assert!(
        capability.modes.iter().all(|m| !m.is_default),
        "a default here would override the user's own settings.json"
    );

    // Codex is the other way round, and for a reason that is not symmetry: its mode is *two*
    // protocol fields that only mean something together, so wtm has to send both or neither.
    assert_eq!(
        wtm_agent::codex_modes()
            .iter()
            .filter(|m| m.is_default)
            .count(),
        1,
        "codex must start somewhere, since wtm is the one composing its two settings"
    );
}

#[test]
fn the_ultracode_rung_becomes_a_settings_key_because_the_effort_flag_rejects_it() {
    // `--effort` takes `low|medium|high|xhigh|max` and nothing else, so passing the sixth rung
    // through verbatim would fail at spawn. The CLI's own words: ultracode is "set per session via
    // the `ultracode` settings key (--settings or apply_flag_settings)".
    let argv = Claude.argv(&SessionRequest {
        cwd: "/tmp/worktree".to_owned(),
        effort: Some("ultracode".to_owned()),
        ..SessionRequest::default()
    });

    assert!(
        !argv.contains(&"ultracode".to_owned()),
        "the rung's name must never reach the CLI as a flag value: {argv:?}"
    );

    let effort = argv.iter().position(|a| a == "--effort").expect("--effort");
    // `max`, not the documented minimum of `xhigh`: the ladder puts ultracode above max, so picking
    // the top rung must not quietly buy less reasoning than the rung below it.
    assert_eq!(argv[effort + 1], "max");

    let settings = argv
        .iter()
        .position(|a| a == "--settings")
        .expect("--settings");
    assert_eq!(argv[settings + 1], r#"{"ultracode":true}"#);
}

#[test]
fn an_ordinary_effort_still_passes_straight_through() {
    // The translation above must be the special case and not the rule — a plain rung reaching the
    // CLI as `--effort xhigh` with no settings overlay is what keeps a user's own `settings.json`
    // untouched on every session that did not ask for ultracode.
    let argv = Claude.argv(&SessionRequest {
        cwd: "/tmp/worktree".to_owned(),
        effort: Some("xhigh".to_owned()),
        ..SessionRequest::default()
    });

    let effort = argv.iter().position(|a| a == "--effort").expect("--effort");
    assert_eq!(argv[effort + 1], "xhigh");
    assert!(!argv.contains(&"--settings".to_owned()));
}

#[test]
fn the_mode_the_init_message_reports_is_spelled_the_way_the_flag_spells_it() {
    // One state, two spellings: the flag takes `manual`, this message says `default`. Passing the
    // message's word back through the picker would offer a mode the flag rejects, so it is
    // normalized at the boundary rather than in the UI.
    let argv = Claude.argv(&SessionRequest {
        cwd: "/tmp/worktree".to_owned(),
        mode: Some("default".to_owned()),
        ..SessionRequest::default()
    });
    let at = argv
        .iter()
        .position(|a| a == "--permission-mode")
        .expect("--permission-mode");
    assert_eq!(argv[at + 1], "manual");
}

#[test]
fn the_slash_commands_the_init_message_lists_become_the_composers_skill_list() {
    // Already on the wire and previously discarded. The CLI calls these skills itself —
    // `--disable-slash-commands` is documented as "Disable all skills" — and this is its own merged
    // list of user, project, plugin and built-in, which is why wtm does not go looking on disk.
    let mut d = driver();
    let _ = d.open();

    let steps = d.on_line(
        r#"{"type":"system","subtype":"init","cwd":"/tmp/worktree","session_id":"e8581508-0000-4000-8000-06a0e8581508","tools":["Bash"],"model":"claude-haiku-4-5-20251001","permissionMode":"acceptEdits","slash_commands":["review","frontend-design"]}"#,
    );

    let listed = events(&steps)
        .into_iter()
        .find_map(|e| match e {
            AgentEvent::SkillsListed { skills } => Some(skills),
            _ => None,
        })
        .expect("SkillsListed");
    let names: Vec<&str> = listed.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["review", "frontend-design"]);
    // No descriptions on this transport, and none invented. A `None` here is what tells the UI to
    // render one column instead of two.
    assert!(listed.iter().all(|s| s.description.is_none()));

    // And the mode it resolved to, which wtm cannot otherwise know: it deliberately passes no
    // `--permission-mode`, so `~/.claude/settings.json` is the only thing that decided this.
    match events(&steps).first().expect("SessionReady") {
        AgentEvent::SessionReady { mode, .. } => assert_eq!(mode.as_deref(), Some("acceptEdits")),
        other => panic!("expected SessionReady, got {other:?}"),
    }
}

#[test]
fn a_session_with_no_slash_commands_emits_no_empty_skill_row() {
    // An absent list and an empty one are the same thing, and neither should push an event that
    // makes the composer show a `/` affordance with nothing behind it.
    let mut d = driver();
    let _ = d.open();
    let steps = d.on_line(
        r#"{"type":"system","subtype":"init","session_id":"e8581508-0000-4000-8000-06a0e8581508","tools":[],"model":"claude-haiku-4-5-20251001"}"#,
    );
    assert!(
        !events(&steps)
            .iter()
            .any(|e| matches!(e, AgentEvent::SkillsListed { .. }))
    );
}

#[test]
fn changing_the_model_or_the_mode_writes_control_requests_rather_than_needing_a_restart() {
    // Both subtypes were read off the shipped binary, like `--permission-prompt-tool` before them.
    // This test is the record of what was observed, so a CLI that renames one fails here rather
    // than silently leaving the picker showing a setting the session is not using.
    let mut d = driver();
    let _ = d.open();

    let sent = writes(&d.reconfigure(Some("opus"), Some("acceptEdits")));
    assert_eq!(sent.len(), 2, "one frame each, and nothing else");

    assert_eq!(sent[0]["type"], "control_request");
    assert_eq!(sent[0]["request"]["subtype"], "set_model");
    assert_eq!(sent[0]["request"]["model"], "opus");
    assert_eq!(sent[1]["request"]["subtype"], "set_permission_mode");
    assert_eq!(sent[1]["request"]["mode"], "acceptEdits");
    // The correlation id lives at the top level on this transport, not inside `request` — the same
    // trap the module header calls out for inbound control requests.
    assert!(sent[0]["request_id"].is_string());
    assert_ne!(sent[0]["request_id"], sent[1]["request_id"]);
}

#[test]
fn reconfiguring_only_one_setting_leaves_the_other_alone() {
    // `None` has to mean "do not mention it" rather than "clear it". A caller changing the mode
    // does not necessarily know the model, and a frame asserting a stale one would undo a change
    // the user made a moment earlier.
    let mut d = driver();
    let _ = d.open();
    assert_eq!(writes(&d.reconfigure(None, Some("plan"))).len(), 1);
    assert!(writes(&d.reconfigure(None, None)).is_empty());
}

#[test]
fn a_refused_settings_change_is_reported_instead_of_being_swallowed() {
    // The failure mode that makes a live mode picker safe to ship. If the CLI does not know the
    // subtype, the picker would otherwise show `bypassPermissions` while the session kept asking —
    // or worse, show `manual` while it did not.
    let mut d = driver();
    let _ = d.open();

    let steps = d.on_line(
        r#"{"type":"control_response","response":{"subtype":"error","request_id":"x","error":"unknown subtype"}}"#,
    );
    match events(&steps).first().expect("a Notice") {
        AgentEvent::Notice { level, message } => {
            assert_eq!(*level, wtm_core::model::NoticeLevel::Warn);
            assert!(message.contains("unknown subtype"), "{message}");
        }
        other => panic!("expected Notice, got {other:?}"),
    }

    // A successful one is silence. These are replies to frames nobody is waiting on, so rendering
    // them would put a collapsed protocol row in the transcript after every Stop press.
    assert!(
        d.on_line(
            r#"{"type":"control_response","response":{"subtype":"success","request_id":"x"}}"#
        )
        .is_empty()
    );
}

#[test]
fn a_turn_the_cli_could_not_run_reports_why_instead_of_an_empty_pane() {
    // Captured from a real session whose OAuth had lapsed, and the reason this test exists: wtm
    // showed the user's message, `0 in · 0 out`, and nothing else. The CLI had said exactly what
    // was wrong, twice, and both copies were discarded — the `assistant` text because "the deltas
    // already carried it" (there were no deltas), and `is_error` because `result` was only ever
    // read for token counts.
    //
    // Note `subtype: "success"` alongside `is_error: true`. The subtype describes the shape of the
    // reply, not the outcome, so matching on it would not have caught this.
    let mut d = driver();
    let _ = d.open();

    let synthetic = d.on_line(
        r#"{"type":"assistant","message":{"id":"7aeab005","model":"<synthetic>","role":"assistant","stop_reason":"stop_sequence","type":"message","content":[{"type":"text","text":"Failed to authenticate: OAuth session expired and could not be refreshed"}]}}"#,
    );
    match events(&synthetic)
        .first()
        .expect("the reason, as a message")
    {
        AgentEvent::Message { text } => assert!(text.contains("OAuth session expired"), "{text}"),
        other => panic!("expected Message, got {other:?}"),
    }

    let finished = d.on_line(
        r#"{"type":"result","subtype":"success","is_error":true,"result":"Failed to authenticate: OAuth session expired and could not be refreshed","terminal_reason":"api_error","num_turns":1,"session_id":"s","total_cost_usd":0,"usage":{"input_tokens":0,"output_tokens":0},"modelUsage":{}}"#,
    );
    let seen = events(&finished);
    match seen.first().expect("a Failed event") {
        AgentEvent::Failed { message } => assert!(message.contains("OAuth"), "{message}"),
        other => panic!("expected Failed first, got {other:?}"),
    }
    // The failure reads above the row that closes the turn, because it is the reason it closed.
    assert!(matches!(seen.get(1), Some(AgentEvent::TurnFinished { .. })));

    // The zeros themselves were never wrong. No request reached the model, so nothing was spent —
    // which is why the fix is to explain the row rather than to change it.
    match seen.get(1).expect("TurnFinished") {
        AgentEvent::TurnFinished { usage, .. } => {
            assert_eq!((usage.tokens_in, usage.tokens_out), (0, 0));
        }
        other => panic!("expected TurnFinished, got {other:?}"),
    }
}

#[test]
fn a_streamed_reply_is_not_repeated_by_the_message_that_completes_it() {
    // The other half of the same change, and the regression it must not cause: when deltas *did*
    // arrive, the `assistant` message is a duplicate of what is already on screen. Emitting both
    // would double every reply this provider gives — which is far more visible than the bug being
    // fixed, and is why the original unconditional drop existed.
    let mut d = driver();
    let _ = d.open();
    d.on_line(
        r#"{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}}"#,
    );
    let delta = d.on_line(
        r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hi there"}}}"#,
    );
    assert!(matches!(
        events(&delta).first(),
        Some(AgentEvent::MessageDelta { .. })
    ));

    let whole = d.on_line(
        r#"{"type":"assistant","message":{"id":"m1","model":"claude-haiku-4-5-20251001","role":"assistant","type":"message","content":[{"type":"text","text":"Hi there"}]}}"#,
    );
    assert!(
        events(&whole).is_empty(),
        "the deltas carried it; got {:?}",
        events(&whole)
    );

    // And the flag is per message, not per turn: a second reply in the same turn that streams
    // nothing — a synthetic refusal after a tool call — must still be shown.
    let second = d.on_line(
        r#"{"type":"assistant","message":{"id":"m2","model":"<synthetic>","role":"assistant","type":"message","content":[{"type":"text","text":"Refused"}]}}"#,
    );
    match events(&second).first().expect("the second reply") {
        AgentEvent::Message { text } => assert_eq!(text, "Refused"),
        other => panic!("expected Message, got {other:?}"),
    }
}

#[test]
fn a_successful_result_stays_silent_about_failure_and_still_reports_its_tokens() {
    // The guard on the arm above: `is_error: false` must not produce a `Failed` row, and the usage
    // mapping that was already correct must stay correct. A successful result really does populate
    // the top-level `usage` — the zeros in the failing case were the absence of a request, not a
    // field read from the wrong place.
    let mut d = driver();
    let _ = d.open();
    let steps = d.on_line(
        r#"{"type":"result","subtype":"success","is_error":false,"session_id":"s","total_cost_usd":0.03,"usage":{"input_tokens":26,"output_tokens":931,"cache_read_input_tokens":81276},"modelUsage":{"claude-haiku-4-5-20251001":{"contextWindow":200000}}}"#,
    );
    let seen = events(&steps);
    assert_eq!(seen.len(), 1, "no Failed row on a successful turn");
    match seen[0] {
        AgentEvent::TurnFinished { usage, .. } => {
            assert_eq!((usage.tokens_in, usage.tokens_out), (26, 931));
            assert_eq!(usage.cached, 81276);
            assert_eq!(usage.context_window, Some(200_000));
        }
        other => panic!("expected TurnFinished, got {other:?}"),
    }
}
