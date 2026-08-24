//! Cursor ACP fixtures at the pure protocol boundary.
//!
//! Cursor's protocol is JSON-RPC and almost every useful field is dynamically read from JSON.
//! These fixtures therefore protect the wire spellings that the compiler cannot: handshake
//! order, live configuration ids, extension requests and streamed tool updates.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;

use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use wtm_agent::cursor::{Cursor, parse_capability};
use wtm_agent::provider::{McpServer, Protocol, Provider, SessionRequest, Step};
use wtm_core::model::{
    AgendaStatus, AgentEvent, ApprovalAnswer, ApprovalRequest, NoticeLevel, Usage,
};

fn writes(steps: &[Step]) -> Vec<Value> {
    steps
        .iter()
        .filter_map(|step| match step {
            Step::Write(frame) => Some(serde_json::from_str(frame).expect("a frame must be JSON")),
            _ => None,
        })
        .collect()
}

fn events(steps: &[Step]) -> Vec<&AgentEvent> {
    steps
        .iter()
        .filter_map(|step| match step {
            Step::Emit(event) => Some(event),
            _ => None,
        })
        .collect()
}

fn opened_driver(req: &SessionRequest) -> Box<dyn Protocol> {
    let mut driver = Cursor.protocol(req);
    driver.open();
    driver.on_line(r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1}}"#);
    driver.on_line(r#"{"jsonrpc":"2.0","id":2,"result":{}}"#);
    driver.on_line(
        r#"{"jsonrpc":"2.0","id":3,"result":{"sessionId":"cursor-session","configOptions":[{"id":"cursor_model","category":"model","type":"select","currentValue":"auto","options":[{"value":"auto","name":"Auto"},{"value":"grok-4.6","name":"Grok 4.6"}]},{"id":"thought_level","category":"thought_level","type":"select","currentValue":"high","options":[{"value":"high","name":"High"},{"value":"xhigh","name":"Extra high"}]}],"modes":{"currentModeId":"agent","availableModes":[{"id":"agent","name":"Agent"},{"id":"plan","name":"Plan"}]}}}"#,
    );
    driver
}

#[test]
fn the_argv_puts_cursor_root_options_before_the_acp_subcommand() {
    let argv = Cursor.argv(&SessionRequest {
        extra_args: vec!["--api-key".to_owned(), "secret-from-config".to_owned()],
        ..SessionRequest::default()
    });

    assert_eq!(argv, ["agent", "--api-key", "secret-from-config", "acp"]);
}

#[test]
fn the_handshake_authenticates_opens_the_worktree_and_applies_every_picker_setting() {
    let mut mcp = BTreeMap::new();
    mcp.insert(
        "wtm".to_owned(),
        McpServer {
            command: "/opt/wtm".to_owned(),
            args: vec!["handoff-server".to_owned()],
            env: BTreeMap::from([("WTM_TOKEN".to_owned(), "one-use-token".to_owned())]),
        },
    );
    let mut driver = Cursor.protocol(&SessionRequest {
        cwd: "/tmp/worktree".to_owned(),
        model: Some("grok-4.6".to_owned()),
        effort: Some("xhigh".to_owned()),
        mode: Some("plan".to_owned()),
        instructions: Some("Use wtm delegation tools.".to_owned()),
        mcp,
        ..SessionRequest::default()
    });

    let opening = writes(&driver.open());
    assert_eq!(opening[0]["method"], "initialize");
    assert_eq!(
        opening[0]["params"]["clientCapabilities"]["terminal"],
        false
    );
    assert_eq!(
        writes(&driver.on_line(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#))[0]["method"],
        "authenticate"
    );
    assert_eq!(
        writes(&driver.on_line(r#"{"jsonrpc":"2.0","id":2,"result":{}}"#))[0]["method"],
        "session/new"
    );
    let open = writes(&driver.on_line(r#"{"jsonrpc":"2.0","id":2,"result":{}}"#));
    assert!(
        open.is_empty(),
        "a duplicate auth reply must not open twice"
    );

    // Start over only to inspect the session/new frame without obscuring handshake state above.
    let mut driver = Cursor.protocol(&SessionRequest {
        cwd: "/tmp/worktree".to_owned(),
        model: Some("grok-4.6".to_owned()),
        effort: Some("xhigh".to_owned()),
        mode: Some("plan".to_owned()),
        instructions: Some("Use wtm delegation tools.".to_owned()),
        mcp: BTreeMap::from([(
            "wtm".to_owned(),
            McpServer {
                command: "/opt/wtm".to_owned(),
                args: vec!["handoff-server".to_owned()],
                env: BTreeMap::from([("WTM_TOKEN".to_owned(), "one-use-token".to_owned())]),
            },
        )]),
        ..SessionRequest::default()
    });
    driver.open();
    driver.on_line(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#);
    let session_new = writes(&driver.on_line(r#"{"jsonrpc":"2.0","id":2,"result":{}}"#));
    assert_eq!(session_new[0]["params"]["cwd"], "/tmp/worktree");
    assert_eq!(session_new[0]["params"]["mcpServers"][0]["name"], "wtm");
    assert_eq!(
        session_new[0]["params"]["mcpServers"][0]["env"][0],
        json!({ "name": "WTM_TOKEN", "value": "one-use-token" })
    );

    let queued = driver.send_turn("Review this.", &[]);
    assert!(matches!(events(&queued)[0], AgentEvent::UserEcho { .. }));
    let ready = driver.on_line(
        r#"{"jsonrpc":"2.0","id":3,"result":{"sessionId":"cursor-session","configOptions":[{"id":"cursor_model","category":"model","currentValue":"auto","options":[]},{"id":"thinking","category":"thought_level","currentValue":"high","options":[]}],"modes":{"currentModeId":"agent","availableModes":[]}}}"#,
    );
    assert!(ready.contains(&Step::Ready));
    assert!(events(&ready).iter().any(|event| matches!(
        event,
        AgentEvent::SessionReady {
            provider_session_id,
            model: Some(model),
            effort: Some(effort),
            mode: Some(mode),
            ..
        } if provider_session_id == "cursor-session"
            && model == "grok-4.6"
            && effort == "xhigh"
            && mode == "plan"
    )));
    let frames = writes(&ready);
    assert_eq!(frames[0]["method"], "session/set_config_option");
    assert_eq!(frames[0]["params"]["configId"], "cursor_model");
    assert_eq!(frames[1]["params"]["configId"], "thinking");
    assert_eq!(frames[2]["method"], "session/set_mode");
    assert_eq!(frames[3]["method"], "session/prompt");
    assert_eq!(
        frames[3]["params"]["prompt"][0]["text"],
        "Use wtm delegation tools.\n\nUser request:\nReview this."
    );
}

#[test]
fn live_reconfiguration_uses_the_config_ids_cursor_advertised() {
    let mut driver = opened_driver(&SessionRequest {
        cwd: "/tmp/worktree".to_owned(),
        ..SessionRequest::default()
    });

    let frames = writes(&driver.reconfigure(Some("grok-4.6"), Some("xhigh"), Some("plan")));
    assert_eq!(frames.len(), 3);
    assert_eq!(frames[0]["method"], "session/set_config_option");
    assert_eq!(frames[0]["params"]["configId"], "cursor_model");
    assert_eq!(frames[0]["params"]["value"], "grok-4.6");
    assert_eq!(frames[1]["params"]["configId"], "thought_level");
    assert_eq!(frames[1]["params"]["value"], "xhigh");
    assert_eq!(frames[2]["method"], "session/set_mode");
}

#[test]
fn streaming_updates_preserve_prose_reasoning_tools_diffs_plans_commands_and_usage() {
    let mut driver = opened_driver(&SessionRequest::default());

    let prose = driver.on_line(
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"cursor-session","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Done"}}}}"#,
    );
    assert_eq!(
        events(&prose),
        [&AgentEvent::MessageDelta {
            text: "Done".to_owned()
        }]
    );

    let thought = driver.on_line(
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"Checking"}}}}"#,
    );
    assert_eq!(
        events(&thought),
        [&AgentEvent::ReasoningDelta {
            text: "Checking".to_owned()
        }]
    );

    let tool = driver.on_line(
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"tool_call","toolCallId":"edit-1","title":"Edit app.rs","kind":"edit","content":[{"type":"diff","path":"src/app.rs","oldText":"old","newText":"new"}]}}}"#,
    );
    assert!(events(&tool).iter().any(|event| matches!(
        event,
        AgentEvent::ToolStarted { id, name, .. } if id == "edit-1" && name == "edit"
    )));
    assert!(events(&tool).iter().any(|event| matches!(
        event,
        AgentEvent::Patch { id, unified_diff }
            if id == "edit-1:src/app.rs" && unified_diff.contains("-old")
    )));

    let plan = driver.on_line(
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"plan","entries":[{"content":"Inspect","status":"completed"},{"content":"Fix","status":"in_progress"}]}}}"#,
    );
    assert!(matches!(
        events(&plan)[0],
        AgentEvent::AgendaUpdated { steps, .. }
            if steps[0].status == AgendaStatus::Completed
                && steps[1].status == AgendaStatus::InProgress
    ));

    let commands = driver.on_line(
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"available_commands_update","availableCommands":[{"name":"review","description":"Review the diff"}]}}}"#,
    );
    assert!(matches!(
        events(&commands)[0],
        AgentEvent::SkillsListed { skills }
            if skills[0].name == "review" && skills[0].description.as_deref() == Some("Review the diff")
    ));

    let usage = driver.on_line(
        r#"{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"usage_update","inputTokens":12,"outputTokens":3,"cachedInputTokens":5,"totalTokens":20,"contextWindow":200000}}}"#,
    );
    assert_eq!(
        events(&usage),
        [&AgentEvent::Usage(Usage {
            tokens_in: 12,
            tokens_out: 3,
            cached: 5,
            context_used: 20,
            context_window: Some(200_000),
        })]
    );
}

#[test]
fn a_permission_request_blocks_and_replies_with_cursors_own_option_id() {
    let mut driver = opened_driver(&SessionRequest::default());
    let request = driver.on_line(
        r#"{"jsonrpc":"2.0","id":"permission-1","method":"session/request_permission","params":{"toolCall":{"title":"Run tests"},"options":[{"optionId":"allow-once","name":"Allow once","kind":"allow_once"},{"optionId":"remember-this-session","name":"Always allow","kind":"allow_always"},{"optionId":"reject-once","name":"Reject","kind":"reject_once"}]}}"#,
    );
    assert!(matches!(
        events(&request)[0],
        AgentEvent::ApprovalRequested {
            id,
            blocking: true,
            request: ApprovalRequest::Permissions { summary, .. },
        } if id == "cursor:permission-1" && summary == "Run tests"
    ));

    let answer = driver.answer("cursor:permission-1", &ApprovalAnswer::AllowForSession);
    let frame = &writes(&answer)[0];
    assert_eq!(frame["id"], "permission-1");
    assert_eq!(
        frame["result"]["outcome"]["optionId"],
        "remember-this-session"
    );
}

#[test]
fn cursor_questions_show_labels_but_answer_with_stable_option_ids() {
    let mut driver = opened_driver(&SessionRequest::default());
    let request = driver.on_line(
        r#"{"jsonrpc":"2.0","id":91,"method":"cursor/ask_question","params":{"title":"Need input","questions":[{"id":"q1","prompt":"Which mode?","options":[{"id":"agent","label":"Agent"},{"id":"plan","label":"Plan"}],"allowMultiple":false}]}}"#,
    );
    assert!(matches!(
        events(&request)[0],
        AgentEvent::ApprovalRequested {
            request: ApprovalRequest::UserInput { questions },
            ..
        } if questions[0].options[1].label == "Plan"
    ));

    let answer = driver.answer(
        "cursor:91",
        &ApprovalAnswer::UserInput {
            answers: BTreeMap::from([("q1".to_owned(), vec!["Plan".to_owned()])]),
            notes: None,
        },
    );
    assert_eq!(
        writes(&answer)[0]["result"]["outcome"]["answers"][0]["selectedOptionIds"],
        json!(["plan"])
    );
}

#[test]
fn cursor_extension_notifications_have_first_class_transcript_rows() {
    let mut driver = opened_driver(&SessionRequest::default());
    let todos = driver.on_line(
        r#"{"jsonrpc":"2.0","method":"cursor/update_todos","params":{"toolCallId":"todo-1","todos":[{"id":"1","content":"Inspect","status":"completed"},{"id":"2","content":"Test","status":"pending"}],"merge":false}}"#,
    );
    assert!(matches!(
        events(&todos)[0],
        AgentEvent::AgendaUpdated { steps, .. }
            if steps[0].text == "Inspect" && steps[0].status == AgendaStatus::Completed
    ));

    let task = driver.on_line(
        r#"{"jsonrpc":"2.0","method":"cursor/task","params":{"toolCallId":"sub-1","description":"Explore codebase","prompt":"Find auth","subagentType":"explore","model":"fast","durationMs":42}}"#,
    );
    assert!(matches!(
        events(&task)[0],
        AgentEvent::ToolStarted { id, name, title }
            if id == "sub-1" && name == "cursor_subagent" && title.as_deref() == Some("Explore codebase")
    ));
    assert!(matches!(
        events(&task)[1],
        AgentEvent::ToolFinished { ok: true, .. }
    ));

    let image = driver.on_line(
        r#"{"jsonrpc":"2.0","method":"cursor/generate_image","params":{"toolCallId":"image-1","description":"Icon","filePath":"/tmp/icon.png"}}"#,
    );
    assert!(matches!(
        events(&image)[0],
        AgentEvent::Notice { level: NoticeLevel::Info, message }
            if message.contains("/tmp/icon.png")
    ));
}

#[test]
fn the_live_capability_preserves_cursor_model_effort_and_mode_labels() {
    let capability = parse_capability(&json!({
        "result": {
            "configOptions": [
                {
                    "id": "cursor_model",
                    "category": "model",
                    "currentValue": "grok-4.6",
                    "options": [
                        { "value": "auto", "name": "Auto" },
                        { "value": "grok-4.6", "name": "Grok 4.6 High Fast", "description": "Fast review model" }
                    ]
                },
                {
                    "id": "thought_level",
                    "category": "thought_level",
                    "currentValue": "high",
                    "options": [
                        { "value": "medium", "name": "Medium" },
                        { "value": "high", "name": "High" }
                    ]
                }
            ],
            "modes": {
                "currentModeId": "agent",
                "availableModes": [
                    { "id": "agent", "name": "Agent" },
                    { "id": "ask", "name": "Ask" }
                ]
            }
        }
    }));

    assert_eq!(capability.models.len(), 2);
    assert_eq!(capability.models[1].label, "Grok 4.6 High Fast");
    assert_eq!(capability.models[1].default_effort.as_deref(), Some("high"));
    assert_eq!(capability.models[1].efforts[1].effort, "high");
    assert!(capability.models[1].is_default);
    assert_eq!(capability.modes[1].id, "ask");
}
