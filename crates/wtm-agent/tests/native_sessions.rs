//! Native adapters observe public provider facts without answering approvals or scraping a TUI.
//! The Codex thread envelope is reduced from the installed 0.165.0 handshake; hook payloads use
//! Claude's documented lifecycle schema. Neither fixture pretends to prove live model execution.
//!
//! The launch tests exist because the native argv was once a second copy of WTM Chat's, and it
//! drifted: `--add-dir` went missing, `fastMode: false` overrode the user's own setting, and the
//! Codex app-server lost the repository's `extra_args` to the TUI.
#![allow(clippy::unwrap_used)]
use std::collections::BTreeMap;
use std::path::Path;

use serde_json::json;
use wtm_agent::{
    McpServer, Provider, SessionRequest,
    claude::Claude,
    codex::Codex,
    native::{
        CodexObserver, Status, Update, claude_argv, claude_hook, codex_options, codex_tui_argv,
        validate_extra_args,
    },
};
use wtm_core::model::AgentEvent;

fn observer() -> CodexObserver {
    let mut observer = CodexObserver::default();
    let mut request = json!({"id":1,"method":"thread/start","params":{}});
    observer.client(&mut request, "WTM context");
    observer.server(&json!({"id":1,"result":{"thread":{"id":"native-thread","status":{"type":"idle"}},"model":"example-model"}}));
    observer
}

fn settings(argv: &[String]) -> Vec<serde_json::Value> {
    argv.windows(2)
        .filter(|pair| pair[0] == "--settings")
        .map(|pair| serde_json::from_str(&pair[1]).unwrap())
        .collect()
}

fn hooks() -> serde_json::Map<String, serde_json::Value> {
    let mut overlay = serde_json::Map::new();
    overlay.insert("hooks".into(), json!({"Stop": []}));
    overlay
}

/// The mode a `turn/start` with these policy fields is reported as, once Codex accepts it.
fn acknowledged_mode(params: serde_json::Value) -> Option<String> {
    let mut observer = observer();
    let mut params = params;
    params["threadId"] = json!("native-thread");
    let mut request = json!({"id":20,"method":"turn/start","params":params});
    observer.client(&mut request, "context");
    match observer
        .server(&json!({"id":20,"result":{"turn":{"id":"turn"}}}))
        .as_slice()
    {
        [Update::Event(AgentEvent::ModeChanged { mode })] => Some(mode.clone()),
        _ => None,
    }
}

#[test]
fn context_preserves_the_callers_instructions_and_only_changes_thread_open_requests() {
    let mut observer = CodexObserver::default();
    let mut request = json!({"id":42,"method":"thread/resume","params":{"threadId":"existing","developerInstructions":"User instructions"}});
    observer.client(&mut request, "WTM instructions");
    assert_eq!(
        request["params"]["developerInstructions"],
        "User instructions\n\nWTM instructions"
    );
    assert_eq!(request["params"]["threadId"], "existing");
    let mut approval = json!({"id":7,"result":{"decision":"decline"}});
    let original = approval.clone();
    observer.client(&mut approval, "WTM instructions");
    assert_eq!(approval, original);
}

#[test]
fn a_native_approval_is_only_a_status_and_other_conversations_cannot_change_it() {
    let mut observer = observer();
    let approval = json!({"id":7,"method":"item/commandExecution/requestApproval","params":{"threadId":"native-thread","itemId":"command"}});
    let updates = observer.server(&approval);
    assert!(matches!(
        updates.as_slice(),
        [Update::Status(Status::NeedsApproval)]
    ));
    assert!(observer.server(&json!({"method":"turn/started","params":{"threadId":"other-thread","turn":{"id":"other-turn"}}})).is_empty());
    assert!(observer.turn.is_none());
}

#[test]
fn a_provider_request_that_reuses_a_tui_request_id_is_not_read_as_its_reply() {
    let mut observer = observer();
    let mut request = json!({"id":99,"method":"turn/start","params":{"threadId":"native-thread","sandboxPolicy":{"type":"dangerFullAccess"},"approvalPolicy":"never"}});
    observer.client(&mut request, "context");
    let approval = json!({"id":99,"method":"item/commandExecution/requestApproval","params":{"threadId":"native-thread","itemId":"command"}});
    assert!(matches!(
        observer.server(&approval).as_slice(),
        [Update::Status(Status::NeedsApproval)]
    ));
    // The real reply still finds its request, which proves the approval did not consume it.
    assert!(matches!(
        observer.server(&json!({"id":99,"result":{"turn":{"id":"turn"}}})).as_slice(),
        [Update::Event(AgentEvent::ModeChanged { mode })] if mode == "full-access"
    ));
}

#[test]
fn native_turn_identity_and_plans_come_from_protocol_events() {
    let mut observer = observer();
    observer.server(&json!({"method":"turn/started","params":{"threadId":"native-thread","turn":{"id":"turn-one"}}}));
    assert_eq!(observer.turn.as_deref(), Some("turn-one"));
    let plan = observer.server(&json!({"method":"turn/plan/updated","params":{"threadId":"native-thread","turnId":"turn-one","plan":[{"step":"Read source","status":"inProgress"}]}}));
    assert!(plan.iter().any(
        |u| matches!(u, Update::Event(AgentEvent::AgendaUpdated { steps, .. }) if steps.len() == 1)
    ));
    let complete = observer.server(&json!({"method":"turn/completed","params":{"threadId":"native-thread","turn":{"id":"turn-one","status":"completed","error":null}}}));
    assert!(observer.turn.is_none());
    assert!(
        complete
            .iter()
            .any(|u| matches!(u, Update::Status(Status::Idle)))
    );
}

#[test]
fn an_unrecognised_thread_status_keeps_the_last_known_one_instead_of_claiming_a_lost_connection() {
    let mut observer = observer();
    let unknown = json!({"method":"thread/status/changed","params":{"threadId":"native-thread","status":{"type":"somethingNew"}}});
    assert!(observer.server(&unknown).is_empty());
    let failed = json!({"method":"thread/status/changed","params":{"threadId":"native-thread","status":{"type":"systemError"}}});
    assert!(matches!(
        observer.server(&failed).as_slice(),
        [Update::Status(Status::Unavailable)]
    ));
    let mut opened = CodexObserver::default();
    opened.client(
        &mut json!({"id":1,"method":"thread/start","params":{}}),
        "context",
    );
    let reply = opened
        .server(&json!({"id":1,"result":{"thread":{"id":"t","status":{"type":"somethingNew"}}}}));
    assert!(
        matches!(reply.last(), Some(Update::Status(Status::Unavailable))),
        "a thread that opened in an unknown state must not be called idle"
    );
}

#[test]
fn native_claude_launch_shares_wtm_chats_flags_and_only_swaps_the_transport() {
    let request = SessionRequest {
        cwd: "/tmp/example".into(),
        mode: Some("default".into()),
        resume: Some("same-conversation".into()),
        instructions: Some("WTM context".into()),
        model: Some("example-model".into()),
        effort: Some("high".into()),
        extra_args: vec!["--verbose".into()],
        ..SessionRequest::default()
    };
    let native = claude_argv(&request, hooks());
    assert!(!native.iter().any(|a| matches!(
        a.as_str(),
        "-p" | "--print" | "--input-format" | "--output-format" | "--permission-prompt-tool"
    )));
    for pair in [
        ["--resume", "same-conversation"],
        ["--permission-mode", "manual"],
        ["--append-system-prompt", "WTM context"],
        ["--add-dir", "/tmp/example"],
    ] {
        assert!(native.windows(2).any(|a| a == pair), "missing {pair:?}");
    }
    // Everything after the transport flags is identical to WTM Chat's, the hooks' settings aside.
    let chat = Claude.argv(&request);
    let tail = |argv: &[String]| {
        let mut kept = argv
            .iter()
            .skip_while(|a| *a != "--resume")
            .cloned()
            .collect::<Vec<_>>();
        if let Some(at) = kept.iter().position(|a| a == "--settings") {
            kept.drain(at..at + 2);
        }
        kept
    };
    assert_eq!(tail(&native), tail(&chat));
}

#[test]
fn native_claude_never_turns_off_a_users_own_fast_mode_and_keeps_one_settings_object() {
    let off = claude_argv(
        &SessionRequest {
            fast: Some(false),
            ..SessionRequest::default()
        },
        hooks(),
    );
    let [overlay] = settings(&off).try_into().unwrap();
    assert!(overlay.get("fastMode").is_none());
    assert!(overlay.get("hooks").is_some());
    let on = claude_argv(
        &SessionRequest {
            fast: Some(true),
            effort: Some(wtm_agent::capability::ULTRACODE.into()),
            ..SessionRequest::default()
        },
        hooks(),
    );
    let [overlay] = settings(&on).try_into().unwrap();
    assert_eq!(overlay["fastMode"], true);
    assert_eq!(overlay["ultracode"], true);
    assert!(overlay.get("hooks").is_some());
    assert!(on.windows(2).any(|a| a == ["--effort", "max"]));
}

#[test]
fn native_codex_uses_the_same_resolved_read_only_policy_as_wtm_chat() {
    let args = codex_options(&SessionRequest {
        mode: Some("read-only".into()),
        ..SessionRequest::default()
    });
    assert!(args.windows(2).any(|a| a == ["--sandbox", "read-only"]));
    assert!(
        args.windows(2)
            .any(|a| a == ["--ask-for-approval", "on-request"])
    );
}

#[test]
fn the_codex_app_server_gets_wtm_chats_argv_and_the_tui_only_repeats_config_overrides() {
    let request = SessionRequest {
        cwd: "/tmp/example".into(),
        resume: Some("thread".into()),
        extra_args: vec![
            "--enable".into(),
            "feature".into(),
            "-c".into(),
            "model_provider=\"example\"".into(),
            "--profile=work".into(),
        ],
        mcp: BTreeMap::from([(
            "wtm".to_owned(),
            McpServer {
                command: "wtm".into(),
                args: vec![],
                env: BTreeMap::new(),
            },
        )]),
        ..SessionRequest::default()
    };
    let server = Codex.argv(&request);
    assert!(server.ends_with(&request.extra_args), "{server:?}");
    let tui = codex_tui_argv(&request, Path::new("/tmp/socket"));
    assert_eq!(tui[1..3], ["resume", "thread"]);
    assert!(
        tui.windows(2)
            .any(|a| a == ["--remote", "unix:///tmp/socket"])
    );
    assert!(tui.windows(2).any(|a| a == ["--cd", "/tmp/example"]));
    assert!(tui.ends_with(&[
        "-c".to_owned(),
        "model_provider=\"example\"".to_owned(),
        "--profile=work".to_owned(),
    ]));
    assert!(!tui.iter().any(|a| a == "--enable"));
    assert!(!tui.iter().any(|a| a.starts_with("mcp_servers")));
}

#[test]
fn hooks_report_lifecycle_without_claiming_delivery_or_provider_permission() {
    let updates = claude_hook(
        &json!({"hook_event_name":"PermissionRequest","session_id":"native-thread","tool_name":"Bash"}),
    );
    assert!(matches!(
        updates.as_slice(),
        [Update::Status(Status::NeedsApproval)]
    ));
    let updates = claude_hook(&json!({"hook_event_name":"UserPromptSubmit","prompt":"Continue"}));
    assert!(matches!(
        updates.as_slice(),
        [
            Update::Event(AgentEvent::UserEcho { .. }),
            Update::Status(Status::Working)
        ]
    ));
    assert!(claude_hook(&json!({"hook_event_name":"FutureHook"})).is_empty());
}

#[test]
fn a_claude_stop_ends_the_turn_without_inventing_its_usage_or_id() {
    let updates = claude_hook(&json!({"hook_event_name":"Stop","session_id":"native-thread"}));
    assert!(
        !updates
            .iter()
            .any(|u| matches!(u, Update::Event(AgentEvent::TurnFinished { .. }))),
        "the Stop hook reports no tokens, so a TurnFinished would present zeros as fact"
    );
    assert!(matches!(
        updates.as_slice(),
        [Update::Activity(_), Update::Status(Status::Idle)]
    ));
}

#[test]
fn native_events_are_bounded_like_wtm_chats_before_anything_sees_them() {
    let prompt = "x".repeat(1024 * 1024);
    let updates = claude_hook(&json!({"hook_event_name":"UserPromptSubmit","prompt":prompt}));
    let Some(Update::Event(AgentEvent::UserEcho { text })) = updates.first() else {
        panic!("echo expected");
    };
    assert!(
        text.len() < 70 * 1024,
        "{} bytes reached plugins",
        text.len()
    );

    let mut observer = observer();
    let output = "y".repeat(3 * 1024 * 1024);
    let message = json!({"method":"item/completed","params":{"threadId":"native-thread","turnId":"t","item":{"type":"commandExecution","id":"c","command":"make","status":"failed","aggregatedOutput":output,"exitCode":1}}});
    for update in observer.server(&message) {
        if let Update::Event(event) = update {
            assert!(serde_json::to_vec(&event).unwrap().len() < 3 * 1024 * 1024);
        }
    }
}

#[test]
fn transport_changing_extra_arguments_are_rejected_per_provider_instead_of_silently_overridden() {
    for flag in [
        "--settings",
        "--session-id",
        "--print",
        "-p",
        "-c",
        "--resume=other",
    ] {
        assert!(
            validate_extra_args("claude", &[flag.into()]).is_err(),
            "{flag}"
        );
    }
    for flag in ["--remote=unix://elsewhere", "--listen", "--stdio"] {
        assert!(
            validate_extra_args("codex", &[flag.into()]).is_err(),
            "{flag}"
        );
    }
    // Codex's `-p` is `--profile` and its `-c` is a config override: both the user's to set.
    for flag in ["-p", "-c", "--profile=work"] {
        assert!(
            validate_extra_args("codex", &[flag.into()]).is_ok(),
            "{flag}"
        );
    }
    assert!(validate_extra_args("claude", &["--verbose".into()]).is_ok());
}

#[test]
fn delegation_mode_tracks_only_acknowledged_policy_changes_and_retains_read_only() {
    let mut observer = observer();
    let mut request = json!({"id":12,"method":"turn/start","params":{"threadId":"native-thread","sandboxPolicy":{"type":"readOnly"},"approvalPolicy":"on-request"}});
    observer.client(&mut request, "context");
    let updates = observer.server(&json!({"id":12,"result":{"turn":{"id":"turn"}}}));
    assert!(
        matches!(updates.as_slice(), [Update::Event(AgentEvent::ModeChanged { mode })] if mode == "read-only")
    );
    request["id"] = json!(13);
    request["params"]["sandboxPolicy"] = json!({"type":"dangerFullAccess"});
    request["params"]["approvalPolicy"] = json!("never");
    observer.client(&mut request, "context");
    let rejected = observer.server(&json!({"id":13,"error":{"message":"rejected"}}));
    assert!(matches!(
        rejected.as_slice(),
        [
            Update::Status(Status::Idle),
            Update::Event(AgentEvent::Failed { .. })
        ]
    ));
}

#[test]
fn the_auto_and_full_access_presets_are_recognised_only_with_their_exact_policy_pairs() {
    let auto = json!({"sandboxPolicy":{"type":"workspaceWrite"},"approvalPolicy":"on-request","approvalsReviewer":"auto_review"});
    assert_eq!(acknowledged_mode(auto).as_deref(), Some("auto"));
    let reviewed_by_user = json!({"sandboxPolicy":{"type":"workspaceWrite"},"approvalPolicy":"on-request","approvalsReviewer":"user"});
    assert_eq!(
        acknowledged_mode(reviewed_by_user).as_deref(),
        Some("default")
    );
    let full = json!({"sandboxPolicy":{"type":"dangerFullAccess"},"approvalPolicy":"never"});
    assert_eq!(acknowledged_mode(full).as_deref(), Some("full-access"));
    // Full access that still asks is not WTM's full-access preset, and a stricter answer is safe.
    let asking = json!({"sandboxPolicy":{"type":"dangerFullAccess"},"approvalPolicy":"on-request"});
    assert_eq!(acknowledged_mode(asking).as_deref(), Some("read-only"));
    let string_form = json!({"sandbox":"danger-full-access","approvalPolicy":"never"});
    assert_eq!(
        acknowledged_mode(string_form).as_deref(),
        Some("full-access")
    );
}
