//! Cursor CLI, over the Agent Client Protocol exposed by `agent acp`.
//!
//! ACP is the client seam rather than an MCP wrapper. Wtm owns the child process, transcript and
//! approvals, while the MCP servers in [`SessionRequest`] are handed to Cursor at `session/new` so
//! the same internal handoff tools are available here as on every other provider.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use wtm_core::model::{
    AgendaStatus, AgendaStep, AgentAttachment, AgentCapability, AgentEvent, AgentMode, AgentModel,
    AgentSkill, ApprovalAnswer, ApprovalRequest, EffortOption, ModeRisk, NoticeLevel, Usage,
    UserInputOption, UserInputQuestion,
};

use crate::provider::{Protocol, Provider, ProviderId, SessionRequest, Step};

pub const ID: &str = "cursor";

/// Cursor Agent executable names and app-managed locations, in preference order.
///
/// Cursor has shipped both `cursor-agent` and `agent`. The desktop app also installs its own
/// `cursor-agent` below VS Code global storage without adding that directory to `PATH`; treating
/// the app as proof that the agent is on `PATH` would still make the eventual spawn fail, so the
/// composition root probes these exact candidates and carries the winning path in
/// [`SessionRequest::executable`]. The Cursor-specific name wins over bare `agent` because that
/// name is also used by unrelated CLIs.
#[must_use]
pub fn executable_candidates(home: Option<&Path>) -> Vec<PathBuf> {
    let mut candidates = vec![PathBuf::from("cursor-agent")];

    if let Some(home) = home {
        if cfg!(target_os = "macos") {
            candidates.push(home.join(
                "Library/Application Support/Cursor/User/globalStorage/anysphere.cursor-agent-worker/agent-cli/.local/bin/cursor-agent",
            ));
        } else if cfg!(target_os = "linux") {
            candidates.push(home.join(
                ".config/Cursor/User/globalStorage/anysphere.cursor-agent-worker/agent-cli/.local/bin/cursor-agent",
            ));
        }
    }

    candidates.push(PathBuf::from("agent"));
    candidates
}

#[derive(Debug)]
pub struct Cursor;

impl Provider for Cursor {
    fn id(&self) -> ProviderId {
        ProviderId::new(ID)
    }

    fn program(&self) -> &'static str {
        "agent"
    }

    fn argv(&self, req: &SessionRequest) -> Vec<String> {
        // Root CLI options precede the ACP subcommand (`agent --api-key … acp`). Keeping project
        // `extra_args` there makes authentication and endpoint overrides usable without teaching
        // wtm Cursor's moving root-option vocabulary.
        let mut argv = vec![
            req.executable
                .clone()
                .unwrap_or_else(|| self.program().to_owned()),
        ];
        argv.extend(req.extra_args.iter().cloned());
        argv.push("acp".to_owned());
        argv
    }

    fn protocol(&self, req: &SessionRequest) -> Box<dyn Protocol> {
        Box::new(CursorProtocol::new(req.clone()))
    }

    fn seed_skills(&self, req: &SessionRequest) -> Vec<AgentSkill> {
        crate::skills::cursor(
            std::path::Path::new(&req.cwd),
            std::env::var_os("HOME")
                .as_deref()
                .map(std::path::Path::new),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Initializing,
    Authenticating,
    Starting,
    Ready,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolShape {
    Command,
    Tool,
}

struct CursorProtocol {
    req: SessionRequest,
    phase: Phase,
    next_id: i64,
    init_id: Option<i64>,
    auth_id: Option<i64>,
    open_id: Option<i64>,
    prompt_ids: BTreeMap<i64, String>,
    session_id: Option<String>,
    queued: Vec<(String, Vec<AgentAttachment>)>,
    pending: BTreeMap<String, Pending>,
    tools: BTreeMap<String, ToolShape>,
    model_config_id: Option<String>,
    effort_config_id: Option<String>,
    instructions_sent: bool,
    /// Answer `session/request_permission` here, because Cursor has no Auto mode of its own.
    ///
    /// Its advertised modes are Agent / Plan / Ask. Sending `modeId: "auto"` is rejected, and
    /// "Always this session" only sticks for one class of tool — the next `just check` asks
    /// again. So Auto is a wtm policy: the picker shows it, the wire stays on `agent`, and
    /// permission cards are answered with allow-once. Clarification questions still surface.
    auto_approve: bool,
}

struct Pending {
    rpc_id: Value,
    kind: PendingKind,
}

enum PendingKind {
    Permission {
        allow_once: String,
        allow_always: String,
        reject_once: String,
    },
    Questions {
        option_ids: BTreeMap<String, BTreeMap<String, String>>,
    },
    Plan,
}

impl CursorProtocol {
    fn new(req: SessionRequest) -> Self {
        let auto_approve = req.mode.as_deref() == Some("auto");
        Self {
            req,
            phase: Phase::Initializing,
            next_id: 1,
            init_id: None,
            auth_id: None,
            open_id: None,
            prompt_ids: BTreeMap::new(),
            session_id: None,
            queued: Vec::new(),
            pending: BTreeMap::new(),
            tools: BTreeMap::new(),
            model_config_id: None,
            effort_config_id: None,
            instructions_sent: false,
            auto_approve,
        }
    }

    fn request(&mut self, method: &str, params: &Value) -> (i64, Step) {
        let id = self.next_id;
        self.next_id += 1;
        (
            id,
            Step::Write(
                json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
                    .to_string(),
            ),
        )
    }

    fn mcp_servers(&self) -> Vec<Value> {
        self.req
            .mcp
            .iter()
            .map(|(name, server)| {
                let env = server
                    .env
                    .iter()
                    .map(|(name, value)| json!({ "name": name, "value": value }))
                    .collect::<Vec<_>>();
                json!({
                    "name": name,
                    "command": server.command,
                    "args": server.args,
                    "env": env,
                })
            })
            .collect()
    }

    fn open_session(&mut self) -> Step {
        let mut params = json!({
            "cwd": self.req.cwd,
            "mcpServers": self.mcp_servers(),
        });
        let method = if let Some(session) = self.req.resume.clone() {
            params["sessionId"] = json!(session);
            "session/load"
        } else {
            // ACP has no fork operation. A side question must be a new conversation rather than
            // silently appending its temporary turn to the parent's durable Cursor session.
            "session/new"
        };
        let (id, step) = self.request(method, &params);
        self.open_id = Some(id);
        step
    }

    fn configure(&mut self, result: &Value) -> Vec<Step> {
        let Some(session) = self.session_id.clone() else {
            return Vec::new();
        };
        let configs = result
            .get("configOptions")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default();
        self.model_config_id = config_id(configs, "model");
        self.effort_config_id = config_id(configs, "thought_level");
        let mut steps = Vec::new();

        for (config_id, wanted) in [
            (self.model_config_id.clone(), self.req.model.clone()),
            (self.effort_config_id.clone(), self.req.effort.clone()),
        ] {
            let Some(wanted) = wanted else { continue };
            let Some(config_id) = config_id else {
                continue;
            };
            let (_, step) = self.request(
                "session/set_config_option",
                &json!({ "sessionId": session, "configId": config_id, "value": wanted }),
            );
            steps.push(step);
        }

        if let Some(mode) = self.req.mode.clone() {
            self.auto_approve = mode == "auto";
            let (_, step) = self.request(
                "session/set_mode",
                &json!({ "sessionId": session, "modeId": wire_mode(&mode) }),
            );
            steps.push(step);
        }
        steps
    }

    fn prompt_frame(&mut self, text: &str, attachments: &[AgentAttachment]) -> Option<Vec<Step>> {
        let session = self.session_id.clone()?;
        let mut prompt = Vec::new();
        let model_text = if self.instructions_sent {
            text.to_owned()
        } else if let Some(instructions) = self.req.instructions.as_deref() {
            self.instructions_sent = true;
            format!("{instructions}\n\nUser request:\n{text}")
        } else {
            self.instructions_sent = true;
            text.to_owned()
        };
        if !model_text.is_empty() {
            prompt.push(json!({ "type": "text", "text": model_text }));
        }
        for attachment in attachments {
            if attachment.mime.starts_with("image/") {
                prompt.push(json!({
                    "type": "image",
                    "mimeType": attachment.mime,
                    "data": attachment.data_base64,
                }));
            } else {
                prompt.push(json!({
                    "type": "text",
                    "text": format!(
                        "Attached file `{}` is available at `{}`.",
                        attachment.name, attachment.path
                    ),
                }));
            }
        }

        let (id, write) = self.request(
            "session/prompt",
            &json!({ "sessionId": session, "prompt": prompt }),
        );
        let turn = format!("cursor-{id}");
        self.prompt_ids.insert(id, turn.clone());
        Some(vec![Step::Emit(AgentEvent::TurnStarted { turn }), write])
    }

    fn on_reply(&mut self, id: i64, message: &Value) -> Vec<Step> {
        if let Some(error) = message.get("error") {
            let detail = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Cursor rejected an ACP request");
            return vec![Step::Emit(AgentEvent::Failed {
                message: detail.to_owned(),
            })];
        }
        let result = message.get("result").cloned().unwrap_or(Value::Null);

        if Some(id) == self.init_id {
            self.init_id = None;
            self.phase = Phase::Authenticating;
            let (auth_id, step) =
                self.request("authenticate", &json!({ "methodId": "cursor_login" }));
            self.auth_id = Some(auth_id);
            return vec![step];
        }
        if Some(id) == self.auth_id {
            self.auth_id = None;
            self.phase = Phase::Starting;
            return vec![self.open_session()];
        }
        if Some(id) == self.open_id {
            self.open_id = None;
            let session = result
                .get("sessionId")
                .and_then(Value::as_str)
                .or(self.req.resume.as_deref())
                .unwrap_or_default()
                .to_owned();
            self.session_id = Some(session.clone());
            self.phase = Phase::Ready;

            // The following frames immediately apply the request. Report that selected value to
            // the UI rather than the server's pre-configuration default or the picker would jump
            // backwards while the set-option replies are in flight.
            let model = self
                .req
                .model
                .clone()
                .or_else(|| selected_config(&result, "model"));
            let effort = self
                .req
                .effort
                .clone()
                .or_else(|| selected_config(&result, "thought_level"));
            let mode = self.req.mode.clone().or_else(|| {
                result
                    .pointer("/modes/currentModeId")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            });
            let mut steps = vec![Step::Emit(AgentEvent::SessionReady {
                provider_session_id: session,
                model,
                effort,
                mode,
                tools: Vec::new(),
            })];
            steps.extend(self.configure(&result));
            steps.push(Step::Ready);
            for (text, attachments) in std::mem::take(&mut self.queued) {
                if let Some(prompt) = self.prompt_frame(&text, &attachments) {
                    steps.extend(prompt);
                }
            }
            return steps;
        }
        if let Some(turn) = self.prompt_ids.remove(&id) {
            return vec![Step::Emit(AgentEvent::TurnFinished {
                turn,
                usage: Usage::default(),
                cost_usd: None,
            })];
        }
        Vec::new()
    }

    fn on_request(&mut self, rpc_id: Value, method: &str, params: &Value) -> Vec<Step> {
        let id = format!("cursor:{}", value_id(&rpc_id));
        match method {
            "session/request_permission" => {
                let options = params
                    .get("options")
                    .and_then(Value::as_array)
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                let option = |kind: &str, fallback: &str| {
                    options
                        .iter()
                        .find(|option| {
                            option.get("kind").and_then(Value::as_str) == Some(kind)
                                || option.get("optionId").and_then(Value::as_str) == Some(fallback)
                        })
                        .and_then(|option| option.get("optionId").and_then(Value::as_str))
                        .unwrap_or(fallback)
                        .to_owned()
                };
                self.pending.insert(
                    id.clone(),
                    Pending {
                        rpc_id,
                        kind: PendingKind::Permission {
                            allow_once: option("allow_once", "allow-once"),
                            allow_always: option("allow_always", "allow-always"),
                            reject_once: option("reject_once", "reject-once"),
                        },
                    },
                );
                // Auto answers here rather than showing a card. The option *names* must not
                // become the card body either: those are Allow / Always / Deny, which the
                // buttons already are, and printing them made every Grok prompt look identical.
                if self.auto_approve {
                    return self.answer(&id, &ApprovalAnswer::Allow);
                }
                vec![Step::Emit(AgentEvent::ApprovalRequested {
                    id,
                    blocking: true,
                    request: permission_request(params),
                })]
            }
            "cursor/ask_question" => {
                let mut option_ids = BTreeMap::new();
                let questions = params
                    .get("questions")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .map(|question| {
                        let question_id = text(question, "id");
                        let choices = question
                            .get("options")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                            .map(|option| {
                                let label = text(option, "label");
                                (label, text(option, "id"))
                            })
                            .collect::<Vec<_>>();
                        option_ids.insert(
                            question_id.clone(),
                            choices.iter().cloned().collect::<BTreeMap<_, _>>(),
                        );
                        UserInputQuestion {
                            id: question_id,
                            header: params
                                .get("title")
                                .and_then(Value::as_str)
                                .unwrap_or("Question")
                                .to_owned(),
                            question: text(question, "prompt"),
                            options: choices
                                .into_iter()
                                .map(|(label, _)| UserInputOption {
                                    label,
                                    description: None,
                                })
                                .collect(),
                            multiple: question
                                .get("allowMultiple")
                                .and_then(Value::as_bool)
                                .unwrap_or(false),
                            allows_other: false,
                            secret: false,
                        }
                    })
                    .collect();
                self.pending.insert(
                    id.clone(),
                    Pending {
                        rpc_id,
                        kind: PendingKind::Questions { option_ids },
                    },
                );
                vec![Step::Emit(AgentEvent::ApprovalRequested {
                    id,
                    blocking: true,
                    request: ApprovalRequest::UserInput { questions },
                })]
            }
            "cursor/create_plan" => {
                let markdown = text(params, "plan");
                self.pending.insert(
                    id.clone(),
                    Pending {
                        rpc_id,
                        kind: PendingKind::Plan,
                    },
                );
                vec![Step::Emit(AgentEvent::ApprovalRequested {
                    id,
                    blocking: true,
                    request: ApprovalRequest::PlanReview {
                        markdown,
                        path: None,
                    },
                })]
            }
            _ => vec![Step::Emit(AgentEvent::Raw {
                provider: ID.to_owned(),
                event: method.to_owned(),
                payload: params.clone(),
            })],
        }
    }

    fn on_notification(&mut self, method: &str, params: &Value) -> Vec<Step> {
        match method {
            "session/update" => self.on_update(params.get("update").unwrap_or(&Value::Null)),
            "cursor/update_todos" => vec![Step::Emit(AgentEvent::AgendaUpdated {
                explanation: None,
                steps: todos(params),
            })],
            "cursor/task" => {
                let id = text(params, "toolCallId");
                let description = text(params, "description");
                let output = serde_json::to_string_pretty(params).ok();
                vec![
                    Step::Emit(AgentEvent::ToolStarted {
                        id: id.clone(),
                        name: "cursor_subagent".to_owned(),
                        title: Some(description),
                    }),
                    Step::Emit(AgentEvent::ToolFinished {
                        id,
                        ok: true,
                        output,
                    }),
                ]
            }
            "cursor/generate_image" => vec![Step::Emit(AgentEvent::Notice {
                level: NoticeLevel::Info,
                message: params.get("filePath").and_then(Value::as_str).map_or_else(
                    || "Cursor generated an image.".to_owned(),
                    |path| format!("Cursor generated an image at {path}."),
                ),
            })],
            _ => vec![Step::Emit(AgentEvent::Raw {
                provider: ID.to_owned(),
                event: method.to_owned(),
                payload: params.clone(),
            })],
        }
    }

    fn on_update(&mut self, update: &Value) -> Vec<Step> {
        let kind = update
            .get("sessionUpdate")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match kind {
            "agent_message_chunk" => content_text(update.get("content"))
                .map_or_else(Vec::new, |text| {
                    vec![Step::Emit(AgentEvent::MessageDelta { text })]
                }),
            "agent_thought_chunk" => content_text(update.get("content"))
                .map_or_else(Vec::new, |text| {
                    vec![Step::Emit(AgentEvent::ReasoningDelta { text })]
                }),
            "user_message_chunk" | "current_mode_update" | "config_option_update" => Vec::new(),
            "tool_call" => self.tool_started(update),
            "tool_call_update" => self.tool_updated(update),
            "plan" => vec![Step::Emit(AgentEvent::AgendaUpdated {
                explanation: None,
                steps: plan_entries(update),
            })],
            "available_commands_update" => vec![Step::Emit(AgentEvent::SkillsListed {
                skills: available_commands(update),
            })],
            "usage_update" => vec![Step::Emit(AgentEvent::Usage(usage(update)))],
            _ => vec![Step::Emit(AgentEvent::Raw {
                provider: ID.to_owned(),
                event: format!("session/update:{kind}"),
                payload: update.clone(),
            })],
        }
    }

    fn tool_started(&mut self, update: &Value) -> Vec<Step> {
        let id = text(update, "toolCallId");
        let title = text(update, "title");
        let command = update
            .get("rawInput")
            .and_then(|input| input.get("command"))
            .and_then(Value::as_str);
        let shape =
            if command.is_some() || update.get("kind").and_then(Value::as_str) == Some("execute") {
                ToolShape::Command
            } else {
                ToolShape::Tool
            };
        self.tools.insert(id.clone(), shape);

        let mut steps = vec![Step::Emit(match shape {
            ToolShape::Command => AgentEvent::CommandStarted {
                id: id.clone(),
                command: command.unwrap_or(&title).to_owned(),
                cwd: None,
            },
            ToolShape::Tool => AgentEvent::ToolStarted {
                id: id.clone(),
                name: update
                    .get("kind")
                    .and_then(Value::as_str)
                    .unwrap_or("tool")
                    .to_owned(),
                title: Some(title),
            },
        })];
        steps.extend(patches(update, &id).into_iter().map(Step::Emit));
        steps
    }

    fn tool_updated(&mut self, update: &Value) -> Vec<Step> {
        let id = text(update, "toolCallId");
        let mut steps: Vec<Step> = patches(update, &id).into_iter().map(Step::Emit).collect();
        let status = update.get("status").and_then(Value::as_str);
        if !matches!(status, Some("completed" | "failed")) {
            return steps;
        }
        let ok = status == Some("completed");
        let output = tool_output(update);
        match self.tools.get(&id).copied().unwrap_or(ToolShape::Tool) {
            ToolShape::Command => {
                if let Some(output) = output.filter(|output| !output.is_empty()) {
                    steps.push(Step::Emit(AgentEvent::CommandOutput {
                        id: id.clone(),
                        chunk: output,
                    }));
                }
                steps.push(Step::Emit(AgentEvent::CommandFinished {
                    id,
                    exit_code: (!ok).then_some(1),
                }));
            }
            ToolShape::Tool => {
                steps.push(Step::Emit(AgentEvent::ToolFinished { id, ok, output }));
            }
        }
        steps
    }
}

impl Protocol for CursorProtocol {
    fn open(&mut self) -> Vec<Step> {
        let (id, step) = self.request(
            "initialize",
            &json!({
                "protocolVersion": 1,
                "clientCapabilities": {
                    "fs": { "readTextFile": false, "writeTextFile": false },
                    "terminal": false,
                },
                "clientInfo": { "name": "wtm", "version": env!("CARGO_PKG_VERSION") },
            }),
        );
        self.init_id = Some(id);
        vec![step]
    }

    fn on_line(&mut self, line: &str) -> Vec<Step> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Vec::new();
        }
        let Ok(message) = serde_json::from_str::<Value>(trimmed) else {
            return vec![Step::Emit(AgentEvent::Notice {
                level: NoticeLevel::Warn,
                message: trimmed.to_owned(),
            })];
        };

        if let Some(method) = message.get("method").and_then(Value::as_str) {
            let params = message.get("params").cloned().unwrap_or(Value::Null);
            if let Some(id) = message.get("id").cloned() {
                return self.on_request(id, method, &params);
            }
            return self.on_notification(method, &params);
        }
        if let Some(id) = message.get("id").and_then(Value::as_i64) {
            return self.on_reply(id, &message);
        }
        vec![Step::Emit(AgentEvent::Raw {
            provider: ID.to_owned(),
            event: "unrecognized".to_owned(),
            payload: message,
        })]
    }

    fn send_turn(&mut self, text: &str, attachments: &[AgentAttachment]) -> Vec<Step> {
        let mut steps = attachment_steps(attachments);
        steps.push(Step::Emit(AgentEvent::UserEcho {
            text: text.to_owned(),
        }));
        if self.phase == Phase::Ready {
            if let Some(prompt) = self.prompt_frame(text, attachments) {
                steps.extend(prompt);
            }
        } else {
            self.queued.push((text.to_owned(), attachments.to_vec()));
        }
        steps
    }

    fn reconfigure(
        &mut self,
        model: Option<&str>,
        effort: Option<&str>,
        mode: Option<&str>,
        // No analogue here: this provider's speed axis is the effort ladder above, so the picker
        // never offers the control and this is always `None`. See `AgentCapability::supports_fast`.
        _fast: Option<bool>,
    ) -> Vec<Step> {
        if let Some(model) = model {
            self.req.model = Some(model.to_owned());
        }
        if let Some(effort) = effort {
            self.req.effort = Some(effort.to_owned());
        }
        if let Some(mode) = mode {
            self.req.mode = Some(mode.to_owned());
            self.auto_approve = mode == "auto";
        }
        let Some(session) = self.session_id.clone() else {
            return Vec::new();
        };
        let mut steps = Vec::new();
        for (config_id, wanted) in [
            (self.model_config_id.clone(), model),
            (self.effort_config_id.clone(), effort),
        ] {
            let (Some(config_id), Some(wanted)) = (config_id, wanted) else {
                continue;
            };
            let (_, step) = self.request(
                "session/set_config_option",
                &json!({ "sessionId": session, "configId": config_id, "value": wanted }),
            );
            steps.push(step);
        }
        if let Some(mode) = mode {
            let (_, step) = self.request(
                "session/set_mode",
                &json!({ "sessionId": session, "modeId": wire_mode(mode) }),
            );
            steps.push(step);
        }
        // ACP config ids are server-defined. An older agent that did not advertise one leaves that
        // selection for the next session instead of receiving an invented id.
        steps
    }

    fn answer(&mut self, id: &str, answer: &ApprovalAnswer) -> Vec<Step> {
        let Some(pending) = self.pending.remove(id) else {
            return vec![Step::Emit(AgentEvent::Notice {
                level: NoticeLevel::Warn,
                message: format!("No pending approval `{id}` to answer."),
            })];
        };
        let result = match (&pending.kind, answer) {
            (
                PendingKind::Permission {
                    allow_once,
                    allow_always: _,
                    reject_once: _,
                },
                ApprovalAnswer::Allow,
            ) => json!({ "outcome": { "outcome": "selected", "optionId": allow_once } }),
            (
                PendingKind::Permission {
                    allow_once: _,
                    allow_always,
                    reject_once: _,
                },
                ApprovalAnswer::AllowForSession,
            ) => json!({ "outcome": { "outcome": "selected", "optionId": allow_always } }),
            (
                PendingKind::Permission {
                    allow_once: _,
                    allow_always: _,
                    reject_once,
                },
                ApprovalAnswer::Deny { .. },
            ) => json!({ "outcome": { "outcome": "selected", "optionId": reject_once } }),
            (PendingKind::Questions { option_ids }, ApprovalAnswer::UserInput { answers, .. }) => {
                let answers = answers
                    .iter()
                    .map(|(question_id, selected)| {
                        let selected = selected
                            .iter()
                            .map(|label| {
                                option_ids
                                    .get(question_id)
                                    .and_then(|options| options.get(label))
                                    .unwrap_or(label)
                            })
                            .collect::<Vec<_>>();
                        json!({
                            "questionId": question_id,
                            "selectedOptionIds": selected,
                        })
                    })
                    .collect::<Vec<_>>();
                json!({ "outcome": { "outcome": "answered", "answers": answers } })
            }
            (PendingKind::Questions { .. }, ApprovalAnswer::Deny { message }) => json!({
                "outcome": { "outcome": "skipped", "reason": message }
            }),
            (PendingKind::Plan, ApprovalAnswer::Allow | ApprovalAnswer::AllowForSession) => {
                json!({ "outcome": { "outcome": "accepted" } })
            }
            (PendingKind::Plan, ApprovalAnswer::Deny { message }) => json!({
                "outcome": { "outcome": "rejected", "reason": message }
            }),
            _ => {
                self.pending.insert(id.to_owned(), pending);
                return vec![Step::Emit(AgentEvent::Failed {
                    message: "That answer does not match Cursor's pending request.".to_owned(),
                })];
            }
        };
        vec![
            Step::Write(
                json!({ "jsonrpc": "2.0", "id": pending.rpc_id, "result": result }).to_string(),
            ),
            Step::Emit(AgentEvent::ApprovalResolved { id: id.to_owned() }),
        ]
    }

    fn interrupt(&mut self) -> Vec<Step> {
        let Some(session) = self.session_id.clone() else {
            return Vec::new();
        };
        let mut steps = vec![Step::Write(
            json!({ "jsonrpc": "2.0", "method": "session/cancel", "params": { "sessionId": session } })
                .to_string(),
        )];
        for (id, pending) in std::mem::take(&mut self.pending) {
            steps.push(Step::Write(
                json!({
                    "jsonrpc": "2.0",
                    "id": pending.rpc_id,
                    "result": { "outcome": { "outcome": "cancelled" } },
                })
                .to_string(),
            ));
            steps.push(Step::Emit(AgentEvent::ApprovalResolved { id }));
        }
        steps
    }

    fn abandon(&mut self) -> Vec<Step> {
        self.interrupt()
    }
}

fn value_id(value: &Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned)
}

/// Cursor's session mode on the wire. Auto is a wtm policy, not one of its three modes.
fn wire_mode(mode: &str) -> &str {
    if mode == "auto" { "agent" } else { mode }
}

/// What the card should show — the tool, not the Allow / Always / Reject options.
///
/// Those option names used to be stuffed into `Permissions.items`, so every prompt rendered as
/// the same three verbs plus a one-line title. The buttons already are those verbs. A shell
/// command is a command card; everything else lists the kind and paths, never the choices.
fn permission_request(params: &Value) -> ApprovalRequest {
    let tool = params.get("toolCall").unwrap_or(&Value::Null);
    let title = tool
        .get("title")
        .or_else(|| params.get("title"))
        .and_then(Value::as_str)
        .unwrap_or("Cursor wants to run a tool");
    let kind = tool.get("kind").and_then(Value::as_str);
    let raw = tool.get("rawInput").unwrap_or(&Value::Null);
    let command = raw
        .get("command")
        .or_else(|| raw.get("commandLine"))
        .and_then(Value::as_str);
    if command.is_some() || kind == Some("execute") {
        return ApprovalRequest::Command {
            command: command.unwrap_or(title).to_owned(),
            cwd: raw
                .get("workingDirectory")
                .or_else(|| raw.get("cwd"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            reason: None,
        };
    }
    ApprovalRequest::Permissions {
        summary: title.to_owned(),
        items: tool_permission_items(tool),
    }
}

fn tool_permission_items(tool: &Value) -> Vec<String> {
    let mut items = Vec::new();
    if let Some(kind) = tool.get("kind").and_then(Value::as_str) {
        items.push(kind.to_owned());
    }
    if let Some(content) = tool.get("content").and_then(Value::as_array) {
        for part in content {
            if let Some(path) = part.get("path").and_then(Value::as_str)
                && !items.iter().any(|item| item == path)
            {
                items.push(path.to_owned());
            }
        }
    }
    let raw = tool.get("rawInput").unwrap_or(&Value::Null);
    for key in ["path", "file_path", "filePath", "target"] {
        if let Some(path) = raw.get(key).and_then(Value::as_str)
            && !items.iter().any(|item| item == path)
        {
            items.push(path.to_owned());
        }
    }
    items
}

fn text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn content_text(content: Option<&Value>) -> Option<String> {
    let content = content?;
    match content.get("type").and_then(Value::as_str) {
        Some("text") => content
            .get("text")
            .and_then(Value::as_str)
            .map(str::to_owned),
        Some("resource") => content
            .pointer("/resource/text")
            .and_then(Value::as_str)
            .map(str::to_owned),
        _ => None,
    }
}

fn attachment_steps(attachments: &[AgentAttachment]) -> Vec<Step> {
    if attachments.is_empty() {
        Vec::new()
    } else {
        vec![Step::Emit(AgentEvent::Attachments {
            attachments: attachments.to_vec(),
        })]
    }
}

fn config_id(configs: &[Value], category: &str) -> Option<String> {
    configs
        .iter()
        .find(|option| option.get("category").and_then(Value::as_str) == Some(category))
        .and_then(|option| option.get("id").and_then(Value::as_str))
        .map(str::to_owned)
}

fn selected_config(result: &Value, category: &str) -> Option<String> {
    result
        .get("configOptions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|option| option.get("category").and_then(Value::as_str) == Some(category))
        .and_then(|option| option.get("currentValue").and_then(Value::as_str))
        .map(str::to_owned)
}

fn status(value: &str) -> AgendaStatus {
    match value {
        "completed" => AgendaStatus::Completed,
        "in_progress" | "inProgress" => AgendaStatus::InProgress,
        _ => AgendaStatus::Pending,
    }
}

fn todos(value: &Value) -> Vec<AgendaStep> {
    value
        .get("todos")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|todo| AgendaStep {
            text: text(todo, "content"),
            status: status(
                todo.get("status")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            ),
        })
        .collect()
}

fn plan_entries(value: &Value) -> Vec<AgendaStep> {
    value
        .get("entries")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|entry| AgendaStep {
            text: entry
                .get("content")
                .or_else(|| entry.get("text"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            status: status(
                entry
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            ),
        })
        .collect()
}

fn available_commands(value: &Value) -> Vec<AgentSkill> {
    value
        .get("availableCommands")
        .or_else(|| value.get("commands"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|command| AgentSkill {
            name: text(command, "name"),
            description: command
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_owned),
            scope: Some("cursor".to_owned()),
        })
        .collect()
}

fn usage(value: &Value) -> Usage {
    let number = |keys: &[&str]| {
        keys.iter()
            .find_map(|key| value.get(*key).and_then(Value::as_u64))
            .unwrap_or_default()
    };
    Usage {
        tokens_in: number(&["inputTokens", "input_tokens"]),
        tokens_out: number(&["outputTokens", "output_tokens"]),
        cached: number(&["cachedInputTokens", "cached_input_tokens"]),
        context_used: number(&["totalTokens", "total_tokens"]),
        context_window: value
            .get("contextWindow")
            .or_else(|| value.get("context_window"))
            .and_then(Value::as_u64),
    }
}

fn patches(update: &Value, id: &str) -> Vec<AgentEvent> {
    update
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|content| content.get("type").and_then(Value::as_str) == Some("diff"))
        .map(|content| {
            let path = text(content, "path");
            let old = content
                .get("oldText")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let new = content
                .get("newText")
                .and_then(Value::as_str)
                .unwrap_or_default();
            AgentEvent::Patch {
                id: format!("{id}:{path}"),
                unified_diff: format!("--- {path}\n+++ {path}\n@@\n-{old}\n+{new}\n"),
            }
        })
        .collect()
}

fn tool_output(update: &Value) -> Option<String> {
    if let Some(output) = update.get("rawOutput") {
        return Some(match output.as_str() {
            Some(output) => output.to_owned(),
            None => serde_json::to_string_pretty(output).unwrap_or_else(|_| output.to_string()),
        });
    }
    update
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find_map(|entry| {
            (entry.get("type").and_then(Value::as_str) == Some("content"))
                .then(|| content_text(entry.get("content")))
                .flatten()
        })
}

/// Cursor's live model and mode selectors, parsed from a session-open response.
#[must_use]
pub fn parse_capability(reply: &Value) -> AgentCapability {
    let result = reply.get("result").unwrap_or(reply);
    let configs = result
        .get("configOptions")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let model_config = configs
        .iter()
        .find(|option| option.get("category").and_then(Value::as_str) == Some("model"));
    let effort_config = configs
        .iter()
        .find(|option| option.get("category").and_then(Value::as_str) == Some("thought_level"));
    let efforts = effort_config
        .map(select_options)
        .unwrap_or_default()
        .into_iter()
        .map(|(id, _, description)| EffortOption {
            effort: id,
            description,
        })
        .collect::<Vec<_>>();
    let current_model = model_config
        .and_then(|option| option.get("currentValue"))
        .and_then(Value::as_str);
    let models = model_config
        .map(select_options)
        .unwrap_or_default()
        .into_iter()
        .map(|(id, label, description)| AgentModel {
            is_default: Some(id.as_str()) == current_model,
            id,
            label,
            description,
            implied_mode: None,
            default_effort: effort_config
                .and_then(|option| option.get("currentValue"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            efforts: efforts.clone(),
        })
        .collect();
    let live_modes = parse_modes(result);
    AgentCapability {
        models,
        modes: if live_modes.is_empty() {
            cursor_modes()
        } else {
            live_modes
        },
        models_are_live: true,
        supports_fast: false,
    }
}

fn select_options(config: &Value) -> Vec<(String, String, Option<String>)> {
    let mut values = Vec::new();
    collect_options(config.get("options"), &mut values);
    values
}

fn collect_options(value: Option<&Value>, out: &mut Vec<(String, String, Option<String>)>) {
    let Some(value) = value else { return };
    if let Some(array) = value.as_array() {
        for option in array {
            let id = option
                .get("value")
                .or_else(|| option.get("id"))
                .and_then(Value::as_str);
            if let Some(id) = id {
                out.push((
                    id.to_owned(),
                    option
                        .get("name")
                        .or_else(|| option.get("label"))
                        .and_then(Value::as_str)
                        .unwrap_or(id)
                        .to_owned(),
                    option
                        .get("description")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                ));
            } else {
                collect_options(option.get("options"), out);
            }
        }
        return;
    }
    collect_options(value.get("options"), out);
    if let Some(groups) = value.get("groups").and_then(Value::as_array) {
        for group in groups {
            collect_options(group.get("options"), out);
        }
    }
}

fn parse_modes(result: &Value) -> Vec<AgentMode> {
    let current = result
        .pointer("/modes/currentModeId")
        .and_then(Value::as_str);
    let mut modes: Vec<AgentMode> = result
        .pointer("/modes/availableModes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|mode| {
            let id = mode.get("id").and_then(Value::as_str)?.to_owned();
            Some(AgentMode {
                label: mode
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(&id)
                    .to_owned(),
                description: mode
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                is_default: Some(id.as_str()) == current,
                risk: if matches!(id.as_str(), "plan" | "ask") {
                    ModeRisk::Normal
                } else {
                    ModeRisk::Elevated
                },
                id,
            })
        })
        .collect();
    // Cursor does not advertise Auto. Without this the picker only offers Agent / Plan / Ask
    // and every shell prompt comes back, which is the complaint that added the mode.
    if !modes.is_empty() && !modes.iter().any(|mode| mode.id == "auto") {
        let insert_at = modes
            .iter()
            .position(|mode| mode.id == "agent")
            .map_or(0, |i| i + 1);
        modes.insert(insert_at, cursor_auto_mode());
    }
    modes
}

fn cursor_auto_mode() -> AgentMode {
    AgentMode {
        id: "auto".to_owned(),
        label: "Auto".to_owned(),
        description: Some(
            "Use tools without asking for each one. Clarification questions are still shown"
                .to_owned(),
        ),
        is_default: false,
        risk: ModeRisk::Elevated,
    }
}

#[must_use]
pub fn cursor_modes() -> Vec<AgentMode> {
    [
        (
            "agent",
            "Agent",
            "Use tools and edit the worktree, asking when Cursor requires approval",
            ModeRisk::Elevated,
        ),
        (
            "auto",
            "Auto",
            "Use tools without asking for each one. Clarification questions are still shown",
            ModeRisk::Elevated,
        ),
        (
            "plan",
            "Plan",
            "Research and propose a plan without editing the worktree",
            ModeRisk::Normal,
        ),
        (
            "ask",
            "Ask",
            "Answer questions without editing the worktree",
            ModeRisk::Normal,
        ),
    ]
    .into_iter()
    .map(|(id, label, description, risk)| AgentMode {
        id: id.to_owned(),
        label: label.to_owned(),
        description: Some(description.to_owned()),
        is_default: id == "agent",
        risk,
    })
    .collect()
}

/// Frames used by the short-lived capability probe in the composition root.
#[must_use]
pub fn initialize_frame(id: i64) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "protocolVersion": 1,
            "clientCapabilities": {
                "fs": { "readTextFile": false, "writeTextFile": false },
                "terminal": false,
            },
            "clientInfo": { "name": "wtm-capability-probe", "version": env!("CARGO_PKG_VERSION") },
        },
    })
    .to_string()
}

#[must_use]
pub fn authenticate_frame(id: i64) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "authenticate",
        "params": { "methodId": "cursor_login" },
    })
    .to_string()
}

#[must_use]
pub fn new_session_frame(id: i64, cwd: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "session/new",
        "params": { "cwd": cwd, "mcpServers": [] },
    })
    .to_string()
}
