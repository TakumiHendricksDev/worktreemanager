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
    AgendaStatus, AgendaStep, AgentAttachment, AgentEvent, AgentSkill, ApprovalAnswer,
    ApprovalRequest, NoticeLevel, Usage, UserInputOption, UserInputQuestion,
};

use crate::provider::{McpServer, Protocol, Provider, ProviderId, SessionRequest, Step};

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
        argv.extend(mcp_overrides(&req.mcp));
        argv.extend(req.extra_args.iter().cloned());
        argv
    }

    fn protocol(&self, req: &SessionRequest) -> Box<dyn Protocol> {
        Box::new(CodexProtocol::new(req.clone()))
    }
}

/// Whether a name can be a TOML bare key, and therefore a safe `-c` path segment.
///
/// The check exists because `-c` takes a *dotted path* and the documented grammar is
/// `foo.bar.baz` — a segment containing a dot would silently nest one level deeper than intended,
/// and a segment containing a quote would produce something that parses as neither a bare nor a
/// quoted key. TOML's own bare-key rule is exactly the safe set, so it is the one used.
///
/// Quoting the segment instead was the obvious alternative and was rejected: whether this CLI's
/// path splitter honours `a."b.c".d` is not something wtm can know without depending on an
/// undocumented parser detail, and guessing wrong would put a server under the wrong key rather
/// than fail loudly.
fn is_bare_key(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Every MCP server as `-c mcp_servers.…` overrides.
///
/// Codex has no `--mcp-config`: its servers come from `~/.codex/config.toml`, and `-c` is the
/// documented way to override a value that file would otherwise supply. So the same set Claude
/// receives as one JSON blob arrives here as a flat list of dotted assignments.
///
/// Values are emitted as JSON, which is deliberate rather than lazy: `-c` parses the right-hand
/// side as TOML, and a JSON string and a TOML basic string are the same grammar, as are a JSON
/// array of strings and a TOML array. A JSON *object* is not a TOML inline table — `:` versus `=` —
/// which is why `env` is expanded into one assignment per variable rather than emitted whole.
fn mcp_overrides(servers: &BTreeMap<String, McpServer>) -> Vec<String> {
    let mut argv = Vec::new();
    for (name, server) in servers {
        if !is_bare_key(name) {
            // Skipped rather than mangled. The caller validates too and reports this properly; this
            // arm is the backstop that keeps a bad name from landing under the wrong key.
            tracing::warn!(
                name,
                "skipping an MCP server whose name is not a TOML bare key"
            );
            continue;
        }
        let mut push = |path: String, value: &Value| {
            argv.push("-c".to_owned());
            argv.push(format!("{path}={value}"));
        };
        push(
            format!("mcp_servers.{name}.command"),
            &json!(server.command),
        );
        push(format!("mcp_servers.{name}.args"), &json!(server.args));
        for (key, value) in &server.env {
            if !is_bare_key(key) {
                tracing::warn!(
                    key,
                    "skipping an MCP env var whose name is not a TOML bare key"
                );
                continue;
            }
            push(format!("mcp_servers.{name}.env.{key}"), &json!(value));
        }
    }
    argv
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
    /// The `skills/list` request id, cleared when its reply arrives. `None` the rest of the time,
    /// which is most of a session's life.
    skills_id: Option<i64>,
    thread_id: Option<String>,
    /// The in-flight turn reported by `turn/started`.
    ///
    /// Codex requires both this and `threadId` on `turn/interrupt`. Keeping only the thread id
    /// made Stop look wired while every request was rejected as missing `turnId`.
    active_turn_id: Option<String>,
    /// Turns submitted before the handshake finished, replayed once it does.
    ///
    /// Without this, typing into a pane the instant it opens loses the message: the composer is
    /// live before `thread/start` has come back. Queuing is invisible when the handshake is fast
    /// and is the difference between a lost first prompt and a slightly late one when it is not.
    queued: Vec<(String, Vec<AgentAttachment>)>,
    /// Server-initiated requests awaiting a user answer, keyed by the id the frontend hands back.
    pending: BTreeMap<String, Pending>,
    /// The most recent token counts.
    ///
    /// Cached because `turn/completed` reports none — verified on the wire — so without this a
    /// finished turn shows a row of zeros. `thread/tokenUsage/updated` is the only source.
    usage: Usage,
}

/// One server-initiated request the user has not answered yet.
struct Pending {
    /// The JSON-RPC id to reply on. The reply *must* carry this and nothing else identifies it.
    rpc_id: Value,
    kind: PendingKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingKind {
    Approval,
    UserInput,
}

impl CodexProtocol {
    /// Turn a server-initiated request into an approval card, or into a `Raw` row.
    ///
    /// Approval and user-input methods are the ones a person can act on. Everything else that
    /// arrives as a request is surfaced as `Raw` rather than silently ignored, and it stays in
    /// `pending` so `abandon` can still reply. A request left unanswered blocks the server forever.
    fn on_server_request(&mut self, rpc_id: i64, method: &str, params: &Value) -> Vec<Step> {
        // Our own id for the card. The JSON-RPC id is unique within a session and is what the
        // reply has to carry, so using it as the key means `answer` needs no second lookup.
        let id = rpc_id.to_string();
        let text = |key: &str| params.get(key).and_then(Value::as_str).map(str::to_owned);

        let (request, kind) = match method {
            "item/commandExecution/requestApproval" | "execCommandApproval" => {
                (
                    ApprovalRequest::Command {
                        // `command` is nullable in the schema even for a command approval, so an
                        // absent one becomes a visible placeholder rather than an empty card.
                        command: text("command")
                            .unwrap_or_else(|| "(command not reported)".to_owned()),
                        cwd: text("cwd"),
                        reason: text("reason"),
                    },
                    PendingKind::Approval,
                )
            }
            "item/fileChange/requestApproval" | "applyPatchApproval" => {
                (
                    ApprovalRequest::FileChange {
                        // The diff arrives with the `item/fileChange/*` stream rather than in the
                        // approval params, so this card names what is being asked and the patch row
                        // above it shows the change.
                        unified_diff: text("patch").or_else(|| text("diff")).unwrap_or_default(),
                        reason: text("reason").or_else(|| text("grantRoot")),
                    },
                    PendingKind::Approval,
                )
            }
            "item/permissions/requestApproval" => (
                ApprovalRequest::Permissions {
                    summary: text("reason").unwrap_or_else(|| {
                        "The session is asking for extra permissions".to_owned()
                    }),
                    items: permission_items(params),
                },
                PendingKind::Approval,
            ),
            "item/tool/requestUserInput" => (
                ApprovalRequest::UserInput {
                    questions: codex_questions(params),
                },
                PendingKind::UserInput,
            ),
            _ => {
                self.pending.insert(
                    id.clone(),
                    Pending {
                        rpc_id: json!(rpc_id),
                        kind: PendingKind::Approval,
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
                kind,
            },
        );
        vec![Step::Emit(AgentEvent::ApprovalRequested {
            id,
            blocking: params
                .get("isBlocking")
                .and_then(Value::as_bool)
                .unwrap_or(true),
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
            skills_id: None,
            thread_id: None,
            active_turn_id: None,
            queued: Vec::new(),
            pending: BTreeMap::new(),
            usage: Usage::default(),
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

    /// The frame that opens a thread — fork, resume, or start, in that order.
    ///
    /// One function for both because every parameter except the thread id is shared, and the
    /// two drifting apart is how a resumed session would quietly lose its sandbox setting.
    fn open_thread(&mut self) -> Step {
        let mut params = json!({ "cwd": self.req.cwd });
        if let Some(model) = &self.req.model {
            params["model"] = json!(model);
        }
        if let Some(mode) = &self.req.mode {
            let (approval, sandbox, _) = expand_mode(mode);
            params["approvalPolicy"] = json!(approval);
            // The half that used to be missing. Sending only `approvalPolicy` left the sandbox at
            // whatever `~/.codex/config.toml` said, so two sessions wtm believed were configured
            // identically could have different filesystem reach.
            //
            // The string form here, the object form on `turn/start`. See `expand_mode`.
            params["sandbox"] = json!(sandbox);
        }
        // `developerInstructions`, not `baseInstructions`. Both exist on this frame and the names
        // are close enough to be a trap: `baseInstructions` *replaces* the server's own prompt,
        // taking the user's `AGENTS.md` with it. See `SessionRequest::instructions`.
        //
        // Sent on `thread/resume` too, by way of sharing this function. That is correct rather than
        // incidental — a resumed session is running in the same window as a fresh one, so the fact
        // this conveys is just as true, and the CLI does not carry it in the transcript.
        if let Some(instructions) = &self.req.instructions {
            params["developerInstructions"] = json!(instructions);
        }

        let method = if let Some(fork) = self.req.fork.clone() {
            params["threadId"] = json!(fork);
            params["ephemeral"] = json!(self.req.ephemeral);
            "thread/fork"
        } else if let Some(resume) = self.req.resume.clone() {
            params["threadId"] = json!(resume);
            "thread/resume"
        } else {
            "thread/start"
        };

        let (id, step) = self.request(method, &params);
        self.open_id = Some(id);
        step
    }

    fn turn_frame(&mut self, text: &str, attachments: &[AgentAttachment]) -> Option<Step> {
        let thread = self.thread_id.clone()?;
        let mut input = Vec::new();
        if !text.is_empty() {
            input.push(json!({ "type": "text", "text": text }));
        }
        for attachment in attachments {
            if attachment.mime.starts_with("image/") {
                input.push(json!({ "type": "localImage", "path": attachment.path }));
            } else {
                input.push(json!({
                    "type": "text",
                    "text": format!("Attached file `{}` is available at `{}`.", attachment.name, attachment.path),
                }));
            }
        }
        let mut params = json!({
            "threadId": thread,
            "input": input,
        });
        // Sent per turn rather than only at thread open because the server treats both as
        // "this turn and subsequent ones", so a mid-session model change needs no new thread.
        if let Some(model) = &self.req.model {
            params["model"] = json!(model);
        }
        if let Some(effort) = &self.req.effort {
            params["effort"] = json!(effort);
        }
        // Same argument as model and effort, and the reason this provider needs no restart to
        // change its mode. Note the key *and the type*: `turn/start` takes `sandboxPolicy`, a
        // tagged object, where `thread/start` takes `sandbox`, a plain string. Sending the string
        // to both is what broke every turn on this provider once; see `expand_mode`.
        if let Some(mode) = &self.req.mode {
            let (approval, _, policy) = expand_mode(mode);
            params["approvalPolicy"] = json!(approval);
            params["sandboxPolicy"] = policy;
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

            let mut steps = vec![Step::Emit(AgentEvent::SessionReady {
                provider_session_id: thread,
                model: self.req.model.clone(),
                effort: self.req.effort.clone(),
                mode: self.req.mode.clone(),
                tools: Vec::new(),
            })];

            // `thread/resume` is the one open response whose `thread.turns` is populated. Repaint
            // the durable conversation before announcing readiness so a resumed pane is the
            // conversation the user picked rather than an empty composer attached to it.
            // A fork deliberately does not inherit the transcript: `/btw` is a one-answer overlay.
            if self.req.resume.is_some() && self.req.fork.is_none() {
                steps.extend(history_from_thread(
                    result.and_then(|reply| reply.get("thread")),
                ));
            }
            steps.push(Step::Ready);

            // Asked for after the session is usable, never before it. Skills are a composer
            // convenience and nobody is blocked on them, so gating `Ready` behind this reply would
            // trade a pane that opens late for a list nobody has asked to see yet.
            //
            // Scoped to this worktree: the method takes `cwds` and resolves repo-scoped skills
            // against each, so a session in one worktree must not offer another's.
            let (id, step) = self.request("skills/list", &json!({ "cwds": [self.req.cwd] }));
            self.skills_id = Some(id);
            steps.push(step);

            for (text, attachments) in std::mem::take(&mut self.queued) {
                if let Some(step) = self.turn_frame(&text, &attachments) {
                    steps.push(step);
                }
            }
            return steps;
        }

        if Some(id) == self.skills_id {
            self.skills_id = None;
            return vec![Step::Emit(AgentEvent::SkillsListed {
                skills: parse_skills(message),
            })];
        }

        // A reply to something we sent and no longer care about — a `turn/start`
        // acknowledgement, for instance, whose interesting content arrives as notifications.
        Vec::new()
    }

    /// A server-initiated notification.
    #[allow(clippy::too_many_lines)]
    fn on_notification(&mut self, method: &str, params: &Value) -> Vec<Step> {
        let text = |key: &str| {
            params
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };

        let event = match method {
            // `params.turn.id`, not `params.turnId`. Verified on the wire: both turn
            // notifications carry a whole turn object, and reading the flat key gave an empty
            // string — which is a turn id that matches nothing.
            "turn/started" => {
                let turn = turn_id(params);
                self.active_turn_id = (!turn.is_empty()).then(|| turn.clone());
                AgentEvent::TurnStarted { turn }
            }
            "turn/completed" => {
                let turn = turn_id(params);
                // Turns do not overlap on one thread. Clear unconditionally so a malformed or
                // newer completion payload cannot leave Stop targeting a turn that already ended.
                self.active_turn_id = None;
                AgentEvent::TurnFinished {
                    turn,
                    // `turn/completed` carries **no usage at all** — also verified on the wire,
                    // where reading one produced a row of zeros. Token counts arrive separately on
                    // `thread/tokenUsage/updated`, so the last one seen is what this reports.
                    usage: self.usage,
                    // Codex reports tokens and no currency. See the `agent` model's docs for why
                    // this is left empty rather than priced here.
                    cost_usd: None,
                }
            }
            "item/agentMessage/delta" => AgentEvent::MessageDelta {
                text: text("delta"),
            },
            "item/reasoning/textDelta" | "item/reasoning/summaryTextDelta" => {
                AgentEvent::ReasoningDelta {
                    text: text("delta"),
                }
            }
            "thread/tokenUsage/updated" => {
                // Cached, because `turn/completed` has none of its own to report.
                self.usage = usage_from(Some(params));
                AgentEvent::Usage(self.usage)
            }
            "thread/compacted" => AgentEvent::Notice {
                level: NoticeLevel::Info,
                message: "Context compacted.".to_owned(),
            },
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
            // Recognised, and deliberately not shown. These are lifecycle chatter with nothing in
            // them for a reader, and a real turn emitted eleven of them — six
            // `mcpServer/startupStatus/updated` alone — so `Raw`-ing them buries the reply in
            // rows nobody wants. Listed explicitly rather than pattern-matched, because the
            // difference between this arm and the one below is "we know and there is nothing to
            // say" versus "we do not know", and collapsing the two would make an unrecognised
            // event silently disappear.
            "thread/started"
            | "thread/status/changed"
            | "thread/settings/updated"
            | "mcpServer/startupStatus/updated"
            | "remoteControl/status/changed"
            | "account/updated"
            | "account/rateLimits/updated"
            | "serverRequest/resolved" => return Vec::new(),
            // Everything else — and there are around seventy notification methods — becomes a
            // collapsed row. Deliberate: see the `agent` model's docs on why this is the design
            // rather than a fallback.
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

        // camelCase, and this is where the mapping was wrong. The `ThreadItem` schema spells
        // every type this way — `agentMessage`, `commandExecution`, `fileChange` — while
        // `codex exec --json` emits the *same items* in snake_case. The first fixtures here came
        // from an `exec` capture, so every test passed against a spelling the app server never
        // sends, and a real turn showed `item/started:agentMessage` falling through to `Raw`.
        //
        // Only camelCase is accepted: this adapter talks to the app server and nothing else, and
        // accepting both would be pretending otherwise.
        let event = match (item_type, started) {
            ("agentMessage", false) => AgentEvent::Message {
                text: item
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            },

            ("commandExecution", true) => AgentEvent::CommandStarted {
                id,
                command: item
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                cwd: item.get("cwd").and_then(Value::as_str).map(str::to_owned),
            },
            ("commandExecution", false) => AgentEvent::CommandFinished {
                id,
                // `exitCode` first. A command item has not been observed on this transport yet —
                // it needs a writable sandbox — so the camelCase spelling is inferred from the
                // `ThreadItem` convention the rest of this function was just corrected to, and
                // the snake_case fallback is what an `exec --json` capture showed. When a real
                // one is seen, delete the loser.
                exit_code: item
                    .get("exitCode")
                    .or_else(|| item.get("exit_code"))
                    .and_then(Value::as_i64)
                    .and_then(|c| i32::try_from(c).ok()),
            },
            ("fileChange", false) => AgentEvent::Patch {
                id,
                unified_diff: item
                    .get("diff")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            },
            ("mcpToolCall" | "webSearch" | "dynamicToolCall", true) => AgentEvent::ToolStarted {
                title: item.get("title").and_then(Value::as_str).map(str::to_owned),
                id,
                name: item_type.to_owned(),
            },
            ("mcpToolCall" | "webSearch" | "dynamicToolCall", false) => AgentEvent::ToolFinished {
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
            // All three already arrived another way, so emitting the item as well would
            // duplicate it: a streaming message would get an empty bubble ahead of it, reasoning
            // arrives as deltas, and a `userMessage` is the echo the composer already showed.
            ("agentMessage", true) | ("reasoning" | "userMessage", _) => return Vec::new(),
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
                self.on_notification(method, &params)
            }
            None => vec![Step::Emit(AgentEvent::Raw {
                provider: ID.to_owned(),
                event: "unrecognized".to_owned(),
                payload: message,
            })],
        }
    }

    fn send_turn(&mut self, text: &str, attachments: &[AgentAttachment]) -> Vec<Step> {
        if self.phase == Phase::Ready {
            let mut steps = attachment_steps(attachments);
            steps.push(Step::Emit(AgentEvent::UserEcho {
                text: text.to_owned(),
            }));
            if attachments.is_empty()
                && text.trim_start().starts_with("/compact")
                && let Some(thread) = self.thread_id.clone()
            {
                let (_, step) =
                    self.request("thread/compact/start", &json!({ "threadId": thread }));
                steps.push(Step::Emit(AgentEvent::Notice {
                    level: NoticeLevel::Info,
                    message: "Compacting context…".to_owned(),
                }));
                steps.push(step);
                return steps;
            }
            steps.extend(self.turn_frame(text, attachments));
            return steps;
        }

        // Queued, and echoed anyway. The message is visibly in the transcript even though the
        // handshake has not finished, which is the difference between "slow" and "broken".
        self.queued.push((text.to_owned(), attachments.to_vec()));
        let mut steps = attachment_steps(attachments);
        steps.push(Step::Emit(AgentEvent::UserEcho {
            text: text.to_owned(),
        }));
        steps
    }

    /// Change the model or the mode on a running thread.
    ///
    /// No frame is written, and that is the whole implementation: `turn_frame` re-sends `model`,
    /// `effort`, `approvalPolicy` and `sandboxPolicy` on *every* `turn/start`, and the server
    /// treats each as "this turn and subsequent ones". So the change lands by mutating the request
    /// the next turn will be built from.
    ///
    /// The consequence is worth being explicit about, because it differs from Claude: a change made
    /// while a turn is already running does not affect that turn. It affects the next one. There is
    /// no protocol method to re-approve a turn already in flight, and inventing one by interrupting
    /// and resubmitting would throw away work the user did not ask to discard.
    fn reconfigure(
        &mut self,
        model: Option<&str>,
        effort: Option<&str>,
        mode: Option<&str>,
    ) -> Vec<Step> {
        if let Some(model) = model {
            self.req.model = Some(model.to_owned());
        }
        if let Some(effort) = effort {
            self.req.effort = Some(effort.to_owned());
        }
        if let Some(mode) = mode {
            self.req.mode = Some(mode.to_owned());
        }
        Vec::new()
    }

    fn answer(&mut self, id: &str, answer: &ApprovalAnswer) -> Vec<Step> {
        // Removed rather than read: the first answer wins, and a second one for the same request
        // finds nothing here. That is the whole concurrency story — two panes, or a click and a
        // keystroke, cannot both reply and desynchronise the server's view of the turn.
        let Some(pending) = self.pending.remove(id) else {
            return Vec::new();
        };

        if pending.kind == PendingKind::UserInput {
            let ApprovalAnswer::UserInput { answers, notes } = answer else {
                self.pending.insert(id.to_owned(), pending);
                return vec![Step::Emit(AgentEvent::Failed {
                    message: "This question needs answers, not a permission decision.".to_owned(),
                })];
            };
            let mut answers = answers.clone();
            append_notes(&mut answers, notes.as_deref());
            let answers = answers
                .into_iter()
                .map(|(key, answers)| (key, json!({ "answers": answers })))
                .collect::<serde_json::Map<_, _>>();
            return vec![
                Step::Write(
                    json!({
                        "jsonrpc": "2.0",
                        "id": pending.rpc_id,
                        "result": { "answers": answers },
                    })
                    .to_string(),
                ),
                Step::Emit(AgentEvent::ApprovalResolved { id: id.to_owned() }),
            ];
        }

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
            ApprovalAnswer::UserInput { .. } => {
                self.pending.insert(id.to_owned(), pending);
                return vec![Step::Emit(AgentEvent::Failed {
                    message: "This is a permission request, not a question.".to_owned(),
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
        let Some(turn) = self.active_turn_id.clone() else {
            return Vec::new();
        };
        let (_, step) = self.request(
            "turn/interrupt",
            &json!({ "threadId": thread, "turnId": turn }),
        );
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
                    Step::Write(match entry.kind {
                        PendingKind::Approval => json!({
                            "jsonrpc": "2.0",
                            "id": entry.rpc_id,
                            "result": { "decision": "decline" },
                        })
                        .to_string(),
                        PendingKind::UserInput => json!({
                            "jsonrpc": "2.0",
                            "id": entry.rpc_id,
                            "result": { "answers": {} },
                        })
                        .to_string(),
                    }),
                    Step::Emit(AgentEvent::ApprovalResolved { id }),
                ]
            })
            .collect()
    }
}

/// Token counts from a `thread/tokenUsage/updated`.
///
/// The shape, captured from the wire rather than inferred:
///
/// ```text
/// params.tokenUsage.total = { totalTokens, inputTokens, cachedInputTokens, outputTokens, … }
/// params.tokenUsage.last  = { … the same keys, for the turn just finished … }
/// params.tokenUsage.modelContextWindow
/// ```
///
/// Two things this got wrong before a real turn was run: the key is `tokenUsage`, not `usage`, and
/// the counts are nested under `total` rather than sitting on it. Both read as zero, which in the
/// UI is a token row of zeros on every turn — wrong in a way that looks like a feature that has not
/// been finished rather than a bug.
///
/// **`last`, not `total`.** `total` is lifetime billing usage and can exceed the model's context
/// window after only a few tool rounds. `last` is the latest server-computed prompt footprint, which
/// is the numerator that can truthfully be compared with `modelContextWindow`.
fn usage_from(params: Option<&Value>) -> Usage {
    let Some(usage) = params.and_then(|p| p.get("tokenUsage")) else {
        return Usage::default();
    };
    let current = usage.get("last").unwrap_or(usage);
    let field = |key: &str| current.get(key).and_then(Value::as_u64).unwrap_or_default();
    Usage {
        tokens_in: field("inputTokens"),
        tokens_out: field("outputTokens"),
        cached: field("cachedInputTokens"),
        context_used: field("totalTokens"),
        context_window: usage.get("modelContextWindow").and_then(Value::as_u64),
    }
}

/// Visible user and assistant messages from a resumed app-server thread.
fn history_from_thread(thread: Option<&Value>) -> Vec<Step> {
    thread
        .and_then(|thread| thread.get("turns"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|turn| turn.get("items").and_then(Value::as_array))
        .flatten()
        .filter_map(|item| match item.get("type").and_then(Value::as_str) {
            Some("userMessage") => {
                let text = item
                    .get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
                    .filter_map(|part| part.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n");
                (!text.is_empty()).then_some(Step::Emit(AgentEvent::UserEcho { text }))
            }
            Some("agentMessage") => item
                .get("text")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .map(|text| {
                    Step::Emit(AgentEvent::Message {
                        text: text.to_owned(),
                    })
                }),
            _ => None,
        })
        .collect()
}

fn codex_questions(params: &Value) -> Vec<UserInputQuestion> {
    params
        .get("questions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .map(|(index, question)| {
            let string = |key: &str| {
                question
                    .get(key)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned()
            };
            let options = question
                .get("options")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|option| UserInputOption {
                    label: option
                        .get("label")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    description: option
                        .get("description")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                })
                .collect();
            UserInputQuestion {
                id: question
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .map_or_else(|| format!("question-{index}"), str::to_owned),
                header: string("header"),
                question: string("question"),
                options,
                // Codex's current schema does not expose multi-select; keep the normalized field
                // so a protocol addition does not require a new UI shape.
                multiple: question
                    .get("multiSelect")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                allows_other: question
                    .get("isOther")
                    .and_then(Value::as_bool)
                    .unwrap_or(true),
                secret: question
                    .get("isSecret")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            }
        })
        .collect()
}

fn append_notes(answers: &mut BTreeMap<String, Vec<String>>, notes: Option<&str>) {
    let Some(notes) = notes.map(str::trim).filter(|notes| !notes.is_empty()) else {
        return;
    };
    if let Some((_, values)) = answers.iter_mut().next_back() {
        values.push(format!("Notes: {notes}"));
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

/// The turn id from a `turn/*` notification.
///
/// Nested at `params.turn.id`. The flat `params.turnId` that an `item/*` notification carries does
/// not exist on these two, and reading it yielded an empty string — a turn id matching nothing.
fn turn_id(params: &Value) -> String {
    params
        .get("turn")
        .and_then(|t| t.get("id"))
        .or_else(|| params.get("turnId"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// One wtm mode preset, as the protocol fields it stands for.
///
/// Returns `(approvalPolicy, sandbox, sandboxPolicy)`. See [`crate::capability::codex_modes`] for
/// why the two axes are presented as three presets rather than as nine combinations.
///
/// # The sandbox is spelled two different ways and they are not interchangeable
///
/// `thread/start` takes `sandbox`, a **string** from `SandboxMode`: `read-only`, `workspace-write`,
/// `danger-full-access`. `turn/start` takes `sandboxPolicy`, a **tagged object** from
/// `SandboxPolicy`: `{"type":"readOnly"}`, `{"type":"workspaceWrite"}`, `{"type":"dangerFullAccess"}`
/// — kebab against camel, and a different JSON type. Both are returned here so the two call sites
/// cannot pick the wrong one.
///
/// This is not a hypothetical. The first version of this function returned one string and sent it
/// to both, so every `turn/start` was rejected by the server and turns produced nothing at all —
/// a session that took a message, reported `0 in 0 out`, and answered nothing. The tests passed,
/// because they asserted the string this code sent rather than the shape the server accepts. That
/// is exactly the failure this file's own header warns about, one paragraph long, about fixtures
/// invented from a schema instead of captured from the wire.
///
/// An id this build does not know falls back to the middle preset rather than to the permissive
/// one. A stale `wtm.toml` naming a mode a later version renamed must not silently open the sandbox
/// — the safe direction for an unknown value is the cautious one, and if it is wrong the user sees
/// approval prompts rather than unreviewed writes.
fn expand_mode(mode: &str) -> (&'static str, &'static str, Value) {
    match mode {
        "read-only" => ("on-request", "read-only", json!({ "type": "readOnly" })),
        "full-access" => (
            "never",
            "danger-full-access",
            json!({ "type": "dangerFullAccess" }),
        ),
        _ => (
            "on-request",
            "workspace-write",
            json!({ "type": "workspaceWrite" }),
        ),
    }
}

/// Turn a `skills/list` reply into the domain's skill list.
///
/// The reply is grouped by cwd — the method takes several and answers for each — so the groups are
/// flattened. Disabled skills are dropped: they are in the answer so a settings UI can show them
/// switched off, and offering one in a composer would insert a name that does nothing.
///
/// `shortDescription` first, because the schema says the long `description` is the model-facing
/// prompt and the short one is the human-facing label. Falling back to the long one is still better
/// than a bare name in a two-column list.
fn parse_skills(reply: &Value) -> Vec<AgentSkill> {
    let Some(groups) = reply
        .get("result")
        .and_then(|r| r.get("data"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };

    let text = |value: &Value, key: &str| {
        value
            .get(key)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };

    groups
        .iter()
        .filter_map(|group| group.get("skills").and_then(Value::as_array))
        .flatten()
        .filter(|skill| skill.get("enabled").and_then(Value::as_bool) != Some(false))
        .filter_map(|skill| {
            Some(AgentSkill {
                name: text(skill, "name")?,
                description: text(skill, "shortDescription").or_else(|| text(skill, "description")),
                scope: text(skill, "scope"),
            })
        })
        .collect()
}

/// The frames that ask the app server for its model catalogue.
///
/// Returned as data rather than sent, because this crate cannot spawn — see the crate docs. The
/// composition root writes these, collects lines until it sees the reply to id 3, and hands it to
/// [`parse_models`].
///
/// Ids 1–3 rather than a counter: this is a throwaway connection with no other traffic on it.
#[must_use]
pub fn model_list_frames() -> Vec<String> {
    vec![
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "clientInfo": { "name": CLIENT_NAME, "title": "Worktree Manager", "version": env!("CARGO_PKG_VERSION") } },
        })
        .to_string(),
        json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }).to_string(),
        // No `experimentalApi` capability: `model/list` does not need one, verified by asking.
        json!({ "jsonrpc": "2.0", "id": 3, "method": "model/list", "params": {} }).to_string(),
    ]
}

/// The JSON-RPC id [`model_list_frames`] expects its answer on.
pub const MODEL_LIST_ID: i64 = 3;

/// Turn a `model/list` reply into the domain's model list.
///
/// Hidden models are dropped: the server marks the ones it does not want in a picker, and a hidden
/// entry in ours would offer something the user has no way to understand. Everything else is passed
/// through, including effort ladders that differ between models of the same provider — which is the
/// fact the whole capability query exists for.
#[must_use]
pub fn parse_models(reply: &Value) -> Vec<wtm_core::model::AgentModel> {
    let Some(data) = reply
        .get("result")
        .and_then(|r| r.get("data"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };

    data.iter()
        .filter(|m| m.get("hidden").and_then(Value::as_bool) != Some(true))
        .map(|m| {
            let text = |key: &str| m.get(key).and_then(Value::as_str).map(str::to_owned);
            // `model` is what `turn/start` wants; `id` is the catalogue's own key and has been seen
            // to match. Preferring `model` means the value handed back is the one that works.
            let id = text("model").or_else(|| text("id")).unwrap_or_default();
            wtm_core::model::AgentModel {
                label: text("displayName").unwrap_or_else(|| id.clone()),
                id,
                description: text("description"),
                is_default: m.get("isDefault").and_then(Value::as_bool) == Some(true),
                // `model/list` advertises no mode coupling, so no Codex model implies one.
                implied_mode: None,
                default_effort: text("defaultReasoningEffort"),
                efforts: m
                    .get("supportedReasoningEfforts")
                    .and_then(Value::as_array)
                    .map(|list| {
                        list.iter()
                            .filter_map(|e| {
                                Some(wtm_core::model::EffortOption {
                                    effort: e.get("reasoningEffort")?.as_str()?.to_owned(),
                                    description: e
                                        .get("description")
                                        .and_then(Value::as_str)
                                        .map(str::to_owned),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            }
        })
        .collect()
}
