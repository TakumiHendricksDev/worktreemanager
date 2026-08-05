//! What an agent session tells us, normalized across providers.
//!
//! # Why there is one enum and not one per provider
//!
//! Two CLIs speak two protocols, and the frontend should render one transcript. Mapping both
//! onto a single event type is what keeps `TranscriptItem.svelte` from growing a
//! `switch (provider)` at every branch — and what makes a third provider a new module in
//! `wtm-agent` rather than a new column in the UI.
//!
//! # Why [`AgentEvent::Raw`] is the design and not a fallback
//!
//! Both protocols are explicitly experimental. `codex app-server` is labelled so on its own
//! `--help`, and Claude Code's control envelopes are not in its public documentation at all.
//! Both will grow event kinds inside a patch release.
//!
//! An exhaustive match over a moving protocol has two failure modes and both are bad: match
//! strictly and an unknown kind breaks the transcript on the day the user upgrades their CLI;
//! drop the unknown and information disappears with nothing to indicate it ever arrived. So an
//! unrecognised event becomes `Raw`, the UI renders it as a collapsed row naming its provider
//! and kind, and a CLI upgrade degrades to *slightly noisier* rather than *broken*.
//!
//! That is the mechanism behind the claim that adding to this feature does not break what
//! exists. It is worth defending: a reviewer who "tidies up" `Raw` into a `_ => return None`
//! has removed the only thing standing between a CLI release and a blank pane.
//!
//! # What is deliberately not here
//!
//! No cost in dollars. Claude reports `total_cost_usd`; Codex reports tokens and no currency at
//! all. A `cost_usd: Option<f64>` that is always `None` for one provider is honest, which is why
//! [`TurnFinished`](AgentEvent::TurnFinished) has one — but pricing Codex's tokens ourselves to
//! fill it in would put a number on screen that goes stale silently.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// How hard a provider was asked to think.
///
/// A plain string, not an enum, and that is load-bearing. Codex's own schema calls this "a
/// non-empty reasoning effort value advertised by the model" and the ladder genuinely differs
/// between models of the same provider — `gpt-5.6-sol` offers `ultra`, `gpt-5.5` stops at
/// `xhigh`. An enum here would either be wrong for some model or would have to be widened on
/// every provider release, and neither is a thing the domain should own.
pub type Effort = String;

/// One step of a provider's live plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgendaStep {
    pub text: String,
    pub status: AgendaStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgendaStatus {
    Pending,
    InProgress,
    Completed,
}

/// Token accounting for a turn.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub tokens_in: u64,
    pub tokens_out: u64,
    /// Cache reads, where the provider reports them. Shown because on a long session it is
    /// most of the input and its absence makes the totals look alarming.
    pub cached: u64,
    pub context_window: Option<u64>,
}

/// Something the session needs a human to decide before it can continue.
///
/// Separate shapes rather than one `{ title, body }`, because a shell command and a diff want
/// genuinely different cards and flattening them would produce an unreadable blob for both.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ApprovalRequest {
    /// Run this command in the worktree.
    Command {
        command: String,
        cwd: Option<String>,
        /// The provider's own justification, when it gives one.
        reason: Option<String>,
    },
    /// Apply this patch.
    FileChange {
        /// Unified diff, rendered as one. Not parsed here: a diff viewer is a frontend
        /// concern and the domain has no stake in hunk boundaries.
        unified_diff: String,
        reason: Option<String>,
    },
    /// Grant a capability for the rest of the session.
    Permissions {
        summary: String,
        /// The individual grants being asked for, so the card can list them.
        items: Vec<String>,
    },
    /// A plan is ready and the session is waiting to leave planning mode.
    ///
    /// Its own variant rather than a `ToolInput` because it is the one approval whose *body* is
    /// a document the user may want to keep — see `Brief` in the plan store.
    PlanReview {
        markdown: String,
        /// Where the provider wrote it, when it wrote it anywhere.
        path: Option<String>,
    },
    /// A tool wants a value from the user.
    ToolInput { tool: String, prompt: String },
}

/// What the user answered.
///
/// `AllowWithEdits` exists because Claude Code's `allow` can carry an `updatedInput`, letting a
/// GUI rewrite a tool call before it runs. Codex has no equivalent verb, so a provider adapter
/// that receives this must reject it rather than silently running the original — and the UI must
/// not offer the affordance where it cannot be honoured. A union that quietly degraded would be
/// the worst of the three options.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ApprovalAnswer {
    Allow,
    /// Allow, and do not ask again for the rest of this session.
    AllowForSession,
    /// Allow, with the request's payload replaced. Claude Code only.
    AllowWithEdits {
        input: serde_json::Value,
    },
    Deny {
        message: Option<String>,
    },
}

/// One thing that happened in an agent session.
///
/// `#[serde(tag = "kind")]` and `camelCase` fields, matching `ProgressEvent` and every other
/// type that crosses the IPC boundary, so the frontend switches on `kind` and TypeScript can
/// mirror it as a discriminated union.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentEvent {
    /// The handshake completed. Until this arrives the session cannot be sent a turn.
    ///
    /// `provider_session_id` is the provider's own id, which is not always ours: Claude Code
    /// accepts a `--session-id` we choose, while Codex assigns a `UUIDv7` we have to store. Both
    /// report it here so resume has one place to read it from.
    #[serde(rename_all = "camelCase")]
    SessionReady {
        provider_session_id: String,
        model: Option<String>,
        effort: Option<Effort>,
        /// Tool names the provider says it has. For display only — wtm does not gate on it.
        tools: Vec<String>,
    },
    TurnStarted {
        turn: String,
    },
    #[serde(rename_all = "camelCase")]
    TurnFinished {
        turn: String,
        usage: Usage,
        /// `None` where the provider reports no currency. See the module docs.
        cost_usd: Option<f64>,
    },
    /// The user's own message, echoed back by the provider.
    ///
    /// Rendered rather than discarded because it is the provider's acknowledgement that the
    /// turn was received — without it, a slow first token is indistinguishable from a dropped
    /// message.
    UserEcho {
        text: String,
    },
    MessageDelta {
        text: String,
    },
    /// A complete assistant message, for providers that only report whole ones.
    Message {
        text: String,
    },
    /// Thinking, where the provider streams it. Kept separate from `MessageDelta` so the UI can
    /// collapse it by default without losing it.
    ReasoningDelta {
        text: String,
    },
    #[serde(rename_all = "camelCase")]
    ToolStarted {
        id: String,
        name: String,
        /// A one-line human summary the provider supplies, when it does.
        title: Option<String>,
    },
    ToolFinished {
        id: String,
        ok: bool,
        output: Option<String>,
    },
    CommandStarted {
        id: String,
        command: String,
        cwd: Option<String>,
    },
    /// Incremental command output.
    ///
    /// Only Codex reports this. Claude Code delivers a tool result whole, so its command cards
    /// simply have no output until they finish. Chunking a finished result to fake a stream was
    /// considered and rejected: it would be a lie about latency, and the honest absence is
    /// something a user can learn.
    CommandOutput {
        id: String,
        chunk: String,
    },
    #[serde(rename_all = "camelCase")]
    CommandFinished {
        id: String,
        exit_code: Option<i32>,
    },
    /// A patch the session produced or applied.
    #[serde(rename_all = "camelCase")]
    Patch {
        id: String,
        unified_diff: String,
    },
    /// The live step list, replaced wholesale on each update.
    ///
    /// Codex only. Claude Code's plan is a single markdown document behind a blocking approval,
    /// which arrives as [`ApprovalRequest::PlanReview`] instead — one is a progress widget and
    /// the other is a gate, and rendering both with one component would feel wrong for
    /// whichever lost.
    AgendaUpdated {
        explanation: Option<String>,
        steps: Vec<AgendaStep>,
    },
    #[serde(rename_all = "camelCase")]
    ApprovalRequested {
        /// Opaque to the frontend, which hands it back verbatim. The adapter uses it to
        /// correlate the answer with the provider's own outstanding request.
        id: String,
        /// True when the turn cannot proceed until this is answered, so the UI knows to render
        /// a card that cannot be scrolled past rather than an inline chip.
        blocking: bool,
        request: ApprovalRequest,
    },
    /// An approval stopped needing an answer — because it was answered, withdrawn, or the turn
    /// was interrupted. The card collapses on this rather than waiting forever.
    ApprovalResolved {
        id: String,
    },
    Usage(Usage),
    Notice {
        level: NoticeLevel,
        message: String,
    },
    /// The session failed in a way that is worth showing in the transcript.
    ///
    /// Distinct from the session *ending*, which is reported by the host's exit rather than as
    /// an event: this is the provider telling us something went wrong while it is still alive.
    Failed {
        message: String,
    },
    /// An event this build does not recognise. See the module docs — this is deliberate.
    Raw {
        provider: String,
        /// The provider's own discriminator, so the collapsed row can name it.
        ///
        /// Named `event` rather than the more natural `kind` because `kind` is this enum's own
        /// serde tag, and a variant field of that name is a compile error rather than a subtle
        /// bug — which is the good outcome, but only once.
        event: String,
        payload: serde_json::Value,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeLevel {
    Info,
    Warn,
}

/// One model a provider offers, and the effort ladder it actually supports.
///
/// The ladder is per *model*, not per provider, because that is what the providers report:
/// `model/list` gives `gpt-5.6-sol` six efforts including `ultra` and `gpt-5.5` four. A picker
/// built on a per-provider ladder would offer rungs the selected model rejects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModel {
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    pub is_default: bool,
    pub default_effort: Option<Effort>,
    pub efforts: Vec<EffortOption>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffortOption {
    pub effort: Effort,
    pub description: Option<String>,
}

/// What a provider can do on this machine, as answered at runtime where possible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCapability {
    pub models: Vec<AgentModel>,
    /// Permission or approval modes this provider accepts, in the provider's own spelling.
    pub modes: Vec<String>,
    /// True when [`Self::models`] came from asking the CLI rather than from a compiled table.
    ///
    /// Surfaced so the UI can say "as reported by codex" versus "as of this wtm build", which
    /// is the difference between a stale list being the CLI's fault and being ours.
    pub models_are_live: bool,
    /// Provider-specific switches that are neither model nor effort — Claude's `ultracode`,
    /// which is a boolean requiring effort at or above `xhigh`, not a sixth rung.
    pub flags: BTreeMap<String, String>,
}
