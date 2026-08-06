//! Claude Code, over `-p` with stream-json in both directions.
//!
//! # The flag without which approvals do not exist
//!
//! `--permission-prompt-tool stdio` is **mandatory** and is not in the CLI's `--help` or its public
//! documentation. Without it, `can_use_tool` control requests never arrive: the tool is auto-denied
//! and shows up only afterwards in `result.permission_denials`, so a session appears to work while
//! every edit silently fails. The Agent SDK passes it internally, which is why nothing published
//! mentions it.
//!
//! # Two channels on one pipe
//!
//! Unlike Codex's single JSON-RPC stream, this transport carries two interleaved things:
//!
//!   * **messages** — `{"type":"system"|"assistant"|"user"|"result"|"stream_event"}`, the transcript;
//!   * **control** — `{"type":"control_request"|"control_response"}`, correlated by a top-level
//!     `request_id` in its own namespace, which is how a GUI answers a permission prompt.
//!
//! Both are lines on stdout and both are answered on stdin. That second channel is the reason this
//! provider is a compiled module rather than TOML: "read this, correlate it by that field, reply on
//! the same pipe with this shape" is a program.
//!
//! # `--session-id` must be fresh
//!
//! Reusing one is a hard error on stderr — *"Session ID … is already in use"* — and the process
//! exits immediately, which presents as a pane that dies before it says anything. wtm mints a v4
//! UUID per session, so this only bites a caller that tries to reuse one to resume; resuming is
//! `--resume`, which is a different flag.
//!
//! # Everything here was captured from the wire
//!
//! `claude 2.1.221`, driven with a real turn that made a tool call. The `content_block_delta`
//! nesting, the `control_request` envelope's top-level `request_id`, and `result` carrying both
//! `total_cost_usd` and `modelUsage.<model>.contextWindow` are all observed rather than inferred —
//! the previous provider's fixtures were written from the wrong transport and every test passed
//! while the mapping was wrong.

use std::collections::BTreeMap;

use serde_json::{Value, json};
use wtm_core::model::{AgentEvent, ApprovalAnswer, ApprovalRequest, NoticeLevel, Usage};

use crate::provider::{Protocol, Provider, ProviderId, SessionRequest, Step};

pub const ID: &str = "claude";

/// Tools whose approval is really "run this command".
const COMMAND_TOOLS: &[&str] = &["Bash", "BashOutput", "KillShell"];

/// Tools whose approval is really "change these files".
const FILE_TOOLS: &[&str] = &["Write", "Edit", "NotebookEdit", "MultiEdit"];

#[derive(Debug)]
pub struct Claude;

impl Provider for Claude {
    fn id(&self) -> ProviderId {
        ProviderId::new(ID)
    }

    fn program(&self) -> &'static str {
        "claude"
    }

    fn argv(&self, req: &SessionRequest) -> Vec<String> {
        let mut argv: Vec<String> = [
            self.program(),
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            // Required for stream-json output per the CLI's own docs.
            "--verbose",
            // Without this there are no deltas at all — text arrives as whole `assistant` messages,
            // so a transcript would appear a paragraph at a time.
            "--include-partial-messages",
            // See the module docs. This one is the difference between having approvals and not.
            "--permission-prompt-tool",
            "stdio",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();

        // Resume by id, or claim one. Claude is the provider that lets wtm *choose* the session id,
        // which is why `SessionReady` reports whatever it ends up being rather than assuming.
        if let Some(resume) = &req.resume {
            argv.push("--resume".to_owned());
            argv.push(resume.clone());
        } else {
            argv.push("--session-id".to_owned());
            argv.push(uuid::Uuid::new_v4().to_string());
        }

        if let Some(model) = &req.model {
            argv.push("--model".to_owned());
            argv.push(model.clone());
        }
        if let Some(effort) = &req.effort {
            argv.push("--effort".to_owned());
            argv.push(effort.clone());
        }
        if let Some(mode) = &req.mode {
            argv.push("--permission-mode".to_owned());
            argv.push(mode.clone());
        }
        if let Some(mcp) = &req.mcp_config {
            argv.push("--mcp-config".to_owned());
            argv.push(mcp.clone());
        }

        // The cwd is already the worktree, but `--add-dir` is what puts it in the allow-list for
        // tools that check one — without it an edit inside the session's own directory can be
        // refused with `blocked_path`, which reads as a bug in wtm.
        argv.push("--add-dir".to_owned());
        argv.push(req.cwd.clone());

        argv.extend(req.extra_args.iter().cloned());
        argv
    }

    fn protocol(&self, _req: &SessionRequest) -> Box<dyn Protocol> {
        Box::new(ClaudeProtocol::default())
    }
}

/// One outstanding `can_use_tool`.
struct Pending {
    /// The control channel's own id, which the response must echo.
    request_id: String,
    /// The tool input as it arrived. Replayed in an allow because `updatedInput` was required
    /// before 2.1.207 and sending the original is the correct no-op.
    input: Value,
    /// What the CLI itself suggested granting, used by "always this session".
    suggestions: Value,
}

#[derive(Default)]
struct ClaudeProtocol {
    /// Which content block index is currently a thinking block, so its deltas route to reasoning
    /// rather than to the message. Tracked because a `content_block_delta` says only its index.
    thinking_blocks: std::collections::BTreeSet<u64>,
    pending: BTreeMap<String, Pending>,
    /// Tool calls seen this turn, so a `tool_result` can name the tool it belongs to.
    tools: BTreeMap<String, String>,
    turn: u64,
}

impl ClaudeProtocol {
    /// A stable-enough turn label. Claude reports no turn id of its own, so this counts them.
    fn turn_label(&self) -> String {
        self.turn.to_string()
    }

    /// One `stream_event`, which wraps an Anthropic SSE event.
    fn on_stream_event(&mut self, event: &Value) -> Vec<Step> {
        let kind = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let index = event
            .get("index")
            .and_then(Value::as_u64)
            .unwrap_or_default();

        match kind {
            "content_block_start" => {
                // Remembered so this block's deltas can be routed. A `content_block_delta` carries
                // only an index, so without this a thinking delta is indistinguishable from text.
                let block = event
                    .get("content_block")
                    .and_then(|b| b.get("type"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if block == "thinking" {
                    self.thinking_blocks.insert(index);
                } else {
                    self.thinking_blocks.remove(&index);
                }
                Vec::new()
            }
            "content_block_delta" => {
                let delta = event.get("delta");
                let text = |key: &str| {
                    delta
                        .and_then(|d| d.get(key))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned()
                };
                match delta.and_then(|d| d.get("type")).and_then(Value::as_str) {
                    Some("text_delta") => {
                        vec![Step::Emit(AgentEvent::MessageDelta { text: text("text") })]
                    }
                    Some("thinking_delta") => vec![Step::Emit(AgentEvent::ReasoningDelta {
                        text: text("thinking"),
                    })],
                    // A tool call's arguments streaming in. Nothing to show: the call is announced
                    // as a whole in the `assistant` message that follows.
                    _ => Vec::new(),
                }
            }
            // Frame boundaries with nothing in them for a reader. `message_start` carries a usage
            // snapshot, but the authoritative one is on `result`.
            _ => Vec::new(),
        }
    }

    /// An `assistant` message. Its content blocks are already-complete versions of what the deltas
    /// carried, plus the tool calls, which only arrive here.
    fn on_assistant(&mut self, message: &Value) -> Vec<Step> {
        let Some(blocks) = message.get("content").and_then(Value::as_array) else {
            return Vec::new();
        };

        blocks
            .iter()
            .filter_map(|block| {
                match block.get("type").and_then(Value::as_str) {
                    Some("tool_use") => {
                        let id = block
                            .get("id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned();
                        let name = block
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned();
                        self.tools.insert(id.clone(), name.clone());
                        Some(Step::Emit(AgentEvent::ToolStarted {
                            title: tool_title(&name, block.get("input")),
                            id,
                            name,
                        }))
                    }
                    // `text` and `thinking` already arrived as deltas; emitting them again would
                    // duplicate every reply. Dropped here rather than in the frontend, so a
                    // provider that streams and a provider that does not both work unchanged.
                    _ => None,
                }
            })
            .collect()
    }

    /// A `user` message, which on this transport means a tool result coming back.
    fn on_user(&mut self, message: &Value) -> Vec<Step> {
        let Some(blocks) = message.get("content").and_then(Value::as_array) else {
            return Vec::new();
        };

        blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
            .map(|block| {
                let id = block
                    .get("tool_use_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                self.tools.remove(&id);
                Step::Emit(AgentEvent::ToolFinished {
                    ok: block.get("is_error").and_then(Value::as_bool) != Some(true),
                    output: block
                        .get("content")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    id,
                })
            })
            .collect()
    }

    /// A `can_use_tool` control request. The one thing on this channel a user acts on.
    fn on_can_use_tool(&mut self, request_id: &str, request: &Value) -> Vec<Step> {
        let tool = request
            .get("tool_name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let input = request.get("input").cloned().unwrap_or(Value::Null);
        let description = request
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_owned);

        let approval = if tool == "ExitPlanMode" {
            // Plan approval arrives as an ordinary tool permission, which is the whole reason a GUI
            // can intercept it. `plan` is the markdown; `planFilePath` is where the CLI wrote it.
            ApprovalRequest::PlanReview {
                markdown: input
                    .get("plan")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                path: input
                    .get("planFilePath")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            }
        } else if COMMAND_TOOLS.contains(&tool.as_str()) {
            ApprovalRequest::Command {
                command: input
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                cwd: None,
                reason: description,
            }
        } else if FILE_TOOLS.contains(&tool.as_str()) {
            ApprovalRequest::FileChange {
                // The tool input is the change rather than a diff — `Write` carries whole contents,
                // `Edit` carries old and new strings. Rendered as the payload it is, because
                // synthesising a unified diff here would be inventing line numbers.
                unified_diff: file_change_preview(&tool, &input),
                reason: request
                    .get("blocked_path")
                    .and_then(Value::as_str)
                    .map(|p| format!("outside the allowed directories: {p}"))
                    .or(description),
            }
        } else {
            ApprovalRequest::ToolInput {
                tool: tool.clone(),
                prompt: description.unwrap_or_else(|| format!("{tool} wants to run")),
            }
        };

        self.pending.insert(
            request_id.to_owned(),
            Pending {
                request_id: request_id.to_owned(),
                input,
                suggestions: request
                    .get("permission_suggestions")
                    .cloned()
                    .unwrap_or(Value::Null),
            },
        );

        vec![Step::Emit(AgentEvent::ApprovalRequested {
            id: request_id.to_owned(),
            // The CLI does not proceed without a reply, so every one of these blocks.
            blocking: true,
            request: approval,
        })]
    }

    /// The frame that answers a control request.
    fn control_response(request_id: &str, response: &Value) -> Step {
        Step::Write(
            json!({
                "type": "control_response",
                "response": {
                    "subtype": "success",
                    "request_id": request_id,
                    "response": response,
                },
            })
            .to_string(),
        )
    }
}

impl Protocol for ClaudeProtocol {
    fn open(&mut self) -> Vec<Step> {
        // Ready at once, and no handshake to send.
        //
        // This CLI emits **nothing at all** until it receives a turn — verified by waiting sixty
        // seconds for a `system`/`init` that never came, then seeing it arrive as the first line
        // after a user message. So readiness here means "the process is up and will accept a
        // turn", which is true immediately, and gating on `init` would leave a pane saying
        // "starting…" until someone typed into it.
        //
        // The opposite of the app server, where `initialize` and `thread/start` are two round
        // trips that must complete first. Two providers, two honest answers to the same question,
        // which is the reason `Step::Ready` is a step a provider chooses rather than something the
        // session layer infers.
        vec![Step::Ready]
    }

    fn on_line(&mut self, line: &str) -> Vec<Step> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Vec::new();
        }

        let Ok(message) = serde_json::from_str::<Value>(trimmed) else {
            // Not JSON. Usually a human-readable complaint, and the only clue the user gets.
            return vec![Step::Emit(AgentEvent::Notice {
                level: NoticeLevel::Warn,
                message: trimmed.to_owned(),
            })];
        };

        let kind = message
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();

        match kind {
            "system" => match message.get("subtype").and_then(Value::as_str) {
                Some("init") => {
                    let tools = message
                        .get("tools")
                        .and_then(Value::as_array)
                        .map(|a| {
                            a.iter()
                                .filter_map(Value::as_str)
                                .map(str::to_owned)
                                .collect()
                        })
                        .unwrap_or_default();
                    vec![
                        Step::Emit(AgentEvent::SessionReady {
                            provider_session_id: message
                                .get("session_id")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                            model: message
                                .get("model")
                                .and_then(Value::as_str)
                                .map(str::to_owned),
                            effort: None,
                            tools,
                        }),
                        Step::Ready,
                    ]
                }
                // Recognised, and deliberately not shown.
                //
                // `thinking_tokens` is a running estimate emitted many times a second — a real turn
                // sent sixteen for one reply, and there is already a reasoning *stream* for the
                // content. `status` and `post_turn_summary` are the CLI talking to its own UI.
                // Suppressed rather than `Raw`-ed because a real turn produced three of them around
                // a two-word answer, and each one is a collapsed row nobody wants.
                //
                // The `hook_*` pair is the worst case of that and the reason this list is worth
                // maintaining. A user's `SessionStart` hooks fire before anything else, so on a
                // fresh pane they are the *only* two rows — and because `SessionPane` gates its
                // "Ask Claude something." prompt on an empty transcript, they replaced the empty
                // state with two lines of protocol debris the moment a pane opened.
                Some(
                    "thinking_tokens"
                    | "status"
                    | "post_turn_summary"
                    | "turn_starting"
                    | "turn_duration"
                    | "session_state_changed"
                    | "vcs_state_changed"
                    | "file_snapshot"
                    | "hook_started"
                    | "hook_response",
                ) => Vec::new(),
                Some("api_error" | "permission_error") => {
                    vec![Step::Emit(AgentEvent::Failed {
                        message: message
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("the CLI reported an error")
                            .to_owned(),
                    })]
                }
                other => vec![Step::Emit(AgentEvent::Raw {
                    provider: ID.to_owned(),
                    event: format!("system:{}", other.unwrap_or("?")),
                    payload: message.clone(),
                })],
            },
            "stream_event" => match message.get("event") {
                Some(event) => self.on_stream_event(event),
                None => Vec::new(),
            },
            "assistant" => match message.get("message") {
                Some(inner) => self.on_assistant(inner),
                None => Vec::new(),
            },
            "user" => match message.get("message") {
                Some(inner) => self.on_user(inner),
                None => Vec::new(),
            },
            "control_request" => {
                let request_id = message
                    .get("request_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let request = message.get("request").cloned().unwrap_or(Value::Null);
                match request.get("subtype").and_then(Value::as_str) {
                    Some("can_use_tool") => self.on_can_use_tool(&request_id, &request),
                    // Something on the control channel this build does not handle. Surfaced *and*
                    // kept, so `abandon` can still answer it — an unanswered control request
                    // blocks the CLI exactly as an unanswered approval does.
                    other => {
                        self.pending.insert(
                            request_id.clone(),
                            Pending {
                                request_id,
                                input: Value::Null,
                                suggestions: Value::Null,
                            },
                        );
                        vec![Step::Emit(AgentEvent::Raw {
                            provider: ID.to_owned(),
                            event: format!("control:{}", other.unwrap_or("?")),
                            payload: request,
                        })]
                    }
                }
            }
            "result" => {
                let usage = message.get("usage");
                let field = |key: &str| {
                    usage
                        .and_then(|u| u.get(key))
                        .and_then(Value::as_u64)
                        .unwrap_or_default()
                };
                vec![Step::Emit(AgentEvent::TurnFinished {
                    turn: self.turn_label(),
                    usage: Usage {
                        tokens_in: field("input_tokens"),
                        tokens_out: field("output_tokens"),
                        cached: field("cache_read_input_tokens"),
                        context_window: context_window_of(&message),
                    },
                    // Claude reports real currency, where Codex reports none. Surfaced rather than
                    // normalized away, because the number is genuinely available on one side.
                    cost_usd: message.get("total_cost_usd").and_then(Value::as_f64),
                })]
            }
            "rate_limit_event" => Vec::new(),
            "error" => vec![Step::Emit(AgentEvent::Failed {
                message: message
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("the CLI reported an error")
                    .to_owned(),
            })],
            other => vec![Step::Emit(AgentEvent::Raw {
                provider: ID.to_owned(),
                event: other.to_owned(),
                payload: message.clone(),
            })],
        }
    }

    fn send_turn(&mut self, text: &str) -> Vec<Step> {
        self.turn += 1;
        vec![
            Step::Emit(AgentEvent::UserEcho {
                text: text.to_owned(),
            }),
            // Claude announces no turn start of its own, so this is where one exists. Emitted
            // rather than inferred from the first delta, so the composer can show "working…"
            // during the seconds before any token arrives.
            Step::Emit(AgentEvent::TurnStarted {
                turn: self.turn_label(),
            }),
            Step::Write(
                json!({
                    "type": "user",
                    "message": { "role": "user", "content": text },
                    "parent_tool_use_id": null,
                })
                .to_string(),
            ),
        ]
    }

    fn answer(&mut self, id: &str, answer: &ApprovalAnswer) -> Vec<Step> {
        // Removed rather than read: the first answer wins, exactly as on the other provider.
        let Some(pending) = self.pending.remove(id) else {
            return Vec::new();
        };

        let response = match answer {
            // `updatedInput` carries the original back. It was *required* before 2.1.207, and
            // echoing what arrived is the correct no-op on every version.
            ApprovalAnswer::Allow => json!({
                "behavior": "allow",
                "updatedInput": pending.input,
            }),
            ApprovalAnswer::AllowForSession => {
                let mut response = json!({
                    "behavior": "allow",
                    "updatedInput": pending.input,
                });
                // The CLI proposes what "always" should mean — a mode change, a rule, a directory —
                // and it knows better than we do: for `Bash` it suggested a pattern rule, for
                // `Write` a mode change. Passing its own suggestion back is what makes this button
                // grant the right thing rather than the broadest thing.
                if !pending.suggestions.is_null() {
                    response["updatedPermissions"] = pending.suggestions.clone();
                }
                response
            }
            // The verb Codex has no equivalent for. This is the provider it exists for.
            ApprovalAnswer::AllowWithEdits { input } => json!({
                "behavior": "allow",
                "updatedInput": input,
            }),
            ApprovalAnswer::Deny { message } => json!({
                "behavior": "deny",
                "message": message.clone().unwrap_or_else(|| "Denied in wtm".to_owned()),
            }),
        };

        vec![
            Self::control_response(&pending.request_id, &response),
            Step::Emit(AgentEvent::ApprovalResolved { id: id.to_owned() }),
        ]
    }

    fn interrupt(&mut self) -> Vec<Step> {
        // A control *request* from us to the CLI, which is the one direction that channel runs in
        // both ways. The id is ours to choose and the reply is not interesting.
        vec![Step::Write(
            json!({
                "type": "control_request",
                "request_id": uuid::Uuid::new_v4().to_string(),
                "request": { "subtype": "interrupt" },
            })
            .to_string(),
        )]
    }

    fn abandon(&mut self) -> Vec<Step> {
        let pending = std::mem::take(&mut self.pending);
        pending
            .into_iter()
            .flat_map(|(id, entry)| {
                [
                    Self::control_response(
                        &entry.request_id,
                        &json!({ "behavior": "deny", "message": "The session was closed" }),
                    ),
                    Step::Emit(AgentEvent::ApprovalResolved { id }),
                ]
            })
            .collect()
    }
}

/// A one-line summary of a tool call, for the transcript's tool row.
///
/// Best-effort and deliberately short: the row says what is happening, and the approval card is
/// where the whole payload belongs.
fn tool_title(name: &str, input: Option<&Value>) -> Option<String> {
    let input = input?;
    let text = |key: &str| input.get(key).and_then(Value::as_str);
    match name {
        "Bash" => text("description")
            .or_else(|| text("command"))
            .map(str::to_owned),
        "Read" | "Write" | "Edit" | "NotebookEdit" => {
            text("file_path").map(|p| format!("{name} {p}"))
        }
        "Glob" | "Grep" => text("pattern").map(|p| format!("{name} {p}")),
        "Task" => text("description").map(str::to_owned),
        _ => None,
    }
}

/// What a file-changing tool is proposing, rendered as the payload it actually is.
///
/// Not a unified diff: `Write` carries whole contents and `Edit` carries an old and a new string, so
/// producing one would mean inventing line numbers. Showing the real shape is more honest and, for
/// an `Edit`, more readable than a synthesised hunk.
fn file_change_preview(tool: &str, input: &Value) -> String {
    let text = |key: &str| input.get(key).and_then(Value::as_str).unwrap_or_default();
    let path = text("file_path");
    match tool {
        "Edit" | "MultiEdit" => format!(
            "{path}\n\n- {}\n+ {}",
            text("old_string"),
            text("new_string")
        ),
        _ => {
            let content = text("content");
            format!("{path}\n\n{content}")
        }
    }
}

/// The model's context window, from `result.modelUsage.<model>.contextWindow`.
///
/// Nested under the model name, which is not known in advance — so the first entry is taken. A
/// single turn only ever uses one model unless a fallback fired, and in that case either answer is
/// as true as the other.
fn context_window_of(result: &Value) -> Option<u64> {
    result
        .get("modelUsage")?
        .as_object()?
        .values()
        .find_map(|m| m.get("contextWindow").and_then(Value::as_u64))
}
