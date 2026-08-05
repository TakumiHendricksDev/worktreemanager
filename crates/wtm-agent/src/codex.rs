//! The Codex CLI, over `codex app-server`.
//!
//! # Why the app server and not `codex exec`
//!
//! `codex exec --json` is the obvious choice and it is wrong twice over, both verified against
//! `codex-cli 0.144.6`:
//!
//! - **It cannot ask for approval.** There is no `--ask-for-approval` flag on `exec` at all
//!   (`error: unexpected argument '-a' found`), so a session there either runs unsupervised or
//!   not at all. Interactive Codex means the app server, full stop.
//! - **It emits no deltas.** Its stream is coarse `item.completed` events, so a transcript
//!   would appear a paragraph at a time. `item/agentMessage/delta` exists only here.
//!
//! The app server is also the surface `OpenAI` built for GUI frontends, and it can emit its own
//! schema — `codex app-server generate-json-schema --out DIR` — which is what makes the mapping
//! below checkable rather than guessed.
//!
//! # Three things the real server does that the spec does not say
//!
//! All three were found by driving it, and each would be a silent bug:
//!
//! 1. **Replies omit `jsonrpc`.** A response is `{"id":1,"result":{…}}`, not
//!    `{"jsonrpc":"2.0","id":1,…}`. Validating strict JSON-RPC 2.0 rejects every reply.
//! 2. **Requests before `initialize` are refused**, and `initialized` must follow it, so the
//!    handshake is two frames before anything useful.
//! 3. **`thread/start` alone does not mean ready.** It is a second round trip after
//!    `initialize`, which is why [`Step::Ready`] exists separately from `SessionReady`.
//!
//! Not needed, and worth recording so nobody adds it defensively: `capabilities.experimentalApi`.
//! Some methods are gated behind it, but `initialize`, `thread/start`, `thread/resume`,
//! `thread/list`, `turn/start` and `model/list` are not.

use std::collections::BTreeMap;

use serde_json::{Value, json};
use wtm_core::model::{
    AgendaStatus, AgendaStep, AgentEvent, ApprovalAnswer, ApprovalRequest, NoticeLevel, Usage,
};

use crate::provider::{Protocol, Provider, ProviderId, SessionRequest, Step};

pub const ID: &str = "codex";

/// What wtm calls itself in the handshake.
///
/// Sent because the server otherwise labels the session `source: "vscode"` — verified in a real
/// `thread/start` reply. Claiming to be another editor in the user's own rollout history is the
/// kind of small dishonesty that makes a log untrustworthy later.
const CLIENT_NAME: &str = "wtm";

#[derive(Debug)]
pub struct Codex;

impl Provider for Codex {
    fn id(&self) -> ProviderId {
        ProviderId::new(ID)
    }

    fn program(&self) -> &'static str {
        "codex"
    }

    fn argv(&self, req: &SessionRequest) -> Vec<String> {
        let mut argv = vec![
            self.program().to_owned(),
            "app-server".to_owned(),
            // stdio is the default, but saying so is cheap and means a future change of default
            // cannot silently move this onto a socket.
            "--stdio".to_owned(),
        ];
        argv.extend(req.extra_args.iter().cloned());
        argv
    }

    fn protocol(&self, req: &SessionRequest) -> Box<dyn Protocol> {
        Box::new(CodexProtocol::new(req.clone()))
    }
}

/// Where a session is in its two-step handshake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// `initialize` sent, waiting for its reply.
    Initializing,
    /// `thread/start` or `thread/resume` sent, waiting for the thread id.
    Starting,
    Ready,
}

struct CodexProtocol {
    req: SessionRequest,
    phase: Phase,
    /// Monotonic JSON-RPC request id. Starts at 1 because 0 is indistinguishable from a missing
    /// field in some JSON tooling and there is no reason to find out which.
    next_id: i64,
    /// Which request id was `initialize` / the thread open, so a reply can be recognised without
    /// keeping a map of every id ever sent.
    init_id: Option<i64>,
    open_id: Option<i64>,
    thread_id: Option<String>,
    /// Turns submitted before the handshake finished, replayed once it does.
    ///
    /// Without this, typing into a pane the instant it opens loses the message: the composer is
    /// live before `thread/start` has come back. Queuing is invisible when the handshake is fast
    /// and is the difference between a lost first prompt and a slightly late one when it is not.
    queued: Vec<String>,
    /// Server-initiated requests awaiting a user answer, keyed by the id the frontend hands back.
    pending: BTreeMap<String, Pending>,
}

/// One server-initiated request the user has not answered yet.
struct Pending {
    /// The JSON-RPC id to reply on. The reply *must* carry this and nothing else identifies it.
    rpc_id: Value,
}

impl CodexProtocol {
    /// Turn a server-initiated request into an approval card, or into a `Raw` row.
    ///
    /// The three approval methods are the ones a user can act on. Everything else that arrives as
    /// a request — an MCP elicitation, a tool asking for input — is surfaced as `Raw` for now
    /// rather than silently ignored, and it stays in `pending` so `abandon` can decline it. A
    /// request that is neither answered nor declined leaves the server blocked forever.
    fn on_server_request(&mut self, rpc_id: i64, method: &str, params: &Value) -> Vec<Step> {
        // Our own id for the card. The JSON-RPC id is unique within a session and is what the
        // reply has to carry, so using it as the key means `answer` needs no second lookup.
        let id = rpc_id.to_string();
        let text = |key: &str| params.get(key).and_then(Value::as_str).map(str::to_owned);

        let request = match method {
            "item/commandExecution/requestApproval" | "execCommandApproval" => {
                ApprovalRequest::Command {
                    // `command` is nullable in the schema even for a command approval, so an
                    // absent one becomes a visible placeholder rather than an empty card.
                    command: text("command").unwrap_or_else(|| "(command not reported)".to_owned()),
                    cwd: text("cwd"),
                    reason: text("reason"),
                }
            }
            "item/fileChange/requestApproval" | "applyPatchApproval" => {
                ApprovalRequest::FileChange {
                    // The diff arrives with the `item/fileChange/*` stream rather than in the
                    // approval params, so this card names what is being asked and the patch row
                    // above it shows the change.
                    unified_diff: text("patch").or_else(|| text("diff")).unwrap_or_default(),
                    reason: text("reason").or_else(|| text("grantRoot")),
                }
            }
            "item/permissions/requestApproval" => ApprovalRequest::Permissions {
                summary: text("reason")
                    .unwrap_or_else(|| "The session is asking for extra permissions".to_owned()),
                items: permission_items(params),
            },
            _ => {
                self.pending.insert(
                    id.clone(),
                    Pending {
                        rpc_id: json!(rpc_id),
                    },
                );
                return vec![Step::Emit(AgentEvent::Raw {
                    provider: ID.to_owned(),
                    event: method.to_owned(),
                    payload: params.clone(),
                })];
            }
        };

        self.pending.insert(
            id.clone(),
            Pending {
                rpc_id: json!(rpc_id),
            },
        );
        vec![Step::Emit(AgentEvent::ApprovalRequested {
            id,
            // Always blocking: the server does not continue the turn until it has a reply, so a
            // card the user can scroll past would stall the session with no explanation.
            blocking: true,
            request,
        })]
    }

    fn new(req: SessionRequest) -> Self {
        Self {
            req,
            phase: Phase::Initializing,
            next_id: 1,
            init_id: None,
            open_id: None,
            thread_id: None,
            queued: Vec::new(),
            pending: BTreeMap::new(),
        }
    }

    fn take_id(&mut self) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn request(&mut self, method: &str, params: &Value) -> (i64, Step) {
        let id = self.take_id();
        (
            id,
            Step::Write(
                json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
                    .to_string(),
            ),
        )
    }

    /// The frame that opens a thread — `thread/resume` when resuming, `thread/start` otherwise.
    ///
    /// One function for both because every parameter except the thread id is shared, and the
    /// two drifting apart is how a resumed session would quietly lose its sandbox setting.
    fn open_thread(&mut self) -> Step {
        let mut params = json!({ "cwd": self.req.cwd });
        if let Some(model) = &self.req.model {
            params["model"] = json!(model);
        }
        if let Some(mode) = &self.req.mode {
            params["approvalPolicy"] = json!(mode);
        }

        let method = if let Some(resume) = self.req.resume.clone() {
            params["threadId"] = json!(resume);
            "thread/resume"
        } else {
            "thread/start"
        };

        let (id, step) = self.request(method, &params);
        self.open_id = Some(id);
        step
    }

    fn turn_frame(&mut self, text: &str) -> Option<Step> {
        let thread = self.thread_id.clone()?;
        let mut params = json!({
            "threadId": thread,
            "input": [{ "type": "text", "text": text }],
        });
        // Sent per turn rather than only at thread open because the server treats both as
        // "this turn and subsequent ones", so a mid-session model change needs no new thread.
        if let Some(model) = &self.req.model {
            params["model"] = json!(model);
        }
        if let Some(effort) = &self.req.effort {
            params["effort"] = json!(effort);
        }
        let (_, step) = self.request("turn/start", &params);
        Some(step)
    }

    /// A reply to one of our requests.
    fn on_reply(&mut self, id: i64, message: &Value) -> Vec<Step> {
        if let Some(error) = message.get("error") {
            let detail = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("the app server rejected a request");
            return vec![Step::Emit(AgentEvent::Failed {
                message: detail.to_owned(),
            })];
        }

        if Some(id) == self.init_id {
            // `initialized` is a notification, so it has no id and no reply to wait for; the
            // thread open follows immediately in the same batch.
            let notify = Step::Write(
                json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }).to_string(),
            );
            self.phase = Phase::Starting;
            let open = self.open_thread();
            return vec![notify, open];
        }

        if Some(id) == self.open_id {
            let result = message.get("result");
            let thread = result
                .and_then(|r| r.get("thread"))
                .and_then(|t| t.get("id"))
                .and_then(Value::as_str)
                .map(str::to_owned);

            let Some(thread) = thread else {
                return vec![Step::Emit(AgentEvent::Failed {
                    message: "the app server opened a thread with no id".to_owned(),
                })];
            };

            self.thread_id = Some(thread.clone());
            self.phase = Phase::Ready;

            let mut steps = vec![
                Step::Emit(AgentEvent::SessionReady {
                    provider_session_id: thread,
                    model: self.req.model.clone(),
                    effort: self.req.effort.clone(),
                    tools: Vec::new(),
                }),
                Step::Ready,
            ];

            for text in std::mem::take(&mut self.queued) {
                if let Some(step) = self.turn_frame(&text) {
                    steps.push(step);
                }
            }
            return steps;
        }

        // A reply to something we sent and no longer care about — a `turn/start`
        // acknowledgement, for instance, whose interesting content arrives as notifications.
        Vec::new()
    }

    /// A server-initiated notification.
    #[allow(clippy::too_many_lines)]
    fn on_notification(method: &str, params: &Value) -> Vec<Step> {
        let text = |key: &str| {
            params
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };

        let event = match method {
            "turn/started" => AgentEvent::TurnStarted {
                turn: text("turnId"),
            },
            "turn/completed" => AgentEvent::TurnFinished {
                turn: text("turnId"),
                usage: usage_from(params.get("usage")),
                // Codex reports tokens and no currency. See the `agent` model's docs for why
                // this is left empty rather than priced here.
                cost_usd: None,
            },
            "item/agentMessage/delta" => AgentEvent::MessageDelta {
                text: text("delta"),
            },
            "item/reasoning/textDelta" | "item/reasoning/summaryTextDelta" => {
                AgentEvent::ReasoningDelta {
                    text: text("delta"),
                }
            }
            "thread/tokenUsage/updated" => AgentEvent::Usage(usage_from(params.get("usage"))),
            "turn/plan/updated" => AgentEvent::AgendaUpdated {
                explanation: params
                    .get("explanation")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                steps: agenda_from(params.get("plan")),
            },
            "item/started" | "item/completed" => return Self::on_item(method, params),
            "item/commandExecution/outputDelta" => AgentEvent::CommandOutput {
                id: text("itemId"),
                chunk: text("chunk"),
            },
            "item/fileChange/patchUpdated" | "turn/diff/updated" => AgentEvent::Patch {
                id: text("itemId"),
                unified_diff: params
                    .get("diff")
                    .or_else(|| params.get("patch"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            },
            "warning" | "guardianWarning" | "configWarning" | "deprecationNotice" => {
                AgentEvent::Notice {
                    level: NoticeLevel::Warn,
                    message: params
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or(method)
                        .to_owned(),
                }
            }
            "error" => AgentEvent::Failed {
                message: params
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("the app server reported an error")
                    .to_owned(),
            },
            // Everything else — and there are around seventy notification methods, most of
            // them irrelevant to a transcript — becomes a collapsed row. Deliberate: see the
            // `agent` model's docs on why this is the design rather than a fallback.
            _ => AgentEvent::Raw {
                provider: ID.to_owned(),
                event: method.to_owned(),
                payload: params.clone(),
            },
        };

        vec![Step::Emit(event)]
    }

    /// `item/started` and `item/completed` carry a nested item whose own `type` decides what it
    /// is. Split out because that second level of dispatch made `on_notification` unreadable.
    fn on_item(method: &str, params: &Value) -> Vec<Step> {
        let Some(item) = params.get("item") else {
            return vec![Step::Emit(AgentEvent::Raw {
                provider: ID.to_owned(),
                event: method.to_owned(),
                payload: params.clone(),
            })];
        };

        let started = method == "item/started";
        let id = item
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let item_type = item.get("type").and_then(Value::as_str).unwrap_or_default();

        let event = match (item_type, started) {
            ("agent_message", false) => AgentEvent::Message {
                text: item
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            },

            ("command_execution", true) => AgentEvent::CommandStarted {
                id,
                command: item
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                cwd: item.get("cwd").and_then(Value::as_str).map(str::to_owned),
            },
            ("command_execution", false) => AgentEvent::CommandFinished {
                id,
                exit_code: item
                    .get("exit_code")
                    .and_then(Value::as_i64)
                    .and_then(|c| i32::try_from(c).ok()),
            },
            ("file_change", false) => AgentEvent::Patch {
                id,
                unified_diff: item
                    .get("diff")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            },
            ("mcp_tool_call" | "web_search" | "tool_search", true) => AgentEvent::ToolStarted {
                title: item.get("title").and_then(Value::as_str).map(str::to_owned),
                id,
                name: item_type.to_owned(),
            },
            ("mcp_tool_call" | "web_search" | "tool_search", false) => AgentEvent::ToolFinished {
                id,
                ok: item
                    .get("status")
                    .and_then(Value::as_str)
                    .is_none_or(|s| s != "failed"),
                output: item
                    .get("output")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            },
            // Both already arrived as deltas, so emitting the item as well would duplicate the
            // text — an empty bubble ahead of a streaming message, or the whole thought twice.
            ("agent_message", true) | ("reasoning", _) => return Vec::new(),
            _ => AgentEvent::Raw {
                provider: ID.to_owned(),
                event: format!("{method}:{item_type}"),
                payload: item.clone(),
            },
        };

        vec![Step::Emit(event)]
    }
}

impl Protocol for CodexProtocol {
    fn open(&mut self) -> Vec<Step> {
        let (id, step) = self.request(
            "initialize",
            &json!({
                "clientInfo": {
                    "name": CLIENT_NAME,
                    "title": "Worktree Manager",
                    "version": env!("CARGO_PKG_VERSION"),
                }
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

        // Not valid JSON at all. Surfaced rather than dropped: when a CLI is misconfigured this
        // is usually a human-readable complaint, and it is the only clue the user gets.
        let Ok(message) = serde_json::from_str::<Value>(trimmed) else {
            return vec![Step::Emit(AgentEvent::Notice {
                level: NoticeLevel::Warn,
                message: trimmed.to_owned(),
            })];
        };

        // Note the absence of a `jsonrpc` check. Real replies omit the field — see the module
        // docs — so requiring it would reject every one of them.
        if let Some(id) = message.get("id").and_then(Value::as_i64) {
            if let Some(method) = message.get("method").and_then(Value::as_str) {
                // A server-initiated *request*: it has both an id and a method, and the client is
                // expected to answer. This is how every approval arrives.
                let method = method.to_owned();
                let params = message.get("params").cloned().unwrap_or(Value::Null);
                return self.on_server_request(id, &method, &params);
            }
            return self.on_reply(id, &message);
        }

        match message.get("method").and_then(Value::as_str) {
            Some(method) => {
                let params = message.get("params").cloned().unwrap_or(Value::Null);
                Self::on_notification(method, &params)
            }
            None => vec![Step::Emit(AgentEvent::Raw {
                provider: ID.to_owned(),
                event: "unrecognized".to_owned(),
                payload: message,
            })],
        }
    }

    fn send_turn(&mut self, text: &str) -> Vec<Step> {
        if self.phase == Phase::Ready {
            let mut steps = vec![Step::Emit(AgentEvent::UserEcho {
                text: text.to_owned(),
            })];
            steps.extend(self.turn_frame(text));
            return steps;
        }

        // Queued, and echoed anyway. The message is visibly in the transcript even though the
        // handshake has not finished, which is the difference between "slow" and "broken".
        self.queued.push(text.to_owned());
        vec![Step::Emit(AgentEvent::UserEcho {
            text: text.to_owned(),
        })]
    }

    fn answer(&mut self, id: &str, answer: &ApprovalAnswer) -> Vec<Step> {
        // Removed rather than read: the first answer wins, and a second one for the same request
        // finds nothing here. That is the whole concurrency story — two panes, or a click and a
        // keystroke, cannot both reply and desynchronise the server's view of the turn.
        let Some(pending) = self.pending.remove(id) else {
            return Vec::new();
        };

        let decision = match answer {
            ApprovalAnswer::Allow => json!("accept"),
            ApprovalAnswer::AllowForSession => json!("acceptForSession"),
            // `decline`, not `cancel`. Both deny, and the difference is what happens next: the
            // server documents `decline` as "the agent will continue the turn" and `cancel` as
            // "the turn will also be immediately interrupted". Denying one command should not
            // throw away the rest of the work, and the user has a Stop button for when it should.
            ApprovalAnswer::Deny { .. } => json!("decline"),
            ApprovalAnswer::AllowWithEdits { .. } => {
                // Codex has no verb for this. Claude Code's `allow` can carry an `updatedInput`
                // and rewrite the call; there is no equivalent here, so this is refused loudly
                // rather than downgraded to a plain `accept` — running a command the user edited,
                // unedited, is the worst available outcome. The UI does not offer the affordance
                // for this provider; this guards the case where it is offered by mistake.
                self.pending.insert(id.to_owned(), pending);
                return vec![Step::Emit(AgentEvent::Failed {
                    message: "Codex cannot run an edited command — allow it as-is, or deny it."
                        .to_owned(),
                })];
            }
        };

        vec![
            Step::Write(
                json!({
                    "jsonrpc": "2.0",
                    "id": pending.rpc_id,
                    "result": { "decision": decision },
                })
                .to_string(),
            ),
            Step::Emit(AgentEvent::ApprovalResolved { id: id.to_owned() }),
        ]
    }

    fn interrupt(&mut self) -> Vec<Step> {
        let Some(thread) = self.thread_id.clone() else {
            return Vec::new();
        };
        let (_, step) = self.request("turn/interrupt", &json!({ "threadId": thread }));
        vec![step]
    }

    fn abandon(&mut self) -> Vec<Step> {
        // Every outstanding request gets a `decline`, so the server is never left blocked on a
        // reply from a window that has gone. Drained rather than iterated, because answering
        // twice is exactly what `answer`'s `remove` exists to prevent.
        let pending = std::mem::take(&mut self.pending);
        pending
            .into_iter()
            .flat_map(|(id, entry)| {
                [
                    Step::Write(
                        json!({
                            "jsonrpc": "2.0",
                            "id": entry.rpc_id,
                            "result": { "decision": "decline" },
                        })
                        .to_string(),
                    ),
                    Step::Emit(AgentEvent::ApprovalResolved { id }),
                ]
            })
            .collect()
    }
}

fn usage_from(value: Option<&Value>) -> Usage {
    let Some(value) = value else {
        return Usage::default();
    };
    let field = |key: &str| value.get(key).and_then(Value::as_u64).unwrap_or_default();
    Usage {
        tokens_in: field("input_tokens") + field("inputTokens"),
        tokens_out: field("output_tokens") + field("outputTokens"),
        cached: field("cached_input_tokens") + field("cachedInputTokens"),
        context_window: value
            .get("model_context_window")
            .or_else(|| value.get("contextWindow"))
            .and_then(Value::as_u64),
    }
}

fn agenda_from(value: Option<&Value>) -> Vec<AgendaStep> {
    let Some(items) = value.and_then(Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .map(|item| AgendaStep {
            text: item
                .get("step")
                .or_else(|| item.get("text"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            status: match item.get("status").and_then(Value::as_str) {
                Some("completed") => AgendaStatus::Completed,
                Some("in_progress" | "inProgress") => AgendaStatus::InProgress,
                _ => AgendaStatus::Pending,
            },
        })
        .collect()
}

/// The individual grants a permissions request is asking for, flattened for display.
///
/// Best-effort by design: the shape is `{ fileSystem: { read: [...], write: [...] }, network: {
/// enabled } }` with every level optional and a documented migration in progress from `read`/`write`
/// to `entries`. A card that listed nothing would be worse than one that lists what it recognises,
/// so unknown shapes simply contribute nothing and the summary still names the request.
fn permission_items(params: &Value) -> Vec<String> {
    let mut items = Vec::new();
    let profile = params.get("permissions").or_else(|| params.get("profile"));
    let Some(profile) = profile else {
        return items;
    };

    if let Some(fs) = profile.get("fileSystem") {
        for (key, verb) in [("read", "read"), ("write", "write")] {
            if let Some(paths) = fs.get(key).and_then(Value::as_array) {
                for path in paths.iter().filter_map(Value::as_str) {
                    items.push(format!("{verb} {path}"));
                }
            }
        }
    }
    if profile
        .get("network")
        .and_then(|n| n.get("enabled"))
        .and_then(Value::as_bool)
        == Some(true)
    {
        items.push("network access".to_owned());
    }
    items
}
