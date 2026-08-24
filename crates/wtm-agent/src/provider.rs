//! The seam a provider implements.
//!
//! # The protocol is a pure state machine
//!
//! [`Protocol`] never performs I/O. It is handed a line and returns [`Step`]s — events to show
//! and frames to write — and the caller is the only thing that touches a pipe. That is what
//! makes a provider testable by feeding it recorded lines and asserting on what comes back,
//! with no child process, no timing and no `FakePipe` needed for the mapping tests at all.
//!
//! It is also what keeps the two halves of a protocol honest. Codex answers an approval with a
//! JSON-RPC *response* correlated by id; Claude Code answers with a `control_response`
//! correlated by `request_id`. Both are "a frame to write", so both are a `Step::Write`, and
//! neither leaks its correlation scheme into the layer above.
//!
//! # Why `&mut self` rather than a state parameter
//!
//! A protocol driver is genuinely stateful — a thread id, a request counter, a map of
//! outstanding approvals — and threading that through as an explicit bag was tried first. It
//! made every method signature carry a type only one implementation could use. Owning the state
//! is what a per-session object is for; the caller holds it behind the session mutex, which it
//! needs anyway.

use std::collections::BTreeMap;

use wtm_core::model::{AgentAttachment, AgentEvent, ApprovalAnswer, Effort};

/// Which provider. A newtype over a string rather than an enum, because the set is a compiled
/// catalogue that grows, and an enum would put every provider's name in `wtm-core`'s vocabulary
/// for no benefit — nothing in the domain branches on it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProviderId(String);

impl ProviderId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ProviderId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Everything a provider needs to start a session.
///
/// Resolved before it gets here: templates rendered, defaults applied, the worktree path
/// absolute. A provider reads this and never consults config itself, which is what keeps
/// `wtm-config` out of this crate's dependency list.
#[derive(Debug, Clone, Default)]
pub struct SessionRequest {
    /// The worktree the session works in. Becomes `cwd`, and both CLIs honour it.
    pub cwd: String,
    pub model: Option<String>,
    pub effort: Option<Effort>,
    /// The provider's own spelling of a permission or approval mode.
    pub mode: Option<String>,
    /// Appended to the argv the catalogue builds. From `[agent.<id>].extra_args`.
    pub extra_args: Vec<String>,
    /// A session to resume rather than start. The provider's own id.
    pub resume: Option<String>,
    /// A conversation to fork into a separate side session. The provider's own id.
    ///
    /// Distinct from `resume`: a resume continues the named transcript, while a fork may read it
    /// but must never append the side question or its answer back to the parent.
    pub fork: Option<String>,
    /// Whether the fork is temporary UI rather than a conversation the user can resume later.
    pub ephemeral: bool,
    /// MCP servers to hand the CLI, rendered but **not** serialized.
    ///
    /// This used to be a `String` of pre-baked JSON, and the shape was wrong in a way that showed
    /// up as a missing feature rather than a bug: only Claude accepts `--mcp-config`, so `codex.rs`
    /// ignored the field entirely and a repository declaring servers silently got none of them on
    /// one of its two providers. Serializing per provider is what this crate is *for* — the argv
    /// that starts a CLI in the right mode is exactly the thing a provider module owns.
    ///
    /// Keyed, and ordered, because the key is the name the model sees in a tool call
    /// (`mcp__codex__…`) and a set of servers that reordered between launches would produce a
    /// different `-c` argv for an identical config, which is noise in a trust prompt.
    pub mcp: BTreeMap<String, McpServer>,
    /// Guidance appended to the session's system prompt, on top of whatever it already had.
    ///
    /// **Appended, never replacing.** Both CLIs also offer a way to substitute the base prompt
    /// outright — Claude's `--system-prompt`, Codex's `baseInstructions` — and using either would
    /// throw away the instructions that make the CLI work at all, along with the user's own
    /// `CLAUDE.md` or `AGENTS.md`. wtm has one small thing to say and no business saying anything
    /// else.
    ///
    /// What it is for: telling a session a fact about its *environment* that it cannot otherwise
    /// know. A CLI has no idea it is running inside a window where a live pane is the point, so
    /// asked to involve another agent it will reach for whichever skill or shell command matches
    /// the request — which works, and is invisible to the person watching.
    pub instructions: Option<String>,
}

/// One MCP server, with its templates already rendered and its guards already checked.
///
/// Distinct from `wtm_core::model::McpServerSpec`, which is the *declared* form: that one holds
/// template strings a repository wrote, this one holds the argv that will actually be run. Keeping
/// them as two types is what makes "rendered, then guarded, then handed over" a shape the compiler
/// checks rather than a convention — a provider cannot accidentally be given an unrendered spec.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpServer {
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
}

/// One thing the caller should do as a result of feeding a line in.
///
/// Deliberately not a single `(Vec<AgentEvent>, Vec<String>)` return: order matters between the
/// two. A provider that emits `SessionReady` and *then* writes the first turn is different from
/// one that writes first, and a pair of vectors cannot express which came first.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// Show this in the transcript.
    Emit(AgentEvent),
    /// Write this frame to the child's stdin. The newline is the host's business.
    Write(String),
    /// The handshake finished; the session may now be sent turns.
    ///
    /// Its own step rather than being inferred from a `SessionReady` event, because the two are
    /// not the same fact: Codex reports `initialize` and `thread/start` as separate round trips
    /// and is only ready after the second.
    Ready,
}

/// A per-session protocol driver.
pub trait Protocol: Send {
    /// Frames to write immediately after the child is spawned.
    ///
    /// Returns `Step`s rather than bare strings so a provider that needs no handshake can emit
    /// `Ready` here and be done.
    fn open(&mut self) -> Vec<Step>;

    /// Interpret one line of the child's stdout.
    ///
    /// Must never return an error. A line this provider does not understand becomes
    /// [`AgentEvent::Raw`], because the alternative — failing the session on an unrecognised
    /// event — turns a CLI upgrade into a blank pane. See the `agent` model's docs.
    fn on_line(&mut self, line: &str) -> Vec<Step>;

    /// The user submitted a turn.
    fn send_turn(&mut self, text: &str, attachments: &[AgentAttachment]) -> Vec<Step>;

    /// The user changed the model, effort or mode on a session that is already running.
    ///
    /// `None` means "leave that one alone", so the caller can change either without knowing the
    /// other's current value.
    ///
    /// Codex can apply all three on its next turn and Cursor uses ACP's live configuration methods.
    /// Claude can apply model and mode but must restart for effort; its implementation deliberately
    /// ignores that argument and the UI marks it as pending instead of pretending it was applied.
    ///
    /// Default: nothing. A provider that cannot change mid-session is not obliged to pretend.
    fn reconfigure(
        &mut self,
        _model: Option<&str>,
        _effort: Option<&str>,
        _mode: Option<&str>,
    ) -> Vec<Step> {
        Vec::new()
    }

    /// The user answered an outstanding approval.
    fn answer(&mut self, id: &str, answer: &ApprovalAnswer) -> Vec<Step>;

    /// The user asked to stop the current turn.
    fn interrupt(&mut self) -> Vec<Step>;

    /// Decline everything still awaiting an answer.
    ///
    /// Called when a session is closing. A provider that has an outstanding server-initiated
    /// request and is never answered leaves its CLI blocked on a reply that will not come — which
    /// on close is a child that ignores its stdin closing and has to be killed, and on quit is a
    /// process holding a model connection open waiting for a window that is gone.
    ///
    /// Declining rather than accepting, because the alternative is running a command nobody
    /// approved on the way out of the door.
    fn abandon(&mut self) -> Vec<Step>;
}

/// A provider: its identity, its argv, and a factory for per-session drivers.
pub trait Provider: Send + Sync {
    fn id(&self) -> ProviderId;

    /// The program this provider runs. Compiled in, never config — see the crate docs.
    fn program(&self) -> &'static str;

    /// The full argv for a session, including the program at index 0.
    fn argv(&self, req: &SessionRequest) -> Vec<String>;

    /// Build the driver for one session.
    fn protocol(&self, req: &SessionRequest) -> Box<dyn Protocol>;

    /// Skills this CLI would find on disk for `req.cwd`, before it has said anything itself.
    ///
    /// A **seed**, not an answer: whatever the session later reports is merged over it. It exists
    /// because Claude Code emits nothing until it receives a turn, so a fresh pane's `/` menu had
    /// no list at all — see [`crate::skills`] for why duplicating another program's discovery rules
    /// is acceptable at that strength and would not be at any other.
    ///
    /// Defaulted to nothing, so a provider that reports its own promptly needs no opinion here.
    fn seed_skills(&self, req: &SessionRequest) -> Vec<wtm_core::model::AgentSkill> {
        let _ = req;
        Vec::new()
    }
}
