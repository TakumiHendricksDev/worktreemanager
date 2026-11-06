//! The vendor owns the terminal and approvals. These adapters observe documented protocol
//! events; they never interpret terminal bytes or synthesize replies to permission requests.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use wtm_core::model::{AgentEvent, Usage};

use crate::SessionRequest;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Starting,
    Idle,
    Working,
    NeedsApproval,
    Disconnected,
    Unavailable,
    Ended,
}

#[derive(Debug)]
pub enum Update {
    Event(AgentEvent),
    Status(Status),
}

/// Observe the TUI's own connection, so a second client cannot steal its approvals or
/// accidentally resume an empty thread before Codex has persisted it.
#[derive(Debug, Default)]
pub struct CodexObserver {
    opens: BTreeMap<String, String>,
    modes: BTreeMap<String, Option<String>>,
    pub conversation: String,
    pub turn: Option<String>,
    mapping: crate::codex::NativeEvents,
}

impl CodexObserver {
    pub fn client(&mut self, frame: &mut Value, instructions: &str) {
        let method = frame["method"].as_str().unwrap_or_default().to_owned();
        if method == "turn/start"
            && frame["params"]["threadId"].as_str() == Some(&self.conversation)
            && let Some(id) = frame.get("id")
        {
            self.modes
                .insert(id.to_string(), codex_mode(&frame["params"]));
        }
        if matches!(
            method.as_str(),
            "thread/start" | "thread/resume" | "thread/fork"
        ) {
            if let Some(id) = frame.get("id") {
                self.opens.insert(id.to_string(), method);
            }
            let supplied = frame["params"]["developerInstructions"]
                .as_str()
                .unwrap_or_default();
            // The user's own developer instructions remain ahead of the host context.
            frame["params"]["developerInstructions"] =
                json!(format!("{supplied}\n\n{instructions}"));
        }
    }

    pub fn server(&mut self, frame: &Value) -> Vec<Update> {
        if let Some(id) = frame.get("id")
            && let Some(mode) = self.modes.remove(&id.to_string())
        {
            return if frame.get("error").is_none() {
                mode.into_iter()
                    .map(|mode| Update::Event(AgentEvent::ModeChanged { mode }))
                    .collect()
            } else {
                vec![
                    Update::Status(Status::Idle),
                    Update::Event(AgentEvent::Failed {
                        message: frame["error"]["message"]
                            .as_str()
                            .unwrap_or("Provider rejected the turn")
                            .into(),
                    }),
                ]
            };
        }
        if let Some(id) = frame.get("id")
            && self.opens.remove(&id.to_string()).is_some()
        {
            let result = &frame["result"];
            if let Some(id) = result["thread"]["id"].as_str() {
                id.clone_into(&mut self.conversation);
                self.turn = None;
                return vec![
                    Update::Event(AgentEvent::SessionReady {
                        provider_session_id: id.to_owned(),
                        model: result["model"].as_str().map(str::to_owned),
                        effort: result["reasoningEffort"].as_str().map(str::to_owned),
                        mode: codex_mode(result),
                        tools: Vec::new(),
                    }),
                    Update::Status(status(&result["thread"]["status"])),
                ];
            }
            return vec![Update::Status(Status::Unavailable)];
        }
        let params = &frame["params"];
        if self.conversation.is_empty() || params["threadId"].as_str() != Some(&self.conversation) {
            return Vec::new();
        }
        let method = frame["method"].as_str().unwrap_or_default();
        if frame.get("id").is_some() {
            return if method.contains("requestApproval")
                || method.contains("requestUserInput")
                || method == "mcpServer/elicitation/request"
            {
                vec![Update::Status(Status::NeedsApproval)]
            } else {
                Vec::new()
            };
        }
        let mut updates = Vec::new();
        match method {
            "turn/started" => {
                self.turn = params["turn"]["id"].as_str().map(str::to_owned);
                updates.push(Update::Status(Status::Working));
            }
            "turn/completed" => {
                self.turn = None;
                updates.push(Update::Status(Status::Idle));
            }
            "thread/status/changed" => updates.push(Update::Status(status(&params["status"]))),
            "serverRequest/resolved" => updates.push(Update::Status(if self.turn.is_some() {
                Status::Working
            } else {
                Status::Idle
            })),
            _ => {}
        }
        updates.extend(
            self.mapping
                .notification(method, params)
                .into_iter()
                .map(Update::Event),
        );
        updates
    }
}

// Delegation needs WTM's mode vocabulary, not the independent approval-policy axis.
fn codex_mode(params: &Value) -> Option<String> {
    let policy = params
        .get("sandboxPolicy")
        .filter(|p| !p.is_null())
        .or_else(|| params.get("sandbox"))?;
    let mode = match policy["type"].as_str().or_else(|| policy.as_str()) {
        Some("dangerFullAccess" | "danger-full-access") if params["approvalPolicy"] == "never" => {
            "full-access"
        }
        Some("workspaceWrite" | "workspace-write")
            if params["approvalsReviewer"] == "auto_review" =>
        {
            "auto"
        }
        Some("workspaceWrite" | "workspace-write") => "default",
        _ => "read-only",
    };
    Some(mode.to_owned())
}

fn status(value: &Value) -> Status {
    match value["type"].as_str() {
        Some("idle") => Status::Idle,
        Some("active")
            if value["activeFlags"].as_array().is_some_and(|flags| {
                flags
                    .iter()
                    .any(|f| matches!(f.as_str(), Some("waitingOnApproval" | "waitingOnUserInput")))
            }) =>
        {
            Status::NeedsApproval
        }
        Some("active") => Status::Working,
        Some("systemError") => Status::Unavailable,
        _ => Status::Disconnected,
    }
}

pub fn claude_hook(value: &Value) -> Vec<Update> {
    let text = |key: &str| value[key].as_str().unwrap_or_default().to_owned();
    let event = text("hook_event_name");
    match event.as_str() {
        "SessionStart" => vec![
            Update::Event(AgentEvent::SessionReady {
                provider_session_id: text("session_id"),
                model: value["model"].as_str().map(str::to_owned),
                effort: None,
                mode: value["permission_mode"].as_str().map(str::to_owned),
                tools: Vec::new(),
            }),
            Update::Status(Status::Idle),
        ],
        "UserPromptSubmit" => vec![
            Update::Event(AgentEvent::UserEcho {
                text: text("prompt"),
            }),
            Update::Status(Status::Working),
        ],
        "PreToolUse" => vec![
            Update::Event(AgentEvent::ToolStarted {
                id: text("tool_use_id"),
                name: text("tool_name"),
                title: None,
            }),
            Update::Status(Status::Working),
        ],
        "PostToolUse" | "PostToolUseFailure" => vec![
            Update::Event(AgentEvent::ToolFinished {
                id: text("tool_use_id"),
                ok: event == "PostToolUse",
                output: None,
            }),
            Update::Status(Status::Working),
        ],
        "PermissionRequest" => vec![Update::Status(Status::NeedsApproval)],
        "Notification" if text("notification_type") == "permission_prompt" => {
            vec![Update::Status(Status::NeedsApproval)]
        }
        "Stop" | "StopFailure" => vec![
            Update::Event(AgentEvent::TurnFinished {
                turn: String::new(),
                usage: Usage::default(),
                cost_usd: None,
            }),
            Update::Status(Status::Idle),
        ],
        "SessionEnd" => vec![Update::Status(Status::Ended)],
        _ => Vec::new(),
    }
}

/// Extra args come from trusted repository configuration. Protocol-changing flags are refused
/// explicitly; silently dropping one would launch a different session from the one requested.
pub fn validate_extra_args(args: &[String]) -> Result<(), String> {
    for arg in args {
        let name = arg.split('=').next().unwrap_or(arg);
        if matches!(
            name,
            "-p" | "--print"
                | "--remote"
                | "--remote-auth-token-env"
                | "--input-format"
                | "--output-format"
                | "--permission-prompt-tool"
                | "--mcp-config"
                | "--strict-mcp-config"
                | "--settings"
                | "--session-id"
                | "--resume"
                | "--continue"
                | "--fork-session"
                | "--no-daemon"
                | "--listen"
                | "--stdio"
                | "--system-prompt"
                | "--system-prompt-file"
                | "--append-system-prompt"
                | "--append-system-prompt-file"
        ) {
            return Err(format!(
                "Native WTM integration owns {name}. Remove it from extra_args or open a plain terminal."
            ));
        }
    }
    Ok(())
}

pub fn claude_argv(req: &SessionRequest, settings: &Value) -> Vec<String> {
    let mut argv = vec![req.executable.clone().unwrap_or_else(|| "claude".into())];
    if let Some(resume) = &req.resume {
        argv.extend(["--resume".into(), resume.clone()]);
    } else {
        argv.extend(["--session-id".into(), uuid::Uuid::new_v4().to_string()]);
    }
    if let Some(model) = &req.model {
        argv.extend(["--model".into(), model.clone()]);
    }
    if let Some(mode) = &req.mode {
        argv.extend([
            "--permission-mode".into(),
            crate::claude::canonical_mode(mode).to_owned(),
        ]);
    }
    if let Some(effort) = &req.effort {
        argv.extend([
            "--effort".into(),
            if effort == "ultracode" {
                "max".into()
            } else {
                effort.clone()
            },
        ]);
    }
    argv.extend([
        "--settings".into(),
        settings.to_string(),
        "--mcp-config".into(),
        crate::claude::mcp_config_json(&req.mcp),
    ]);
    if let Some(context) = &req.instructions {
        argv.extend(["--append-system-prompt".into(), context.clone()]);
    }
    argv.extend(req.extra_args.iter().cloned());
    argv
}

/// Match the structured launch's resolved policy without changing the user's global config.
pub fn codex_options(req: &SessionRequest) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(model) = &req.model {
        args.extend(["--model".into(), model.clone()]);
    }
    if let Some(effort) = &req.effort {
        args.extend([
            "-c".into(),
            format!("model_reasoning_effort={}", json!(effort)),
        ]);
    }
    if let Some(mode) = &req.mode {
        let (approval, reviewer, sandbox, _) = crate::codex::expand_mode(mode);
        args.extend([
            "--ask-for-approval".into(),
            approval.into(),
            "--sandbox".into(),
            sandbox.into(),
            "-c".into(),
            format!("approvals_reviewer={}", json!(reviewer)),
        ]);
    }
    args
}
