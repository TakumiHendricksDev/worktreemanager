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
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use serde_json::{Value, json};
use wtm_core::model::{
    AgentAttachment, AgentEvent, AgentSkill, ApprovalAnswer, ApprovalRequest, NoticeLevel, Usage,
    UserInputOption, UserInputQuestion,
};

use crate::provider::{McpServer, Protocol, Provider, ProviderId, SessionRequest, Step};

pub const ID: &str = "claude";

/// Tools whose approval is really "run this command".
const COMMAND_TOOLS: &[&str] = &["Bash", "BashOutput", "KillShell"];

/// Tools whose approval is really "change these files".
const FILE_TOOLS: &[&str] = &["Write", "Edit", "NotebookEdit", "MultiEdit"];

/// The settings key that turns the `ultracode` rung on.
const ULTRACODE_KEY: &str = "ultracode";

/// The settings key that turns high-speed mode on.
///
/// # This is also the SDK opt-in, which is why it is not only a live toggle
///
/// `/fast` typed into a wtm pane used to answer "Fast mode is not available in the Agent SDK",
/// and the reason is that wtm drives this CLI over `-p --input-format stream-json` — which *is*
/// the Agent SDK entrypoint, whatever the pane looks like. Read off 2.1.231, the check is:
///
/// ```text
/// let t = sn("flagSettings")?.fastMode === true;
/// if (Rn() && pfr() && !t) return "sdk_opt_in_required";
/// ```
///
/// `flagSettings` is the source the CLI itself labels "command line arguments" — `--settings`. So
/// the gate is not a prohibition, it is an opt-in a host is expected to declare, and declaring it
/// is what makes [`ClaudeProtocol::reconfigure`]'s live toggle legal at all. A session spawned
/// without the overlay cannot be talked into fast mode afterwards.
const FAST_MODE_KEY: &str = "fastMode";

/// Why fast mode is off, in words, given the CLI's own `fast_mode_disabled_reason` token.
///
/// # Mirrored strings, and the fallback that makes drift harmless
///
/// These are the shipped CLI's own phrasings, read off 2.1.231, because the user has one mental
/// model of fast mode and hearing two different explanations of the same refusal would be worse
/// than either. That does mean a reason this build has never heard of would go unphrased — so it
/// does not go unsaid: the token itself is passed through, the same way [`canonical_mode`] passes
/// an unrecognised mode straight to the flag. A future release inventing a new reason degrades to
/// a slightly terse message instead of to silence.
fn fast_mode_refusal(reason: &str) -> String {
    match reason {
        "not_first_party" => "only available when using the Anthropic API directly".to_owned(),
        "model_not_allowed" => "this model is not in your organization's allowed models".to_owned(),
        "preference" => "disabled by your organization".to_owned(),
        "free" => "requires a paid subscription".to_owned(),
        "extra_usage_disabled" => "requires usage credits".to_owned(),
        "network_error" => "unavailable due to network connectivity".to_owned(),
        "pending" => "still checking availability".to_owned(),
        "disabled_by_env" | "unknown" => "unavailable".to_owned(),
        other => format!("unavailable ({other})"),
    }
}

/// The `--settings` overlay for this request, or `None` when it needs none.
///
/// **One flag carrying one object.** Two `--settings` occurrences is not a documented composition
/// and the CLI's precedence between them is unverified — and since `ultracode` and `fastMode` can
/// now be wanted at once, "append another flag" would have been the obvious wrong answer.
///
/// Merged rather than replacing: `--help` calls this "additional settings", so a user's own
/// `settings.json` survives. Key order is whatever `serde_json::Map` gives, which is stable for a
/// given set — argv that reordered between identical launches would be noise in a trust prompt.
fn flag_settings(req: &SessionRequest) -> Option<String> {
    let mut settings = serde_json::Map::new();
    if req.effort.as_deref() == Some(crate::capability::ULTRACODE) {
        settings.insert(ULTRACODE_KEY.to_owned(), Value::Bool(true));
    }
    if req.fast == Some(true) {
        settings.insert(FAST_MODE_KEY.to_owned(), Value::Bool(true));
    }
    if settings.is_empty() {
        return None;
    }
    Some(Value::Object(settings).to_string())
}

/// The one mode this CLI spells two ways, resolved to the one the flag accepts.
///
/// `--permission-mode` takes `manual`; the `init` message reports the same state as `default`. Both
/// mean "ask", and the CLI's own help calls `manual` an accepted alias for `default`. Everything
/// else passes through untouched, including a mode a future release adds and this build has never
/// heard of — the field is a free string end to end for the same reason `model` is.
fn canonical_mode(mode: &str) -> &str {
    if mode == "default" { "manual" } else { mode }
}

/// Recover the visible transcript Claude keeps for a resumed session.
///
/// Claude's streaming process does not replay prior messages on `--resume`; its own terminal UI
/// reads the JSONL store first. The GUI has to do the same or a correctly resumed process is
/// attached to a pane that looks empty. Provider internals stay here rather than becoming a second
/// transcript store in wtm.
fn claude_history(req: &SessionRequest) -> Vec<AgentEvent> {
    let Some(session) = req.resume.as_deref() else {
        return Vec::new();
    };
    // Apart from rejecting path traversal, this avoids walking the store for a malformed record.
    if uuid::Uuid::parse_str(session).is_err() {
        return Vec::new();
    }
    let Some(home) = std::env::var_os("HOME") else {
        return Vec::new();
    };
    let projects = Path::new(&home).join(".claude/projects");
    let Ok(directories) = std::fs::read_dir(projects) else {
        return Vec::new();
    };

    for directory in directories.flatten() {
        let path = directory.path().join(format!("{session}.jsonl"));
        let Ok(file) = File::open(path) else { continue };
        return claude_history_from(BufReader::new(file));
    }
    Vec::new()
}

/// How many replayed events one resumed pane may contribute.
///
/// A real transcript measured for this change held 2,221 rows and reconstructs to a few thousand
/// events. The frontend keeps `MAX_EVENTS` per pane and drops the **oldest** on overflow, so an
/// unbounded replay would spend a live pane's whole budget on history and then start eating the
/// history it just replayed. The tail is kept rather than the head for the same reason a terminal
/// scrolls: the end of a conversation is the part you resumed to continue.
const MAX_HISTORY_EVENTS: usize = 4_000;

/// Tags Claude wraps around text it injected into its own transcript.
///
/// String matching, which — as [`crate::limits`] says at more length — is not a thing this codebase
/// likes and is here because there is nothing better. These rows are ordinary `type: "user"`
/// records with `isMeta` unset, indistinguishable from something a person typed by any field on the
/// row; the wrapper tag is the only signal that exists. Getting it wrong is cheap in one direction
/// and not the other, so each entry is a **whole opening tag** matched at the *start* of the text
/// rather than anywhere in it: a message that discusses `<task-notification>` in the middle of a
/// sentence is a real thing someone said, and dropping it would be silent data loss, while showing
/// one extra injected row is merely noise.
const SYNTHETIC: &[&str] = &[
    "<local-command-caveat>",
    "<local-command-stdout>",
    "<task-notification>",
    "<system-reminder>",
];

/// The two tags a slash-command invocation is spelled with, in either order.
///
/// Observed both ways round on the wire — `<command-name>` first from `/model`, `<command-message>`
/// first from a skill — so this is a set, not a sequence.
const COMMAND_TAGS: &[&str] = &["<command-name>", "<command-message>"];

/// The text between `<tag>` and `</tag>`, if both are there.
fn tag_value(text: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&close)? + start;
    Some(text[start..end].trim().to_owned())
}

/// What one `type: "user"` row in the durable JSONL actually is.
///
/// Three of these four are things the transcript viewer used to render as a right-aligned bubble,
/// which is the transcript claiming the user said something they never typed.
enum UserRow {
    /// Typed by a person.
    Said(String),
    /// A slash command they ran. Not a message, but not nothing either — it is why the model or the
    /// mode changed halfway down a conversation.
    Ran(String),
    /// Injected by the harness: a hook, a caveat, a task notification, a skill body.
    Injected,
}

fn classify_user(row: &Value, text: &str) -> UserRow {
    // Set by the CLI on everything it writes into the conversation on the user's behalf: the
    // local-command caveat, stop-hook feedback, and the body of a skill a slash command expanded.
    // Cheaper and more reliable than the tag list below, so it goes first.
    if row.get("isMeta").and_then(Value::as_bool) == Some(true) {
        return UserRow::Injected;
    }

    let trimmed = text.trim_start();
    if COMMAND_TAGS.iter().any(|tag| trimmed.starts_with(tag)) {
        let name = tag_value(trimmed, "command-name").unwrap_or_default();
        let args = tag_value(trimmed, "command-args").unwrap_or_default();
        // A command whose name did not parse is still not something the user typed as prose.
        if name.is_empty() {
            return UserRow::Injected;
        }
        return UserRow::Ran(format!("{name} {args}").trim_end().to_owned());
    }
    if SYNTHETIC.iter().any(|tag| trimmed.starts_with(tag)) {
        return UserRow::Injected;
    }
    UserRow::Said(text.to_owned())
}

/// Which already-streamed block kinds a call should emit anyway.
///
/// Two fields rather than one flag because the live path's answer differs between them, and a
/// single `prose: bool` quietly made thinking follow text — see
/// `an_assistant_messages_thinking_block_is_dropped_because_the_deltas_already_carried_it`.
#[derive(Clone, Copy)]
struct Prose {
    /// Live: only when nothing streamed, because a refusal or an auth failure arrives as a whole
    /// `assistant` message with no deltas before it and that text is the only copy of the reply.
    text: bool,
    /// Live: never. Thinking has no synthetic case, and re-emitting a completed chain of thought
    /// would put the whole thing in the transcript a second time, after the answer.
    thinking: bool,
}

/// Every visible event one `assistant` content array carries.
///
/// Shared by the live stream and the resume replay, which differ only in [`Prose`]: live deltas
/// have already carried the text and thinking, while a replay has no deltas and must emit both.
/// Sharing it is the point — two copies of the `tool_use` mapping would drift apart, and drifting
/// apart is precisely how a resumed pane came to look nothing like a live one.
fn assistant_events(blocks: &[Value], prose: Prose) -> Vec<AgentEvent> {
    blocks
        .iter()
        .filter_map(|block| {
            let text = |key: &str| block.get(key).and_then(Value::as_str).unwrap_or_default();
            match block.get("type").and_then(Value::as_str) {
                Some("text") if prose.text => {
                    (!text("text").is_empty()).then(|| AgentEvent::Message {
                        text: text("text").to_owned(),
                    })
                }
                // `ReasoningDelta` rather than a whole-message variant because there is no such
                // variant: the frontend folds consecutive deltas into one collapsible row, and a
                // single delta carrying the whole block folds to exactly that.
                Some("thinking") if prose.thinking => {
                    (!text("thinking").is_empty()).then(|| AgentEvent::ReasoningDelta {
                        text: text("thinking").to_owned(),
                    })
                }
                Some("tool_use") => {
                    let name = text("name").to_owned();
                    Some(AgentEvent::ToolStarted {
                        title: tool_title(&name, block.get("input")),
                        id: text("id").to_owned(),
                        name,
                    })
                }
                _ => None,
            }
        })
        .collect()
}

/// Tool results out of a `user` content array.
///
/// On the live transport a `user` message means only this. In the durable JSONL it is one of the
/// shapes a `user` row takes, which is why this is a function rather than the body of `on_user`.
fn tool_results(blocks: &[Value]) -> Vec<AgentEvent> {
    blocks
        .iter()
        .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
        .map(|block| AgentEvent::ToolFinished {
            ok: block.get("is_error").and_then(Value::as_bool) != Some(true),
            output: block
                .get("content")
                .and_then(Value::as_str)
                .map(str::to_owned),
            id: block
                .get("tool_use_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        })
        .collect()
}

/// Concatenated `text` blocks, or the whole content when it is a bare string.
fn row_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn claude_history_from(reader: impl BufRead) -> Vec<AgentEvent> {
    // `filter_map`, not `map_while`. `lines()` yields an `io::Result`, and one unreadable line —
    // a stray non-UTF-8 byte is enough — used to end the iterator, silently truncating every row
    // after it. A transcript that stops halfway is indistinguishable from a short conversation.
    //
    // `lines_filter_map_ok` guards against a reader that returns `Err` forever, which this one
    // cannot: `Lines` reads to the newline *before* it validates UTF-8, so an invalid line is
    // consumed as it fails and the next call starts after it. The iterator still terminates.
    #[allow(clippy::lines_filter_map_ok)]
    let mut events: Vec<AgentEvent> = reader
        .lines()
        .filter_map(Result::ok)
        .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
        // A sidechain is a subagent's own conversation. It is real, but it is not this transcript.
        .filter(|row| row.get("isSidechain").and_then(Value::as_bool) != Some(true))
        .flat_map(|row| {
            let Some(message) = row.get("message") else {
                return Vec::new();
            };
            let Some(content) = message.get("content") else {
                return Vec::new();
            };
            let blocks = content.as_array().map(Vec::as_slice).unwrap_or_default();

            match row.get("type").and_then(Value::as_str) {
                Some("assistant") => assistant_events(
                    blocks,
                    Prose {
                        text: true,
                        thinking: true,
                    },
                ),
                Some("user") => {
                    let mut out = tool_results(blocks);
                    let text = row_text(content);
                    if !text.is_empty() {
                        match classify_user(&row, &text) {
                            UserRow::Said(text) => out.push(AgentEvent::UserEcho { text }),
                            UserRow::Ran(command) => out.push(AgentEvent::Notice {
                                level: NoticeLevel::Info,
                                message: command,
                            }),
                            UserRow::Injected => {}
                        }
                    }
                    out
                }
                _ => Vec::new(),
            }
        })
        .collect();

    if events.len() > MAX_HISTORY_EVENTS {
        let dropped = events.len() - MAX_HISTORY_EVENTS;
        events.drain(..dropped);
        // Said rather than done quietly: a transcript that begins mid-thought with no explanation
        // reads as the bug this whole change is fixing.
        events.insert(
            0,
            AgentEvent::Notice {
                level: NoticeLevel::Info,
                message: format!(
                    "{dropped} earlier events are not shown. The full transcript is in Claude's own session store."
                ),
            },
        );
    }
    events
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
        let executable = req.executable.as_deref().unwrap_or_else(|| self.program());
        let mut argv: Vec<String> = [
            executable,
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
        if let Some(fork) = &req.fork {
            argv.push("--resume".to_owned());
            argv.push(fork.clone());
            argv.push("--fork-session".to_owned());
            if req.ephemeral {
                // Claude's native `/btw` is one response over the parent's context with no tools,
                // and neither side of the exchange belongs in the durable conversation list.
                argv.push("--no-session-persistence".to_owned());
                argv.push("--tools".to_owned());
                argv.push(String::new());
            }
        } else if let Some(resume) = &req.resume {
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
        // apply_flag_settings", and `flag_settings` below carries it.
        //
        // `max` rather than the documented minimum of `xhigh`, because the ladder puts ultracode
        // above max and a user who picks the top rung should not quietly get less reasoning than
        // the rung below it.
        if let Some(effort) = &req.effort {
            argv.push("--effort".to_owned());
            if effort == crate::capability::ULTRACODE {
                argv.push("max".to_owned());
            } else {
                argv.push(effort.clone());
            }
        }
        if let Some(mode) = &req.mode {
            argv.push("--permission-mode".to_owned());
            argv.push(canonical_mode(mode).to_owned());
        }
        // Every flag-settings key in one place, for the reasons on `flag_settings` — and note this
        // is not merely a default for `fast`: without the overlay at spawn the CLI refuses fast
        // mode for the rest of the session. See `FAST_MODE_KEY`.
        if let Some(settings) = flag_settings(req) {
            argv.push("--settings".to_owned());
            argv.push(settings);
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

    fn protocol(&self, req: &SessionRequest) -> Box<dyn Protocol> {
        Box::new(ClaudeProtocol {
            history: claude_history(req),
            fast_wanted: req.fast == Some(true),
            ..ClaudeProtocol::default()
        })
    }

    fn seed_skills(&self, req: &SessionRequest) -> Vec<AgentSkill> {
        crate::skills::claude(
            Path::new(&req.cwd),
            std::env::var_os("HOME").as_deref().map(Path::new),
        )
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
    /// Visible messages recovered from Claude's durable JSONL when this is a resume.
    history: Vec<AgentEvent>,
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
    /// The prompt footprint of the most recent request, for the context meter.
    ///
    /// Cached because `result.usage` is the wrong number for it: the CLI sums usage over *every*
    /// API round trip in a turn, so with prompt caching the conversation is re-counted once per
    /// tool call and a fifteen-call turn reports several times the window. `message_start` is the
    /// only place a single request's own prompt size appears. This is the same `last`-not-`total`
    /// distinction [`crate::codex`] documents, and Claude's wire format hides the choice rather
    /// than offering it.
    context_used: u64,
    /// Whether fast mode has been asked for, by spawn argv or by a later toggle.
    ///
    /// Tracked because wanting it and having it are different facts, and only the first one is
    /// wtm's to know. See [`fast_mode_notice`].
    fast_wanted: bool,
    /// The last fast-mode refusal already reported, so a standing one is said once and not per turn.
    fast_refusal: Option<String>,
}

impl ClaudeProtocol {
    /// A stable-enough turn label. Claude reports no turn id of its own, so this counts them.
    fn turn_label(&self) -> String {
        self.turn.to_string()
    }

    /// Say so when fast mode was asked for and the CLI reports it is not on.
    ///
    /// The `result` message carries `fast_mode_state` and `fast_mode_disabled_reason` on every
    /// turn, which makes this the one setting in the picker whose *actual* value is observable
    /// rather than assumed. Worth using: whether fast mode is on depends on the account being
    /// first-party, the organization allowing it, the model being on its list, credits remaining
    /// and a separate rate limit not being in cooldown — so a pill that reported only what wtm
    /// asked for would be confidently wrong for reasons wtm cannot see.
    ///
    /// Only when asked, because a session that never wanted it has nothing to be told. Deduped on
    /// the reason, because the common refusals are standing conditions — an organization
    /// preference does not change between turns — and repeating one per `result` would be a
    /// transcript full of one sentence. Cleared when it comes on, so a refusal that returns later
    /// is reported again.
    fn fast_mode_notice(&mut self, message: &Value) -> Option<AgentEvent> {
        if !self.fast_wanted {
            return None;
        }
        let state = message.get("fast_mode_state").and_then(Value::as_str)?;
        if state == "on" {
            self.fast_refusal = None;
            return None;
        }
        let reason = message
            .get("fast_mode_disabled_reason")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        if self.fast_refusal.as_deref() == Some(reason) {
            return None;
        }
        self.fast_refusal = Some(reason.to_owned());
        Some(AgentEvent::Notice {
            level: NoticeLevel::Warn,
            message: format!("Fast mode is off — {}.", fast_mode_refusal(reason)),
        })
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
            /*
             * The one place a single request's own prompt size is reported.
             *
             * Output tokens are deliberately excluded: this is the *prompt* footprint, and an
             * assistant message that has just been generated is counted again inside the next
             * request's `cache_read` — adding it here counts it twice, on top of a `result` sum
             * that already counts every round trip. See [`ClaudeProtocol::context_used`].
             *
             * Emitted rather than only cached, so the meter moves during a long turn instead of
             * jumping when it ends.
             */
            "message_start" => {
                let usage = event.get("message").and_then(|m| m.get("usage"));
                let field = |key: &str| {
                    usage
                        .and_then(|u| u.get(key))
                        .and_then(Value::as_u64)
                        .unwrap_or_default()
                };
                let footprint = field("input_tokens")
                    .saturating_add(field("cache_read_input_tokens"))
                    .saturating_add(field("cache_creation_input_tokens"));
                if footprint == 0 {
                    return Vec::new();
                }
                self.context_used = footprint;
                vec![Step::Emit(AgentEvent::Usage(Usage {
                    tokens_in: field("input_tokens"),
                    tokens_out: field("output_tokens"),
                    cached: field("cache_read_input_tokens"),
                    context_used: footprint,
                    // Absent here: `contextWindow` is reported only on `result`. The frontend keeps
                    // the last non-null window it was told, so a mid-turn update still has a
                    // denominator after the first finished turn.
                    context_window: None,
                }))]
            }
            // Frame boundaries with nothing in them for a reader.
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
        let events = assistant_events(
            blocks,
            Prose {
                text: !streamed,
                thinking: false,
            },
        );
        for event in &events {
            if let AgentEvent::ToolStarted { id, name, .. } = event {
                self.tools.insert(id.clone(), name.clone());
            }
        }
        events.into_iter().map(Step::Emit).collect()
    }

    /// A `user` message, which on this transport means a tool result coming back.
    fn on_user(&mut self, message: &Value) -> Vec<Step> {
        let Some(blocks) = message.get("content").and_then(Value::as_array) else {
            return Vec::new();
        };

        let events = tool_results(blocks);
        for event in &events {
            if let AgentEvent::ToolFinished { id, .. } = event {
                self.tools.remove(id);
            }
        }
        events.into_iter().map(Step::Emit).collect()
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

        let approval = if tool == "AskUserQuestion" {
            ApprovalRequest::UserInput {
                questions: claude_questions(&input),
            }
        } else if tool == "ExitPlanMode" {
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
        let mut steps = std::mem::take(&mut self.history)
            .into_iter()
            .map(Step::Emit)
            .collect::<Vec<_>>();
        steps.push(Step::Ready);
        steps
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

                    // Two arrays, and they are not the same list. `slash_commands` is everything
                    // dispatchable by name — built-ins, bundled skills, `.claude/commands/` files
                    // and the user's own skills — while `skills` is the user-invocable skills
                    // alone. Both are `string[]`; neither carries a description.
                    //
                    // Reading the second is what lets the composer tell a repository's own skill
                    // from a built-in, which is the whole ordering rule in `commandsFor`: with ~110
                    // built-ins in the catalogue, a menu that cannot tell them apart buries the
                    // fifty-five somebody actually typed `/` to reach.
                    let own = names("skills")
                        .into_iter()
                        .collect::<std::collections::BTreeSet<_>>();
                    let skills = names("slash_commands")
                        .into_iter()
                        .map(|name| AgentSkill {
                            scope: own.contains(&name).then(|| "skill".to_owned()),
                            name,
                            description: None,
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
                    let reason = message
                        .get("result")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .unwrap_or("the turn failed and the CLI gave no reason");
                    // A limit is a different event from a failure because it has a different
                    // remedy — see `AgentEvent::LimitReached`. This is the route a real exhaustion
                    // arrives by; `rate_limit_event` below is the structured one, whose
                    // limit-reached shape nobody has captured.
                    steps.push(Step::Emit(limit_or_failure(reason)));
                }

                // Before the finish for the same reason a failure is: it explains something about
                // the turn that just ran, so it reads above the row that closes it.
                if let Some(notice) = self.fast_mode_notice(&message) {
                    steps.push(Step::Emit(notice));
                }

                steps.push(Step::Emit(AgentEvent::TurnFinished {
                    turn: self.turn_label(),
                    usage: Usage {
                        // Cumulative over the turn, which is what a cost row wants: these three
                        // are what was billed.
                        tokens_in: field("input_tokens"),
                        tokens_out: field("output_tokens"),
                        cached: field("cache_read_input_tokens"),
                        // *Not* cumulative, and not derived from the fields above. See
                        // [`ClaudeProtocol::context_used`] for why summing them is wrong.
                        context_used: self.context_used,
                        context_window: context_window_of(&message),
                    },
                    // Claude reports real currency, where Codex reports none. Surfaced rather than
                    // normalized away, because the number is genuinely available on one side.
                    cost_usd: message.get("total_cost_usd").and_then(Value::as_f64),
                }));
                steps
            }
            /*
             * The CLI's own rate-limit telemetry, which is mostly reassurance.
             *
             * Silent for the statuses that mean "fine", which is every payload anyone has on record
             * — this arm drew nothing at all before, and for `allowed` it still must: a warning
             * every few turns saying the limit has *not* been reached is how a session teaches its
             * reader to ignore the row that eventually matters.
             *
             * Anything else is taken at its word. Best-effort by admission: the field names below
             * are the ones the allowed payload uses, and a throttled one may spell them
             * differently, in which case the `result` arm above still catches the real exhaustion.
             */
            "rate_limit_event" => {
                let info = message.get("rate_limit_info").unwrap_or(&Value::Null);
                let status = info
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("allowed");
                if matches!(status, "allowed" | "allowed_warning") {
                    return Vec::new();
                }
                vec![Step::Emit(AgentEvent::LimitReached {
                    message: info
                        .get("message")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .unwrap_or("Claude has reached its usage limit.")
                        .to_owned(),
                    resets_at: info
                        .get("resetsAt")
                        .or_else(|| info.get("resets_at"))
                        .and_then(Value::as_u64),
                })]
            }
            "error" => vec![Step::Emit(limit_or_failure(
                message
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("the CLI reported an error"),
            ))],
            other => vec![Step::Emit(AgentEvent::Raw {
                provider: ID.to_owned(),
                event: other.to_owned(),
                payload: message.clone(),
            })],
        }
    }

    fn send_turn(&mut self, text: &str, attachments: &[AgentAttachment]) -> Vec<Step> {
        self.turn += 1;
        let mut content = Vec::new();
        if !text.is_empty() {
            content.push(json!({ "type": "text", "text": text }));
        }
        for attachment in attachments {
            if matches!(
                attachment.mime.as_str(),
                "image/jpeg" | "image/png" | "image/gif" | "image/webp"
            ) {
                content.push(json!({
                    "type": "image",
                    "source": {
                        "type": "base64",
                        "media_type": attachment.mime,
                        "data": attachment.data_base64,
                    }
                }));
            } else {
                content.push(json!({
                    "type": "text",
                    "text": format!("Attached file `{}` is available at `{}`.", attachment.name, attachment.path),
                }));
            }
        }

        let message_content = if attachments.is_empty() {
            json!(text)
        } else {
            Value::Array(content)
        };
        let mut steps = if attachments.is_empty() {
            Vec::new()
        } else {
            vec![Step::Emit(AgentEvent::Attachments {
                attachments: attachments.to_vec(),
            })]
        };
        steps.extend([
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
                    "message": { "role": "user", "content": message_content },
                    "parent_tool_use_id": null,
                })
                .to_string(),
            ),
        ]);
        steps
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
            ApprovalAnswer::UserInput { answers, notes } => {
                let mut updated = pending.input.clone();
                updated["answers"] = claude_answer_map(&pending.input, answers, notes.as_deref());
                json!({
                    "behavior": "allow",
                    "updatedInput": updated,
                })
            }
        };

        vec![
            Self::control_response(&pending.request_id, &response),
            Step::Emit(AgentEvent::ApprovalResolved { id: id.to_owned() }),
        ]
    }

    /// Change the model, the permission mode or fast mode without restarting.
    ///
    /// Three more control requests on the channel `interrupt` already uses. Every subtype is read
    /// off the shipped CLI rather than out of documentation — like `--permission-prompt-tool` in
    /// the module header, they are real and unpublished — so the failure mode matters: a subtype
    /// this CLI version does not know comes back as a `control_response` with `subtype: "error"`,
    /// which [`Self::on_line`] already turns into a `Notice`. A rejected change therefore says so
    /// in the transcript instead of leaving the picker quietly lying about the session's state.
    ///
    /// `apply_flag_settings` is the one whose refusals are *expected* rather than exceptional, and
    /// it does not report them here: fast mode also depends on the account being first-party, the
    /// model being on the organization's allow-list, credits being available and a rate limit not
    /// being in cooldown, none of which this process can know in advance. The CLI answers with the
    /// truth on every turn instead — see [`Self::fast_mode_notice`].
    fn reconfigure(
        &mut self,
        model: Option<&str>,
        _effort: Option<&str>,
        mode: Option<&str>,
        fast: Option<bool>,
    ) -> Vec<Step> {
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
        if let Some(fast) = fast {
            // Recorded before the write, so the reporting in `fast_mode_notice` describes what the
            // user last asked for even if the CLI never gets round to answering.
            self.fast_wanted = fast;
            self.fast_refusal = None;
            let mut settings = serde_json::Map::new();
            settings.insert(FAST_MODE_KEY.to_owned(), Value::Bool(fast));
            steps.push(Self::control_request(&json!({
                "subtype": "apply_flag_settings",
                "settings": Value::Object(settings),
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

/// `reason` as a limit if it names one, otherwise as an ordinary failure.
///
/// One helper for both of the arms that produce a failure, so the two cannot drift into disagreeing
/// about what counts — which would present as "the same exhaustion offers a hand-off on Tuesday and
/// not on Wednesday", depending on which of the CLI's two error shapes it arrived in.
fn limit_or_failure(reason: &str) -> AgentEvent {
    match crate::limits::classify(reason) {
        Some(signal) => AgentEvent::LimitReached {
            message: reason.to_owned(),
            resets_at: signal.resets_at,
        },
        None => AgentEvent::Failed {
            message: reason.to_owned(),
        },
    }
}

/// The main model's context window, from `result.modelUsage.<model>.contextWindow`.
///
/// # Why the busiest entry rather than the first
///
/// Nested under the model name, which is not known in advance, and there is routinely more than
/// one: the CLI bills a small background model for titles and side work in the same turn. Taking
/// "the first" was doubly wrong — `serde_json`'s `Map` is a `BTreeMap` here (no `preserve_order`
/// feature in this tree), so iteration is *alphabetical by model id*, and `claude-haiku-…` sorts
/// ahead of both `claude-opus-…` and `claude-sonnet-…`. A million-token session was being measured
/// against the background model's 200k window and read five times too high.
///
/// The model that read the most input is the one whose window the conversation actually has to fit
/// in, so that is the entry to trust.
fn context_window_of(result: &Value) -> Option<u64> {
    result
        .get("modelUsage")?
        .as_object()?
        .values()
        .filter_map(|m| {
            let window = m.get("contextWindow").and_then(Value::as_u64)?;
            let input = m
                .get("inputTokens")
                .and_then(Value::as_u64)
                .unwrap_or_default();
            Some((input, window))
        })
        .max_by_key(|&(input, _)| input)
        .map(|(_, window)| window)
}

fn claude_questions(input: &Value) -> Vec<UserInputQuestion> {
    input
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
            UserInputQuestion {
                id: question
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                    .map_or_else(|| format!("question-{index}"), str::to_owned),
                header: string("header"),
                question: string("question"),
                options: question
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
                    .collect(),
                multiple: question
                    .get("multiSelect")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                // Claude Code always offers an Other row and a notes field.
                allows_other: true,
                secret: question
                    .get("isSecret")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            }
        })
        .collect()
}

fn claude_answer_map(
    input: &Value,
    answers: &BTreeMap<String, Vec<String>>,
    notes: Option<&str>,
) -> Value {
    let mut out = serde_json::Map::new();
    let questions = input
        .get("questions")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();

    for (index, question) in questions.iter().enumerate() {
        let id = question
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map_or_else(|| format!("question-{index}"), str::to_owned);
        let prompt = question
            .get("question")
            .and_then(Value::as_str)
            .filter(|prompt| !prompt.is_empty())
            .unwrap_or(&id);
        let mut value = answers.get(&id).cloned().unwrap_or_default().join(", ");
        if index + 1 == questions.len()
            && let Some(notes) = notes.map(str::trim).filter(|notes| !notes.is_empty())
        {
            if !value.is_empty() {
                value.push_str(". ");
            }
            value.push_str("Notes: ");
            value.push_str(notes);
        }
        out.insert(prompt.to_owned(), Value::String(value));
    }
    Value::Object(out)
}

#[cfg(test)]
mod history_tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn a_resumed_transcript_replays_tools_and_thinking_the_way_a_live_one_shows_them() {
        // The whole point of the replay: before this, everything but the two prose rows was
        // dropped, so the same conversation looked completely different after a restart.
        let rows = concat!(
            "{\"type\":\"user\",\"message\":{\"content\":\"Fix the parser\"}}\n",
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"thinking\",\"thinking\":\"weighing it\"},{\"type\":\"text\",\"text\":\"Reading it now.\"},{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"Read\",\"input\":{\"file_path\":\"src/lib.rs\"}}]}}\n",
            "{\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"tool_result\",\"tool_use_id\":\"t1\",\"content\":\"fn main() {}\"}]}}\n",
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Done.\"}]}}\n"
        );
        let events = claude_history_from(Cursor::new(rows));
        assert_eq!(
            events,
            vec![
                AgentEvent::UserEcho {
                    text: "Fix the parser".to_owned()
                },
                AgentEvent::ReasoningDelta {
                    text: "weighing it".to_owned()
                },
                AgentEvent::Message {
                    text: "Reading it now.".to_owned()
                },
                AgentEvent::ToolStarted {
                    id: "t1".to_owned(),
                    name: "Read".to_owned(),
                    title: tool_title("Read", Some(&json!({"file_path": "src/lib.rs"}))),
                },
                AgentEvent::ToolFinished {
                    id: "t1".to_owned(),
                    ok: true,
                    output: Some("fn main() {}".to_owned()),
                },
                AgentEvent::Message {
                    text: "Done.".to_owned()
                },
            ]
        );
    }

    #[test]
    fn a_subagents_own_conversation_stays_out_of_the_transcript_that_spawned_it() {
        let rows = "{\"type\":\"assistant\",\"isSidechain\":true,\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"aside\"}]}}\n";
        assert_eq!(claude_history_from(Cursor::new(rows)), Vec::new());
    }

    #[test]
    fn rows_the_harness_injected_are_not_shown_as_things_the_user_said() {
        // Every one of these was rendering as a right-aligned user bubble, which is the transcript
        // claiming somebody typed a task notification. Each shape is copied from a real session.
        let rows = concat!(
            "{\"type\":\"user\",\"isMeta\":true,\"message\":{\"content\":\"<local-command-caveat>Caveat: The messages below were generated by the user while running local commands.</local-command-caveat>\"}}\n",
            "{\"type\":\"user\",\"isMeta\":true,\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"Base directory for this skill: /repo/.claude/skills/review\"}]}}\n",
            "{\"type\":\"user\",\"message\":{\"content\":\"<local-command-stdout>Set model to opus</local-command-stdout>\"}}\n",
            "{\"type\":\"user\",\"message\":{\"content\":\"<task-notification>\\n<task-id>abc</task-id>\\n</task-notification>\"}}\n",
            "{\"type\":\"user\",\"message\":{\"content\":\"<system-reminder>Plan mode is active.</system-reminder>\"}}\n",
            "{\"type\":\"user\",\"message\":{\"content\":\"Can you finish the commit\"}}\n"
        );
        assert_eq!(
            claude_history_from(Cursor::new(rows)),
            vec![AgentEvent::UserEcho {
                text: "Can you finish the commit".to_owned()
            }]
        );
    }

    #[test]
    fn a_slash_command_comes_back_as_a_marker_rather_than_a_message() {
        // Not a message, but not nothing either: it is why the model changed halfway down the
        // conversation. Both tag orders appear on the wire, so both are covered.
        let rows = concat!(
            "{\"type\":\"user\",\"message\":{\"content\":\"<command-name>/model</command-name>\\n<command-message>model</command-message>\\n<command-args>opus</command-args>\"}}\n",
            "{\"type\":\"user\",\"message\":{\"content\":\"<command-message>team-review</command-message>\\n<command-name>/team-review</command-name>\\n<command-args></command-args>\"}}\n"
        );
        assert_eq!(
            claude_history_from(Cursor::new(rows)),
            vec![
                AgentEvent::Notice {
                    level: NoticeLevel::Info,
                    message: "/model opus".to_owned()
                },
                AgentEvent::Notice {
                    level: NoticeLevel::Info,
                    message: "/team-review".to_owned()
                },
            ]
        );
    }

    #[test]
    fn prose_that_merely_mentions_an_injected_tag_is_still_something_the_user_said() {
        // The trap the tag list is anchored for, and the same one `limits.rs` guards against: a
        // `contains` check here would silently delete a message about wtm's own transcript
        // handling, which is a conversation people have in this repository.
        let rows = concat!(
            "{\"type\":\"user\",\"message\":{\"content\":\"why does <task-notification> render as mine?\"}}\n",
            "{\"type\":\"user\",\"message\":{\"content\":\"the <system-reminder> block should be dropped\"}}\n"
        );
        assert_eq!(
            claude_history_from(Cursor::new(rows)),
            vec![
                AgentEvent::UserEcho {
                    text: "why does <task-notification> render as mine?".to_owned()
                },
                AgentEvent::UserEcho {
                    text: "the <system-reminder> block should be dropped".to_owned()
                },
            ]
        );
    }

    #[test]
    fn one_unreadable_line_does_not_truncate_every_row_after_it() {
        // `map_while(Result::ok)` used to end the iterator here, so a single stray byte presented
        // as a conversation that stopped halfway.
        let rows = concat!(
            "{\"type\":\"user\",\"message\":{\"content\":\"first\"}}\n",
            "not json at all\n",
            "{\"type\":\"user\",\"message\":{\"content\":\"second\"}}\n"
        );
        assert_eq!(
            claude_history_from(Cursor::new(rows)),
            vec![
                AgentEvent::UserEcho {
                    text: "first".to_owned()
                },
                AgentEvent::UserEcho {
                    text: "second".to_owned()
                },
            ]
        );
    }

    #[test]
    fn a_transcript_past_the_replay_ceiling_keeps_its_tail_and_says_what_it_dropped() {
        let row = "{\"type\":\"user\",\"message\":{\"content\":\"x\"}}\n";
        let rows = row.repeat(MAX_HISTORY_EVENTS + 5);
        let events = claude_history_from(Cursor::new(rows));

        assert_eq!(events.len(), MAX_HISTORY_EVENTS + 1);
        assert!(matches!(
            events.first(),
            Some(AgentEvent::Notice {
                level: NoticeLevel::Info,
                ..
            })
        ));
        assert!(matches!(events.last(), Some(AgentEvent::UserEcho { .. })));
    }
}
