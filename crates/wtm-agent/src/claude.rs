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
use wtm_core::model::{
    AgentEvent, AgentSkill, ApprovalAnswer, ApprovalRequest, NoticeLevel, Usage,
};

use crate::provider::{McpServer, Protocol, Provider, ProviderId, SessionRequest, Step};

pub const ID: &str = "claude";

/// Tools whose approval is really "run this command".
const COMMAND_TOOLS: &[&str] = &["Bash", "BashOutput", "KillShell"];

/// Tools whose approval is really "change these files".
const FILE_TOOLS: &[&str] = &["Write", "Edit", "NotebookEdit", "MultiEdit"];

/// The settings overlay that turns the `ultracode` rung on. Merged, not replacing — `--help` calls
/// it "additional settings", so a user's own `settings.json` survives.
const ULTRACODE_SETTINGS: &str = r#"{"ultracode":true}"#;

/// The one mode this CLI spells two ways, resolved to the one the flag accepts.
///
/// `--permission-mode` takes `manual`; the `init` message reports the same state as `default`. Both
/// mean "ask", and the CLI's own help calls `manual` an accepted alias for `default`. Everything
/// else passes through untouched, including a mode a future release adds and this build has never
/// heard of — the field is a free string end to end for the same reason `model` is.
fn canonical_mode(mode: &str) -> &str {
    if mode == "default" { "manual" } else { mode }
}

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
        // `ultracode` is the top rung of the CLI's own `/effort` ladder but the *flag* rejects it —
        // `--effort` takes `low|medium|high|xhigh|max` and nothing else. The programmatic route is
        // the `ultracode` settings key, which the CLI documents as settable "via --settings or
        // apply_flag_settings". So the rung is translated into the pair that actually means it.
        //
        // `max` rather than the documented minimum of `xhigh`, because the ladder puts ultracode
        // above max and a user who picks the top rung should not quietly get less reasoning than
        // the rung below it.
        if let Some(effort) = &req.effort {
            if effort == crate::capability::ULTRACODE {
                argv.push("--effort".to_owned());
                argv.push("max".to_owned());
                argv.push("--settings".to_owned());
                argv.push(ULTRACODE_SETTINGS.to_owned());
            } else {
                argv.push("--effort".to_owned());
                argv.push(effort.clone());
            }
        }
        if let Some(mode) = &req.mode {
            argv.push("--permission-mode".to_owned());
            argv.push(canonical_mode(mode).to_owned());
        }
        // One `--mcp-config` carrying every server as JSON. The flag accepts a file path or a
        // literal object; the literal is used because the alternative is a temp file whose lifetime
        // nothing here owns — the CLI reads it at some unspecified point after spawn, so deleting it
        // is a race and leaving it is litter in a directory the user did not choose.
        if !req.mcp.is_empty() {
            argv.push("--mcp-config".to_owned());
            argv.push(mcp_config_json(&req.mcp));
        }

        // `--append-system-prompt`, not `--system-prompt`. The latter replaces the CLI's own
        // instructions wholesale, which would break far more than it fixed. See
        // `SessionRequest::instructions`.
        if let Some(instructions) = &req.instructions {
            argv.push("--append-system-prompt".to_owned());
            argv.push(instructions.clone());
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

/// Every MCP server as the one JSON object `--mcp-config` expects.
///
/// `{"mcpServers": {name: {command, args, env}}}`. Built here rather than by the caller because it
/// is a fact about *this* CLI's flag: Codex takes the same set of servers as a pile of `-c` dotted
/// overrides, and a caller that pre-serialized for one of them is the reason the other silently
/// received nothing for an increment.
fn mcp_config_json(servers: &BTreeMap<String, McpServer>) -> String {
    let mut map = serde_json::Map::new();
    for (name, server) in servers {
        map.insert(
            name.clone(),
            serde_json::json!({
                "command": server.command,
                "args": server.args,
                "env": server.env,
            }),
        );
    }
    serde_json::json!({ "mcpServers": map }).to_string()
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
    /// Whether a text delta has arrived since the last `assistant` message.
    ///
    /// Cleared by each `assistant` message rather than by each turn, because one turn produces
    /// several of them — one per tool round trip — and a later message that streamed nothing must
    /// not be silenced by an earlier one that did. See [`Self::on_assistant`].
    streamed: bool,
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
                        // Recorded so `on_assistant` can tell whether the whole message it is about
                        // to see has already been shown. See there.
                        self.streamed = true;
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
    ///
    /// # Unless nothing was streamed, in which case this is the only copy
    ///
    /// Dropping the text blocks unconditionally was a real bug with a bad presentation. The CLI
    /// answers some turns **without calling the model at all** — an expired OAuth session, a
    /// refusal — and those come back as one `assistant` message with `model: "<synthetic>"`, no
    /// `stream_event` deltas before it, and the reason in a `text` block. So a session whose auth
    /// had lapsed showed a user's message, a usage row of zeros, and *nothing else*: the CLI had
    /// said "Failed to authenticate: OAuth session expired and could not be refreshed" and wtm
    /// discarded it on the grounds that deltas had already carried it. They had not.
    ///
    /// `streamed` makes the assumption a fact this driver checks rather than one it asserts.
    fn on_assistant(&mut self, message: &Value) -> Vec<Step> {
        let Some(blocks) = message.get("content").and_then(Value::as_array) else {
            return Vec::new();
        };

        let streamed = std::mem::take(&mut self.streamed);
        blocks
            .iter()
            .filter_map(|block| {
                match block.get("type").and_then(Value::as_str) {
                    // The whole message, when the deltas did not carry it. `Message` rather than
                    // `MessageDelta`, which is what that variant exists for — a provider that only
                    // reports complete messages.
                    Some("text") if !streamed => {
                        let text = block
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default();
                        (!text.is_empty()).then(|| {
                            Step::Emit(AgentEvent::Message {
                                text: text.to_owned(),
                            })
                        })
                    }
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
                    // `text` and `thinking` that already arrived as deltas; emitting them again
                    // would duplicate every reply. Dropped here rather than in the frontend, so a
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
    /// A control request *from* wtm, in the direction the channel is least documented in.
    ///
    /// The id is ours to choose and no caller waits on the reply — a failure surfaces as the
    /// `Notice` that `on_line` makes out of an error `control_response`, not as a return value.
    fn control_request(request: &Value) -> Step {
        Step::Write(
            json!({
                "type": "control_request",
                "request_id": uuid::Uuid::new_v4().to_string(),
                "request": request,
            })
            .to_string(),
        )
    }

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
                    let names = |key: &str| -> Vec<String> {
                        message
                            .get(key)
                            .and_then(Value::as_array)
                            .map(|a| {
                                a.iter()
                                    .filter_map(Value::as_str)
                                    .map(str::to_owned)
                                    .collect()
                            })
                            .unwrap_or_default()
                    };

                    // The CLI's own merged list of user, project, plugin and built-in commands —
                    // `--help` calls them skills too ("--disable-slash-commands: Disable all
                    // skills"). Names only: `init` carries no descriptions, and re-deriving them by
                    // scanning `~/.claude/skills` would be this app re-implementing another
                    // program's discovery rules and going stale the first time they changed.
                    let skills = names("slash_commands")
                        .into_iter()
                        .map(|name| AgentSkill {
                            name,
                            description: None,
                            scope: None,
                        })
                        .collect::<Vec<_>>();

                    let mut steps = vec![Step::Emit(AgentEvent::SessionReady {
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
                        // Whatever it resolved to, including from a `settings.json` wtm never saw.
                        // Normalised because this message and the flag spell the same mode two
                        // different ways — see `canonical_mode`.
                        mode: message
                            .get("permissionMode")
                            .and_then(Value::as_str)
                            .map(canonical_mode)
                            .map(str::to_owned),
                        tools: names("tools"),
                    })];
                    if !skills.is_empty() {
                        steps.push(Step::Emit(AgentEvent::SkillsListed { skills }));
                    }
                    steps.push(Step::Ready);
                    steps
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
            // The CLI answering one of *our* control requests — an interrupt, a model change, a
            // mode change. Success is uninteresting by construction: nothing awaits these replies,
            // so a success row would be a collapsed `Raw` line after every Stop press.
            //
            // A failure is very interesting, and this arm is the whole reason the picker is allowed
            // to change a running session. `set_model` and `set_permission_mode` are read off the
            // shipped binary rather than out of any published protocol, so a CLI that has renamed
            // or dropped one answers with an error here — and without this the picker would show
            // the new value while the session kept using the old one, which is the worst outcome
            // available for a control that says "bypass permissions".
            "control_response" => {
                let response = message.get("response").unwrap_or(&Value::Null);
                match response.get("subtype").and_then(Value::as_str) {
                    Some("error") => vec![Step::Emit(AgentEvent::Notice {
                        level: NoticeLevel::Warn,
                        message: format!(
                            "The CLI refused a settings change: {}",
                            response
                                .get("error")
                                .and_then(Value::as_str)
                                .unwrap_or("no reason given")
                        ),
                    })],
                    _ => Vec::new(),
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
                let mut steps = Vec::new();

                /*
                 * A turn that failed says so here, and this used to read straight past it.
                 *
                 * `is_error` is `true` and `result` carries the reason, while `subtype` stays
                 * `"success"` — the subtype describes the *shape* of the reply, not the outcome, so
                 * matching on it is not an alternative. Ignoring both is how an expired OAuth
                 * session presented as a pane with a message in it, a usage row of zeros, and no
                 * explanation of any kind.
                 *
                 * Before the finish, not after: the failure is the reason the turn ended, so it
                 * reads above the row that closes it.
                 */
                if message.get("is_error").and_then(Value::as_bool) == Some(true) {
                    steps.push(Step::Emit(AgentEvent::Failed {
                        message: message
                            .get("result")
                            .and_then(Value::as_str)
                            .filter(|s| !s.is_empty())
                            .unwrap_or("the turn failed and the CLI gave no reason")
                            .to_owned(),
                    }));
                }

                steps.push(Step::Emit(AgentEvent::TurnFinished {
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
                }));
                steps
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

    /// Change the model or the permission mode without restarting.
    ///
    /// Two more control requests on the channel `interrupt` already uses. Both subtypes are read
    /// off the shipped CLI rather than out of documentation — like `--permission-prompt-tool` in
    /// the module header, they are real and unpublished — so the failure mode matters: a subtype
    /// this CLI version does not know comes back as a `control_response` with `subtype: "error"`,
    /// which [`Self::on_line`] already turns into a `Notice`. A rejected change therefore says so
    /// in the transcript instead of leaving the picker quietly lying about the session's state.
    fn reconfigure(&mut self, model: Option<&str>, mode: Option<&str>) -> Vec<Step> {
        let mut steps = Vec::new();
        if let Some(model) = model {
            steps.push(Self::control_request(&json!({
                "subtype": "set_model",
                "model": model,
            })));
        }
        if let Some(mode) = mode {
            steps.push(Self::control_request(&json!({
                "subtype": "set_permission_mode",
                "mode": canonical_mode(mode),
            })));
        }
        steps
    }

    fn interrupt(&mut self) -> Vec<Step> {
        // A control *request* from us to the CLI, which is the one direction that channel runs in
        // both ways. The id is ours to choose and the reply is not interesting.
        vec![Self::control_request(&json!({ "subtype": "interrupt" }))]
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
