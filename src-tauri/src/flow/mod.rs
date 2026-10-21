//! The review engine: a review flow from Resolving through Reviewing to Consolidated, its roles run
//! as headless sessions, stage by stage, and its holds.
//!
//! # What a run does, in order
//!
//! 1. **Start** (the caller's thread). Resolve the worktree and the recipe, preflight every
//!    provider the recipe can reach (F3: a plan, never an API key), resolve the base, then find or
//!    make the work item and start the flow. A start refused here leaves nothing behind.
//! 2. **Snapshot** (the run's own thread). The flow moves to Preparing and records the snapshot
//!    every role will read, before any session opens: what a review read is evidence, and it has to
//!    exist before the reading starts.
//! 3. **Plan** the recipe against the snapshot's changed files, and record a step per role,
//!    skipped ones included with the reason.
//! 4. **Review**, stage by stage. A stage's roles run in parallel, at most the recipe's
//!    `concurrency` at once. Each attempt is one session opened with the role's R4 profile, sent
//!    one turn, and read only through `flow_submit`. A stage that reads the snapshot gets one
//!    detached checkout, removed when the stage ends.
//! 5. **Consolidated**, once every stage has run. Consolidation proper — deduplication and the
//!    finding classes — is R6's; this records each role's result as it was submitted.
//!
//! # Holds
//!
//! - **`WaitingCapacity`** when a provider reports its usage limit and the role has no configured
//!   fallback on another provider. The role waits for the reset the provider named and runs again
//!   by itself if the app is open; an unknown reset waits for you. Other roles keep running.
//! - **`Interrupted`** when the app quits mid-run ([`Engine::shutdown`]) or is found at launch to
//!   have stopped without doing so ([`Engine::recover`]). Completed roles keep their results and
//!   nothing is re-sent: a turn sent twice can do its work twice. [`Engine::resume`] runs what is
//!   left.
//!
//! # One notification owner
//!
//! A run session's own events still reach its pane behind the rail, but the window does not
//! announce them (`sessions.svelte.ts` skips a pane with a `flow`). What needs you — a role that
//! failed for good, a limit, a provider's permission prompt, an interruption — is an inbox item
//! here, filed once per `(flow, kind, subject)`, and the inbox is what notifies.
//!
//! # Threads, not tasks
//!
//! Each run is a thread, and each running role a scoped thread inside it, for the reason
//! ARCHITECTURE §3 gives: everything here blocks — git, SQLite, waiting on a session — and a role is
//! a handful of long waits, not thousands of short ones. Waits are polled against the [`Clock`]
//! port, so a test drives every timeout, retry and reset by moving a fake clock.

pub mod brief;
pub mod commands;
pub mod host;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use parking_lot::Mutex;
use serde::Serialize;
use wtm_agent::session::AgentSink;
use wtm_appdb::AppDb;
use wtm_core::flow::{
    AfterFailure, Field, Flow, FlowDir, FlowEvent, FlowId, FlowKind, Hold, HoldKind, InboxItem,
    InboxItemId, InboxKind, InterruptCause, Invocation, InvocationChange, InvocationStatus,
    NewInboxItem, NewInvocation, NewReviewRun, NewStep, NewWorkItem, OnLimit, OutputKind, Phase,
    ReviewRun, RoleProvider, RoleSpec, ScratchLabel, Snapshot, SnapshotId, Status, Step,
    StepChange, StepStatus, Submission, WorkLink, WorkflowSpec, Workspace, after_failure, on_limit,
    resume_at,
};
use wtm_core::model::{AgentEvent, AgentSkill, ApprovalRequest, ExitOutcome, ProjectId, SessionId};
use wtm_core::ports::clock::Clock;
use wtm_core::ports::{FlowStore, FlowStoreError, RunStore};
use wtm_core::usecase::snapshot::{SnapshotCheckout, Snapshots};

use crate::app::{App, FlowRole};
use crate::flow_session::RoleSession;
use crate::flow_tools::FlowCall;
use host::RoleHost;

/// How the engine paces itself. The app uses [`Tuning::default`]; tests shorten the waits so a
/// fake clock, not the wall, decides when things happen.
#[derive(Debug, Clone)]
pub struct Tuning {
    /// How often a waiting role looks at the clock, its cancel flag and the app quitting.
    pub tick_ms: u64,
    /// How long a role with skills waits for its provider to list them before it is refused.
    pub skills_wait_ms: u64,
    /// After a role submits, how long its turn has to end on its own before the session is closed.
    pub settle_ms: u64,
    /// Follow-up turns sent to a role that ends a turn without submitting, before the attempt
    /// counts as failed.
    pub nudges: u32,
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            tick_ms: 200,
            // Claude answers `initialize` in about a second, and Codex has to start its app server
            // and list skills from disk. A minute is slow-but-alive, not stuck.
            skills_wait_ms: 60_000,
            settle_ms: 30_000,
            nudges: 1,
        }
    }
}

/// Who hears that a run changed. The app emits window events; a test records them.
pub trait Notice: Send + Sync {
    /// Something about `flow` changed: its state, a step, an invocation.
    fn changed(&self, flow: FlowId);
    /// An inbox item was filed for the first time. Never called for one that was already open.
    fn filed(&self, item: &InboxItem);
}

/// What the Run Review button asks for.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartReview {
    pub project: String,
    pub worktree: String,
    pub recipe: String,
    /// The ref the change is measured from. `None` tries `origin/HEAD`, then the usual default
    /// branch names.
    #[serde(default)]
    pub base: Option<String>,
}

/// Everything the Work view shows about one review run.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowView {
    pub flow: Flow,
    pub status: Status,
    pub review: ReviewRun,
    pub snapshot: Option<Snapshot>,
    pub steps: Vec<Step>,
    pub invocations: Vec<Invocation>,
    pub inbox: Vec<InboxItem>,
    /// Whether this app has a thread working on it now. A flow that is open and not running is
    /// held, consolidated, or was interrupted.
    pub running: bool,
}

/// One run's controls, shared by its thread and its roles.
#[derive(Debug, Default)]
struct Run {
    cancel: AtomicBool,
    /// Set by a resume while the run is still working: roles waiting for capacity run now.
    wake: AtomicBool,
}

/// What a session told the role thread waiting on it.
#[derive(Debug)]
enum Signal {
    Event(AgentEvent),
    Exited(String),
    Submitted(Submission),
}

/// A role attempt that is running, as the run tools find it.
#[derive(Debug)]
struct Live {
    output: OutputKind,
    tx: Sender<Signal>,
}

/// The engine's ear on one session: the events it acts on, forwarded to the role thread.
struct InvocationSink {
    tx: Sender<Signal>,
}

impl AgentSink for InvocationSink {
    fn on_event(&self, _session: &SessionId, event: &AgentEvent) {
        let wanted = matches!(
            event,
            AgentEvent::SessionReady { .. }
                | AgentEvent::SkillsListed { .. }
                | AgentEvent::TurnFinished { .. }
                | AgentEvent::Failed { .. }
                | AgentEvent::LimitReached { .. }
                | AgentEvent::ApprovalRequested { .. }
        );
        if wanted {
            // A closed channel means the attempt has ended and nobody is listening any more.
            let _ = self.tx.send(Signal::Event(event.clone()));
        }
    }

    fn on_exit(&self, _session: &SessionId, outcome: &ExitOutcome) {
        let _ = self.tx.send(Signal::Exited(outcome.describe()));
    }

    fn on_ready(&self, _session: &SessionId) {}
}

/// How one attempt at a role ended.
#[derive(Debug)]
enum AttemptEnd {
    Submitted,
    /// The role's fault: counts against its retries.
    Failed {
        status: InvocationStatus,
        reason: String,
    },
    /// Cannot succeed however often it is tried: a skill its provider lacks.
    Unrunnable(String),
    Limit {
        resets_at: Option<u64>,
        message: String,
    },
    Cancelled,
    /// The app is quitting. [`Engine::shutdown`] has already written what the record should say.
    Quitting,
}

/// How a role ended, across its attempts.
#[derive(Debug)]
enum RoleEnd {
    Done,
    Stopped,
    Failed(String),
    Limit {
        resets_at: Option<u64>,
        message: String,
    },
}

/// What a run reads while it works, resolved once.
struct RunCtx {
    flow: FlowId,
    project: String,
    worktree: String,
    worktree_path: PathBuf,
    branch: Option<String>,
    recipe: String,
    base: String,
    workflow: WorkflowSpec,
    dir: FlowDir,
    snapshot: Snapshot,
    snapshot_id: SnapshotId,
    changed: Option<Vec<String>>,
    /// The recipe's cap on roles running at once within a stage.
    concurrency: Option<u32>,
}

impl RunCtx {
    fn role(&self, id: &str) -> Result<&RoleSpec, String> {
        self.workflow.role.get(id).ok_or_else(|| {
            format!("role `{id}` is no longer in your workflow config, so it cannot run")
        })
    }
}

/// The detached checkout a stage's snapshot readers share, opened by the first that needs it.
#[derive(Default)]
struct StageCheckout {
    opened: Mutex<Option<Result<SnapshotCheckout, String>>>,
}

pub struct Engine {
    app: Arc<App>,
    host: Arc<dyn RoleHost>,
    clock: Arc<dyn Clock>,
    notice: Arc<dyn Notice>,
    tuning: Tuning,
    runs: Mutex<BTreeMap<FlowId, Arc<Run>>>,
    live: Mutex<BTreeMap<(FlowId, String), Arc<Live>>>,
    /// Held across reading a flow's hold and recording a change to it, so two roles reaching a
    /// limit at once record one hold between them, not a refused second one.
    holds: Mutex<()>,
    /// Held across a start, so two clicks make one flow: the second finds the first's.
    starting: Mutex<()>,
    quitting: AtomicBool,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("runs", &self.runs.lock().len())
            .finish_non_exhaustive()
    }
}

fn text(error: impl std::fmt::Display) -> String {
    error.to_string()
}

impl Engine {
    #[must_use]
    pub fn new(
        app: Arc<App>,
        host: Arc<dyn RoleHost>,
        clock: Arc<dyn Clock>,
        notice: Arc<dyn Notice>,
        tuning: Tuning,
    ) -> Arc<Self> {
        Arc::new(Self {
            app,
            host,
            clock,
            notice,
            tuning,
            runs: Mutex::new(BTreeMap::new()),
            live: Mutex::new(BTreeMap::new()),
            holds: Mutex::new(()),
            starting: Mutex::new(()),
            quitting: AtomicBool::new(false),
        })
    }

    fn db(&self) -> Result<Arc<AppDb>, String> {
        self.app.appdb.get().map_err(|e| e.message)
    }

    fn now(&self) -> u64 {
        self.clock.now_unix_ms()
    }

    fn quitting(&self) -> bool {
        self.quitting.load(Ordering::SeqCst)
    }

    /// Whether this app has a thread working on `flow`.
    #[must_use]
    pub fn is_running(&self, flow: FlowId) -> bool {
        self.runs.lock().contains_key(&flow)
    }

    // ═══════════════════════════════ starting ═══════════════════════════════

    /// Start a review of a worktree with a recipe. Returns the flow, which is an existing one when
    /// the worktree already has a review running or waiting on you.
    ///
    /// # Errors
    ///
    /// Why it did not start, in a sentence. Nothing was recorded.
    pub fn start(self: &Arc<Self>, req: &StartReview) -> Result<FlowId, String> {
        let _one = self.starting.lock();
        let project = self.app.project(&req.project).map_err(text)?;
        let worktree = self
            .app
            .worktree(&project, &req.worktree)
            .map_err(|e| format!("that worktree is not available: {e}"))?;
        let workflow = project.workflow.as_ref().ok_or_else(|| {
            "this project has no review recipes; add `[workflow]` roles and a recipe to your own \
             config for it"
                .to_owned()
        })?;
        let plan = wtm_core::flow::plan(workflow, &req.recipe, None).map_err(text)?;
        let providers: Vec<&str> = plan
            .providers(workflow)
            .into_iter()
            .map(RoleProvider::id)
            .collect();
        self.host.preflight(&providers)?;
        let base = self.resolve_base(&worktree.path, req.base.as_deref())?;

        let db = self.db()?;
        let project_id = ProjectId::from_root(&project.root);
        let path = worktree.path.to_string_lossy().into_owned();
        let now = self.now();
        let existing = db
            .work_item_for_worktree(&project_id, &path)
            .map_err(text)?;
        let item = if let Some(id) = existing {
            db.work_item(id)
                .map_err(text)?
                .ok_or_else(|| format!("work item {id} has gone"))?
        } else {
            let title = worktree
                .branch()
                .map_or_else(|| worktree.dirname().to_owned(), |b| b.as_str().to_owned());
            let item = db
                .create_work_item(
                    &NewWorkItem {
                        project: project_id.clone(),
                        title,
                        issue: None,
                        pull_request: None,
                    },
                    now,
                )
                .map_err(text)?;
            db.link(item.id, &WorkLink::Worktree { path: path.clone() }, now)
                .map_err(text)?;
            item
        };
        let kind = if item.pull_request.is_some() {
            FlowKind::ReviewPr
        } else {
            FlowKind::ReviewMine
        };
        let flow = match db.start_flow(item.id, kind, now) {
            Ok(flow) => flow,
            Err(FlowStoreError::FlowAlreadyOpen { existing }) => {
                let open = db
                    .flow(existing)
                    .map_err(text)?
                    .ok_or_else(|| format!("flow {existing} has gone"))?;
                let superseded = !self.is_running(existing)
                    && open.state.hold.is_none()
                    && open.state.phase == Phase::Consolidated;
                if !superseded {
                    // Running, or waiting on you: the click focuses it rather than starting a
                    // second review of the same work.
                    return Ok(existing);
                }
                // A consolidated review you did not act on is kept as it is, as done, and the new
                // run is the current one. Its results stay in the record.
                db.record(existing, &FlowEvent::Finished, now)
                    .map_err(text)?;
                self.notice.changed(existing);
                db.start_flow(item.id, kind, now).map_err(text)?
            }
            Err(e) => return Err(text(e)),
        };
        db.create_review(
            &NewReviewRun {
                flow: flow.id,
                project: project_id,
                worktree: req.worktree.clone(),
                worktree_path: path,
                recipe: req.recipe.clone(),
                base,
            },
            now,
        )
        .map_err(text)?;
        self.notice.changed(flow.id);
        self.spawn(flow.id)?;
        Ok(flow.id)
    }

    /// The ref a review of `worktree` is measured from.
    fn resolve_base(&self, worktree: &Path, asked: Option<&str>) -> Result<String, String> {
        let resolves = |rev: &str| {
            self.app
                .git
                .rev_parse(worktree, rev)
                .ok()
                .flatten()
                .is_some()
        };
        if let Some(base) = asked.map(str::trim).filter(|b| !b.is_empty()) {
            return if resolves(base) {
                Ok(base.to_owned())
            } else {
                Err(format!(
                    "`{base}` does not name a commit in this repository"
                ))
            };
        }
        [
            "origin/HEAD",
            "origin/main",
            "origin/master",
            "main",
            "master",
        ]
        .into_iter()
        .find(|candidate| resolves(candidate))
        .map(str::to_owned)
        .ok_or_else(|| {
            "wtm could not tell which branch this work is measured from; name the base".to_owned()
        })
    }

    fn spawn(self: &Arc<Self>, flow: FlowId) -> Result<(), String> {
        let run = Arc::new(Run::default());
        {
            let mut runs = self.runs.lock();
            if runs.contains_key(&flow) {
                return Err(format!("run {flow} is already running"));
            }
            runs.insert(flow, Arc::clone(&run));
        }
        let engine = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name(format!("flow-{flow}"))
            .spawn(move || {
                // Removed however the drive ends, a panic included, or the run could never be
                // resumed in this app.
                struct Done<'a>(&'a Engine, FlowId);
                impl Drop for Done<'_> {
                    fn drop(&mut self) {
                        self.0.runs.lock().remove(&self.1);
                        self.0.notice.changed(self.1);
                    }
                }
                let _done = Done(&engine, flow);
                engine.drive(flow, &run);
            });
        if let Err(e) = spawned {
            self.runs.lock().remove(&flow);
            return Err(format!("wtm could not start the run's thread: {e}"));
        }
        Ok(())
    }

    // ═══════════════════════════════ driving ═══════════════════════════════

    fn drive(&self, flow: FlowId, run: &Run) {
        if let Err(reason) = self.drive_inner(flow, run) {
            if self.quitting() {
                return;
            }
            tracing::warn!(%flow, %reason, "a review run failed");
            self.fail(flow, &reason);
        }
    }

    fn drive_inner(&self, flow: FlowId, run: &Run) -> Result<(), String> {
        let db = self.db()?;
        let review = db
            .review(flow)
            .map_err(text)?
            .ok_or_else(|| format!("run {flow} has no review record"))?;
        let project = self
            .app
            .project(review.project.as_str())
            .map_err(|e| format!("the project is no longer registered: {e}"))?;
        let worktree = self
            .app
            .worktree(&project, &review.worktree)
            .map_err(|e| format!("the worktree is no longer there: {e}"))?;
        let workflow = project
            .workflow
            .clone()
            .ok_or_else(|| "the project's workflow config has gone".to_owned())?;

        let mut state = self.state(flow)?;
        if state.phase == Phase::Resolving {
            state = self.record(
                flow,
                &FlowEvent::Advanced {
                    to: Phase::Preparing,
                },
            )?;
        }

        let dir = FlowDir::new(&self.app.config.paths().flow_dir, flow);
        let snapshots = Snapshots {
            git: Arc::clone(&self.app.git),
            files: Arc::clone(&self.app.files),
        };
        let (snapshot_id, snapshot) = if let Some(id) = review.snapshot {
            (
                id,
                db.snapshot(id)
                    .map_err(text)?
                    .ok_or_else(|| format!("snapshot {id} is missing from the record"))?,
            )
        } else {
            let label = ScratchLabel::new("take").ok_or("a scratch label")?;
            let snapshot = snapshots
                .take(&worktree.path, &review.base, &dir, &label)
                .map_err(|e| format!("the snapshot could not be taken: {e}"))?;
            let id = db
                .record_snapshot(flow, &snapshot, self.now())
                .map_err(text)?;
            self.notice.changed(flow);
            (id, snapshot)
        };
        let changed = self
            .app
            .git
            .tree_changes(&worktree.path, &snapshot.base_sha, &snapshot.tree_sha)
            .ok()
            .filter(|listed| !listed.truncated)
            .map(|listed| listed.paths);

        if db.steps(flow).map_err(text)?.is_empty() {
            let known: Option<BTreeSet<String>> = changed
                .as_ref()
                .map(|paths| paths.iter().cloned().collect());
            let plan =
                wtm_core::flow::plan(&workflow, &review.recipe, known.as_ref()).map_err(text)?;
            let mut steps = Vec::new();
            for (stage, planned) in (0u32..).zip(&plan.stages) {
                for role in &planned.roles {
                    let spec = workflow.role.get(&role.id);
                    steps.push(NewStep {
                        stage,
                        role: role.id.clone(),
                        provider: role.provider,
                        model: spec.and_then(|s| s.model.clone()),
                        profile: role.profile,
                        workspace: role.workspace,
                        timeout_ms: role.timeout_ms,
                        retries: role.retries,
                        status: StepStatus::Pending,
                        stands_in_for: None,
                        handover: None,
                        detail: None,
                    });
                }
                for skipped in &planned.skipped {
                    let Some(spec) = workflow.role.get(&skipped.id) else {
                        continue;
                    };
                    steps.push(NewStep {
                        stage,
                        role: skipped.id.clone(),
                        provider: spec.provider,
                        model: spec.model.clone(),
                        profile: spec.profile,
                        workspace: spec.workspace(),
                        timeout_ms: spec.timeout_ms(),
                        retries: spec.retries,
                        status: StepStatus::Skipped,
                        stands_in_for: None,
                        handover: None,
                        detail: Some("none of its paths changed".to_owned()),
                    });
                }
            }
            db.add_steps(flow, &steps, self.now()).map_err(text)?;
        }

        if state.phase == Phase::Preparing {
            self.record(
                flow,
                &FlowEvent::Advanced {
                    to: Phase::Reviewing,
                },
            )?;
        }
        self.notice.changed(flow);

        let ctx = RunCtx {
            flow,
            project: review.project.as_str().to_owned(),
            worktree: review.worktree.clone(),
            worktree_path: worktree.path.clone(),
            branch: worktree.branch().map(|b| b.as_str().to_owned()),
            recipe: review.recipe.clone(),
            base: review.base.clone(),
            concurrency: workflow
                .recipe
                .get(&review.recipe)
                .and_then(|r| r.concurrency),
            workflow,
            dir,
            snapshot,
            snapshot_id,
            changed,
        };

        let stages: BTreeSet<u32> = db
            .steps(flow)
            .map_err(text)?
            .iter()
            .map(|s| s.stage)
            .collect();
        for stage in stages {
            if run.cancel.load(Ordering::SeqCst) || self.quitting() {
                break;
            }
            let pending: Vec<Step> = db
                .steps(flow)
                .map_err(text)?
                .into_iter()
                .filter(|s| s.stage == stage && s.status.wants_a_run())
                .collect();
            if !pending.is_empty() {
                self.run_stage(&ctx, run, &snapshots, pending);
            }
        }

        if self.quitting() || run.cancel.load(Ordering::SeqCst) {
            return Ok(());
        }
        // Ended meanwhile (cancelled from elsewhere): nothing more to record.
        if self.state(flow)?.end.is_some() {
            return Ok(());
        }
        self.record(
            flow,
            &FlowEvent::Advanced {
                to: Phase::Consolidated,
            },
        )?;
        Ok(())
    }

    fn run_stage(&self, ctx: &RunCtx, run: &Run, snapshots: &Snapshots, steps: Vec<Step>) {
        let checkout = StageCheckout::default();
        let workers = ctx
            .concurrency
            .map_or(steps.len(), |cap| (cap as usize).min(steps.len()))
            .max(1);
        let queue = Mutex::new(VecDeque::from(steps));
        std::thread::scope(|scope| {
            for slot in 0..workers {
                let worker = std::thread::Builder::new()
                    .name(format!("flow-{}-role-{slot}", ctx.flow))
                    .spawn_scoped(scope, || {
                        loop {
                            let next = queue.lock().pop_front();
                            let Some(step) = next else { break };
                            self.run_role(ctx, run, snapshots, &checkout, step);
                        }
                    });
                if let Err(e) = worker {
                    tracing::warn!(flow = %ctx.flow, error = %e, "could not start a role thread");
                }
            }
        });
        if let Some(Ok(opened)) = checkout.opened.lock().take()
            && let Err(e) = snapshots.close_checkout(&ctx.worktree_path, &opened)
        {
            tracing::warn!(flow = %ctx.flow, error = %e, "could not remove a snapshot checkout");
        }
    }

    fn checkout_path(
        ctx: &RunCtx,
        snapshots: &Snapshots,
        checkout: &StageCheckout,
    ) -> Result<PathBuf, String> {
        let mut opened = checkout.opened.lock();
        let result = opened.get_or_insert_with(|| {
            snapshots
                .open_checkout(&ctx.worktree_path, &ctx.snapshot, &ctx.dir)
                .map_err(|e| format!("the snapshot checkout could not be made: {e}"))
        });
        result
            .as_ref()
            .map(|c| c.path.clone())
            .map_err(Clone::clone)
    }

    fn run_role(
        &self,
        ctx: &RunCtx,
        run: &Run,
        snapshots: &Snapshots,
        checkout: &StageCheckout,
        step: Step,
    ) {
        let Ok(db) = self.db() else { return };
        let mut step = step;
        // A step run again by a person — a resume after an interruption or a failure — gets its
        // whole retry budget back. The failures it had are in its invocations.
        if step.failures > 0
            && let Ok(reset) = db.update_step(
                step.id,
                &StepChange {
                    failures: Some(0),
                    ..StepChange::default()
                },
                self.now(),
            )
        {
            step = reset;
        }
        loop {
            match self.attempts(ctx, run, snapshots, checkout, &mut step) {
                RoleEnd::Done | RoleEnd::Stopped => return,
                RoleEnd::Failed(reason) => {
                    let already = self.roles_run(ctx.flow);
                    let chain = ctx.workflow.fallback_chain(&step.role);
                    match after_failure(
                        step.failures.max(step.retries + 1),
                        step.retries,
                        &chain,
                        &already,
                    ) {
                        AfterFailure::Fallback(alternative) => {
                            match self.hand_over(ctx, &step, &alternative, &reason) {
                                Some(next) => step = next,
                                None => return,
                            }
                        }
                        AfterFailure::Retry | AfterFailure::Ask => {
                            self.give_up(ctx, &step, &reason);
                            return;
                        }
                    }
                }
                RoleEnd::Limit { resets_at, message } => {
                    let already = self.roles_run(ctx.flow);
                    let chain: Vec<(String, RoleProvider)> = ctx
                        .workflow
                        .fallback_chain(&step.role)
                        .into_iter()
                        .filter_map(|id| {
                            let provider = ctx.workflow.role.get(&id)?.provider;
                            Some((id, provider))
                        })
                        .collect();
                    match on_limit(step.provider, &chain, &already) {
                        OnLimit::SwitchTo(alternative) => {
                            self.file(&NewInboxItem {
                                flow: ctx.flow,
                                kind: InboxKind::Capacity,
                                subject: step.role.clone(),
                                title: format!(
                                    "{} reached its usage limit; `{}` ran `{alternative}` instead",
                                    step.provider, step.role
                                ),
                                body: format!(
                                    "{message}\n\nThe role's configured fallback `{alternative}` \
                                     took over from a written summary of where `{}` got to. \
                                     wtm never uses an API key.",
                                    step.role
                                ),
                            });
                            let why =
                                format!("{} reached its usage limit ({message})", step.provider);
                            match self.hand_over(ctx, &step, &alternative, &why) {
                                Some(next) => step = next,
                                None => return,
                            }
                        }
                        OnLimit::Wait => {
                            if !self.wait_for_capacity(ctx, run, &step, resets_at, &message) {
                                return;
                            }
                            if let Ok(fresh) = db.steps(ctx.flow)
                                && let Some(current) = fresh.into_iter().find(|s| s.id == step.id)
                            {
                                step = current;
                            }
                        }
                    }
                }
            }
        }
    }

    /// Roles this run has a step for, which a fallback is never chosen from twice.
    fn roles_run(&self, flow: FlowId) -> BTreeSet<String> {
        self.db()
            .and_then(|db| db.steps(flow).map_err(text))
            .map(|steps| steps.into_iter().map(|s| s.role).collect())
            .unwrap_or_default()
    }

    /// Attempt a role until it submits, runs out of retries, or something outside it stops it.
    fn attempts(
        &self,
        ctx: &RunCtx,
        run: &Run,
        snapshots: &Snapshots,
        checkout: &StageCheckout,
        step: &mut Step,
    ) -> RoleEnd {
        let Ok(db) = self.db() else {
            return RoleEnd::Stopped;
        };
        loop {
            if self.quitting() {
                return RoleEnd::Stopped;
            }
            if run.cancel.load(Ordering::SeqCst) {
                self.update_step(step, &StepChange::status(StepStatus::Cancelled));
                return RoleEnd::Stopped;
            }
            let attempt = db
                .invocations(ctx.flow)
                .map_or(0, |all| all.iter().filter(|i| i.step == step.id).count())
                + 1;
            let attempt = u32::try_from(attempt).unwrap_or(u32::MAX);
            match self.attempt(ctx, run, snapshots, checkout, step, attempt) {
                AttemptEnd::Submitted => return RoleEnd::Done,
                AttemptEnd::Quitting => return RoleEnd::Stopped,
                AttemptEnd::Cancelled => {
                    self.update_step(step, &StepChange::status(StepStatus::Cancelled));
                    return RoleEnd::Stopped;
                }
                AttemptEnd::Limit { resets_at, message } => {
                    return RoleEnd::Limit { resets_at, message };
                }
                AttemptEnd::Unrunnable(reason) => return RoleEnd::Failed(reason),
                AttemptEnd::Failed { reason, .. } => {
                    let failures = step.failures + 1;
                    step.failures = failures;
                    self.update_step(
                        step,
                        &StepChange {
                            failures: Some(failures),
                            ..StepChange::default()
                        }
                        .detail(reason.clone()),
                    );
                    if failures > step.retries {
                        return RoleEnd::Failed(reason);
                    }
                    tracing::info!(flow = %ctx.flow, role = %step.role, attempt, %reason, "retrying a role");
                }
            }
        }
    }

    /// One attempt: one session, opened, sent its turn, and read until it submits or stops.
    fn attempt(
        &self,
        ctx: &RunCtx,
        run: &Run,
        snapshots: &Snapshots,
        checkout: &StageCheckout,
        step: &mut Step,
        attempt: u32,
    ) -> AttemptEnd {
        let Ok(db) = self.db() else {
            return AttemptEnd::Quitting;
        };
        let spec = match ctx.role(&step.role) {
            Ok(spec) => spec.clone(),
            Err(reason) => return AttemptEnd::Unrunnable(reason),
        };
        let cwd = match step.workspace {
            Workspace::Snapshot => match Self::checkout_path(ctx, snapshots, checkout) {
                Ok(path) => path,
                Err(reason) => {
                    return AttemptEnd::Failed {
                        status: InvocationStatus::Failed,
                        reason,
                    };
                }
            },
            Workspace::Worktree => ctx.worktree_path.clone(),
        };
        let invocation = match db.start_invocation(
            &NewInvocation {
                flow: ctx.flow,
                step: step.id,
                attempt,
                role: step.role.clone(),
                provider: step.provider,
                model: step.model.clone(),
                profile: step.profile,
                snapshot: Some(ctx.snapshot_id),
                cwd: cwd.to_string_lossy().into_owned(),
            },
            self.now(),
        ) {
            Ok(invocation) => invocation,
            Err(e) => {
                return AttemptEnd::Failed {
                    status: InvocationStatus::Failed,
                    reason: text(e),
                };
            }
        };
        self.update_step(
            step,
            &StepChange {
                detail: Field::Clear,
                resets_at: Field::Clear,
                ..StepChange::status(StepStatus::Running)
            },
        );

        let (tx, rx) = mpsc::channel();
        let key = (ctx.flow, step.role.clone());
        self.live.lock().insert(
            key.clone(),
            Arc::new(Live {
                output: spec.output,
                tx: tx.clone(),
            }),
        );
        let end = self.converse(ctx, run, step, &spec, &cwd, invocation.id, tx, &rx);
        self.live.lock().remove(&key);

        if !matches!(end, AttemptEnd::Quitting) {
            let (status, detail) = match &end {
                AttemptEnd::Submitted => (InvocationStatus::Submitted, None),
                AttemptEnd::Failed { status, reason } => (*status, Some(reason.clone())),
                AttemptEnd::Unrunnable(reason) => (InvocationStatus::Failed, Some(reason.clone())),
                AttemptEnd::Limit { message, .. } => {
                    (InvocationStatus::LimitReached, Some(message.clone()))
                }
                AttemptEnd::Cancelled => (InvocationStatus::Cancelled, None),
                AttemptEnd::Quitting => unreachable!("matched above"),
            };
            let _ = db.update_invocation(
                invocation.id,
                &InvocationChange::ended(status, detail, self.now()),
                self.now(),
            );
            self.notice.changed(ctx.flow);
        }
        end
    }

    #[allow(clippy::too_many_arguments)]
    fn converse(
        &self,
        ctx: &RunCtx,
        run: &Run,
        step: &mut Step,
        spec: &RoleSpec,
        cwd: &Path,
        invocation: wtm_core::flow::InvocationId,
        tx: Sender<Signal>,
        rx: &Receiver<Signal>,
    ) -> AttemptEnd {
        let Ok(db) = self.db() else {
            return AttemptEnd::Quitting;
        };
        let facts = brief::BriefFacts {
            role: &step.role,
            spec,
            recipe: &ctx.recipe,
            worktree_path: &ctx.worktree_path.to_string_lossy(),
            branch: ctx.branch.as_deref(),
            base: &ctx.base,
            snapshot: &ctx.snapshot,
            changed: ctx.changed.as_deref(),
            handover: step.handover.as_deref(),
        };
        let session_spec = RoleSession {
            project: ctx.project.clone(),
            worktree: ctx.worktree.clone(),
            role: FlowRole {
                flow: ctx.flow,
                role: step.role.clone(),
                authority: step.profile,
            },
            provider: step.provider.id().to_owned(),
            model: step.model.clone(),
            effort: spec.effort.clone(),
            brief: brief::brief(&facts),
            resume: None,
            cwd: Some(cwd.to_path_buf()),
            skills: spec.skill_names().map(str::to_owned).collect(),
        };
        let session = match self
            .host
            .open(&session_spec, Arc::new(InvocationSink { tx }))
        {
            Ok(session) => session,
            Err(reason) => {
                return AttemptEnd::Failed {
                    status: InvocationStatus::Failed,
                    reason: format!("its session did not start: {reason}"),
                };
            }
        };
        let _ = db.update_invocation(
            invocation,
            &InvocationChange {
                status: Some(InvocationStatus::Running),
                session: Some(session.as_str().to_owned()),
                ..InvocationChange::default()
            },
            self.now(),
        );
        self.notice.changed(ctx.flow);

        let end = self.read_session(ctx, run, step, spec, &session, invocation, rx);
        if !matches!(end, AttemptEnd::Quitting) {
            self.host.close(&session);
        }
        end
    }

    /// Read one session until it hands in a result or stops without one.
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn read_session(
        &self,
        ctx: &RunCtx,
        run: &Run,
        step: &mut Step,
        spec: &RoleSpec,
        session: &SessionId,
        invocation: wtm_core::flow::InvocationId,
        rx: &Receiver<Signal>,
    ) -> AttemptEnd {
        let Ok(db) = self.db() else {
            return AttemptEnd::Quitting;
        };
        let started = self.clock.monotonic_ms();
        let deadline = started.saturating_add(step.timeout_ms);
        let skills_deadline = started.saturating_add(self.tuning.skills_wait_ms);
        let mut skills_checked = spec.skills.is_empty();
        let mut sent = false;
        let mut nudges = self.tuning.nudges;
        let mut submitted = false;
        let mut settle_at: Option<u64> = None;

        let say = |text: &str| -> Result<(), AttemptEnd> {
            self.host
                .send(session, text)
                .map_err(|reason| AttemptEnd::Failed {
                    status: InvocationStatus::Failed,
                    reason: format!("its session would not take the turn: {reason}"),
                })
        };
        if skills_checked {
            if let Err(end) = say(&brief::opening_turn(&step.role)) {
                return end;
            }
            sent = true;
        }

        loop {
            match rx.recv_timeout(Duration::from_millis(self.tuning.tick_ms)) {
                Ok(Signal::Submitted(submission)) => {
                    self.update_step(
                        step,
                        &StepChange {
                            result: Some(submission),
                            detail: Field::Clear,
                            ..StepChange::status(StepStatus::Done)
                        },
                    );
                    let _ = db.update_invocation(
                        invocation,
                        &InvocationChange {
                            status: Some(InvocationStatus::Submitted),
                            ..InvocationChange::default()
                        },
                        self.now(),
                    );
                    submitted = true;
                    settle_at = Some(self.clock.monotonic_ms() + self.tuning.settle_ms);
                }
                Ok(Signal::Event(event)) => match event {
                    AgentEvent::SkillsListed { skills } if !skills_checked => {
                        if let Some(problem) = missing_skill(&step.role, spec, &skills) {
                            return AttemptEnd::Unrunnable(problem);
                        }
                        skills_checked = true;
                        if let Err(end) = say(&brief::opening_turn(&step.role)) {
                            return end;
                        }
                        sent = true;
                    }
                    AgentEvent::SessionReady {
                        provider_session_id,
                        ..
                    } => {
                        let _ = db.update_invocation(
                            invocation,
                            &InvocationChange {
                                provider_session: Some(provider_session_id),
                                ..InvocationChange::default()
                            },
                            self.now(),
                        );
                    }
                    AgentEvent::TurnFinished { .. } if sent => {
                        if submitted {
                            return AttemptEnd::Submitted;
                        }
                        if nudges == 0 {
                            return AttemptEnd::Failed {
                                status: InvocationStatus::Failed,
                                reason: "it ended its turn without handing in a result".to_owned(),
                            };
                        }
                        nudges -= 1;
                        if let Err(end) = say(&brief::nudge(spec.output)) {
                            return end;
                        }
                    }
                    AgentEvent::LimitReached { message, resets_at } => {
                        if submitted {
                            return AttemptEnd::Submitted;
                        }
                        return AttemptEnd::Limit { resets_at, message };
                    }
                    AgentEvent::Failed { message } => {
                        if submitted {
                            return AttemptEnd::Submitted;
                        }
                        return AttemptEnd::Failed {
                            status: InvocationStatus::Failed,
                            reason: message,
                        };
                    }
                    AgentEvent::ApprovalRequested { request, .. } => {
                        self.file(&NewInboxItem {
                            flow: ctx.flow,
                            kind: InboxKind::Permission,
                            subject: step.role.clone(),
                            title: format!(
                                "`{}` asked for a permission its profile should not need",
                                step.role
                            ),
                            body: format!(
                                "{}\n\nA run session's profile decides what it may do, so this \
                                 prompt means the profile and the provider disagree. The role \
                                 waits until its timeout. Its session is behind the run's rail.",
                                describe(&request)
                            ),
                        });
                    }
                    _ => {}
                },
                Ok(Signal::Exited(summary)) => {
                    if submitted {
                        return AttemptEnd::Submitted;
                    }
                    return AttemptEnd::Failed {
                        status: InvocationStatus::Failed,
                        reason: format!("its session ended: {summary}"),
                    };
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    return AttemptEnd::Failed {
                        status: InvocationStatus::Failed,
                        reason: "the session's events stopped arriving".to_owned(),
                    };
                }
            }

            if self.quitting() {
                return AttemptEnd::Quitting;
            }
            if run.cancel.load(Ordering::SeqCst) {
                return AttemptEnd::Cancelled;
            }
            let now = self.clock.monotonic_ms();
            if submitted && settle_at.is_some_and(|at| now >= at) {
                return AttemptEnd::Submitted;
            }
            if !skills_checked && now >= skills_deadline {
                return AttemptEnd::Unrunnable(
                    wtm_core::flow::SkillProblem::Unlisted {
                        role: step.role.clone(),
                        provider: spec.provider,
                    }
                    .to_string(),
                );
            }
            if now >= deadline {
                if submitted {
                    return AttemptEnd::Submitted;
                }
                return AttemptEnd::Failed {
                    status: InvocationStatus::TimedOut,
                    reason: format!("it had no result after {}", minutes(step.timeout_ms)),
                };
            }
        }
    }

    /// Record that `step` is replaced by `alternative`, and make the alternative's step.
    fn hand_over(&self, ctx: &RunCtx, step: &Step, alternative: &str, why: &str) -> Option<Step> {
        let db = self.db().ok()?;
        let spec = ctx.workflow.role.get(alternative)?;
        let progress: Vec<String> = db
            .invocations(ctx.flow)
            .ok()?
            .into_iter()
            .filter(|i| i.step == step.id)
            .filter_map(|i| i.progress)
            .collect();
        let handover = brief::handover(&step.role, why, &progress);
        self.update_step(
            step,
            &StepChange::status(StepStatus::Replaced)
                .detail(format!("handed over to `{alternative}`: {why}")),
        );
        let added = db
            .add_steps(
                ctx.flow,
                &[NewStep {
                    stage: step.stage,
                    role: alternative.to_owned(),
                    provider: spec.provider,
                    model: spec.model.clone(),
                    profile: spec.profile,
                    workspace: spec.workspace(),
                    timeout_ms: spec.timeout_ms(),
                    retries: spec.retries,
                    status: StepStatus::Pending,
                    stands_in_for: Some(step.role.clone()),
                    handover: Some(handover),
                    detail: None,
                }],
                self.now(),
            )
            .ok()?;
        self.notice.changed(ctx.flow);
        added.into_iter().next()
    }

    /// A role is out of attempts and fallbacks: say so where you will see it.
    fn give_up(&self, ctx: &RunCtx, step: &Step, reason: &str) {
        self.update_step(
            step,
            &StepChange::status(StepStatus::Failed).detail(reason.to_owned()),
        );
        let tries = step.retries + 1;
        self.file(&NewInboxItem {
            flow: ctx.flow,
            kind: InboxKind::Failure,
            subject: step.role.clone(),
            title: format!("`{}` failed: {reason}", step.role),
            body: format!(
                "{} after {tries} attempt{}, and the role has no fallback left. The other roles' \
                 results are kept, and the review is incomplete until you retry it or accept it.",
                capitalise(reason),
                if tries == 1 { "" } else { "s" }
            ),
        });
    }

    /// Hold the role until its provider's limit resets. `false` when the run stopped meanwhile.
    fn wait_for_capacity(
        &self,
        ctx: &RunCtx,
        run: &Run,
        step: &Step,
        resets_at: Option<u64>,
        message: &str,
    ) -> bool {
        self.update_step(
            step,
            &StepChange {
                resets_at: Field::from(resets_at),
                ..StepChange::status(StepStatus::WaitingCapacity)
            }
            .detail(message.to_owned()),
        );
        self.hold_capacity(ctx.flow, step.provider, resets_at);
        let when = resets_at.map_or_else(
            || {
                "The provider did not say when it resets, so it waits until you resume it."
                    .to_owned()
            },
            |at| {
                format!(
                    "It resets at Unix time {at}; the role runs again by itself then, if wtm is open."
                )
            },
        );
        self.file(&NewInboxItem {
            flow: ctx.flow,
            kind: InboxKind::Capacity,
            subject: step.role.clone(),
            title: format!(
                "`{}` is paused: {} limit reached",
                step.role, step.provider
            ),
            body: format!(
                "{message}\n\n{when} Roles that finished keep their results. No fallback on another \
                 provider is configured for this role, so nothing switches by itself, and wtm never \
                 uses an API key."
            ),
        });

        let resume = resume_at(resets_at);
        loop {
            std::thread::sleep(Duration::from_millis(self.tuning.tick_ms));
            if self.quitting() {
                return false;
            }
            if run.cancel.load(Ordering::SeqCst) {
                self.update_step(step, &StepChange::status(StepStatus::Cancelled));
                return false;
            }
            if run.wake.swap(false, Ordering::SeqCst) {
                break;
            }
            if resume.is_some_and(|at| self.now() >= at) {
                break;
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
            let _ = db.resolve_inbox(ctx.flow, InboxKind::Capacity, Some(&step.role), self.now());
        }
        self.release_capacity(ctx.flow);
        self.notice.changed(ctx.flow);
        true
    }

    fn hold_capacity(&self, flow: FlowId, provider: RoleProvider, resets_at: Option<u64>) {
        let _held = self.holds.lock();
        let Ok(state) = self.state(flow) else { return };
        if state.end.is_none() && state.hold.is_none() {
            let _ = self.record(
                flow,
                &FlowEvent::Held {
                    hold: Hold::WaitingCapacity {
                        provider: provider.id().to_owned(),
                        resets_at,
                    },
                },
            );
        }
    }

    /// Release a capacity hold once its role runs again, and hold again for the next role still
    /// waiting, if there is one, so the hold always names a provider that is actually out.
    fn release_capacity(&self, flow: FlowId) {
        let _held = self.holds.lock();
        let Ok(state) = self.state(flow) else { return };
        if !matches!(state.hold, Some(Hold::WaitingCapacity { .. })) {
            return;
        }
        let _ = self.record(
            flow,
            &FlowEvent::Released {
                hold: HoldKind::WaitingCapacity,
            },
        );
        let still = self.db().ok().and_then(|db| {
            db.steps(flow)
                .ok()?
                .into_iter()
                .find(|s| s.status == StepStatus::WaitingCapacity)
        });
        if let Some(step) = still {
            let _ = self.record(
                flow,
                &FlowEvent::Held {
                    hold: Hold::WaitingCapacity {
                        provider: step.provider.id().to_owned(),
                        resets_at: step.resets_at,
                    },
                },
            );
        }
    }

    // ═══════════════════════════════ the run tools ═══════════════════════════════

    /// Answer a run tool call for a role this engine runs. `None` when the call is not for one of
    /// its review runs, which leaves it to the ledger.
    pub fn on_tool(&self, role: &FlowRole, call: &FlowCall) -> Option<Result<String, String>> {
        let live = self
            .live
            .lock()
            .get(&(role.flow, role.role.clone()))
            .cloned();
        let ours = live.is_some()
            || self
                .db()
                .ok()
                .and_then(|db| db.review(role.flow).ok().flatten())
                .is_some();
        if !ours {
            return None;
        }
        Some(match (call, live) {
            (FlowCall::Submit(submission), Some(live)) => {
                if submission.kind() != live.output {
                    return Some(Err(format!(
                        "this role hands in `{}`, not `{}`. Call `flow_submit` again with kind `{}`.",
                        wtm_core::flow::submission::kind_name(live.output),
                        wtm_core::flow::submission::kind_name(submission.kind()),
                        wtm_core::flow::submission::kind_name(live.output),
                    )));
                }
                let summary = submission.summary();
                if live.tx.send(Signal::Submitted(submission.clone())).is_err() {
                    return Some(Err(ended()));
                }
                Ok(format!(
                    "Received {summary}. That is your result; end your turn now. Calling \
                     `flow_submit` again replaces it."
                ))
            }
            (FlowCall::Progress(progress), Some(_)) => {
                self.note_progress(role, &progress.text);
                Ok("Noted.".to_owned())
            }
            (FlowCall::Ask(_), Some(_)) => Ok(
                "A review run does not stop for answers yet. Choose the most reasonable reading, \
                 say which you chose and why in the finding it affects, and carry on."
                    .to_owned(),
            ),
            (FlowCall::Submit(_) | FlowCall::Progress(_) | FlowCall::Ask(_), None) => Err(ended()),
            _ => return None,
        })
    }

    fn note_progress(&self, role: &FlowRole, line: &str) {
        let Ok(db) = self.db() else { return };
        let Ok(invocations) = db.invocations(role.flow) else {
            return;
        };
        if let Some(current) = invocations
            .iter()
            .rev()
            .find(|i| i.role == role.role && i.status.is_live())
        {
            let _ = db.update_invocation(
                current.id,
                &InvocationChange {
                    progress: Some(line.to_owned()),
                    ..InvocationChange::default()
                },
                self.now(),
            );
            self.notice.changed(role.flow);
        }
    }

    // ═══════════════════════════════ holds and recovery ═══════════════════════════════

    /// Run what is left of a review: the roles a quit cut off, a limit paused, or that failed.
    ///
    /// # Errors
    ///
    /// When the review has ended, is waiting on something a resume does not answer, or has nothing
    /// left to run.
    pub fn resume(self: &Arc<Self>, flow: FlowId) -> Result<(), String> {
        if let Some(run) = self.runs.lock().get(&flow) {
            // Still working: whatever waits for capacity runs now rather than at the reset.
            run.wake.store(true, Ordering::SeqCst);
            return Ok(());
        }
        let db = self.db()?;
        let review = db
            .review(flow)
            .map_err(text)?
            .ok_or_else(|| format!("run {flow} is not a review"))?;
        let state = self.state(flow)?;
        if let Some(end) = &state.end {
            return Err(format!("this review has already ended ({end:?})"));
        }
        let steps = db.steps(flow).map_err(text)?;
        if state.phase == Phase::Consolidated && !steps.iter().any(|s| s.status.wants_a_run()) {
            return Err(
                "every role of this review has its result; there is nothing to resume".to_owned(),
            );
        }
        let project = self.app.project(review.project.as_str()).map_err(text)?;
        if let Some(workflow) = &project.workflow
            && let Ok(plan) = wtm_core::flow::plan(workflow, &review.recipe, None)
        {
            let providers: Vec<&str> = plan
                .providers(workflow)
                .into_iter()
                .map(RoleProvider::id)
                .collect();
            self.host.preflight(&providers)?;
        }

        {
            let _held = self.holds.lock();
            match state.hold.as_ref().map(Hold::kind) {
                Some(kind @ (HoldKind::Interrupted | HoldKind::WaitingCapacity)) => {
                    self.record(flow, &FlowEvent::Released { hold: kind })?;
                }
                Some(other) => {
                    return Err(format!(
                        "this review is waiting on {other:?}, which a resume does not answer"
                    ));
                }
                None => {}
            }
        }
        if state.phase == Phase::Consolidated {
            self.record(
                flow,
                &FlowEvent::Advanced {
                    to: Phase::Reviewing,
                },
            )?;
        }
        for step in steps.iter().filter(|s| s.status.wants_a_run()) {
            let _ = db.update_step(
                step.id,
                &StepChange {
                    status: Some(StepStatus::Pending),
                    resets_at: Field::Clear,
                    ..StepChange::default()
                },
                self.now(),
            );
        }
        for kind in [
            InboxKind::Interrupted,
            InboxKind::Failure,
            InboxKind::Capacity,
        ] {
            let _ = db.resolve_inbox(flow, kind, None, self.now());
        }
        self.notice.changed(flow);
        self.spawn(flow)
    }

    /// Cancel a review from wherever it is.
    ///
    /// # Errors
    ///
    /// When the review has already ended.
    pub fn cancel(&self, flow: FlowId) -> Result<(), String> {
        if let Some(run) = self.runs.lock().get(&flow) {
            run.cancel.store(true, Ordering::SeqCst);
        }
        let db = self.db()?;
        {
            let _held = self.holds.lock();
            self.record(flow, &FlowEvent::Cancelled)?;
        }
        if !self.is_running(flow) {
            for step in db.steps(flow).map_err(text)? {
                if !step.status.is_settled() {
                    let _ = db.update_step(
                        step.id,
                        &StepChange::status(StepStatus::Cancelled),
                        self.now(),
                    );
                }
            }
        }
        for kind in [
            InboxKind::Interrupted,
            InboxKind::Failure,
            InboxKind::Capacity,
            InboxKind::Permission,
        ] {
            let _ = db.resolve_inbox(flow, kind, None, self.now());
        }
        self.notice.changed(flow);
        Ok(())
    }

    /// The app is quitting: every running review is interrupted, honestly, before the processes go.
    ///
    /// Called from the exit callback, so it only writes the record. The sessions are ended by the
    /// app's own exit path, and the run threads stop writing once they see the flag.
    pub fn shutdown(&self) {
        self.quitting.store(true, Ordering::SeqCst);
        let flows: Vec<FlowId> = self.runs.lock().keys().copied().collect();
        for flow in flows {
            self.interrupt(flow, InterruptCause::Quit);
        }
    }

    /// At launch: every open review whose processes are gone becomes Interrupted, keeping what
    /// finished and re-sending nothing. Returns the reviews it interrupted.
    ///
    /// Opens the database only if it exists, so a launch for someone who never used the Work view
    /// creates nothing.
    ///
    /// # Errors
    ///
    /// When the database cannot be read.
    pub fn recover(&self) -> Result<Vec<FlowId>, String> {
        if !self.app.files.exists(&self.app.config.paths().flow_db) {
            return Ok(Vec::new());
        }
        let db = self.db()?;
        let mut interrupted = Vec::new();
        for review in db.open_reviews().map_err(text)? {
            if self.is_running(review.flow) {
                continue;
            }
            let state = self.state(review.flow)?;
            // A quit already said so of a flow held `Interrupted`, and a consolidated one was not
            // running. `shutdown` turns a capacity wait into an interruption, so one still waiting
            // here was left by a crash, like a flow caught mid-phase with no hold at all.
            let gone = matches!(
                (&state.hold, state.phase),
                (Some(Hold::WaitingCapacity { .. }), _)
                    | (None, Phase::Resolving | Phase::Preparing | Phase::Reviewing)
            );
            if gone {
                self.interrupt(review.flow, InterruptCause::Crash);
                interrupted.push(review.flow);
            }
        }
        Ok(interrupted)
    }

    fn interrupt(&self, flow: FlowId, cause: InterruptCause) {
        let Ok(db) = self.db() else { return };
        let now = self.now();
        let steps = db.steps(flow).unwrap_or_default();
        let cut: Vec<&Step> = steps
            .iter()
            .filter(|s| matches!(s.status, StepStatus::Running | StepStatus::WaitingCapacity))
            .collect();
        for step in &cut {
            let _ = db.update_step(
                step.id,
                &StepChange {
                    resets_at: Field::Clear,
                    ..StepChange::status(StepStatus::Interrupted)
                },
                now,
            );
        }
        for invocation in db.invocations(flow).unwrap_or_default() {
            if invocation.status.is_live() {
                let _ = db.update_invocation(
                    invocation.id,
                    &InvocationChange::ended(
                        InvocationStatus::Interrupted,
                        Some(match cause {
                            InterruptCause::Quit => "wtm quit while it ran".to_owned(),
                            _ => "wtm stopped while it ran".to_owned(),
                        }),
                        now,
                    ),
                    now,
                );
            }
        }
        {
            let _held = self.holds.lock();
            let Ok(state) = self.state(flow) else { return };
            if state.end.is_some() {
                return;
            }
            if let Some(Hold::WaitingCapacity { .. }) = state.hold {
                let _ = self.record(
                    flow,
                    &FlowEvent::Released {
                        hold: HoldKind::WaitingCapacity,
                    },
                );
            }
            if let Err(e) = self.record(
                flow,
                &FlowEvent::Held {
                    hold: Hold::Interrupted { cause },
                },
            ) {
                tracing::warn!(%flow, error = %e, "could not record an interruption");
                return;
            }
        }
        let _ = db.resolve_inbox(flow, InboxKind::Capacity, None, now);
        let done = steps
            .iter()
            .filter(|s| s.status == StepStatus::Done)
            .count();
        let counted = steps
            .iter()
            .filter(|s| !matches!(s.status, StepStatus::Skipped | StepStatus::Replaced))
            .count();
        let names: Vec<String> = cut.iter().map(|s| format!("`{}`", s.role)).collect();
        let title = match cause {
            InterruptCause::Quit => "wtm quit during this review".to_owned(),
            _ => "wtm stopped unexpectedly during this review".to_owned(),
        };
        let cut_off = if names.is_empty() {
            "No role was mid-turn.".to_owned()
        } else {
            format!(
                "{} {} cut off, and nothing was sent again.",
                names.join(", "),
                if names.len() == 1 { "was" } else { "were" }
            )
        };
        self.file(&NewInboxItem {
            flow,
            kind: InboxKind::Interrupted,
            subject: "run".to_owned(),
            title,
            body: format!(
                "{done} of {counted} roles had finished; their results are kept. {cut_off} Resume \
                 runs the remaining roles against the same snapshot."
            ),
        });
        self.notice.changed(flow);
    }

    fn fail(&self, flow: FlowId, reason: &str) {
        {
            let _held = self.holds.lock();
            if self.state(flow).is_ok_and(|state| state.end.is_none())
                && let Err(e) = self.record(
                    flow,
                    &FlowEvent::Failed {
                        reason: reason.to_owned(),
                    },
                )
            {
                tracing::warn!(%flow, error = %e, "could not record a failed run");
            }
        }
        self.file(&NewInboxItem {
            flow,
            kind: InboxKind::Failure,
            subject: "run".to_owned(),
            title: "This review could not run".to_owned(),
            body: capitalise(reason),
        });
        self.notice.changed(flow);
    }

    // ═══════════════════════════════ reading ═══════════════════════════════

    /// Everything about one review run.
    ///
    /// # Errors
    ///
    /// When it does not exist or the database cannot be read.
    pub fn view(&self, flow: FlowId) -> Result<FlowView, String> {
        let db = self.db()?;
        let state = db
            .flow(flow)
            .map_err(text)?
            .ok_or_else(|| format!("no run {flow}"))?;
        let review = db
            .review(flow)
            .map_err(text)?
            .ok_or_else(|| format!("run {flow} is not a review"))?;
        let snapshot = match review.snapshot {
            Some(id) => db.snapshot(id).map_err(text)?,
            None => None,
        };
        Ok(FlowView {
            status: state.state.status(),
            flow: state,
            snapshot,
            steps: db.steps(flow).map_err(text)?,
            invocations: db.invocations(flow).map_err(text)?,
            inbox: db.inbox(Some(flow), None).map_err(text)?,
            running: self.is_running(flow),
            review,
        })
    }

    /// The newest review of a worktree, if it has one. Reads nothing, and creates no database, for
    /// someone who has never run a review.
    ///
    /// # Errors
    ///
    /// When the database cannot be read.
    pub fn latest(&self, project: &str, worktree: &str) -> Result<Option<FlowView>, String> {
        if !self.app.files.exists(&self.app.config.paths().flow_db) {
            return Ok(None);
        }
        let db = self.db()?;
        let project = ProjectId::from_root(Path::new(project));
        match db.reviews_of(&project, worktree).map_err(text)?.first() {
            Some(review) => self.view(review.flow).map(Some),
            None => Ok(None),
        }
    }

    /// Open inbox items, newest first, for one project or every one.
    ///
    /// # Errors
    ///
    /// When the database cannot be read.
    pub fn inbox(&self, project: Option<&str>) -> Result<Vec<InboxItem>, String> {
        if !self.app.files.exists(&self.app.config.paths().flow_db) {
            return Ok(Vec::new());
        }
        let project = project.map(|p| ProjectId::from_root(Path::new(p)));
        self.db()?.inbox(None, project.as_ref()).map_err(text)
    }

    /// Resolve an inbox item, as a person does.
    ///
    /// # Errors
    ///
    /// When the database cannot be written.
    pub fn resolve(&self, item: InboxItemId) -> Result<bool, String> {
        self.db()?
            .resolve_inbox_item(item, self.now())
            .map_err(text)
    }

    // ═══════════════════════════════ small helpers ═══════════════════════════════

    fn state(&self, flow: FlowId) -> Result<wtm_core::flow::FlowState, String> {
        self.db()?
            .flow(flow)
            .map_err(text)?
            .map(|f| f.state)
            .ok_or_else(|| format!("no run {flow}"))
    }

    fn record(&self, flow: FlowId, event: &FlowEvent) -> Result<wtm_core::flow::FlowState, String> {
        let recorded = self.db()?.record(flow, event, self.now()).map_err(text)?;
        self.notice.changed(flow);
        Ok(recorded.state)
    }

    fn update_step(&self, step: &Step, change: &StepChange) {
        if self.quitting() {
            return;
        }
        if let Ok(db) = self.db() {
            match db.update_step(step.id, change, self.now()) {
                Ok(_) => self.notice.changed(step.flow),
                Err(e) => tracing::warn!(step = %step.id, error = %e, "could not update a step"),
            }
        }
    }

    fn file(&self, item: &NewInboxItem) {
        let Ok(db) = self.db() else { return };
        match db.file_inbox(item, self.now()) {
            Ok(Some(filed)) => self.notice.filed(&filed),
            Ok(None) => {}
            Err(e) => tracing::warn!(flow = %item.flow, error = %e, "could not file an inbox item"),
        }
    }
}

/// The skill problem that stops `role`, given what its provider listed, or `None`.
///
/// Compared with the provider's own list, never wtm's reading of skill directories, and the
/// problem is R2's [`SkillProblem`](wtm_core::flow::SkillProblem), so it names the provider. A
/// Claude run's skills are listed under the plugin `flow_session` wrote them into; that prefix is
/// taken off before comparing, and nothing else is.
fn missing_skill(role: &str, spec: &RoleSpec, listed: &[AgentSkill]) -> Option<String> {
    let prefix = format!("{}:", wtm_agent::authority::RUN_SKILL_PLUGIN);
    let names: BTreeSet<&str> = listed
        .iter()
        .map(|skill| {
            let name = skill.name.trim_start_matches('/');
            match spec.provider {
                RoleProvider::Claude => name.strip_prefix(prefix.as_str()).unwrap_or(name),
                RoleProvider::Codex => name,
            }
        })
        .collect();
    spec.skill_names()
        .find(|skill| !names.contains(skill))
        .map(|skill| {
            wtm_core::flow::SkillProblem::Missing {
                role: role.to_owned(),
                provider: spec.provider,
                skill: skill.to_owned(),
            }
            .to_string()
        })
}

fn describe(request: &ApprovalRequest) -> String {
    match request {
        ApprovalRequest::Command { command, .. } => format!("It asked to run `{command}`."),
        ApprovalRequest::FileChange { .. } => "It asked to change files.".to_owned(),
        ApprovalRequest::Permissions { summary, .. } => format!("It asked for: {summary}."),
        ApprovalRequest::PlanReview { .. } => "It asked to leave planning mode.".to_owned(),
        ApprovalRequest::ToolInput { tool, .. } => format!("`{tool}` asked for input."),
        ApprovalRequest::UserInput { .. } => "It asked you a question.".to_owned(),
    }
}

fn ended() -> String {
    "This attempt has ended, so nothing more is accepted from it. End your turn.".to_owned()
}

fn minutes(ms: u64) -> String {
    let minutes = ms / 60_000;
    if minutes >= 1 {
        format!("{minutes} min")
    } else {
        format!("{} s", ms / 1000)
    }
}

fn capitalise(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}
