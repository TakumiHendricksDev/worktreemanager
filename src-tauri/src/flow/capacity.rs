//! Capacity, in the engine: places for run sessions, a role waiting at its provider's limit, what
//! you choose for it, and the handover a role switched elsewhere is given.
//!
//! The rules are `wtm_core::flow::capacity`'s; this is where they meet sessions, the clock and the
//! inbox.
//!
//! # A place before the session, and only for the session
//!
//! Every attempt asks the [`Seats`] for a place before its invocation is recorded, so a role over a
//! cap waits as itself — its step pending, the Reviews tab saying "queued: Codex at 3/3" — rather
//! than as a session that seems to be opening. The place is held for exactly as long as the session
//! is, and is let go when the attempt ends however it ends. A role waiting at a usage limit, or for
//! its environment, holds no place: it has no session, and holding one would starve the run's
//! other roles of the places its wait is not using. That is also what keeps a parent and its
//! children from deadlocking under a cap of one: nothing that waits on another session holds a
//! place while it waits.
//!
//! # Waiting at a limit
//!
//! The role's thread waits, looking at the clock, its run's cancel flag, the app quitting, and what
//! you chose. It runs again by itself at the reset the provider named, but only when the clock says
//! wtm was awake throughout: a Mac that slept, or a wait picked up again at launch, asks the
//! provider first ([`RoleHost::limits`]). A provider that cannot say is never guessed at; the role
//! waits for you.
//!
//! # A debug build can be told to hit a limit
//!
//! A real usage limit cannot be reached on demand without spending the limit, so a debug build reads
//! `WTM_DEV_LIMIT` (`role=seconds` or `role=unknown`, comma-separated): the first attempt of each
//! named role in a run ends at a limit resetting that many seconds later, or at one with no reset,
//! before any session opens. Nothing is spent and the record says it was injected. A release build
//! ignores the variable.

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::time::Duration;

use parking_lot::Mutex;
use serde::Serialize;
use wtm_core::flow::RunPart;
use wtm_core::flow::{
    AfterRecheck, CapacityChoice, Field, FileStat, FlowId, Handover, HandoverAnswer,
    HandoverFinding, HandoverSnapshot, InboxKind, InboxTarget, Ledger, LimitReading, NewInboxItem,
    NewProviderSwitch, Phase, QueuedSeat, RoleProvider, Runner, Seat, SeatId, SeatRequest, Seating,
    SessionCaps, Step, StepChange, StepId, StepStatus, SwitchReason, SwitchedBy, WaitTurn,
    after_recheck, credit_problem, resume_at, slept, wait_turn,
};
use wtm_core::ports::{FlowStore, InboxStore, RunStore};

use super::{AttemptEnd, Engine, Refusal, Run, RunCtx, on, text};

// ═══════════════════════════════ places ═══════════════════════════════

/// The places run sessions hold, and the line for them. One per engine, so every run, and every
/// session a run opens for its children, counts against the same caps.
#[derive(Debug, Default)]
pub struct Seats {
    ledger: Mutex<Ledger>,
    /// The caps as last read from your config. Read again whenever a place is asked for or let
    /// go, so an edit applies to the next session without a relaunch, and never on a timer.
    caps: Mutex<SessionCaps>,
    /// Numbers each view of the line, in the order the line was in. Two sessions asking at once
    /// announce from two threads, and the window may hear the older view last; the number is what
    /// lets it keep the newer one (found by the click-through, where a stale view hid the queue).
    seq: std::sync::atomic::AtomicU64,
}

impl Seats {
    fn caps(&self) -> SessionCaps {
        *self.caps.lock()
    }
}

/// Who holds places and who waits, with the runs they belong to named, for the window.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SeatsView {
    /// Higher for a later view of the line. The window keeps the highest it has heard.
    pub seq: u64,
    #[serde(flatten)]
    pub seating: Seating,
    /// What each run is of, by flow id: its worktree, or what its work item is called.
    pub labels: BTreeMap<FlowId, String>,
}

/// A place held for one attempt, let go when the attempt is over.
pub(super) struct SeatGuard<'a> {
    engine: &'a Engine,
    id: SeatId,
}

impl Drop for SeatGuard<'_> {
    fn drop(&mut self) {
        self.engine.release_seat(self.id);
    }
}

// ═══════════════════════════════ the debug hook ═══════════════════════════════

/// What a debug build's `WTM_DEV_LIMIT` says: roles whose first attempt in a run meets a limit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LimitFault {
    roles: BTreeMap<String, Injected>,
}

/// One injected limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Injected {
    /// Seconds until it resets, or `None` for a limit with no reset.
    resets_in: Option<u64>,
}

impl LimitFault {
    /// The hook this process was started with. Always `None` in a release build.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        if !cfg!(debug_assertions) {
            return None;
        }
        Self::parse(&std::env::var("WTM_DEV_LIMIT").ok()?)
    }

    fn parse(value: &str) -> Option<Self> {
        let mut roles = BTreeMap::new();
        for entry in value.split(',').map(str::trim).filter(|e| !e.is_empty()) {
            let Some((role, when)) = entry.split_once('=') else {
                tracing::warn!(entry, "WTM_DEV_LIMIT entry is not `role=seconds`; ignored");
                continue;
            };
            let resets_in = match when.trim() {
                "unknown" => None,
                seconds => {
                    let Ok(seconds) = seconds.parse() else {
                        tracing::warn!(entry, "WTM_DEV_LIMIT seconds are not a number; ignored");
                        continue;
                    };
                    Some(seconds)
                }
            };
            roles.insert(role.trim().to_owned(), Injected { resets_in });
        }
        (!roles.is_empty()).then_some(Self { roles })
    }

    /// The injected limit for this attempt, if there is one.
    fn hits(&self, role: &str, attempt: u32) -> Option<Injected> {
        (attempt == 1)
            .then(|| self.roles.get(role).copied())
            .flatten()
    }
}

/// How a wait at a limit ended.
pub(super) enum Waited {
    /// The role runs again: at its reset, after a recheck, because you said try now, or on the
    /// provider you chose to run it on once.
    Resume,
    /// You finished the review without it.
    Skipped,
    /// Cancelled, or the app is quitting.
    Stopped,
}

/// What the Reviews tab and the inbox show about a role waiting at a limit.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CapacityView {
    pub step: StepId,
    pub role: String,
    pub provider: RoleProvider,
    pub model: Option<String>,
    /// Unix seconds, when the provider said.
    pub resets_at: Option<u64>,
    /// The role's configured alternatives, in order, and why none of them took over.
    pub fallbacks: Vec<FallbackState>,
    /// Whether the role may run a model that bills usage credits.
    pub allow_credit_billing: bool,
    /// What the last look at the provider's own figures said, when there was one since the wait
    /// began: after a sleep, or at launch.
    pub recheck: Option<String>,
}

/// One configured alternative, and whether it can take over.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FallbackState {
    pub role: String,
    pub provider: RoleProvider,
    pub model: Option<String>,
    /// Why it did not take over: it is on the provider that ran out, or it already ran.
    pub passed_over: Option<String>,
}

impl Engine {
    // ═══════════════════════════════ places ═══════════════════════════════

    /// The caps as your config says now, kept for the waiters to compare against.
    fn read_caps(&self) -> SessionCaps {
        let caps = self.app.config.user_config().map_or_else(
            |error| {
                tracing::warn!(%error, "could not read [run_sessions]; using the default caps");
                SessionCaps::default()
            },
            |config| config.run_sessions,
        );
        *self.seats.caps.lock() = caps;
        caps
    }

    /// Wait for a place for one attempt. `Err` when the run stopped, or the app quit, while it
    /// waited: the attempt ends the way it would have mid-session.
    pub(super) fn take_seat(
        &self,
        flow: FlowId,
        run: &Run,
        step: &Step,
    ) -> Result<SeatGuard<'_>, AttemptEnd> {
        let caps = self.read_caps();
        let id = self.seats.ledger.lock().ask(
            &caps,
            SeatRequest {
                flow,
                step: Some(step.id),
                role: step.role.clone(),
                provider: step.provider,
            },
            self.now(),
        );
        let guard = SeatGuard { engine: self, id };
        self.announce_seats();
        if self.seats.ledger.lock().is_admitted(id) {
            return Ok(guard);
        }
        tracing::info!(%flow, role = %step.role, "a role waits for a run-session place");
        self.notice.changed(flow);
        loop {
            std::thread::sleep(Duration::from_millis(self.tuning.tick_ms));
            if self.quitting() {
                return Err(AttemptEnd::Quitting);
            }
            if run.cancel.load(Ordering::SeqCst) {
                return Err(AttemptEnd::Cancelled);
            }
            if self.seats.ledger.lock().is_admitted(id) {
                self.notice.changed(flow);
                return Ok(guard);
            }
        }
    }

    fn release_seat(&self, id: SeatId) {
        let caps = self.read_caps();
        let (gone, admitted) = {
            let mut ledger = self.seats.ledger.lock();
            let gone = ledger.seat(id).map(|seat| seat.flow);
            (gone, ledger.release(&caps, id, self.now()))
        };
        for seat in &admitted {
            self.notice.changed(seat.flow);
        }
        if let Some(flow) = gone {
            self.notice.changed(flow);
        }
        self.announce_seats();
    }

    fn announce_seats(&self) {
        self.notice.seats(&self.seats_view());
    }

    /// Who holds places and who waits, everywhere.
    #[must_use]
    pub fn seats_view(&self) -> SeatsView {
        // Numbered under the ledger's lock, so the numbers follow the line's own order.
        let (seating, seq) = {
            let ledger = self.seats.ledger.lock();
            (
                ledger.seating(&self.seats.caps()),
                self.seats.seq.fetch_add(1, Ordering::SeqCst) + 1,
            )
        };
        let flows: Vec<FlowId> = seating
            .holding
            .iter()
            .chain(seating.queued.iter().map(|q| &q.seat))
            .map(|seat| seat.flow)
            .collect();
        let labels = flows
            .into_iter()
            .filter_map(|flow| Some((flow, self.label_of(flow)?)))
            .collect();
        SeatsView {
            seq,
            seating,
            labels,
        }
    }

    /// The seats one run waits with, for its view.
    pub(super) fn queued_in(&self, flow: FlowId) -> Vec<QueuedSeat> {
        self.seats
            .ledger
            .lock()
            .seating(&self.seats.caps())
            .queued
            .into_iter()
            .filter(|q| q.seat.flow == flow)
            .collect()
    }

    /// The places one run holds now.
    pub(super) fn holding_in(&self, flow: FlowId) -> Vec<Seat> {
        self.seats
            .ledger
            .lock()
            .seating(&self.seats.caps())
            .holding
            .into_iter()
            .filter(|s| s.flow == flow)
            .collect()
    }

    fn label_of(&self, flow: FlowId) -> Option<String> {
        let db = self.db().ok()?;
        if let Ok(Some(review)) = db.review(flow) {
            return Some(review.worktree_path.rsplit('/').next()?.to_owned());
        }
        let state = db.flow(flow).ok()??;
        db.work_item(state.work_item).ok()?.map(|item| item.title)
    }

    // ═══════════════════════════════ the debug hook ═══════════════════════════════

    /// The limit `WTM_DEV_LIMIT` injects for this attempt, recorded as the invocation it stands in
    /// for, or `None`.
    pub(super) fn injected_limit(
        &self,
        ctx: &RunCtx,
        step: &Step,
        attempt: u32,
    ) -> Option<AttemptEnd> {
        let injected = self.limit_fault.as_ref()?.hits(&step.role, attempt)?;
        let db = self.db().ok()?;
        let resets_at = injected
            .resets_in
            .map(|seconds| self.now() / 1000 + seconds);
        let message =
            "a usage limit injected by WTM_DEV_LIMIT in a debug build: no session opened \
                       and nothing was spent"
                .to_owned();
        let invocation = db
            .start_invocation(
                &wtm_core::flow::NewInvocation {
                    flow: ctx.flow,
                    step: step.id,
                    attempt,
                    role: step.role.clone(),
                    provider: step.provider,
                    model: step.model.clone(),
                    profile: step.profile,
                    snapshot: Some(ctx.snapshot_id),
                    cwd: ctx.worktree_path.to_string_lossy().into_owned(),
                },
                self.now(),
            )
            .ok()?;
        let _ = db.update_invocation(
            invocation.id,
            &wtm_core::flow::InvocationChange::ended(
                wtm_core::flow::InvocationStatus::LimitReached,
                Some(message.clone()),
                self.now(),
            ),
            self.now(),
        );
        self.notice.changed(ctx.flow);
        Some(AttemptEnd::Limit { resets_at, message })
    }

    // ═══════════════════════════════ waiting at a limit ═══════════════════════════════

    /// Hold the role until its provider's limit resets, a recheck finds room, or you choose.
    ///
    /// `recheck_first` is set when the wait is picked up again at launch: whatever the reset time
    /// said before wtm stopped, the provider is asked before anything runs.
    #[allow(clippy::too_many_lines)]
    pub(super) fn wait_for_capacity(
        &self,
        ctx: &RunCtx,
        run: &Run,
        step: &Step,
        resets_at: Option<u64>,
        message: &str,
        recheck_first: bool,
    ) -> Waited {
        let mut resets_at = resets_at;
        self.update_step(
            step,
            &StepChange {
                resets_at: Field::from(resets_at),
                ..StepChange::status(StepStatus::WaitingCapacity)
            }
            .detail(message.to_owned()),
        );
        self.hold_capacity(ctx.flow, step.provider, resets_at);
        self.file(&self.capacity_item(ctx, step, message));

        let mut last = (self.now(), self.clock.monotonic_ms());
        let mut recheck = recheck_first;
        let mut asked_you = false;
        loop {
            if !recheck {
                std::thread::sleep(Duration::from_millis(self.tuning.tick_ms));
            }
            if self.quitting() {
                return Waited::Stopped;
            }
            if run.cancel.load(Ordering::SeqCst) {
                self.update_step(step, &StepChange::status(StepStatus::Cancelled));
                return Waited::Stopped;
            }
            let chosen = run.choices.lock().remove(&step.id);
            match chosen {
                Some(CapacityChoice::RunOnce { provider, model }) => {
                    self.run_once_here(ctx, step, provider, model.as_deref());
                    break;
                }
                Some(CapacityChoice::Skip) => {
                    self.skip_here(ctx.flow, step);
                    return Waited::Skipped;
                }
                // Waiting is what this loop does; the choice only put the question away.
                Some(CapacityChoice::Wait) | None => {}
            }
            if run.wake.swap(false, Ordering::SeqCst) {
                break;
            }
            let now = (self.now(), self.clock.monotonic_ms());
            let woke = slept(last, now);
            last = now;
            if woke {
                tracing::info!(flow = %ctx.flow, role = %step.role, "the Mac slept during a capacity wait");
            }
            let turn = if recheck {
                WaitTurn::Recheck
            } else {
                wait_turn(resume_at(resets_at), now.0, woke)
            };
            recheck = false;
            match turn {
                WaitTurn::Keep => {}
                WaitTurn::Resume => break,
                WaitTurn::Recheck => {
                    let reading = self.host.limits(step.provider, step.model.as_deref());
                    tracing::info!(flow = %ctx.flow, role = %step.role, ?reading,
                        "asked the provider again during a capacity wait");
                    let now_s = self.now() / 1000;
                    match after_recheck(reading, resets_at, now_s) {
                        AfterRecheck::Resume => {
                            self.note_recheck(step, "it had room again");
                            break;
                        }
                        AfterRecheck::WaitUntil(next) => {
                            if next != resets_at {
                                resets_at = next;
                                self.update_step(
                                    step,
                                    &StepChange {
                                        resets_at: Field::from(resets_at),
                                        ..StepChange::default()
                                    },
                                );
                                self.release_capacity(ctx.flow);
                            }
                            self.note_recheck(
                                step,
                                match reading {
                                    LimitReading::Limited { .. } => "it is still at its limit",
                                    _ => "it said nothing, and the reset has not come yet",
                                },
                            );
                        }
                        AfterRecheck::AskYou => {
                            if !asked_you {
                                asked_you = true;
                                resets_at = None;
                                self.update_step(
                                    step,
                                    &StepChange {
                                        resets_at: Field::Clear,
                                        ..StepChange::default()
                                    }
                                    .detail(format!(
                                        "{} did not say whether its limit has reset, so this role \
                                         waits for you to try it",
                                        step.provider
                                    )),
                                );
                                self.release_capacity(ctx.flow);
                                self.note_recheck(
                                    step,
                                    "it did not say whether its limit reset, so wtm waits for you",
                                );
                            }
                        }
                    }
                }
            }
        }

        self.update_step(
            step,
            &StepChange {
                resets_at: Field::Clear,
                detail: Field::Clear,
                ..StepChange::status(StepStatus::Pending)
            },
        );
        if let Ok(db) = self.db() {
            let _ = db.resolve_inbox(ctx.flow, InboxKind::Capacity, Some(&on(step)), self.now());
        }
        self.rechecks.lock().remove(&step.id);
        self.release_capacity(ctx.flow);
        self.notice.changed(ctx.flow);
        Waited::Resume
    }

    fn note_recheck(&self, step: &Step, said: &str) {
        let line = format!("wtm asked {} again: {said}.", step.provider);
        self.rechecks.lock().insert(step.id, line);
        self.notice.changed(step.flow);
    }

    /// The inbox item a role waiting at a limit files: what waits, until when, what happens by
    /// itself, and what you can do instead.
    fn capacity_item(&self, ctx: &RunCtx, step: &Step, message: &str) -> NewInboxItem {
        let steps = self
            .db()
            .and_then(|db| db.steps(ctx.flow).map_err(text))
            .unwrap_or_default();
        let done = steps
            .iter()
            .filter(|s| s.status == StepStatus::Done)
            .count();
        let counted = steps
            .iter()
            .filter(|s| !matches!(s.status, StepStatus::Skipped | StepStatus::Replaced))
            .count();
        let origin = step.stands_in_for.as_deref().unwrap_or(&step.role);
        let chain = ctx.workflow.fallback_chain(origin);
        let fallbacks = if chain.is_empty() {
            "No fallback is configured for this role, so nothing switches by itself.".to_owned()
        } else {
            format!(
                "Its fallbacks ({}) are on the provider that ran out or have already run, so \
                 nothing switches by itself.",
                chain
                    .iter()
                    .map(|role| format!("`{role}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        NewInboxItem {
            flow: ctx.flow,
            kind: InboxKind::Capacity,
            target: on(step),
            title: format!("`{}` is paused: {} limit reached", step.role, step.provider),
            body: format!(
                "{message}\n\nWaiting: `{}` (stage {} of recipe `{}`). {done} of {counted} roles \
                 had finished; their results are kept, and the review consolidates once this role \
                 runs or you finish without it.\n\nIt runs again by itself when the limit resets, \
                 while wtm is open and the Mac is awake. After a sleep or a relaunch, wtm asks {} \
                 again before running it. {fallbacks} You can run it once on another provider, \
                 keep waiting, or finish the review without it. wtm never uses an API key.",
                step.role,
                step.stage + 1,
                ctx.recipe,
                step.provider,
            ),
        }
    }

    /// Run the waiting role once on `provider`, from a handover, on its own step.
    fn run_once_here(
        &self,
        ctx: &RunCtx,
        step: &Step,
        provider: RoleProvider,
        model: Option<&str>,
    ) {
        let Ok(db) = self.db() else { return };
        let handover = self.handover_for(
            ctx,
            step,
            Runner {
                role: step.role.clone(),
                provider,
                model: model.map(str::to_owned),
            },
            SwitchReason::Once,
            // The provider's own words: the reason says the rest, that you chose to run it here.
            step.detail
                .as_deref()
                .unwrap_or("its provider reported a usage limit"),
        );
        self.update_step(
            step,
            &StepChange {
                provider: Some(provider),
                model: Field::from(model.map(str::to_owned)),
                handover: Field::Set(handover.render()),
                resets_at: Field::Clear,
                ..StepChange::status(StepStatus::Pending)
            }
            .detail(format!(
                "you chose to run it once on {provider}, from a handover"
            )),
        );
        if let Err(error) = db.record_switch(
            &NewProviderSwitch {
                flow: ctx.flow,
                from_step: step.id,
                to_step: step.id,
                reason: SwitchReason::Once,
                by: SwitchedBy::You,
                handover,
            },
            self.now(),
        ) {
            tracing::warn!(flow = %ctx.flow, %error, "could not record a provider switch");
        }
    }

    /// Finish the review without the waiting role.
    fn skip_here(&self, flow: FlowId, step: &Step) {
        self.update_step(
            step,
            &StepChange {
                resets_at: Field::Clear,
                ..StepChange::status(StepStatus::Skipped)
            }
            .detail(format!(
                "you finished the review without it at {}'s usage limit",
                step.provider
            )),
        );
        if let Ok(db) = self.db() {
            let _ = db.resolve_inbox(flow, InboxKind::Capacity, Some(&on(step)), self.now());
        }
        self.rechecks.lock().remove(&step.id);
        self.release_capacity(flow);
        self.notice.changed(flow);
    }

    // ═══════════════════════════════ your choice ═══════════════════════════════

    /// What you chose for a role waiting at its provider's limit: wait, run it once elsewhere, or
    /// finish without it.
    ///
    /// # Errors
    ///
    /// When the role is not waiting for capacity, the provider would not bill a subscription, or
    /// the model bills usage credits the role has not opted into. Nothing changes then.
    pub fn choose_capacity(
        self: &std::sync::Arc<Self>,
        flow: FlowId,
        step: StepId,
        choice: &CapacityChoice,
    ) -> Result<(), Refusal> {
        let db = self.db()?;
        let current = db
            .steps(flow)
            .map_err(text)?
            .into_iter()
            .find(|s| s.id == step)
            .ok_or_else(|| Refusal::from(format!("run {flow} has no step {step}")))?;
        if current.status != StepStatus::WaitingCapacity {
            return Err(format!(
                "`{}` is not waiting for usage capacity any more; it is {:?}",
                current.role, current.status
            )
            .into());
        }
        if let CapacityChoice::RunOnce { provider, model } = choice {
            let model = model.as_deref().map(str::trim).filter(|m| !m.is_empty());
            if *provider == current.provider && model == current.model.as_deref() {
                return Err(format!(
                    "`{}` already runs there, and that is what reached its limit; pick another \
                     provider or model",
                    current.role
                )
                .into());
            }
            let review = db
                .review(flow)
                .map_err(text)?
                .ok_or_else(|| format!("run {flow} is not a review"))?;
            let project = self.app.project(review.project.as_str()).map_err(text)?;
            let workflow = self.run_workflow(flow, RunPart::Review, &project)?;
            let spec = workflow.role.get(&current.role).ok_or_else(|| {
                format!(
                    "role `{}` is no longer in your workflow config",
                    current.role
                )
            })?;
            if let Some(problem) = credit_problem(&current.role, spec, *provider, model) {
                return Err(Refusal {
                    message: problem.to_string(),
                    billing: Vec::new(),
                    credits: vec![problem],
                });
            }
            // F3's check, for the provider it would now run on: a subscription, never an API key.
            self.host.preflight(&[provider.id()])?;
        }
        if matches!(choice, CapacityChoice::Wait) {
            // You put the question away: the role goes on waiting, as it would have, and the item
            // is dismissed rather than resolved, because the limit is not settled.
            for item in db.inbox(Some(flow), None).map_err(text)? {
                if item.kind == InboxKind::Capacity
                    && item.target
                        == (InboxTarget::Step {
                            step,
                            role: current.role.clone(),
                        })
                {
                    db.dismiss_inbox_item(item.id, self.now()).map_err(text)?;
                }
            }
            self.notice.changed(flow);
            return Ok(());
        }
        let choice = match choice {
            CapacityChoice::RunOnce { provider, model } => CapacityChoice::RunOnce {
                provider: *provider,
                model: model
                    .as_deref()
                    .map(str::trim)
                    .filter(|m| !m.is_empty())
                    .map(str::to_owned),
            },
            other => other.clone(),
        };
        let live = self.runs.lock().get(&flow).cloned();
        if let Some(run) = live {
            // The role's thread is waiting on exactly this; it acts on its next look.
            run.choices.lock().insert(step, choice);
            return Ok(());
        }
        // Nobody is waiting in this app — the wait was cut off — so the choice is applied here and
        // the run carries on from it.
        let ctx = self.capacity_ctx(flow)?;
        match &choice {
            CapacityChoice::RunOnce { provider, model } => {
                self.run_once_here(&ctx, &current, *provider, model.as_deref());
                let _ =
                    db.resolve_inbox(flow, InboxKind::Capacity, Some(&on(&current)), self.now());
            }
            CapacityChoice::Skip => self.skip_here(flow, &current),
            CapacityChoice::Wait => {}
        }
        self.resume(flow)
    }

    /// Just enough of a run's context to write a handover from outside its thread.
    fn capacity_ctx(&self, flow: FlowId) -> Result<RunCtx, Refusal> {
        let db = self.db()?;
        let review = db
            .review(flow)
            .map_err(text)?
            .ok_or_else(|| format!("run {flow} is not a review"))?;
        let project = self.app.project(review.project.as_str()).map_err(text)?;
        let worktree = self
            .app
            .worktree(&project, &review.worktree)
            .map_err(|e| format!("the worktree is no longer there: {e}"))?;
        let workflow = self.run_workflow(flow, RunPart::Review, &project)?;
        let snapshot_id = review.snapshot.ok_or("this review has no snapshot yet")?;
        let snapshot = db
            .snapshot(snapshot_id)
            .map_err(text)?
            .ok_or("the review's snapshot is missing from the record")?;
        Ok(RunCtx {
            flow,
            project: review.project.as_str().to_owned(),
            project_id: review.project.clone(),
            worktree: review.worktree.clone(),
            worktree_path: worktree.path.clone(),
            branch: worktree.branch().map(|b| b.as_str().to_owned()),
            recipe: review.recipe.clone(),
            base: review.base.clone(),
            concurrency: None,
            patterns: Vec::new(),
            pull_request: None,
            revision: None,
            fix: None,
            dir: wtm_core::flow::FlowDir::new(&self.app.config.paths().flow_dir, flow),
            workflow,
            snapshot,
            snapshot_id,
            changed: None,
        })
    }

    /// The capacity card's facts for a run's waiting role, when one waits.
    pub(super) fn capacity_view(&self, flow: FlowId, steps: &[Step]) -> Option<CapacityView> {
        let step = steps
            .iter()
            .find(|s| s.status == StepStatus::WaitingCapacity)?;
        let db = self.db().ok()?;
        let review = db.review(flow).ok()??;
        let workflow = self.kept_workflow(
            flow,
            RunPart::Review,
            self.app.project(review.project.as_str()).ok().as_ref(),
        );
        let origin = step.stands_in_for.as_deref().unwrap_or(&step.role);
        let ran: std::collections::BTreeSet<&str> = steps.iter().map(|s| s.role.as_str()).collect();
        let spent = self.spent_in(flow, review.snapshot);
        let fallbacks = workflow
            .as_ref()
            .map(|workflow| {
                workflow
                    .fallback_chain(origin)
                    .into_iter()
                    .filter_map(|role| {
                        let spec = workflow.role.get(&role)?;
                        let passed_over = if ran.contains(role.as_str()) {
                            Some("already ran in this review".to_owned())
                        } else if spec.provider == step.provider || spent.contains(&spec.provider) {
                            Some(format!("on {}, which ran out", spec.provider))
                        } else {
                            None
                        };
                        Some(FallbackState {
                            provider: spec.provider,
                            model: spec.model.clone(),
                            role,
                            passed_over,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Some(CapacityView {
            step: step.id,
            role: step.role.clone(),
            provider: step.provider,
            model: step.model.clone(),
            resets_at: step.resets_at,
            fallbacks,
            allow_credit_billing: workflow
                .as_ref()
                .and_then(|w| w.role.get(&step.role))
                .is_some_and(|spec| spec.allow_credit_billing),
            recheck: self.rechecks.lock().get(&step.id).cloned(),
        })
    }

    // ═══════════════════════════════ the handover ═══════════════════════════════

    /// What a role taking over from `step` is told: the flow's own record of where things stand.
    pub(super) fn handover_for(
        &self,
        ctx: &RunCtx,
        step: &Step,
        to: Runner,
        reason: SwitchReason,
        why: &str,
    ) -> Handover {
        let db = self.db().ok();
        let invocations: Vec<_> = db
            .as_ref()
            .and_then(|db| db.invocations(ctx.flow).ok())
            .unwrap_or_default()
            .into_iter()
            .filter(|i| i.step == step.id)
            .collect();
        let origin = step
            .stands_in_for
            .clone()
            .unwrap_or_else(|| step.role.clone());
        let objective = ctx
            .workflow
            .role
            .get(&origin)
            .map(|spec| spec.objective.trim().to_owned())
            .unwrap_or_default();
        let diff = wtm_git::push::numstat(
            self.app.runner.as_ref(),
            &ctx.worktree_path,
            &ctx.snapshot.base_sha,
            &ctx.snapshot.tree_sha,
        )
        .ok()
        .map(|files| {
            files
                .into_iter()
                .map(|(path, added, removed)| FileStat {
                    path,
                    added,
                    removed,
                })
                .collect()
        });
        let findings = db
            .as_ref()
            .and_then(|db| db.findings(ctx.flow).ok())
            .unwrap_or_default()
            .into_iter()
            .filter(|record| record.finding.dismissed.is_none())
            .map(|record| {
                let finding = &record.finding;
                let mut by: Vec<String> = record
                    .evidence
                    .iter()
                    .map(|e| e.source.role.clone())
                    .collect();
                by.dedup();
                HandoverFinding {
                    title: finding.title.clone(),
                    place: finding.location.as_ref().map(|at| {
                        if record.lines {
                            format!("{}:{}", at.path, at.start_line)
                        } else {
                            at.path.clone()
                        }
                    }),
                    class: tag_of(&finding.class),
                    severity: tag_of(&finding.severity),
                    by,
                }
            })
            .collect();
        let answers = self
            .answers_for(ctx, &origin)
            .into_iter()
            .filter_map(|question| {
                let answer = question.answer.as_ref()?;
                let chosen = answer
                    .choice
                    .and_then(|i| question.options.get(i))
                    .map(String::as_str);
                let words = match (chosen, answer.text.trim()) {
                    (Some(chosen), "") => chosen.to_owned(),
                    (Some(chosen), note) => format!("{chosen} ({note})"),
                    (None, note) => note.to_owned(),
                };
                Some(HandoverAnswer {
                    question: question.text,
                    answer: words,
                })
            })
            .collect();
        Handover {
            from: Runner {
                role: step.role.clone(),
                provider: step.provider,
                model: step.model.clone(),
            },
            to,
            reason,
            why: why.to_owned(),
            objective,
            stage: step.stage,
            attempts: u32::try_from(invocations.len()).unwrap_or(u32::MAX),
            progress: invocations.into_iter().filter_map(|i| i.progress).collect(),
            plan: None,
            snapshot: Some(HandoverSnapshot {
                head: ctx.snapshot.head_sha.clone(),
                tree: ctx.snapshot.tree_sha.clone(),
                base: ctx.snapshot.base_sha.clone(),
            }),
            diff,
            findings,
            answers,
        }
    }

    /// Whether `flow` has a role waiting at a limit and nothing mid-turn, which is the one kind of
    /// held run a quit or a launch picks up again by itself: no turn was cut off.
    pub(super) fn only_waiting_for_capacity(&self, flow: FlowId) -> bool {
        let Ok(db) = self.db() else { return false };
        let steps = db.steps(flow).unwrap_or_default();
        steps
            .iter()
            .any(|s| s.status == StepStatus::WaitingCapacity)
            && !steps.iter().any(|s| s.status == StepStatus::Running)
            && self
                .state(flow)
                .is_ok_and(|state| state.phase == Phase::Reviewing && state.end.is_none())
    }
}

/// A unit-like enum's serde tag, as the findings schema spells it.
fn tag_of<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(tag)) => tag,
        Ok(serde_json::Value::Object(object)) => object
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dev_limit_hook_names_roles_and_their_resets_and_hits_only_a_first_attempt() {
        let fault = LimitFault::parse("correctness=90, ui=unknown,bad,x=soon").unwrap();
        assert_eq!(
            fault.hits("correctness", 1),
            Some(Injected {
                resets_in: Some(90)
            })
        );
        assert_eq!(fault.hits("ui", 1), Some(Injected { resets_in: None }));
        assert_eq!(
            fault.hits("correctness", 2),
            None,
            "the next attempt is real"
        );
        assert_eq!(fault.hits("x", 1), None, "a malformed entry is ignored");
        assert_eq!(LimitFault::parse(" , "), None);
    }
}
