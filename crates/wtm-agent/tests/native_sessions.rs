//! Native adapters observe public provider facts without answering approvals or scraping a TUI.
//! The Codex thread envelope is reduced from the installed 0.165.0 handshake; hook payloads use
//! Claude's documented lifecycle schema. Neither fixture pretends to prove live model execution.
use serde_json::json;
use wtm_agent::{
    SessionRequest,
    native::{
        CodexObserver, Status, Update, claude_argv, claude_hook, codex_options, validate_extra_args,
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
fn native_claude_retains_its_interactive_interface_and_canonical_permission_mode() {
    let request = SessionRequest {
        cwd: "/tmp/example".into(),
        mode: Some("default".into()),
        resume: Some("same-conversation".into()),
        instructions: Some("WTM context".into()),
        ..SessionRequest::default()
    };
    let argv = claude_argv(&request, &json!({"hooks":{}}));
    assert!(!argv.iter().any(|a| matches!(
        a.as_str(),
        "-p" | "--print" | "--input-format" | "--output-format"
    )));
    assert!(
        argv.windows(2)
            .any(|a| a == ["--resume", "same-conversation"])
    );
    assert!(
        argv.windows(2)
            .any(|a| a == ["--permission-mode", "manual"])
    );
    assert!(
        argv.windows(2)
            .any(|a| a == ["--append-system-prompt", "WTM context"])
    );
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
fn transport_changing_extra_arguments_are_rejected_instead_of_silently_overridden() {
    for flag in [
        "--remote=unix://elsewhere",
        "--settings",
        "--session-id",
        "--print",
    ] {
        assert!(validate_extra_args(&[flag.into()]).is_err());
    }
    assert!(validate_extra_args(&["--verbose".into()]).is_ok());
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
