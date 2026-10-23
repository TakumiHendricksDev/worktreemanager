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
//! 5. **Consolidated**, once every stage has run. Each role's result is kept as it was submitted,
//!    and every time one is, the run's findings are consolidated again (`findings.rs`): reports
//!    about the same place for the same reason merged, every report kept as evidence.
//!
//! A consolidated review can be **rerun** against a new snapshot when the worktree has moved on:
//! only the roles whose files changed, or all of them (`findings.rs`). The run's earlier results
//! stay until a rerun role replaces its own. A teammate's pull request is followed to its new
//! heads the same way, after its review worktree is brought to the head (`pull_request.rs`).
//!
//! # Holds
//!
//! - **`WaitingCapacity`** when a provider reports its usage limit and none of the role's configured
//!   fallbacks can take over. The role waits for the reset the provider named and runs again by
//!   itself while the app is open and the Mac awake; after a sleep or a relaunch it asks the
//!   provider first, and an unknown reset waits for you. Meanwhile you can run it once elsewhere or
//!   finish without it (`capacity.rs`). Other roles keep running, and a quit leaves the wait to be
//!   picked up at the next launch, since no turn was cut off.
//! - **`Interrupted`** when the app quits mid-run ([`Engine::shutdown`]) or is found at launch to
//!   have stopped without doing so ([`Engine::recover`]). Completed roles keep their results and
//!   nothing is re-sent: a turn sent twice can do its work twice. [`Engine::resume`] runs what is
//!   left.
//!
//! # One notification owner
//!
//! A run session's own events still reach its pane behind the rail, but the window does not
//! announce them (`sessions.svelte.ts` skips a pane with a `flow`). What needs you — a question, a
//! provider's permission prompt, a role that failed for good, a limit, an interruption, a review
//! that is ready to read — is an inbox item here, filed once per `(target, kind)` while it is open,
//! and the inbox is what notifies. Routine progress (`flow_progress`, a role starting or finishing)
//! changes the run and says nothing. An item is closed by what it is about being settled — an
//! answer, a resume, a rerun — or by a person dismissing it, never by a notification going away.
//!
//! # Threads, not tasks
//!
//! Each run is a thread, and each running role a scoped thread inside it, for the reason
//! ARCHITECTURE §3 gives: everything here blocks — git, SQLite, waiting on a session — and a role is
//! a handful of long waits, not thousands of short ones. Waits are polled against the [`Clock`]
//! port, so a test drives every timeout, retry and reset by moving a fake clock.

pub mod brief;
pub mod capacity;
pub mod checks;
pub mod commands;
pub mod comments;
pub mod design_commands;
pub mod designer;
pub mod env;
pub mod findings;
pub mod fix;
pub mod host;
pub mod moves;
pub mod outbox;
pub mod plan_commands;
pub mod planner;
pub mod publish;
pub mod pull_request;
pub mod routes;
pub mod runtime;
pub mod start;
pub mod start_commands;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Weak};
use std::time::Duration;

use parking_lot::Mutex;
use serde::Serialize;
use wtm_agent::session::AgentSink;
use wtm_appdb::AppDb;
use wtm_core::flow::{
    AfterFailure, Answer, CapacityChoice, CheckRun, CheckSpec, ContextLink, CreditProblem, Field,
    FindingRecord, Flow, FlowDir, FlowEvent, FlowId, FlowKind, Hold, HoldKind, InboxItem,
    InboxItemId, InboxKind, InboxTarget, InterruptCause, Invocation, InvocationChange,
    InvocationStatus, NewInboxItem, NewInvocation, NewProviderSwitch, NewQuestion, NewReviewRun,
    NewStep, NewWorkItem, OnLimit, OutputKind, Phase, Question, QuestionId, ReviewRun, RoleKind,
    RoleProvider, RoleSpec, Runner, ScratchLabel, SkillProblem, Snapshot, SnapshotId, Status, Step,
    StepChange, StepId, StepStatus, Submission, SwitchReason, SwitchedBy, WorkLink, WorkflowSpec,
    Workspace, after_failure, credit_problem, on_limit,
};
use wtm_core::model::{AgentEvent, AgentSkill, ApprovalRequest, ExitOutcome, ProjectId, SessionId};
use wtm_core::ports::clock::Clock;
use wtm_core::ports::{FixStore, FlowStore, FlowStoreError, InboxStore, LeaseStore, RunStore};
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
    /// Who holds run-session places and who waits changed. Not an inbox item: a queue is routine,
    /// and says so where it is shown without notifying anyone.
    fn seats(&self, view: &capacity::SeatsView) {
        let _ = view;
    }
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

/// Why a review did not start, rerun or resume. Nothing was recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Refusal {
    pub message: String,
    /// Providers that would not bill a subscription, when that is the reason. The Reviews tab says
    /// "can't start: billing" and names them; for every other reason this is empty.
    pub billing: Vec<String>,
    /// Roles that would run a model billing usage credits without having opted in, when that is
    /// the reason.
    pub credits: Vec<CreditProblem>,
}

impl From<String> for Refusal {
    fn from(message: String) -> Self {
        Self {
            message,
            billing: Vec::new(),
            credits: Vec::new(),
        }
    }
}

impl From<&str> for Refusal {
    fn from(message: &str) -> Self {
        Self::from(message.to_owned())
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
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
    /// The run's findings that are still reported, dismissed ones included.
    pub findings: Vec<FindingRecord>,
    /// Every check the app ran, or skipped, for a role of this run, oldest first.
    pub checks: Vec<CheckRun>,
    /// The checks this run's roles may ask for, from your config, in the order they are listed.
    /// One with no record in `checks` was not run, which is what the Reviews tab says of it.
    pub expected_checks: Vec<findings::CheckChoice>,
    /// What the roles were given to judge by: rule revisions, instruction sources, skills.
    pub applied: findings::Applied,
    /// The pull request the review is of, for a teammate's; `None` for a review of your own work.
    pub pull_request: Option<pull_request::PullRequestReview>,
    /// The run's local fixes, oldest first. The last is the current one.
    pub fixes: Vec<wtm_core::flow::Fix>,
    /// Who holds the reviewed worktree now: a writer, or a review reading it.
    pub leases: Vec<wtm_core::flow::Lease>,
    /// The writer roles your config has, which a local fix can be given to.
    pub writers: Vec<fix::WriterChoice>,
    /// The pictures runtime roles took, and wtm took for them, oldest first. Findings cite them as
    /// `s<id>` in their evidence.
    pub screenshots: Vec<wtm_core::flow::Screenshot>,
    /// What Start environment and run would start and run now, while a check is blocked or a
    /// runtime role waits for its services. `None` when nothing is.
    pub pending_start: Option<runtime::StartView>,
    /// The run's roles waiting for a run-session place, with the cap each waits for.
    pub queued: Vec<wtm_core::flow::QueuedSeat>,
    /// The places the run's sessions hold now.
    pub holding: Vec<wtm_core::flow::Seat>,
    /// Every time a role's work moved to another role or provider, oldest first, with what the
    /// new runner was handed.
    pub switches: Vec<wtm_core::flow::ProviderSwitch>,
    /// The role waiting at its provider's limit, when one is, and what can be done about it.
    pub capacity: Option<capacity::CapacityView>,
}

/// One run's controls, shared by its thread and its roles.
#[derive(Debug, Default)]
struct Run {
    cancel: AtomicBool,
    /// Set by a resume while the run is still working: roles waiting for capacity run now.
    wake: AtomicBool,
    /// The reviewer lease a review drive holds on its worktree while it reads it, released when
    /// the drive ends. A fix's writer lease is the fix's, not the drive's (`fix.rs`).
    lease: Mutex<Option<wtm_core::flow::LeaseId>>,
    /// Set when you decline starting the environment a runtime role waits for (`runtime.rs`).
    declined: AtomicBool,
    /// What you chose for roles waiting at a usage limit, taken by each role's own thread.
    choices: Mutex<BTreeMap<StepId, CapacityChoice>>,
}

/// What a session told the role thread waiting on it.
#[derive(Debug)]
enum Signal {
    Event(AgentEvent),
    Exited(String),
    Submitted(Submission),
    /// A planner asked a question with `flow_ask`, and the run now waits for the answer.
    Asked(QuestionId),
    /// A designer published this revision of its run's design with `prototype_publish`.
    Published(u32),
}

/// A role attempt that is running, as the run tools find it.
#[derive(Debug)]
struct Live {
    output: OutputKind,
    tx: Sender<Signal>,
    step: StepId,
    invocation: wtm_core::flow::InvocationId,
    /// Where the role reads, which is where a check it asks for runs.
    cwd: PathBuf,
    /// The snapshot tree the role reads, which a check it asks for is recorded against.
    tree_sha: String,
    /// The checks it may ask for, by id.
    checks: BTreeMap<String, CheckSpec>,
    dir: FlowDir,
    /// What a writer fixing findings is held to, for a fix's role; `None` for a reviewer.
    fix: Option<Arc<fix::FixLive>>,
    /// What a planner's turn is, for a planner; `None` for a review's role.
    planner: Option<Arc<planner::PlannerLive>>,
    /// What a designer's turn answers, for a designer; `None` for every other role.
    designer: Option<Arc<designer::DesignerLive>>,
    /// The project and worktree, which a check that needs services asks the gate about.
    project: String,
    worktree: String,
    /// The workflow's `start_services`, which a check without its own setting follows.
    start_services: Option<wtm_core::flow::StartServices>,
    /// The incognito pane a runtime role drives, by id.
    browser: Option<String>,
}

/// The engine's ear on one session: the events it acts on, forwarded to the role thread.
struct InvocationSink {
    tx: Sender<Signal>,
}

impl AgentSink for InvocationSink {
    fn on_event(&self, _session: &SessionId, event: &AgentEvent) {
        // The words a turn says are wanted too: a planner's reply in a discussion is a message,
        // not a submission, and the role thread keeps the last one. A review ignores them.
        let wanted = matches!(
            event,
            AgentEvent::SessionReady { .. }
                | AgentEvent::SkillsListed { .. }
                | AgentEvent::TurnStarted { .. }
                | AgentEvent::TurnFinished { .. }
                | AgentEvent::ToolStarted { .. }
                | AgentEvent::MessageDelta { .. }
                | AgentEvent::Message { .. }
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
    Unrunnable(String, Option<SkillProblem>),
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
    Failed(String, Option<SkillProblem>),
    Limit {
        resets_at: Option<u64>,
        message: String,
    },
}

/// What a run reads while it works, resolved once.
struct RunCtx {
    flow: FlowId,
    project: String,
    project_id: ProjectId,
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
    /// The repository instruction sources the project names, or the default ones.
    patterns: Vec<String>,
    /// The pull request under review, for a teammate's, as the forge last described it.
    pull_request: Option<wtm_core::flow::PullRequest>,
    /// When this round follows the pull request to a new head: the head it moved on from.
    revision: Option<(String, String)>,
    /// The fix this drive's writer is making, when it is a fix's drive.
    fix: Option<Arc<fix::FixLive>>,
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
    /// This engine, for the paths that are handed only `&self` and must start a thread: an answer
    /// that resumes a planner, a worktree that became ready to plan.
    me: Weak<Engine>,
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
    /// Held across reading a run's findings and saving their consolidation, so two roles
    /// submitting at once consolidate one after the other rather than each over a stale list.
    consolidating: Mutex<()>,
    /// Numbers the scratch indexes a staleness check uses, so two checks of one run never share
    /// an `index.lock`.
    scratch: AtomicU64,
    quitting: AtomicBool,
    /// The publications this process is sending or checking now (`outbox.rs`). One sender per
    /// publication; an operation left executing by a process that is gone is in nobody's entry.
    publishing: Mutex<BTreeMap<wtm_core::flow::ApprovalId, wtm_core::flow::Activity>>,
    /// A debug build's `WTM_DEV_PUBLISH_FAULT`, for click-throughs. `None` in a release build.
    fault: Option<outbox::Fault>,
    /// Whether a once-only fault has happened in this process.
    fired: AtomicBool,
    /// Start Work's lock and the worktrees being made now (`start.rs`).
    starts: start::Starts,
    /// The environment supervisor, attached once it exists. Weak, because it holds this engine.
    environment: Mutex<Option<std::sync::Weak<dyn runtime::Environment>>>,
    /// Places for run sessions, across every run (`capacity.rs`).
    seats: capacity::Seats,
    /// A debug build's `WTM_DEV_LIMIT`, for click-throughs. `None` in a release build.
    limit_fault: Option<capacity::LimitFault>,
    /// What the last recheck of a waiting role's provider said, by step, until it stops waiting.
    rechecks: Mutex<BTreeMap<StepId, String>>,
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
        Arc::new_cyclic(|me| Self {
            me: me.clone(),
            app,
            host,
            clock,
            notice,
            tuning,
            runs: Mutex::new(BTreeMap::new()),
            live: Mutex::new(BTreeMap::new()),
            holds: Mutex::new(()),
            starting: Mutex::new(()),
            consolidating: Mutex::new(()),
            scratch: AtomicU64::new(0),
            quitting: AtomicBool::new(false),
            publishing: Mutex::new(BTreeMap::new()),
            fault: outbox::Fault::from_env(),
            fired: AtomicBool::new(false),
            starts: start::Starts::default(),
            environment: Mutex::new(None),
            seats: capacity::Seats::default(),
            limit_fault: capacity::LimitFault::from_env(),
            rechecks: Mutex::new(BTreeMap::new()),
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
    pub fn start(self: &Arc<Self>, req: &StartReview) -> Result<FlowId, Refusal> {
        let _one = self.starting.lock();
        let project = self.app.project(&req.project).map_err(text)?;
        let worktree = self
            .app
            .worktree(&project, &req.worktree)
            .map_err(|e| format!("that worktree is not available: {e}"))?;
        let workflow = project.workflow.as_ref().ok_or_else(|| {
            Refusal::from(
                "this project has no review recipes; add `[workflow]` roles and a recipe to your \
                 own config for it",
            )
        })?;
        let plan = wtm_core::flow::plan(workflow, &req.recipe, None).map_err(text)?;
        refuse_credits(workflow, &req.recipe)?;
        let providers: Vec<&str> = plan
            .providers(workflow)
            .into_iter()
            .map(RoleProvider::id)
            .collect();
        self.host.preflight(&providers)?;
        let path = worktree.path.to_string_lossy().into_owned();
        // A writer has the worktree to itself until its fix is done: say so before anything is
        // recorded. The reviewer lease `spawn` takes refuses it again, in the store.
        self.refuse_if_written(&path)?;
        // A pull request's review is measured from its own base, whichever button started it.
        let asked = req
            .base
            .clone()
            .or_else(|| self.pull_request_base(&project, &path));
        let base = self.resolve_base(&worktree.path, asked.as_deref())?;

        let db = self.db()?;
        let project_id = ProjectId::from_root(&project.root);
        let now = self.now();
        let existing = db
            .work_item_for_worktree(&project_id, &path)
            .map_err(text)?;
        let item = if let Some(id) = existing {
            db.work_item(id)
                .map_err(text)?
                .ok_or_else(|| Refusal::from(format!("work item {id} has gone")))?
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
                    .ok_or_else(|| Refusal::from(format!("flow {existing} has gone")))?;
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
                // Its items are about a review that is no longer the current one.
                self.settle(existing, &ALL_KINDS);
                self.notice.changed(existing);
                db.start_flow(item.id, kind, now).map_err(text)?
            }
            Err(e) => return Err(text(e).into()),
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
        if let Err(reason) = self.spawn(flow.id) {
            // Lost a race for the worktree, or no thread: the run says so rather than sitting
            // in Resolving with nothing behind it.
            self.fail(flow.id, &reason);
            return Err(reason.into());
        }
        Ok(flow.id)
    }

    /// The ref a review of a worktree would be measured from when none is named.
    ///
    /// # Errors
    ///
    /// When the worktree cannot be found, or none of the usual base names resolve in it.
    pub fn default_base(&self, project: &str, worktree: &str) -> Result<String, String> {
        let project = self.app.project(project).map_err(text)?;
        let worktree = self
            .app
            .worktree(&project, worktree)
            .map_err(|e| format!("that worktree is not available: {e}"))?;
        self.resolve_base(&worktree.path, None)
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
        // A review reads its worktree for as long as it runs, so it holds a reviewer lease that
        // long, and a writer holding the worktree refuses it here, in the store's transaction,
        // whatever the checks before this one saw. A fix's drive runs under the fix's own lease.
        if !self
            .state(flow)
            .is_ok_and(|state| state.phase == Phase::Fixing)
        {
            match self.take_reviewer_lease(flow) {
                Ok(lease) => *run.lease.lock() = lease,
                Err(reason) => {
                    self.runs.lock().remove(&flow);
                    return Err(reason);
                }
            }
        }
        let engine = Arc::clone(self);
        let thread_run = Arc::clone(&run);
        let spawned = std::thread::Builder::new()
            .name(format!("flow-{flow}"))
            .spawn(move || {
                // Removed however the drive ends, a panic included, or the run could never be
                // resumed in this app; and its reviewer lease with it, or the worktree could never
                // be fixed.
                struct Done<'a>(&'a Engine, FlowId, &'a Run);
                impl Drop for Done<'_> {
                    fn drop(&mut self) {
                        if let Some(lease) = self.2.lease.lock().take() {
                            self.0
                                .release_lease(lease, wtm_core::flow::LeaseRelease::Completed);
                        }
                        self.0.runs.lock().remove(&self.1);
                        self.0.notice.changed(self.1);
                    }
                }
                let run = thread_run;
                let _done = Done(&engine, flow, &run);
                engine.drive(flow, &run);
            });
        if let Err(e) = spawned {
            if let Some(lease) = run.lease.lock().take() {
                self.release_lease(lease, wtm_core::flow::LeaseRelease::Failed);
            }
            self.runs.lock().remove(&flow);
            return Err(format!("wtm could not start the run's thread: {e}"));
        }
        Ok(())
    }

    // ═══════════════════════════════ driving ═══════════════════════════════

    fn drive(&self, flow: FlowId, run: &Run) {
        if self
            .state(flow)
            .is_ok_and(|state| state.phase == Phase::Fixing)
        {
            if let Err(reason) = self.drive_fix(flow, run) {
                if self.quitting() {
                    return;
                }
                tracing::warn!(%flow, %reason, "a local fix failed");
                self.fail_fix(flow, &reason);
            }
            return;
        }
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
            project_id: review.project.clone(),
            worktree: review.worktree.clone(),
            worktree_path: worktree.path.clone(),
            branch: worktree.branch().map(|b| b.as_str().to_owned()),
            recipe: review.recipe.clone(),
            base: review.base.clone(),
            concurrency: workflow
                .recipe
                .get(&review.recipe)
                .and_then(|r| r.concurrency),
            patterns: crate::knowledge::patterns(&project).0,
            pull_request: db
                .flow(flow)
                .ok()
                .flatten()
                .filter(|state| state.state.kind == FlowKind::ReviewPr)
                .and_then(|state| {
                    use wtm_core::ports::PullReviewStore as _;
                    db.mirrored_pr(state.work_item).ok().flatten()
                })
                .map(|mirror| mirror.pull_request),
            revision: self.revision(flow, &snapshot),
            fix: None,
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
        // Each submission consolidated as it arrived; this catches a run in which none did, so a
        // rerun whose roles all failed still supersedes nothing it should not.
        self.consolidate(flow);
        self.record(
            flow,
            &FlowEvent::Advanced {
                to: Phase::Consolidated,
            },
        )?;
        self.file_ready(flow);
        Ok(())
    }

    /// The review is consolidated: say so once, with how much it found and the worst of it.
    fn file_ready(&self, flow: FlowId) {
        let Ok(db) = self.db() else { return };
        let findings = db.findings(flow).unwrap_or_default();
        let standing: Vec<&wtm_core::flow::Finding> = findings
            .iter()
            .map(|record| &record.finding)
            .filter(|finding| finding.dismissed.is_none())
            .collect();
        let steps = db.steps(flow).unwrap_or_default();
        let failed: Vec<&str> = steps
            .iter()
            .filter(|s| s.status == StepStatus::Failed)
            .map(|s| s.role.as_str())
            .collect();
        let (title, body) = wtm_core::flow::review_ready(&standing, &failed);
        self.file(&NewInboxItem {
            flow,
            kind: InboxKind::ReviewReady,
            target: InboxTarget::Flow,
            title,
            body,
        });
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
        // A wait picked up again: at launch, or by a run driven again while one of its roles still
        // waited. Whatever the reset said before, the provider is asked before anything runs.
        if step.status == StepStatus::WaitingCapacity {
            let message = step
                .detail
                .clone()
                .unwrap_or_else(|| format!("{} had reached its usage limit", step.provider));
            match self.wait_for_capacity(ctx, run, &step, step.resets_at, &message, true) {
                capacity::Waited::Resume => match self.fresh(&step) {
                    Some(current) => step = current,
                    None => return,
                },
                capacity::Waited::Skipped | capacity::Waited::Stopped => return,
            }
        }
        loop {
            match self.attempts(ctx, run, snapshots, checkout, &mut step) {
                RoleEnd::Done | RoleEnd::Stopped => return,
                RoleEnd::Failed(reason, problem) => {
                    let already = self.roles_run(ctx.flow);
                    let spent = self.spent_providers(ctx);
                    let chain: Vec<String> = ctx
                        .workflow
                        .fallback_chain(origin(&step))
                        .into_iter()
                        .filter(|id| {
                            ctx.workflow
                                .role
                                .get(id)
                                .is_some_and(|spec| !spent.contains(&spec.provider))
                        })
                        .collect();
                    match after_failure(
                        step.failures.max(step.retries + 1),
                        step.retries,
                        &chain,
                        &already,
                    ) {
                        AfterFailure::Fallback(alternative) => {
                            match self.hand_over(
                                ctx,
                                &step,
                                &alternative,
                                SwitchReason::Failure,
                                &reason,
                            ) {
                                Some(next) => step = next,
                                None => return,
                            }
                        }
                        AfterFailure::Retry | AfterFailure::Ask => {
                            self.give_up(ctx, &step, &reason, problem);
                            return;
                        }
                    }
                }
                RoleEnd::Limit { resets_at, message } => {
                    let already = self.roles_run(ctx.flow);
                    let spent = self.spent_providers(ctx);
                    // The chain is the role's own, whoever is running it now: when its first
                    // alternative also runs out, the next comes from the same list. An
                    // alternative on a provider that has reached a limit in this round would
                    // only reach it again.
                    let chain: Vec<(String, RoleProvider)> = ctx
                        .workflow
                        .fallback_chain(origin(&step))
                        .into_iter()
                        .filter_map(|id| {
                            let provider = ctx.workflow.role.get(&id)?.provider;
                            Some((id, provider))
                        })
                        .filter(|(_, provider)| !spent.contains(provider))
                        .collect();
                    match on_limit(step.provider, &chain, &already) {
                        OnLimit::SwitchTo(alternative) => {
                            let to = ctx.workflow.role.get(&alternative).map_or_else(
                                || "another provider".to_owned(),
                                |spec| spec.provider.to_string(),
                            );
                            self.file(&NewInboxItem {
                                flow: ctx.flow,
                                kind: InboxKind::Capacity,
                                target: on(&step),
                                title: format!(
                                    "{} reached its usage limit; `{alternative}` took over `{}` \
                                     on {to}",
                                    step.provider,
                                    origin(&step)
                                ),
                                body: format!(
                                    "{message}\n\n`{alternative}` is the next role in `{}`'s \
                                     configured fallback chain, and it took over from a written \
                                     summary of where the run got to: the step, the snapshot, the \
                                     diff's shape, the findings so far and your answers. No \
                                     conversation was carried over. wtm never uses an API key.",
                                    origin(&step)
                                ),
                            });
                            let why =
                                format!("{} reached its usage limit ({message})", step.provider);
                            match self.hand_over(
                                ctx,
                                &step,
                                &alternative,
                                SwitchReason::Limit,
                                &why,
                            ) {
                                Some(next) => step = next,
                                None => return,
                            }
                        }
                        OnLimit::Wait => {
                            match self
                                .wait_for_capacity(ctx, run, &step, resets_at, &message, false)
                            {
                                capacity::Waited::Resume => match self.fresh(&step) {
                                    Some(current) => step = current,
                                    None => return,
                                },
                                capacity::Waited::Skipped | capacity::Waited::Stopped => return,
                            }
                        }
                    }
                }
            }
        }
    }

    /// The providers that reported a usage limit in this round of the run: attempts against its
    /// current snapshot. A later round starts with every provider worth trying again.
    fn spent_providers(&self, ctx: &RunCtx) -> BTreeSet<RoleProvider> {
        self.spent_in(ctx.flow, Some(ctx.snapshot_id))
    }

    /// [`Self::spent_providers`], for a run read from its record rather than its thread.
    fn spent_in(&self, flow: FlowId, snapshot: Option<SnapshotId>) -> BTreeSet<RoleProvider> {
        self.db()
            .and_then(|db| db.invocations(flow).map_err(text))
            .unwrap_or_default()
            .into_iter()
            .filter(|i| i.status == InvocationStatus::LimitReached && i.snapshot == snapshot)
            .map(|i| i.provider)
            .collect()
    }

    /// The step as the record has it now, after something outside this thread changed it.
    fn fresh(&self, step: &Step) -> Option<Step> {
        self.db()
            .ok()?
            .steps(step.flow)
            .ok()?
            .into_iter()
            .find(|s| s.id == step.id)
    }

    /// The answered questions `role` asked in any review of this worktree, oldest first.
    ///
    /// Any review, not just this run: Run again starts a new run of the same work, and the
    /// question card promises the answer reaches the role "when it runs again". An answer is about
    /// the work, which the worktree's runs share, not about one attempt at reviewing it.
    fn answers_for(&self, ctx: &RunCtx, role: &str) -> Vec<Question> {
        let Ok(db) = self.db() else {
            return Vec::new();
        };
        let mut flows: Vec<FlowId> = db
            .reviews_of(&ctx.project_id, &ctx.worktree)
            .unwrap_or_default()
            .into_iter()
            .map(|review| review.flow)
            .collect();
        if !flows.contains(&ctx.flow) {
            flows.push(ctx.flow);
        }
        flows.sort();
        flows
            .into_iter()
            .flat_map(|flow| db.questions(flow).unwrap_or_default())
            .filter(|q| q.role.as_deref() == Some(role) && q.answer.is_some())
            .collect()
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
        // A runtime role exercises the running app, so it starts only once every service it needs
        // answers: waiting for you to start them if it has to (`runtime.rs`).
        let mut services = Vec::new();
        if let Ok(spec) = ctx.role(&step.role)
            && spec.kind == RoleKind::Runtime
        {
            match self.runtime_ready(ctx, run, step, spec) {
                runtime::EnvReady::Up(up) => services = up,
                runtime::EnvReady::Stopped => return RoleEnd::Stopped,
            }
        }
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
            match self.attempt(ctx, run, snapshots, checkout, step, attempt, &services) {
                AttemptEnd::Submitted => return RoleEnd::Done,
                AttemptEnd::Quitting => return RoleEnd::Stopped,
                AttemptEnd::Cancelled => {
                    self.update_step(step, &StepChange::status(StepStatus::Cancelled));
                    return RoleEnd::Stopped;
                }
                AttemptEnd::Limit { resets_at, message } => {
                    return RoleEnd::Limit { resets_at, message };
                }
                AttemptEnd::Unrunnable(reason, problem) => return RoleEnd::Failed(reason, problem),
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
                        return RoleEnd::Failed(reason, None);
                    }
                    tracing::info!(flow = %ctx.flow, role = %step.role, attempt, %reason, "retrying a role");
                }
            }
        }
    }

    /// One attempt: one session, opened, sent its turn, and read until it submits or stops.
    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn attempt(
        &self,
        ctx: &RunCtx,
        run: &Run,
        snapshots: &Snapshots,
        checkout: &StageCheckout,
        step: &mut Step,
        attempt: u32,
        services: &[wtm_core::flow::GatedService],
    ) -> AttemptEnd {
        let Ok(db) = self.db() else {
            return AttemptEnd::Quitting;
        };
        let mut spec = match ctx.role(&step.role) {
            Ok(spec) => spec.clone(),
            Err(reason) => return AttemptEnd::Unrunnable(reason, None),
        };
        // A writer fixing findings hands in a handoff, whatever a recipe would have it hand in.
        if ctx.fix.is_some() {
            spec.output = OutputKind::Handoff;
        }
        if let Some(limit) = self.injected_limit(ctx, step, attempt) {
            return limit;
        }
        // Run once on another provider, at a limit: its model is the one you chose, and the
        // role's effort is spelled for the provider it names, so it is left to the other one's
        // default.
        if step.provider != spec.provider {
            spec.provider = step.provider;
            spec.effort = None;
        }
        if let Some(problem) =
            credit_problem(&step.role, &spec, step.provider, step.model.as_deref())
        {
            return AttemptEnd::Unrunnable(problem.to_string(), None);
        }
        // A place among the run sessions before the attempt is recorded, held until it ends.
        let _seat = match self.take_seat(ctx.flow, run, step) {
            Ok(seat) => seat,
            Err(end) => return end,
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

        let setup = if spec.kind == RoleKind::Runtime {
            match self.open_runtime(ctx, step, &spec, invocation.id, services.to_vec()) {
                Ok(setup) => Some(setup),
                Err(reason) => {
                    let reason = format!("its runtime could not be set up: {reason}");
                    let _ = db.update_invocation(
                        invocation.id,
                        &InvocationChange::ended(
                            InvocationStatus::Failed,
                            Some(reason.clone()),
                            self.now(),
                        ),
                        self.now(),
                    );
                    return AttemptEnd::Failed {
                        status: InvocationStatus::Failed,
                        reason,
                    };
                }
            }
        } else {
            None
        };

        let (tx, rx) = mpsc::channel();
        let key = (ctx.flow, step.role.clone());
        self.live.lock().insert(
            key.clone(),
            Arc::new(Live {
                output: spec.output,
                tx: tx.clone(),
                step: step.id,
                invocation: invocation.id,
                cwd: cwd.clone(),
                tree_sha: ctx.snapshot.tree_sha.clone(),
                checks: spec
                    .checks
                    .iter()
                    .filter_map(|id| {
                        let check = ctx.workflow.check.get(id)?;
                        Some((id.clone(), check.clone()))
                    })
                    .collect(),
                dir: ctx.dir.clone(),
                fix: ctx.fix.clone(),
                planner: None,
                designer: None,
                project: ctx.project.clone(),
                worktree: ctx.worktree.clone(),
                start_services: ctx.workflow.start_services,
                browser: setup
                    .as_ref()
                    .and_then(|s| s.pane.as_ref())
                    .map(|p| p.id.clone()),
            }),
        );
        let end = self.converse(
            ctx,
            run,
            step,
            &spec,
            &cwd,
            &invocation,
            setup.as_ref(),
            tx,
            &rx,
        );
        self.live.lock().remove(&key);
        if let Some(setup) = &setup {
            self.close_runtime(setup);
        }
        // A role that read the worktree read it while you could edit it: ask git whether you did.
        if matches!(end, AttemptEnd::Submitted)
            && step.workspace == Workspace::Worktree
            && step.profile == wtm_core::flow::RoleProfile::Reviewer
        {
            self.recheck_tree(ctx, step);
        }

        if !matches!(end, AttemptEnd::Quitting) {
            let (status, detail) = match &end {
                AttemptEnd::Submitted => (InvocationStatus::Submitted, None),
                AttemptEnd::Failed { status, reason } => (*status, Some(reason.clone())),
                AttemptEnd::Unrunnable(reason, _) => {
                    (InvocationStatus::Failed, Some(reason.clone()))
                }
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
        invocation: &Invocation,
        setup: Option<&runtime::RuntimeSetup>,
        tx: Sender<Signal>,
        rx: &Receiver<Signal>,
    ) -> AttemptEnd {
        let Ok(db) = self.db() else {
            return AttemptEnd::Quitting;
        };
        let (knowledge, sources) = self.knowledge_for(ctx, step, spec, cwd, invocation.attempt);
        if !sources.is_empty() {
            let _ = db.update_invocation(
                invocation.id,
                &InvocationChange {
                    instruction_sources: Some(sources),
                    ..InvocationChange::default()
                },
                self.now(),
            );
        }
        let checks: Vec<(String, CheckSpec)> = spec
            .checks
            .iter()
            .filter_map(|id| Some((id.clone(), ctx.workflow.check.get(id)?.clone())))
            .collect();
        // A role that takes a design, such as a UI consistency reviewer, is given the issue's
        // approved revision when the work under review has one whose approval still holds.
        let design = if spec.inputs.contains(&wtm_core::flow::RoleInput::Design) {
            self.approved_design_for(ctx.flow)
        } else {
            None
        };
        // Following a pull request to a new head, the earlier findings are what the round is for,
        // so a role is given them whatever it lists as inputs; the ones you dismissed are settled
        // and are not asked about again.
        let earlier = self.earlier_findings(step, &ctx.snapshot.tree_sha, ctx.revision.is_some());
        let answers = self.answers_for(ctx, &step.role);
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
            knowledge: knowledge.as_deref(),
            checks: &checks,
            earlier: &earlier,
            pull_request: ctx.pull_request.as_ref(),
            revision: ctx
                .revision
                .as_ref()
                .map(|(from, to)| (from.as_str(), to.as_str())),
            answers: &answers,
            runtime: setup,
            design: design.as_deref(),
        };
        let invocation = invocation.id;
        let session_spec = RoleSession {
            project: ctx.project.clone(),
            worktree: ctx.worktree.clone(),
            role: FlowRole {
                flow: ctx.flow,
                role: step.role.clone(),
                authority: step.profile,
                browser: setup.and_then(|s| s.pane.as_ref()).map(|p| p.id.clone()),
                design: false,
            },
            provider: step.provider.id().to_owned(),
            model: step.model.clone(),
            effort: spec.effort.clone(),
            brief: match &ctx.fix {
                Some(fix) => brief::fix_brief(&facts, fix),
                None => brief::brief(&facts),
            },
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
        let opening = if ctx.fix.is_some() {
            brief::fix_opening_turn(&step.role)
        } else {
            brief::opening_turn(&step.role)
        };
        if skills_checked {
            if let Err(end) = say(&opening) {
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
                    self.consolidate(ctx.flow);
                    submitted = true;
                    settle_at = Some(self.clock.monotonic_ms() + self.tuning.settle_ms);
                }
                Ok(Signal::Event(event)) => match event {
                    AgentEvent::SkillsListed { skills } if !skills_checked => {
                        if let Some(problem) = missing_skill(&step.role, spec, &skills) {
                            return AttemptEnd::Unrunnable(problem.to_string(), Some(problem));
                        }
                        skills_checked = true;
                        if let Err(end) = say(&opening) {
                            return end;
                        }
                        sent = true;
                    }
                    AgentEvent::SessionReady {
                        provider_session_id,
                        model,
                        ..
                    } => {
                        // The model the provider says it is running, which for a role that named
                        // none is the CLI's default: a backstop for the check made before the
                        // session opened, which could only read the model the role named.
                        if let Some(problem) =
                            credit_problem(&step.role, spec, step.provider, model.as_deref())
                        {
                            return AttemptEnd::Unrunnable(
                                format!(
                                    "{problem}. The session reported it at its start, and wtm \
                                     closed it then"
                                ),
                                None,
                            );
                        }
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
                            kind: InboxKind::Approval,
                            target: on(step),
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
                // Only a planner's turn is told of a question it asked; a review role carries on.
                Ok(Signal::Asked(_) | Signal::Published(_)) | Err(RecvTimeoutError::Timeout) => {}
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
                let problem = SkillProblem::Unlisted {
                    role: step.role.clone(),
                    provider: spec.provider,
                };
                return AttemptEnd::Unrunnable(problem.to_string(), Some(problem));
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

    /// Record that `step` is replaced by `alternative`, make the alternative's step, and keep the
    /// switch with the handover the alternative starts from.
    fn hand_over(
        &self,
        ctx: &RunCtx,
        step: &Step,
        alternative: &str,
        reason: SwitchReason,
        why: &str,
    ) -> Option<Step> {
        let db = self.db().ok()?;
        let spec = ctx.workflow.role.get(alternative)?;
        let handover = self.handover_for(
            ctx,
            step,
            Runner {
                role: alternative.to_owned(),
                provider: spec.provider,
                model: spec.model.clone(),
            },
            reason,
            why,
        );
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
                    // The role whose work this is, not the alternative that ran out before it:
                    // the next alternative comes from that role's own chain.
                    stands_in_for: Some(origin(step).to_owned()),
                    handover: Some(handover.render()),
                    detail: None,
                }],
                self.now(),
            )
            .ok()?;
        let next = added.into_iter().next()?;
        if let Err(error) = db.record_switch(
            &NewProviderSwitch {
                flow: ctx.flow,
                from_step: step.id,
                to_step: next.id,
                reason,
                by: SwitchedBy::Config,
                handover,
            },
            self.now(),
        ) {
            tracing::warn!(flow = %ctx.flow, %error, "could not record a provider switch");
        }
        self.notice.changed(ctx.flow);
        Some(next)
    }

    /// A role is out of attempts and fallbacks: say so where you will see it.
    fn give_up(&self, ctx: &RunCtx, step: &Step, reason: &str, problem: Option<SkillProblem>) {
        self.update_step(
            step,
            &StepChange {
                skill_problem: Field::from(problem),
                ..StepChange::status(StepStatus::Failed).detail(reason.to_owned())
            },
        );
        let tries = step.retries + 1;
        self.file(&NewInboxItem {
            flow: ctx.flow,
            kind: InboxKind::Failure,
            target: on(step),
            title: format!("`{}` failed: {reason}", step.role),
            body: format!(
                "{} after {tries} attempt{}, and the role has no fallback left. The other roles' \
                 results are kept, and the review is incomplete until you retry it or accept it.",
                capitalise(reason),
                if tries == 1 { "" } else { "s" }
            ),
        });
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
                .is_some()
            || self.is_planning(role.flow)
            || self.is_designing(role.flow);
        if !ours {
            return None;
        }
        Some(match (call, live) {
            // A designer's result is a published revision, and only `prototype_publish` makes one.
            (FlowCall::Submit(_), Some(live)) if live.designer.is_some() => Err(
                "A designer hands in its design with `prototype_publish`, not `flow_submit`: \
                 publish the revision, then end your turn."
                    .to_owned(),
            ),
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
            (FlowCall::Ask(ask), Some(live)) => match &live.planner {
                Some(planner) => self.planner_ask(role, &live, planner, ask),
                None => Ok(self.ask(role, &live, ask)),
            },
            (FlowCall::RunCheck { name }, Some(live)) => self.run_check(role, &live, name),
            // A run of this engine's commits only through a fix, held to the fix's files. Never
            // through the ledger's unscoped commit, which is for sessions no engine runs.
            (FlowCall::Checkpoint { message }, Some(live)) => match &live.fix {
                Some(fix) => self.fix_checkpoint(role, &live, fix, message),
                None => {
                    Err("this role is not fixing anything, so it has nothing to commit".to_owned())
                }
            },
            (
                FlowCall::Submit(_)
                | FlowCall::Progress(_)
                | FlowCall::Ask(_)
                | FlowCall::RunCheck { .. }
                | FlowCall::Checkpoint { .. },
                None,
            ) => Err(ended()),
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
    pub fn resume(self: &Arc<Self>, flow: FlowId) -> Result<(), Refusal> {
        // An issue's run cut off while it was being designed: the designer's conversation carries
        // on (`designer.rs`).
        if self
            .state(flow)
            .is_ok_and(|state| state.kind == FlowKind::Implement && state.phase == Phase::Designing)
        {
            return self.start_designer(flow);
        }
        // An issue's run cut off while it was being planned: the planner's conversation carries on
        // (`planner.rs`).
        if self.is_planning(flow)
            && self
                .state(flow)
                .is_ok_and(|state| state.phase == Phase::Investigating)
        {
            return self.resume_planner(flow);
        }
        // An issue's run, interrupted while its worktree was made: the inbox's Resume is Start
        // Work's (`start.rs`), and a review's machinery has nothing to run for it.
        if self.is_start(flow) {
            return self.resume_start(flow, false);
        }
        if let Some(run) = self.runs.lock().get(&flow) {
            // Still working: whatever waits for capacity runs now rather than at the reset.
            run.wake.store(true, Ordering::SeqCst);
            return Ok(());
        }
        if self
            .state(flow)
            .is_ok_and(|state| state.phase == Phase::Fixing && state.end.is_none())
        {
            return self.resume_fix(flow);
        }
        let db = self.db()?;
        let review = db
            .review(flow)
            .map_err(text)?
            .ok_or_else(|| format!("run {flow} is not a review"))?;
        let state = self.state(flow)?;
        if let Some(end) = &state.end {
            return Err(format!("this review has already ended ({end:?})").into());
        }
        let steps = db.steps(flow).map_err(text)?;
        if state.phase == Phase::Consolidated && !steps.iter().any(|s| s.status.wants_a_run()) {
            return Err(
                "every role of this review has its result; there is nothing to resume".into(),
            );
        }
        self.refuse_if_written(&review.worktree_path)?;
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
                    )
                    .into());
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
        // What the resume answers: the interruption, the pause, the failure. A review that was
        // ready is running again, so it is not ready any more. A question stays: resuming answers
        // nothing.
        self.settle(
            flow,
            &[
                InboxKind::Interrupted,
                InboxKind::Failure,
                InboxKind::Capacity,
                InboxKind::ReviewReady,
            ],
        );
        self.notice.changed(flow);
        Ok(self.spawn(flow)?)
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
        if self.is_start(flow) {
            return self.cancel_start(flow);
        }
        // A fix being written is stopped with the review, and its writer lease goes with it.
        self.stop_fix_record(flow);
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
        // A stopped review needs nothing more from you, its questions included: there is nobody
        // left to give an answer to.
        self.settle(flow, &ALL_KINDS);
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
            if self.is_designer_running(flow) {
                self.interrupt_designer(flow, InterruptCause::Quit);
            } else if self.is_planning(flow) {
                self.interrupt_planner(flow, InterruptCause::Quit);
                continue;
            }
            // A run whose only unfinished work waits at a usage limit had no turn in flight, so
            // nothing is cut off: it stays held as it is, and the next launch asks the provider
            // again before anything runs.
            if self.only_waiting_for_capacity(flow) {
                continue;
            }
            self.interrupt(flow, InterruptCause::Quit);
        }
        self.shutdown_starts();
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
    pub fn recover(self: &Arc<Self>) -> Result<Vec<FlowId>, String> {
        if !self.app.files.exists(&self.app.config.paths().flow_db) {
            return Ok(Vec::new());
        }
        let db = self.db()?;
        let mut interrupted = Vec::new();
        let mut waits = Vec::new();
        for review in db.open_reviews().map_err(text)? {
            if self.is_running(review.flow) {
                continue;
            }
            // Left waiting at a usage limit, by a quit or a crash, with nothing mid-turn: the wait
            // is picked up again below, and its role asks the provider before it runs. Nothing is
            // re-sent, because nothing was in flight.
            if self.only_waiting_for_capacity(review.flow) {
                waits.push(review.flow);
                continue;
            }
            let state = self.state(review.flow)?;
            // A quit already said so of a flow held `Interrupted`, and a consolidated one was not
            // running. A capacity wait with a role cut off mid-turn beside it was left by a crash,
            // like a flow caught mid-phase with no hold at all.
            let gone = matches!(
                (&state.hold, state.phase),
                (Some(Hold::WaitingCapacity { .. }), _)
                    | (
                        None,
                        Phase::Resolving | Phase::Preparing | Phase::Reviewing | Phase::Fixing
                    )
            );
            if gone {
                self.interrupt(review.flow, InterruptCause::Crash);
                interrupted.push(review.flow);
            }
        }
        // Before any wait is picked up: a picked-up run holds a lease of its own, and one that is
        // running is passed over, so reconciling after would keep the lease the last process took
        // and never let go (found by the click-through, after a kill mid-wait).
        self.reconcile_leases();
        for flow in waits {
            match self.spawn(flow) {
                Ok(()) => tracing::info!(%flow, "picked up a capacity wait at launch"),
                Err(reason) => {
                    tracing::warn!(%flow, %reason, "could not pick up a capacity wait");
                    self.interrupt(flow, InterruptCause::Crash);
                    interrupted.push(flow);
                }
            }
        }
        // An issue's worktree cut off while it was being made or set up (`start.rs`). Its own
        // failure is logged rather than returned: the reviews above were recovered either way,
        // and saying they were not would send someone looking in the wrong place.
        match self.recover_starts() {
            Ok(held) => interrupted.extend(held),
            Err(error) => tracing::warn!(%error, "could not check started issues at launch"),
        }
        // A planner cut off mid-turn. One waiting for an answer is left as it is: it needs no
        // process, and the answer resumes its conversation however long the app was gone.
        match self.recover_planners() {
            Ok(held) => interrupted.extend(held),
            Err(error) => tracing::warn!(%error, "could not check planners at launch"),
        }
        // A designer cut off mid-turn, the same way.
        match self.recover_designers() {
            Ok(held) => interrupted.extend(held),
            Err(error) => tracing::warn!(%error, "could not check designers at launch"),
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
            // A role waiting for capacity or for its environment waits in a thread that is going
            // away; the interruption is what holds the run now, and a resume asks again.
            if let Some(
                hold @ (Hold::WaitingCapacity { .. }
                | Hold::Blocked {
                    cause: wtm_core::flow::BlockCause::Env,
                }),
            ) = &state.hold
            {
                let _ = self.record(flow, &FlowEvent::Released { hold: hold.kind() });
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
        let fixing = self
            .state(flow)
            .is_ok_and(|state| state.phase == Phase::Fixing);
        let title = match (cause, fixing) {
            (InterruptCause::Quit, false) => "wtm quit during this review".to_owned(),
            (_, false) => "wtm stopped unexpectedly during this review".to_owned(),
            (InterruptCause::Quit, true) => "wtm quit during this local fix".to_owned(),
            (_, true) => "wtm stopped unexpectedly during this local fix".to_owned(),
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
        let body = if fixing {
            format!(
                "{cut_off} The writer still holds the review worktree, and any edits it had not \
                 committed are still there. Resume runs the writer again on top of them; stopping \
                 the fix lets the worktree go."
            )
        } else {
            format!(
                "{done} of {counted} roles had finished; their results are kept. {cut_off} Resume \
                 runs the remaining roles against the same snapshot."
            )
        };
        self.file(&NewInboxItem {
            flow,
            kind: InboxKind::Interrupted,
            target: InboxTarget::Flow,
            title,
            body,
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
            target: InboxTarget::Flow,
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
        // First, before any record is read. The run's thread lets go of the flow only after its
        // roles have joined, so "not running" read first means every write they made is already
        // in the reads below. Read last, it could pair a step still marked running with a thread
        // that has since finished, and a caller waiting for the run to stop would see both.
        let running = self.is_running(flow);
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
        let steps = db.steps(flow).map_err(text)?;
        let invocations = db.invocations(flow).map_err(text)?;
        let workflow = self
            .app
            .project(review.project.as_str())
            .ok()
            .and_then(|project| project.workflow);
        let found = db.findings(flow).map_err(text)?;
        let checks = db.checks(flow).map_err(text)?;
        let pull_request = self.pull_request_view(&state, snapshot.as_ref(), &found, &checks)?;
        let fixes = db.fixes(flow).map_err(text)?;
        let leases = db.open_leases(Some(&review.worktree_path)).map_err(text)?;
        let writers = fix::writer_choices(workflow.as_ref());
        let screenshots = db.screenshots(flow).map_err(text)?;
        let switches = db.switches(flow).map_err(text)?;
        let capacity = self.capacity_view(flow, &steps);
        // What Start environment and run would start and run, when anything is blocked. A config
        // that no longer renders is shown as nothing to start rather than failing the whole view.
        let pending_start = self.pending_start(flow).unwrap_or_else(|e| {
            tracing::debug!(%flow, error = %e, "could not plan starting the environment");
            None
        });
        Ok(FlowView {
            fixes,
            screenshots,
            pending_start,
            queued: self.queued_in(flow),
            holding: self.holding_in(flow),
            switches,
            capacity,
            leases,
            writers,
            status: state.state.status(),
            flow: state,
            snapshot,
            findings: found,
            checks,
            expected_checks: findings::expected_checks(workflow.as_ref(), &steps),
            applied: self.applied(flow, workflow.as_ref(), &steps, &invocations)?,
            steps,
            invocations,
            inbox: db.inbox(Some(flow), None).map_err(text)?,
            running,
            pull_request,
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

    /// Resolve an inbox item: what it was about is settled, such as a ready review read or a
    /// provider's prompt answered in its pane. The item as it now is, or `None` when it was not
    /// open.
    ///
    /// # Errors
    ///
    /// When the database cannot be written.
    pub fn resolve(&self, item: InboxItemId) -> Result<Option<InboxItem>, String> {
        let closed = self
            .db()?
            .resolve_inbox_item(item, self.now())
            .map_err(text)?;
        if let Some(closed) = &closed {
            self.notice.changed(closed.flow);
        }
        Ok(closed)
    }

    /// Dismiss an inbox item: a person putting it away without settling it. A failed role stays
    /// failed and a question unanswered; only the item goes.
    ///
    /// # Errors
    ///
    /// When the database cannot be written.
    pub fn dismiss_item(&self, item: InboxItemId) -> Result<Option<InboxItem>, String> {
        let closed = self
            .db()?
            .dismiss_inbox_item(item, self.now())
            .map_err(text)?;
        if let Some(closed) = &closed {
            self.notice.changed(closed.flow);
        }
        Ok(closed)
    }

    /// Answer a question a run asked. The first answer counts; its item is resolved with it.
    ///
    /// A planner waits for its answers: the last one it waits for resumes its conversation
    /// (`planner.rs`). A review role was told not to wait, so nothing resumes then: the answer is
    /// kept with the run and given to the role that asked when it runs again.
    ///
    /// # Errors
    ///
    /// When the question was answered already, the answer picks no option it offered and says
    /// nothing, or the database cannot be written.
    pub fn answer(
        &self,
        question: QuestionId,
        choice: Option<usize>,
        text_answer: &str,
    ) -> Result<Question, String> {
        let answered = self
            .db()?
            .answer_question(
                question,
                &Answer {
                    choice,
                    text: text_answer.trim().to_owned(),
                    at: self.now(),
                },
            )
            .map_err(text)?;
        self.on_outlier_answer(&answered);
        self.notice.changed(answered.flow);
        self.on_planner_answer(&answered);
        Ok(answered)
    }

    // ═══════════════════════════════ small helpers ═══════════════════════════════

    fn state(&self, flow: FlowId) -> Result<wtm_core::flow::FlowState, String> {
        self.db()?
            .flow(flow)
            .map_err(text)?
            .map(|f| f.state)
            .ok_or_else(|| format!("no run {flow}"))
    }

    /// Record `event` against `flow`. The one route into a run's state, so it is also where the gate
    /// in front of Implementing is held: no caller, today's or a later item's, can move an issue's
    /// run there without the plan and the design it needs approved (`designer.rs`).
    fn record(&self, flow: FlowId, event: &FlowEvent) -> Result<wtm_core::flow::FlowState, String> {
        if let FlowEvent::Advanced {
            to: Phase::Implementing,
        } = event
        {
            self.check_implementing(flow)
                .map_err(|blocked| format!("{flow} cannot start implementing: {blocked}"))?;
        }
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

    /// File an item and announce it, unless an open one already says the same thing about the
    /// same target: the one place a run's notification starts.
    fn file(&self, item: &NewInboxItem) {
        let Ok(db) = self.db() else { return };
        match db.file_inbox(item, self.now()) {
            Ok(Some(filed)) => self.notice.filed(&filed),
            Ok(None) => {}
            Err(e) => tracing::warn!(flow = %item.flow, error = %e, "could not file an inbox item"),
        }
    }

    /// Resolve a run's open items of these kinds, whatever they are about, except a publish's.
    ///
    /// A publish of the review is settled by its own end — published, cancelled, replaced — and
    /// not by the review being resumed or stopped: a review run again does not post what was
    /// left unposted, and stopping it does not say anything about what reached GitHub.
    fn settle(&self, flow: FlowId, kinds: &[InboxKind]) {
        let Ok(db) = self.db() else { return };
        for item in db.inbox(Some(flow), None).unwrap_or_default() {
            if kinds.contains(&item.kind) && !matches!(item.target, InboxTarget::Approval { .. }) {
                let _ = db.resolve_inbox_item(item.id, self.now());
            }
        }
    }

    /// A role asked something: keep the question and tell you, then tell the role to carry on.
    ///
    /// A review role is not held for the answer — holding a run for hours and resuming its
    /// provider session is the planner's machinery, not a review's. So the role chooses a reading
    /// and says so, and the question is kept for you with that said, and its answer given to the
    /// role when it runs again.
    fn ask(&self, role: &FlowRole, live: &Live, ask: &crate::flow_tools::Ask) -> String {
        let carry_on = if live.designer.is_some() {
            "A design does not stop for answers either. Choose the most reasonable reading, say \
             which you chose and why in the revision's reuse notes, and publish it: the person \
             comments on the revision."
        } else {
            "This review does not stop for answers. Choose the most reasonable reading, say which \
             you chose and why in the finding it affects, and carry on."
        };
        let Ok(db) = self.db() else {
            return carry_on.to_owned();
        };
        let question = NewQuestion {
            flow: role.flow,
            step: Some(live.step),
            role: Some(role.role.clone()),
            text: ask.question.trim().to_owned(),
            why: ask.why.trim().to_owned(),
            unblocks: ask.unblocks.trim().to_owned(),
            blocks: None,
            context: ask.refs.iter().map(|r| ContextLink::from_ref(r)).collect(),
            recommendation: ask
                .recommendation
                .as_deref()
                .map(str::trim)
                .filter(|r| !r.is_empty())
                .map(str::to_owned),
            options: ask
                .options
                .iter()
                .map(|o| o.trim().to_owned())
                .filter(|o| !o.is_empty())
                .collect(),
        };
        let title = format!("`{}` asks: {}", role.role, question.text);
        match db.ask(&question, &title, self.now()) {
            Ok((asked, filed)) => {
                if let Some(filed) = filed {
                    self.notice.filed(&filed);
                }
                self.notice.changed(role.flow);
                format!(
                    "Filed as question {}. The person is told, and their answer reaches you if this \
                     role runs again. {carry_on}",
                    asked.id
                )
            }
            Err(e) => {
                tracing::warn!(flow = %role.flow, error = %e, "could not keep a question");
                carry_on.to_owned()
            }
        }
    }
}

/// Every kind of item, for closing all of a run's at once.
const ALL_KINDS: [InboxKind; 6] = [
    InboxKind::Question,
    InboxKind::Approval,
    InboxKind::Failure,
    InboxKind::Capacity,
    InboxKind::Interrupted,
    InboxKind::ReviewReady,
];

/// The role whose work a step is: its own, or the one a fallback stands in for.
fn origin(step: &Step) -> &str {
    step.stands_in_for.as_deref().unwrap_or(&step.role)
}

/// Refuse a recipe that would run a model billing usage credits for a role that has not opted in.
fn refuse_credits(workflow: &WorkflowSpec, recipe: &str) -> Result<(), Refusal> {
    let credits = workflow.credit_problems(recipe);
    if credits.is_empty() {
        return Ok(());
    }
    let message = credits
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(". ");
    Err(Refusal {
        message: format!("{message}. Nothing was started."),
        billing: Vec::new(),
        credits,
    })
}

/// The inbox target for a step: the role, by the step the store numbered.
fn on(step: &Step) -> InboxTarget {
    InboxTarget::Step {
        step: step.id,
        role: step.role.clone(),
    }
}

/// The skill problem that stops `role`, given what its provider listed, or `None`.
///
/// Compared with the provider's own list, never wtm's reading of skill directories, and the
/// problem is R2's [`SkillProblem`](wtm_core::flow::SkillProblem), so it names the provider. A
/// Claude run's skills are listed under the plugin `flow_session` wrote them into; that prefix is
/// taken off before comparing, and nothing else is.
fn missing_skill(role: &str, spec: &RoleSpec, listed: &[AgentSkill]) -> Option<SkillProblem> {
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
        .map(|skill| SkillProblem::Missing {
            role: role.to_owned(),
            provider: spec.provider,
            skill: skill.to_owned(),
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
