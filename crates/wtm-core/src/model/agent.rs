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
    /// Best available estimate of how many tokens currently occupy the model's context.
    ///
    /// This is deliberately separate from `tokens_in`: Codex reports `totalTokens` directly,
    /// while Claude's effective input is split across fresh, cache-read and cache-created tokens.
    /// The UI needs one numerator for the context-window meter and should not have to know which
    /// provider produced it.
    pub context_used: u64,
    pub context_window: Option<u64>,
}

/// One selectable answer in a provider-initiated question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputOption {
    pub label: String,
    pub description: Option<String>,
}

/// One question in a provider-initiated request for user input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputQuestion {
    /// Stable within the request and used as the key in the answer map.
    pub id: String,
    pub header: String,
    pub question: String,
    pub options: Vec<UserInputOption>,
    pub multiple: bool,
    pub allows_other: bool,
    pub secret: bool,
}

/// A file attached to a user turn.
///
/// `data_base64` is kept because Claude's streaming input consumes inline image bytes, while
/// `path` is kept because Codex app-server consumes local images by path. Normalizing both here
/// lets the frontend use one attachment model without forcing either provider through the other's
/// less reliable route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentAttachment {
    pub name: String,
    pub path: String,
    pub mime: String,
    pub size: u64,
    pub data_base64: String,
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
    /// The agent is asking one or more structured questions.
    ///
    /// This is not a permission request. It shares the pinned-card transport because both pause a
    /// turn for a human response, but the UI renders radio buttons, checkboxes and notes rather
    /// than Allow/Deny controls.
    UserInput { questions: Vec<UserInputQuestion> },
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
    /// Answers to a structured user-input request, keyed by [`UserInputQuestion::id`].
    UserInput {
        answers: BTreeMap<String, Vec<String>>,
        notes: Option<String>,
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
        /// The permission or approval mode the provider says it resolved to.
        ///
        /// Load-bearing for Claude, where wtm deliberately passes no `--permission-mode` so as not
        /// to override `~/.claude/settings.json` — see `ProviderEntry::default_mode`. Without this
        /// the UI would have to *guess* which mode a session is in, and the guess would be wrong
        /// for exactly the users who cared enough to configure one.
        mode: Option<String>,
        /// Tool names the provider says it has. For display only — wtm does not gate on it.
        tools: Vec<String>,
    },
    /// What the session can be asked to do by name, for the composer's `/` list.
    ///
    /// Its own event rather than a field on [`SessionReady`](AgentEvent::SessionReady) because
    /// the two providers learn it at different times. Claude puts the whole list on its `init`
    /// line, so it could have ridden along; Codex has to be *asked*, and its answer comes back
    /// several frames after the thread is already open and usable. A field would have meant
    /// either delaying readiness behind a list nobody is blocked on, or one provider filling it
    /// and the other always sending an empty vector.
    ///
    /// Replaces rather than appends: a provider that answers twice is correcting itself.
    SkillsListed {
        skills: Vec<AgentSkill>,
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
    /// Files sent with the following [`UserEcho`](AgentEvent::UserEcho).
    Attachments {
        attachments: Vec<AgentAttachment>,
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
    /// This session cannot continue because a usage or rate limit is exhausted.
    ///
    /// # Why this is not a `Failed`
    ///
    /// Because the two have different remedies, and the remedy is what the UI has to offer. A
    /// failure is news: read it, fix it, try again. A limit is a fork in the road — the work is
    /// unblocked by continuing it *somewhere else*, and wtm is in the unusual position of having
    /// the other provider already installed and a pane free to put it in. Offering "continue on
    /// Codex" against an ordinary error would be nonsense, so the distinction has to survive as
    /// far as the frontend rather than being a string match at the far end.
    ///
    /// Both providers report this by more than one route and none of them is documented, which is
    /// why detection is best-effort and lives in the provider modules. See `wtm_agent::limits`.
    #[serde(rename_all = "camelCase")]
    LimitReached {
        /// The provider's own sentence, shown verbatim. It names the plan and the window, which no
        /// wording invented here could.
        message: String,
        /// Unix seconds at which the provider says the limit lifts, when it says at all.
        ///
        /// Absolute rather than a duration because the frontend renders a clock time and must not
        /// hold a countdown: timers and polling are banned there, and a duration would go stale
        /// the moment the window lost focus.
        resets_at: Option<u64>,
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
    /// The permission mode this model's semantics assume, in the provider's own spelling.
    ///
    /// Set only where a model is *defined by* a mode — Claude's `opusplan` is Opus only while
    /// the session is in plan mode and Sonnet everywhere else, so offering it without the mode
    /// silently offers Sonnet. A seed the pickers apply on selection, never a lock: an explicit
    /// mode choice always wins.
    pub implied_mode: Option<String>,
    pub default_effort: Option<Effort>,
    pub efforts: Vec<EffortOption>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffortOption {
    pub effort: Effort,
    pub description: Option<String>,
}

/// One thing a session can be asked to do by name.
///
/// A Claude slash command and a Codex skill are the same affordance under two names, so they
/// share a type. `description` is `None` for Claude, which reports names and nothing else — the
/// UI must therefore treat a missing description as ordinary rather than as an error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSkill {
    pub name: String,
    pub description: Option<String>,
    /// Where it came from, in the provider's own words — Codex says `user`, `repo`, `system`
    /// or `admin`. `None` where the provider does not say.
    pub scope: Option<String>,
}

/// How much a permission mode lets a session do without asking.
///
/// Three tiers rather than a boolean because the middle one is real: `acceptEdits` writes files
/// without asking but still gates commands, which is neither "asks about everything" nor "does
/// anything at all". The UI colours the mode control from this, so the tiers have to be the
/// provider-independent question — *how surprised could I be by what this does* — rather than a
/// mirror of either CLI's vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModeRisk {
    /// Asks before anything that changes the world.
    Normal,
    /// Acts without asking, inside a sandbox or a narrowed set of actions.
    Elevated,
    /// Acts without asking and without a sandbox.
    Unsandboxed,
}

/// One permission or approval mode a provider offers.
///
/// Structured rather than the bare `Vec<String>` this replaced, for two reasons. `bypassPermissions`
/// is a wire value, not a label, and capitalising it in Svelte would be this app inventing display
/// names for another program's settings — the same argument [`AgentModel`] already makes. And the
/// risk tier has to be decided where the mode's meaning is known: a `name.includes('bypass')` test
/// in the frontend would silently rate Codex's `danger-full-access` as safe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentMode {
    /// The provider's own spelling. This is what goes on the wire, unchanged.
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    pub is_default: bool,
    pub risk: ModeRisk,
}

/// What a provider can do on this machine, as answered at runtime where possible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCapability {
    pub models: Vec<AgentModel>,
    /// Permission or approval modes this provider accepts.
    pub modes: Vec<AgentMode>,
    /// True when [`Self::models`] came from asking the CLI rather than from a compiled table.
    ///
    /// Surfaced so the UI can say "as reported by codex" versus "as of this wtm build", which
    /// is the difference between a stale list being the CLI's fault and being ours.
    pub models_are_live: bool,
}
