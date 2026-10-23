//! Usage capacity: what a role does when its provider runs dry, how many run sessions may be open
//! at once, and what a role that moves to another provider is told.
//!
//! # A limit is the account's, so the answer is the account's too
//!
//! A role that reaches its provider's usage limit has done nothing wrong, and retrying it spends
//! nothing useful until the window resets. So a limit never spends a retry, and what follows it is
//! one of three things, in this order:
//!
//! 1. **The role's own fallback chain**, when its config names one: up to [`MAX_FALLBACKS`]
//!    alternatives, tried in order, skipping any on the provider that ran out (it would meet the
//!    same limit) and any this run already ran. See [`super::on_limit`].
//! 2. **Otherwise you are asked**, in the inbox: run this role on another provider once, keep
//!    waiting, or finish the review without it. Waiting is what happens until you answer, because
//!    switching provider changes what a review is worth and is not something to do on a role's
//!    behalf unless you said so.
//! 3. **At the reset the role runs again by itself**, but only while wtm is open and the Mac is
//!    awake. A Mac that slept past the reset, or a wtm that was not running, asks the provider again
//!    before anything runs ([`after_recheck`]), because a reset time read before a night's sleep says
//!    nothing certain about the morning.
//!
//! # Switching hands over a summary, never a session
//!
//! A conversation on one provider cannot be resumed on another, and a transcript pasted into the
//! next one would carry the first model's guesses as if they were facts. So the role that takes
//! over is given a [`Handover`]: what the flow's own records say — the step and how far it got, the
//! snapshot, the diff's shape, the findings already standing, the answers you gave — and every
//! switch is kept ([`ProviderSwitch`]), with who chose it.
//!
//! # A cap on sessions, not on runs
//!
//! [`SessionCaps`] bounds how many run sessions are open at once, across every project, in total
//! and per provider. It counts sessions rather than runs because what it protects is the
//! subscription: a review whose three roles each open a session spends the window three times as
//! fast as one session would, and a run's child sessions — a fallback, a fix's writer, a planner —
//! are as many sessions as the run's own roles. So a parent and its children draw from the same
//! places and none has an allowance of its own. A provider's native subagents (Claude's `Task`
//! fan-out inside one role) run inside their parent's session and take no place: wtm cannot see
//! them, and the role that started them already holds one. Work over a cap waits in a [`Ledger`],
//! oldest first, and says what it waits for.
//!
//! # Usage figures are the provider's, or unknown
//!
//! Nothing here estimates usage. [`limit_reading`] reads the figures a provider reported, and says
//! [`LimitReading::Unknown`] when it reported none.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::num::NonZeroU32;

use serde::{Deserialize, Serialize};

use super::UnixMs;
use super::recipe::{RoleProvider, RoleSpec, WorkflowSpec};
use super::run::StepId;
use super::state::FlowId;
use crate::model::UsageLimits;

/// The most alternatives a role's fallback chain may name.
///
/// Each alternative is a whole role on another account, usually run with nobody watching, so a
/// long chain mostly spends capacity on reviews you did not choose. Three is enough to cover each
/// provider and a second model on one of them.
pub const MAX_FALLBACKS: usize = 3;

// ═══════════════════════════════ credit billing ═══════════════════════════════

/// Why a model bills usage credits rather than drawing on a subscription's included limits.
///
/// From Claude Code's model configuration docs, "Fable and usage credits" (read 2026-10-22): in
/// non-interactive mode, which is how every run session runs, "Claude Code never asks for consent.
/// When a Fable request there would bill to usage credits, Claude Code bills it without asking."
/// The `best` alias resolves to Fable wherever Fable is available, so it is held to the same rule.
/// No other model is documented to bill that way headless.
#[must_use]
pub fn bills_credits(provider: RoleProvider, model: Option<&str>) -> bool {
    let Some(model) = model.map(|m| m.trim().to_ascii_lowercase()) else {
        return false;
    };
    match provider {
        RoleProvider::Claude => model == "best" || model.contains("fable"),
        RoleProvider::Codex => false,
    }
}

/// A role that would bill usage credits without having said it may.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreditProblem {
    pub role: String,
    pub provider: RoleProvider,
    pub model: String,
}

impl std::fmt::Display for CreditProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "`{}` would run `{}` on {}, which bills usage credits in a headless run without \
             asking. Add `allow_credit_billing = true` to `[workflow.role.{}]` if that is what you \
             want",
            self.role, self.model, self.provider, self.role
        )
    }
}

/// Whether running `spec` on `provider` with `model` needs an opt-in it has not given.
#[must_use]
pub fn credit_problem(
    role: &str,
    spec: &RoleSpec,
    provider: RoleProvider,
    model: Option<&str>,
) -> Option<CreditProblem> {
    (!spec.allow_credit_billing && bills_credits(provider, model)).then(|| CreditProblem {
        role: role.to_owned(),
        provider,
        model: model.unwrap_or_default().trim().to_owned(),
    })
}

impl WorkflowSpec {
    /// Every role a recipe can end up running, its fallbacks included, that names a model billing
    /// usage credits and has not opted in. Checked before a run starts, so a run is refused whole
    /// rather than discovering it halfway, when a fallback takes over.
    #[must_use]
    pub fn credit_problems(&self, recipe: &str) -> Vec<CreditProblem> {
        let Some(spec) = self.recipe.get(recipe) else {
            return Vec::new();
        };
        self.reachable_roles(spec)
            .into_iter()
            .filter_map(|(id, role)| credit_problem(id, role, role.provider, role.model.as_deref()))
            .collect()
    }
}

// ═══════════════════════════════ caps and places ═══════════════════════════════

/// How many run sessions may be open at once: in total, and on each provider.
///
/// Your own config only: `[run_sessions]` in `~/.config/wtm/config.toml`, because the caps are
/// about your subscriptions, which no repository knows. A cap of zero is a config error rather than
/// a run that queues forever.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionCaps {
    #[serde(default = "default_total")]
    pub total: NonZeroU32,
    #[serde(default = "default_provider")]
    pub claude: NonZeroU32,
    #[serde(default = "default_provider")]
    pub codex: NonZeroU32,
}

/// Two providers' worth of [`DEFAULT_PER_PROVIDER`]: both can run a full stage at once.
pub const DEFAULT_TOTAL: u32 = 6;

/// A typical recipe stage on one provider: correctness, conventions and UI together. More than
/// that on one subscription reaches a five-hour window before a review of any size finishes.
pub const DEFAULT_PER_PROVIDER: u32 = 3;

fn default_total() -> NonZeroU32 {
    NonZeroU32::new(DEFAULT_TOTAL).unwrap_or(NonZeroU32::MIN)
}

fn default_provider() -> NonZeroU32 {
    NonZeroU32::new(DEFAULT_PER_PROVIDER).unwrap_or(NonZeroU32::MIN)
}

impl Default for SessionCaps {
    fn default() -> Self {
        Self {
            total: default_total(),
            claude: default_provider(),
            codex: default_provider(),
        }
    }
}

impl SessionCaps {
    #[must_use]
    pub fn for_provider(&self, provider: RoleProvider) -> u32 {
        match provider {
            RoleProvider::Claude => self.claude.get(),
            RoleProvider::Codex => self.codex.get(),
        }
    }

    #[must_use]
    pub fn is_default(&self) -> bool {
        self == &Self::default()
    }
}

id! {
    /// One session's place, or its request for one. Numbered by the ledger, in memory: places do
    /// not outlive the process whose sessions hold them.
    SeatId
}

/// A session that will open, asking for a place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeatRequest {
    pub flow: FlowId,
    pub step: Option<StepId>,
    pub role: String,
    pub provider: RoleProvider,
}

/// A place, held or asked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Seat {
    pub id: SeatId,
    pub flow: FlowId,
    pub step: Option<StepId>,
    pub role: String,
    pub provider: RoleProvider,
    /// When it asked. The queue's order.
    pub asked_at: UnixMs,
    /// When it got its place; `None` while it waits.
    pub admitted_at: Option<UnixMs>,
}

/// What a waiting session waits for: the cap that is full.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QueueReason {
    /// Its provider's places are all taken.
    Provider {
        provider: RoleProvider,
        open: u32,
        cap: u32,
    },
    /// Every place is taken, across providers.
    Total { open: u32, cap: u32 },
}

impl std::fmt::Display for QueueReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Provider {
                provider,
                open,
                cap,
            } => write!(f, "queued: {provider} at {open}/{cap}"),
            Self::Total { open, cap } => write!(f, "queued: {open} of {cap} run sessions open"),
        }
    }
}

/// A session waiting for a place, and where it is in the line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueuedSeat {
    pub seat: Seat,
    pub reason: QueueReason,
    /// 1 for the next to get a place among those waiting for the same cap.
    pub position: u32,
    /// [`QueueReason`]'s words, so every view says it the same way.
    pub words: String,
}

/// Who holds the places and who waits, at one moment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Seating {
    pub caps: SessionCaps,
    pub holding: Vec<Seat>,
    pub queued: Vec<QueuedSeat>,
}

impl Seating {
    /// How many places a provider's sessions hold.
    #[must_use]
    pub fn open_on(&self, provider: RoleProvider) -> u32 {
        count(self.holding.iter().filter(|s| s.provider == provider))
    }

    /// The seat a step holds or waits with, if any.
    #[must_use]
    pub fn queued_for(&self, step: StepId) -> Option<&QueuedSeat> {
        self.queued.iter().find(|q| q.seat.step == Some(step))
    }
}

fn count<'a>(seats: impl Iterator<Item = &'a Seat>) -> u32 {
    u32::try_from(seats.count()).unwrap_or(u32::MAX)
}

/// Every place, held and asked for, in the order they were asked for.
///
/// A place is admitted when the total and its provider both have room, oldest first. Oldest first
/// among those that fit, not strictly oldest first: a Claude session behind a Codex one that waits
/// for a Codex place takes a free Claude place rather than leaving it empty. That cannot starve the
/// Codex one, because it does not want the Claude place, and the total is shared by both: when the
/// total is what is full, every waiter is waiting for the same thing, and the oldest fits first.
///
/// A cap lowered below what is open evicts nothing: a session mid-turn is never ended to make room,
/// for the reason the pane caps give. The places drain as sessions close.
#[derive(Debug, Default, Clone)]
pub struct Ledger {
    seats: Vec<Seat>,
    next: i64,
}

impl Ledger {
    /// Ask for a place. The seat may be admitted at once; [`Self::is_admitted`] says.
    pub fn ask(&mut self, caps: &SessionCaps, request: SeatRequest, now: UnixMs) -> SeatId {
        self.next += 1;
        let id = SeatId(self.next);
        self.seats.push(Seat {
            id,
            flow: request.flow,
            step: request.step,
            role: request.role,
            provider: request.provider,
            asked_at: now,
            admitted_at: None,
        });
        self.admit(caps, now);
        id
    }

    /// Give up a place, or withdraw a request for one. Returns the seats admitted to the room it
    /// left, oldest first.
    pub fn release(&mut self, caps: &SessionCaps, id: SeatId, now: UnixMs) -> Vec<Seat> {
        self.seats.retain(|s| s.id != id);
        self.admit(caps, now)
    }

    /// Fill whatever room there is, oldest first among those that fit. Returns who got a place.
    pub fn admit(&mut self, caps: &SessionCaps, now: UnixMs) -> Vec<Seat> {
        let mut total = count(self.seats.iter().filter(|s| s.admitted_at.is_some()));
        let mut open: BTreeMap<RoleProvider, u32> = BTreeMap::new();
        for seat in self.seats.iter().filter(|s| s.admitted_at.is_some()) {
            *open.entry(seat.provider).or_default() += 1;
        }
        let mut admitted = Vec::new();
        for seat in self.seats.iter_mut().filter(|s| s.admitted_at.is_none()) {
            let on = open.entry(seat.provider).or_default();
            if total < caps.total.get() && *on < caps.for_provider(seat.provider) {
                seat.admitted_at = Some(now);
                total += 1;
                *on += 1;
                admitted.push(seat.clone());
            }
        }
        admitted
    }

    #[must_use]
    pub fn is_admitted(&self, id: SeatId) -> bool {
        self.seats
            .iter()
            .any(|s| s.id == id && s.admitted_at.is_some())
    }

    #[must_use]
    pub fn seat(&self, id: SeatId) -> Option<&Seat> {
        self.seats.iter().find(|s| s.id == id)
    }

    /// Who holds and who waits, with what each waiter waits for.
    #[must_use]
    pub fn seating(&self, caps: &SessionCaps) -> Seating {
        let holding: Vec<Seat> = self
            .seats
            .iter()
            .filter(|s| s.admitted_at.is_some())
            .cloned()
            .collect();
        let total = count(holding.iter());
        let mut lines: BTreeMap<Option<RoleProvider>, u32> = BTreeMap::new();
        let queued = self
            .seats
            .iter()
            .filter(|s| s.admitted_at.is_none())
            .map(|seat| {
                let open = count(holding.iter().filter(|h| h.provider == seat.provider));
                let cap = caps.for_provider(seat.provider);
                let reason = if open >= cap {
                    QueueReason::Provider {
                        provider: seat.provider,
                        open,
                        cap,
                    }
                } else {
                    QueueReason::Total {
                        open: total,
                        cap: caps.total.get(),
                    }
                };
                let line = match reason {
                    QueueReason::Provider { provider, .. } => Some(provider),
                    QueueReason::Total { .. } => None,
                };
                let position = lines.entry(line).or_default();
                *position += 1;
                QueuedSeat {
                    seat: seat.clone(),
                    words: reason.to_string(),
                    reason,
                    position: *position,
                }
            })
            .collect();
        Seating {
            caps: *caps,
            holding,
            queued,
        }
    }
}

// ═══════════════════════════════ limit readings ═══════════════════════════════

/// What a provider's own figures say about whether a role could run now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum LimitReading {
    /// Every window that applies has room.
    Available,
    /// A window that applies is spent. Its reset, in Unix seconds, when the provider said, and the
    /// latest of them when several are spent.
    Limited { resets_at: Option<u64> },
    /// The provider reported no figures that apply. Never read as either of the others.
    Unknown,
}

/// Read `limits` for a role on `model`, at `now_s` (Unix seconds).
///
/// A window applies when it is account-wide or scoped to the model the role runs (Claude reports a
/// weekly window for Fable alone). A spent window whose reset has already passed is over, so it is
/// not spent any more: the figure describes a window that has rolled.
#[must_use]
pub fn limit_reading(limits: &UsageLimits, model: Option<&str>, now_s: u64) -> LimitReading {
    let model = model.map(str::to_ascii_lowercase);
    let applies = |scope: Option<&str>| match (scope, model.as_deref()) {
        (None, _) => true,
        (Some(scope), Some(model)) => model.contains(&scope.to_ascii_lowercase()),
        (Some(_), None) => false,
    };
    let windows: Vec<_> = limits
        .windows
        .iter()
        .filter(|w| applies(w.scope.as_deref()))
        .collect();
    if windows.is_empty() {
        return LimitReading::Unknown;
    }
    let spent: Vec<_> = windows
        .iter()
        .filter(|w| w.used_percent >= 100.0 && w.resets_at.is_none_or(|at| at > now_s))
        .collect();
    if spent.is_empty() {
        return LimitReading::Available;
    }
    let resets_at = if spent.iter().all(|w| w.resets_at.is_some()) {
        spent.iter().filter_map(|w| w.resets_at).max()
    } else {
        None
    };
    LimitReading::Limited { resets_at }
}

// ═══════════════════════════════ waiting, sleeping, waking ═══════════════════════════════

/// How much further the wall clock may move than the monotonic one between two looks before the
/// Mac is taken to have slept in between.
///
/// The monotonic clock (`CLOCK_UPTIME_RAW` behind `Instant` on macOS) does not advance while the
/// Mac sleeps, and the wall clock does. A waiting role looks every fraction of a second, so a gap
/// of half a minute is sleep, not a busy scheduler or a clock being nudged by time sync.
pub const SLEEP_GAP_MS: u64 = 30_000;

/// Whether the Mac slept between two readings of `(wall, monotonic)` milliseconds.
#[must_use]
pub fn slept(before: (UnixMs, u64), now: (UnixMs, u64)) -> bool {
    let wall = now.0.saturating_sub(before.0);
    let monotonic = now.1.saturating_sub(before.1);
    wall > monotonic.saturating_add(SLEEP_GAP_MS)
}

/// What a role waiting for capacity does on one look at the clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitTurn {
    /// Nothing yet.
    Keep,
    /// The Mac slept, or wtm was not running: ask the provider before anything runs.
    Recheck,
    /// The reset the provider named has come, with wtm open and awake throughout.
    Resume,
}

/// Decide one look. `resume_at` is the reset in the clock's milliseconds, `None` when unknown.
#[must_use]
pub fn wait_turn(resume_at: Option<UnixMs>, now: UnixMs, slept: bool) -> WaitTurn {
    if slept {
        return WaitTurn::Recheck;
    }
    match resume_at {
        Some(at) if now >= at => WaitTurn::Resume,
        _ => WaitTurn::Keep,
    }
}

/// What a recheck of the provider's figures means for a waiting role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterRecheck {
    /// It has room again: run.
    Resume,
    /// Still spent: wait for this reset, in Unix seconds, or for a person when it is `None`.
    WaitUntil(Option<u64>),
    /// The provider said nothing that applies, and the reset it named before has passed. wtm cannot
    /// tell, so it does not guess: it waits for you to resume it.
    AskYou,
}

/// Decide after asking the provider again. `resets_at` is the reset the role was waiting for.
#[must_use]
pub fn after_recheck(reading: LimitReading, resets_at: Option<u64>, now_s: u64) -> AfterRecheck {
    match reading {
        LimitReading::Available => AfterRecheck::Resume,
        LimitReading::Limited { resets_at } => AfterRecheck::WaitUntil(resets_at),
        LimitReading::Unknown => match resets_at {
            Some(at) if at > now_s => AfterRecheck::WaitUntil(Some(at)),
            _ => AfterRecheck::AskYou,
        },
    }
}

/// What you chose for a role waiting at its provider's limit, from the inbox or the Reviews tab.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum CapacityChoice {
    /// Keep waiting for the reset. What happens anyway; choosing it puts the question away.
    Wait,
    /// Run the role once on this provider and model, from a handover. A rerun goes back to the
    /// provider its config names.
    RunOnce {
        provider: RoleProvider,
        #[serde(default)]
        model: Option<String>,
    },
    /// Finish the review without this role.
    Skip,
}

// ═══════════════════════════════ the handover ═══════════════════════════════

/// Why a role moved to another provider or role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwitchReason {
    /// Its provider reached a usage limit.
    Limit,
    /// It failed after its retries.
    Failure,
    /// You chose to run it once on another provider, at a limit.
    Once,
}

/// Who decided the switch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SwitchedBy {
    /// The role's own fallback chain, in your config.
    Config,
    /// You, at the moment it happened.
    You,
}

/// Who runs a role: a role id on a provider and model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Runner {
    pub role: String,
    pub provider: RoleProvider,
    pub model: Option<String>,
}

impl std::fmt::Display for Runner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.model {
            Some(model) => write!(f, "`{}` on {} {model}", self.role, self.provider),
            None => write!(f, "`{}` on {}", self.role, self.provider),
        }
    }
}

/// The snapshot the work was against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HandoverSnapshot {
    pub head: String,
    pub tree: String,
    pub base: String,
}

/// One file the change touches, as `git diff --numstat` counts it. `None` counts are a binary file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileStat {
    pub path: String,
    pub added: Option<u32>,
    pub removed: Option<u32>,
}

/// A finding that already stands, as the next role is told it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HandoverFinding {
    pub title: String,
    /// `path:line`, the file alone, or `None` for the whole change.
    pub place: Option<String>,
    /// The class as the findings schema spells it: `confirmed_bug`, `suggestion`…
    pub class: String,
    pub severity: String,
    /// The roles that reported it.
    pub by: Vec<String>,
}

/// A question you answered for the role, and your answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HandoverAnswer {
    pub question: String,
    pub answer: String,
}

/// The most files a handover lists one by one. A diff stat longer than this is the change's
/// shape, and the role has the whole diff to read.
pub const HANDOVER_FILES: usize = 40;

/// What a role taking over is told: the flow's own record, never the first session's transcript.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Handover {
    /// Who stopped, and who takes over.
    pub from: Runner,
    pub to: Runner,
    pub reason: SwitchReason,
    /// The provider's message, the failure, or what you chose, in a sentence.
    pub why: String,
    /// What the role is for, from its config.
    pub objective: String,
    /// The recipe stage it belongs to, from 0.
    pub stage: u32,
    /// Attempts the first runner made.
    pub attempts: u32,
    /// Its `flow_progress` lines, oldest first.
    pub progress: Vec<String>,
    /// The approved plan, for a run that carries one out. `None` for a review, which has none.
    pub plan: Option<String>,
    pub snapshot: Option<HandoverSnapshot>,
    /// `None` when git could not say.
    pub diff: Option<Vec<FileStat>>,
    /// Standing findings from the run's other roles, not dismissed.
    pub findings: Vec<HandoverFinding>,
    pub answers: Vec<HandoverAnswer>,
}

impl Handover {
    /// The handover as the next role's brief carries it, and as the Reviews tab shows it.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        let because = match self.reason {
            SwitchReason::Limit => "its provider reached a usage limit",
            SwitchReason::Failure => "it failed after its retries",
            SwitchReason::Once => {
                "its provider reached a usage limit, and you chose to run it here once"
            }
        };
        let _ = writeln!(
            out,
            "{} stopped before handing in a result, because {because}: {}",
            self.from,
            self.why.trim_end_matches('.')
        );
        let _ = writeln!(
            out,
            "You are {}, taking over its work. Nothing it found reached the run, so review the \
             change in full yourself.",
            self.to
        );
        let _ = writeln!(out, "\nIts objective: {}", self.objective);
        let _ = writeln!(
            out,
            "It made {} attempt{} (stage {}).",
            self.attempts,
            if self.attempts == 1 { "" } else { "s" },
            self.stage + 1
        );
        if self.progress.is_empty() {
            out.push_str("It reported no progress first.\n");
        } else {
            out.push_str("What it reported doing first:\n");
            for line in &self.progress {
                let _ = writeln!(out, "- {line}");
            }
        }
        match &self.plan {
            Some(plan) => {
                let _ = writeln!(out, "\nThe approved plan:\n{plan}");
            }
            None => out.push_str(
                "\nPlan: none. This run reviews a change; it is not carrying out a plan.\n",
            ),
        }
        if let Some(snapshot) = &self.snapshot {
            let _ = writeln!(
                out,
                "Snapshot: head {}, tree {}, measured from {} (uncommitted work included).",
                short(&snapshot.head),
                short(&snapshot.tree),
                short(&snapshot.base)
            );
        }
        match &self.diff {
            Some(files) if !files.is_empty() => {
                let added: u32 = files.iter().filter_map(|f| f.added).sum();
                let removed: u32 = files.iter().filter_map(|f| f.removed).sum();
                let _ = writeln!(
                    out,
                    "Diff stat: {} file{}, +{added} −{removed}",
                    files.len(),
                    if files.len() == 1 { "" } else { "s" }
                );
                for file in files.iter().take(HANDOVER_FILES) {
                    match (file.added, file.removed) {
                        (Some(a), Some(r)) => {
                            let _ = writeln!(out, "- {} +{a} −{r}", file.path);
                        }
                        _ => {
                            let _ = writeln!(out, "- {} (binary)", file.path);
                        }
                    }
                }
                if files.len() > HANDOVER_FILES {
                    let _ = writeln!(out, "- and {} more", files.len() - HANDOVER_FILES);
                }
            }
            Some(_) => out.push_str("Diff stat: no files changed.\n"),
            None => out.push_str("Diff stat: git could not say.\n"),
        }
        if self.findings.is_empty() {
            out.push_str("Findings standing so far: none.\n");
        } else {
            let _ = writeln!(
                out,
                "Findings the run's other roles already stand by ({}); do not report them again \
                 unless you have something to add:",
                self.findings.len()
            );
            for finding in &self.findings {
                let place = finding.place.as_deref().unwrap_or("the whole change");
                let _ = writeln!(
                    out,
                    "- [{}, {}] {} at {place} ({})",
                    finding.class,
                    finding.severity,
                    finding.title,
                    finding.by.join(", ")
                );
            }
        }
        if !self.answers.is_empty() {
            out.push_str("Questions the person answered for this role:\n");
            for answer in &self.answers {
                let _ = writeln!(out, "- {}: {}", answer.question, answer.answer);
            }
        }
        out.push_str(
            "\nThis summary is all that carries over. The first session's conversation is not \
             transferred: a session on one provider cannot be resumed on another.",
        );
        out
    }
}

fn short(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

id! {
    /// One provider switch, as the store numbers it.
    SwitchId
}

/// A switch the run made, kept: from which role and provider to which, why, who chose it, and the
/// handover the new runner was given.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSwitch {
    pub id: SwitchId,
    pub flow: FlowId,
    pub from_step: StepId,
    pub to_step: StepId,
    pub reason: SwitchReason,
    pub by: SwitchedBy,
    pub handover: Handover,
    pub at: UnixMs,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewProviderSwitch {
    pub flow: FlowId,
    pub from_step: StepId,
    pub to_step: StepId,
    pub reason: SwitchReason,
    pub by: SwitchedBy,
    pub handover: Handover,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::LimitWindow;

    fn caps(total: u32, claude: u32, codex: u32) -> SessionCaps {
        SessionCaps {
            total: NonZeroU32::new(total).unwrap(),
            claude: NonZeroU32::new(claude).unwrap(),
            codex: NonZeroU32::new(codex).unwrap(),
        }
    }

    fn ask(
        ledger: &mut Ledger,
        caps: &SessionCaps,
        flow: i64,
        role: &str,
        provider: RoleProvider,
        at: UnixMs,
    ) -> SeatId {
        ledger.ask(
            caps,
            SeatRequest {
                flow: FlowId(flow),
                step: None,
                role: role.to_owned(),
                provider,
            },
            at,
        )
    }

    #[test]
    fn a_session_over_its_providers_cap_waits_and_says_which_cap_is_full() {
        let caps = caps(6, 3, 1);
        let mut ledger = Ledger::default();
        let first = ask(&mut ledger, &caps, 1, "correctness", RoleProvider::Codex, 1);
        let second = ask(&mut ledger, &caps, 2, "correctness", RoleProvider::Codex, 2);
        assert!(ledger.is_admitted(first));
        assert!(!ledger.is_admitted(second));

        let seating = ledger.seating(&caps);
        assert_eq!(seating.queued.len(), 1);
        assert_eq!(seating.queued[0].words, "queued: Codex at 1/1");
        assert_eq!(seating.queued[0].position, 1);
    }

    #[test]
    fn freed_places_go_to_the_oldest_waiter_that_fits() {
        let caps = caps(2, 3, 3);
        let mut ledger = Ledger::default();
        let a = ask(&mut ledger, &caps, 1, "a", RoleProvider::Codex, 1);
        let _b = ask(&mut ledger, &caps, 1, "b", RoleProvider::Codex, 2);
        let c = ask(&mut ledger, &caps, 2, "c", RoleProvider::Claude, 3);
        let d = ask(&mut ledger, &caps, 3, "d", RoleProvider::Codex, 4);
        let seating = ledger.seating(&caps);
        assert_eq!(seating.queued[0].words, "queued: 2 of 2 run sessions open");
        assert_eq!(
            seating
                .queued
                .iter()
                .map(|q| (q.seat.role.as_str(), q.position))
                .collect::<Vec<_>>(),
            [("c", 1), ("d", 2)],
            "both wait for the total, in the order they asked"
        );

        let admitted = ledger.release(&caps, a, 10);
        assert_eq!(admitted.iter().map(|s| s.id).collect::<Vec<_>>(), [c]);
        assert!(!ledger.is_admitted(d), "the newer one still waits");
    }

    #[test]
    fn a_newer_waiter_takes_a_free_place_its_provider_has_while_an_older_one_waits_for_its_own() {
        let caps = caps(3, 2, 1);
        let mut ledger = Ledger::default();
        let _a = ask(&mut ledger, &caps, 1, "a", RoleProvider::Codex, 1);
        let b = ask(&mut ledger, &caps, 1, "b", RoleProvider::Codex, 2);
        let c = ask(&mut ledger, &caps, 2, "c", RoleProvider::Claude, 3);
        assert!(!ledger.is_admitted(b), "Codex is at its cap");
        assert!(
            ledger.is_admitted(c),
            "a Claude place was free and nobody older wanted it"
        );
    }

    #[test]
    fn a_lowered_cap_evicts_nobody_and_new_sessions_wait_until_the_places_drain() {
        let wide = caps(6, 3, 3);
        let narrow = caps(1, 1, 1);
        let mut ledger = Ledger::default();
        let a = ask(&mut ledger, &wide, 1, "a", RoleProvider::Claude, 1);
        let b = ask(&mut ledger, &wide, 1, "b", RoleProvider::Claude, 2);
        let c = ask(&mut ledger, &narrow, 2, "c", RoleProvider::Claude, 3);
        assert!(ledger.is_admitted(a) && ledger.is_admitted(b));
        assert!(!ledger.is_admitted(c));
        assert_eq!(
            ledger.seating(&narrow).queued[0].words,
            "queued: Claude at 2/1"
        );

        assert!(
            ledger.release(&narrow, a, 4).is_empty(),
            "one still over the cap"
        );
        assert_eq!(ledger.release(&narrow, b, 5).len(), 1);
        assert!(ledger.is_admitted(c));
    }

    #[test]
    fn a_withdrawn_request_frees_nothing_and_keeps_the_line_in_order() {
        let caps = caps(1, 1, 1);
        let mut ledger = Ledger::default();
        let a = ask(&mut ledger, &caps, 1, "a", RoleProvider::Codex, 1);
        let b = ask(&mut ledger, &caps, 2, "b", RoleProvider::Codex, 2);
        let c = ask(&mut ledger, &caps, 3, "c", RoleProvider::Codex, 3);
        assert!(ledger.release(&caps, b, 4).is_empty());
        assert_eq!(ledger.seating(&caps).queued[0].seat.id, c);
        assert_eq!(ledger.release(&caps, a, 5)[0].id, c);
    }

    #[test]
    fn caps_default_to_three_a_provider_and_six_in_all_and_refuse_zero() {
        let parsed: SessionCaps = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed, SessionCaps::default());
        assert_eq!(parsed.total.get(), 6);
        assert_eq!(parsed.for_provider(RoleProvider::Codex), 3);
        assert!(serde_json::from_str::<SessionCaps>(r#"{"codex": 0}"#).is_err());
        assert!(serde_json::from_str::<SessionCaps>(r#"{"cursor": 2}"#).is_err());
    }

    #[test]
    fn only_fable_and_the_alias_that_resolves_to_it_bill_credits_headless() {
        for model in ["fable", "Fable", "claude-fable-5-1", "best"] {
            assert!(bills_credits(RoleProvider::Claude, Some(model)), "{model}");
        }
        for model in ["opus", "sonnet", "claude-opus-5-5", "haiku"] {
            assert!(!bills_credits(RoleProvider::Claude, Some(model)), "{model}");
        }
        assert!(!bills_credits(RoleProvider::Claude, None));
        assert!(!bills_credits(RoleProvider::Codex, Some("fable")));
    }

    fn window(minutes: u64, scope: Option<&str>, used: f64, resets: Option<u64>) -> LimitWindow {
        LimitWindow {
            minutes: Some(minutes),
            scope: scope.map(str::to_owned),
            used_percent: used,
            resets_at: resets,
        }
    }

    fn limits(windows: Vec<LimitWindow>) -> UsageLimits {
        UsageLimits {
            provider: "claude".to_owned(),
            plan: None,
            windows,
            reset_credits: None,
        }
    }

    #[test]
    fn a_reading_is_limited_only_by_a_spent_window_that_applies_and_has_not_rolled() {
        let now = 1_000;
        let fable_week = window(10_080, Some("Fable"), 100.0, Some(5_000));
        let five_hours = window(300, None, 40.0, Some(2_000));
        assert_eq!(
            limit_reading(
                &limits(vec![five_hours.clone(), fable_week.clone()]),
                Some("fable"),
                now
            ),
            LimitReading::Limited {
                resets_at: Some(5_000)
            }
        );
        assert_eq!(
            limit_reading(
                &limits(vec![five_hours.clone(), fable_week]),
                Some("opus"),
                now
            ),
            LimitReading::Available,
            "Fable's week says nothing about Opus"
        );
        let rolled = window(300, None, 100.0, Some(900));
        assert_eq!(
            limit_reading(&limits(vec![rolled]), None, now),
            LimitReading::Available
        );
        assert_eq!(
            limit_reading(&limits(Vec::new()), None, now),
            LimitReading::Unknown,
            "no figures is never a guess either way"
        );
        let unknown_reset = window(300, None, 100.0, None);
        assert_eq!(
            limit_reading(&limits(vec![unknown_reset, five_hours]), None, now),
            LimitReading::Limited { resets_at: None }
        );
    }

    #[test]
    fn a_wall_clock_far_ahead_of_the_monotonic_one_is_a_mac_that_slept() {
        assert!(!slept((1_000, 0), (1_200, 200)));
        assert!(!slept((1_000, 0), (25_000, 20)), "within the gap");
        assert!(slept((1_000, 0), (3_600_000, 200)));
    }

    #[test]
    fn a_waiting_role_resumes_at_its_reset_only_when_awake_and_rechecks_after_sleep() {
        assert_eq!(wait_turn(Some(100), 50, false), WaitTurn::Keep);
        assert_eq!(wait_turn(Some(100), 100, false), WaitTurn::Resume);
        assert_eq!(wait_turn(None, 1_000_000, false), WaitTurn::Keep);
        assert_eq!(wait_turn(Some(100), 500, true), WaitTurn::Recheck);
        assert_eq!(wait_turn(Some(100), 50, true), WaitTurn::Recheck);
    }

    #[test]
    fn a_recheck_runs_only_on_real_room_and_never_guesses_past_a_reset() {
        assert_eq!(
            after_recheck(LimitReading::Available, Some(10), 50),
            AfterRecheck::Resume
        );
        assert_eq!(
            after_recheck(
                LimitReading::Limited {
                    resets_at: Some(90)
                },
                Some(10),
                50
            ),
            AfterRecheck::WaitUntil(Some(90))
        );
        assert_eq!(
            after_recheck(LimitReading::Unknown, Some(10), 50),
            AfterRecheck::AskYou,
            "the reset passed and nothing says whether it reset"
        );
        assert_eq!(
            after_recheck(LimitReading::Unknown, Some(90), 50),
            AfterRecheck::WaitUntil(Some(90))
        );
    }

    fn handover() -> Handover {
        Handover {
            from: Runner {
                role: "correctness".to_owned(),
                provider: RoleProvider::Codex,
                model: Some("gpt-5".to_owned()),
            },
            to: Runner {
                role: "correctness-claude".to_owned(),
                provider: RoleProvider::Claude,
                model: Some("sonnet".to_owned()),
            },
            reason: SwitchReason::Limit,
            why: "You've hit your usage limit.".to_owned(),
            objective: "Find bugs the change introduces".to_owned(),
            stage: 0,
            attempts: 1,
            progress: vec!["reading export.rs".to_owned()],
            plan: None,
            snapshot: Some(HandoverSnapshot {
                head: "a".repeat(40),
                tree: "b".repeat(40),
                base: "c".repeat(40),
            }),
            diff: Some(vec![
                FileStat {
                    path: "src/export.rs".to_owned(),
                    added: Some(10),
                    removed: Some(2),
                },
                FileStat {
                    path: "logo.png".to_owned(),
                    added: None,
                    removed: None,
                },
            ]),
            findings: vec![HandoverFinding {
                title: "The last row is dropped".to_owned(),
                place: Some("src/export.rs:3".to_owned()),
                class: "confirmed_bug".to_owned(),
                severity: "blocking".to_owned(),
                by: vec!["conventions".to_owned()],
            }],
            answers: vec![HandoverAnswer {
                question: "CSV or XLSX?".to_owned(),
                answer: "CSV".to_owned(),
            }],
        }
    }

    #[test]
    fn a_handover_carries_the_flows_own_record_and_says_no_conversation_comes_with_it() {
        let text = handover().render();
        for expected in [
            "`correctness` on Codex gpt-5 stopped",
            "usage limit",
            "You are `correctness-claude` on Claude sonnet",
            "Its objective: Find bugs the change introduces",
            "It made 1 attempt (stage 1)",
            "- reading export.rs",
            "Plan: none",
            "head aaaaaaa, tree bbbbbbb, measured from ccccccc",
            "Diff stat: 2 files, +10 −2",
            "- src/export.rs +10 −2",
            "- logo.png (binary)",
            "[confirmed_bug, blocking] The last row is dropped at src/export.rs:3 (conventions)",
            "- CSV or XLSX?: CSV",
            "conversation is not transferred",
        ] {
            assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
        }
    }

    #[test]
    fn a_handover_with_a_plan_hands_the_plan_over_and_a_long_diff_is_cut_to_its_shape() {
        let mut with_plan = handover();
        with_plan.plan = Some("1. Write the header row.".to_owned());
        with_plan.diff = Some(
            (0..HANDOVER_FILES + 5)
                .map(|i| FileStat {
                    path: format!("f{i}"),
                    added: Some(1),
                    removed: Some(0),
                })
                .collect(),
        );
        let text = with_plan.render();
        assert!(text.contains("The approved plan:\n1. Write the header row."));
        assert!(!text.contains("Plan: none"));
        assert!(text.contains("- and 5 more"));
        assert!(!text.contains(&format!("- f{HANDOVER_FILES}")));
    }

    #[test]
    fn a_choice_reads_as_the_window_sends_it() {
        let once: CapacityChoice =
            serde_json::from_str(r#"{"kind":"run_once","provider":"claude","model":"opus"}"#)
                .unwrap();
        assert_eq!(
            once,
            CapacityChoice::RunOnce {
                provider: RoleProvider::Claude,
                model: Some("opus".to_owned())
            }
        );
        let skip: CapacityChoice = serde_json::from_str(r#"{"kind":"skip"}"#).unwrap();
        assert_eq!(skip, CapacityChoice::Skip);
    }
}
