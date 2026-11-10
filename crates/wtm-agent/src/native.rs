//! The vendor owns the terminal and approvals. These adapters observe documented protocol
//! events; they never interpret terminal bytes or synthesize replies to permission requests.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use wtm_core::model::AgentEvent;

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
    /// A line for the session's own activity list that is not a provider event.
    ///
    /// Exists for what a native adapter can see but not measure. Claude's `Stop` hook says a turn
    /// ended and nothing about its tokens or its id, and a `TurnFinished` built from it would hand
    /// plugins zero usage and an empty turn as though the provider had reported them.
    Activity(String),
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

    /// What one provider frame changed. Events are bounded exactly as WTM Chat bounds them,
    /// because plugins receive both as the same `chat.event`.
    pub fn server(&mut self, frame: &Value) -> Vec<Update> {
        let mut updates = self.observe(frame);
        bound(&mut updates);
        updates
    }

    fn observe(&mut self, frame: &Value) -> Vec<Update> {
        // Only a frame without `method` is a reply. The provider numbers its own requests from
        // its own counter, so an approval request can carry the same id as a TUI `turn/start`;
        // reading it as that reply would report a mode change or a failed turn that never was.
        if frame.get("method").is_none() {
            return frame
                .get("id")
                .map(|id| self.reply(&id.to_string(), frame))
                .unwrap_or_default();
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
            // An unrecognised status is news this build cannot read, not a lost connection.
            "thread/status/changed" => {
                updates.extend(status(&params["status"]).map(Update::Status));
            }
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

    fn reply(&mut self, id: &str, frame: &Value) -> Vec<Update> {
        if let Some(mode) = self.modes.remove(id) {
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
        if self.opens.remove(id).is_none() {
            return Vec::new();
        }
        let result = &frame["result"];
        let Some(id) = result["thread"]["id"].as_str() else {
            return vec![Update::Status(Status::Unavailable)];
        };
        id.clone_into(&mut self.conversation);
        self.turn = None;
        vec![
            Update::Event(AgentEvent::SessionReady {
                provider_session_id: id.to_owned(),
                model: result["model"].as_str().map(str::to_owned),
                effort: result["reasoningEffort"].as_str().map(str::to_owned),
                mode: codex_mode(result),
                tools: Vec::new(),
            }),
            // A thread that opened in a state this build cannot name is not known to be idle,
            // and claiming it was would let WTM start a turn into it.
            Update::Status(status(&result["thread"]["status"]).unwrap_or(Status::Unavailable)),
        ]
    }
}

fn bound(updates: &mut [Update]) {
    for update in updates {
        if let Update::Event(event) = update {
            crate::session::bound_transcript_event(event);
        }
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

/// `None` for a status this build does not know, so the caller keeps what it last knew.
fn status(value: &Value) -> Option<Status> {
    Some(match value["type"].as_str()? {
        "idle" => Status::Idle,
        "active"
            if value["activeFlags"].as_array().is_some_and(|flags| {
                flags
                    .iter()
                    .any(|f| matches!(f.as_str(), Some("waitingOnApproval" | "waitingOnUserInput")))
            }) =>
        {
            Status::NeedsApproval
        }
        "active" => Status::Working,
        "systemError" => Status::Unavailable,
        _ => return None,
    })
}

/// What one Claude lifecycle hook says, bounded like any other transcript event.
pub fn claude_hook(value: &Value) -> Vec<Update> {
    let mut updates = hook_updates(value);
    bound(&mut updates);
    updates
}

fn hook_updates(value: &Value) -> Vec<Update> {
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
        // Not `TurnFinished`; see `Update::Activity`. The status is what says the turn ended.
        "Stop" => vec![
            Update::Activity("Turn finished".into()),
            Update::Status(Status::Idle),
        ],
        "StopFailure" => vec![
            Update::Activity("Turn ended with an error".into()),
            Update::Status(Status::Idle),
        ],
        "SessionEnd" => vec![Update::Status(Status::Ended)],
        _ => Vec::new(),
    }
}

/// Flags the native Claude launch sets itself. `-c` and `-r` are Claude's short `--continue` and
/// `--resume`, which would open a conversation other than the one WTM registered.
const CLAUDE_OWNED: &[&str] = &[
    "-p",
    "--print",
    "--input-format",
    "--output-format",
    "--permission-prompt-tool",
    "--mcp-config",
    "--strict-mcp-config",
    "--settings",
    "--session-id",
    "-r",
    "--resume",
    "-c",
    "--continue",
    "--fork-session",
    "--system-prompt",
    "--system-prompt-file",
    "--append-system-prompt",
    "--append-system-prompt-file",
];

/// Flags that would move the Codex app-server or TUI off WTM's private relay. Codex's own `-c`
/// and `-p` are config overrides and `--profile`, which the user is entitled to set.
const CODEX_OWNED: &[&str] = &[
    "--remote",
    "--remote-auth-token-env",
    "--no-daemon",
    "--listen",
    "--stdio",
];

/// Extra args come from trusted repository configuration. Protocol-changing flags are refused
/// explicitly; silently dropping one would launch a different session from the one requested.
///
/// Per provider because the same spelling means different things: `-p` is Claude's `--print`,
/// which would replace the interactive interface, and Codex's `--profile`, which is harmless.
pub fn validate_extra_args(provider: &str, args: &[String]) -> Result<(), String> {
    let owned = match provider {
        "claude" => CLAUDE_OWNED,
        "codex" => CODEX_OWNED,
        _ => return Err("This native adapter supports Claude and Codex".into()),
    };
    for arg in args {
        let name = arg.split('=').next().unwrap_or(arg);
        if owned.contains(&name) {
            return Err(format!(
                "wtm's native integration owns {name}. Remove it from extra_args or open a plain terminal."
            ));
        }
    }
    Ok(())
}

/// The interactive Claude launch: WTM Chat's flags with the stream-json transport swapped for the
/// terminal, and lifecycle hooks added to the same `--settings` object. See
/// [`crate::claude::launch_flags`] for why the rest is shared rather than rebuilt here.
pub fn claude_argv(req: &SessionRequest, overlay: serde_json::Map<String, Value>) -> Vec<String> {
    let mut argv = vec![req.executable.clone().unwrap_or_else(|| "claude".into())];
    argv.extend(crate::claude::launch_flags(req, overlay));
    argv
}

/// The remote Codex TUI, attached to WTM's relay rather than to an app-server of its own.
///
/// `extra_args` go to the app-server, exactly as WTM Chat passes them, because that is the process
/// a repository's configuration has always addressed. Only config overrides and the profile are
/// repeated here: the TUI builds `thread/start` from its own configuration, so an override only the
/// server saw would lose to the TUI's default. Any other flag may be one the TUI does not accept,
/// which would end the pane before it opened.
pub fn codex_tui_argv(req: &SessionRequest, socket: &Path) -> Vec<String> {
    let mut argv = vec![req.executable.clone().unwrap_or_else(|| "codex".into())];
    if let Some(resume) = &req.resume {
        argv.extend(["resume".into(), resume.clone()]);
    }
    argv.extend([
        "--remote".into(),
        format!("unix://{}", socket.display()),
        "--cd".into(),
        req.cwd.clone(),
    ]);
    argv.extend(codex_options(req));
    argv.extend(codex_config_flags(&req.extra_args));
    argv
}

fn codex_config_flags(args: &[String]) -> Vec<String> {
    let mut kept = Vec::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let name = arg.split('=').next().unwrap_or(arg);
        if !matches!(name, "-c" | "--config" | "-p" | "--profile") {
            continue;
        }
        kept.push(arg.clone());
        if !arg.contains('=')
            && let Some(value) = args.next()
        {
            kept.push(value.clone());
        }
    }
    kept
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
