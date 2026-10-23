//! The implementer: an issue's run from "Planned, approved" — or "Design approved", when its plan
//! needed a design — through Implementing, Verifying, Reviewing and Fixing, to "Ready to test".
//!
//! # In order
//!
//! 1. **Start**, by itself once you approve a plan that needs no design, or the design one needs;
//!    or from Start implementing. Refused, with nothing recorded, unless the gate in front of
//!    Implementing passes (`designer.rs`), the project names an implementer (`[workflow.implement]`
//!    in your own config), its provider bills a subscription, and nobody else holds the issue's
//!    worktree. The writer lease is taken first, in the store's transaction, and only then does the
//!    run move to Implementing.
//! 2. **Implementing.** One writer, under the R4 writer profile, in the issue's worktree, given the
//!    approved plan with its acceptance criteria and areas, the approved design and its reuse notes
//!    when there is one, the repository's instructions and your rules. It cannot commit:
//!    `flow_checkpoint` asks the app, which commits only what the plan covers
//!    (`wtm_core::flow::implement::ImplementScope`). A file outside it is refused and becomes a
//!    question; when the writer hands in, what it left within the plan is committed by the app.
//! 3. **Verifying.** The app runs the configured checks itself, through the machinery
//!    `flow_run_check` uses and E3's service gate, against a snapshot of the new tree. Their records
//!    are the evidence; a blocked check is recorded blocked and asked about, never passed.
//! 4. **Reviewing.** The project's review recipe reads that snapshot. The review is a review run of
//!    the issue's own work item — R5's engine, R6's consolidation and staleness, the Reviews tab the
//!    issue already has — driven from this run's thread and under this run's writer lease.
//! 5. **Fixing.** What they found inside the plan goes back to the same writer, in the same
//!    conversation. Then the checks its changes affect run again, and the review roles whose files
//!    changed read again; the rest keep their results. Bounded by `wtm_core::flow::next_round`:
//!    rounds per plan revision, attempts per problem.
//! 6. **Ready to test.** The lease goes, and one inbox item says what was done, what was checked,
//!    and what was left and why.
//!
//! # A plan change goes back to you
//!
//! Work outside the plan — a file the writer changed, or a finding in a file the plan does not
//! cover — is a question with two answers, never "allow it this once": change the plan, or keep to
//! it. Changing it records the next plan revision, covering those files and saying why, and sends
//! the run back to Planned, where it waits for your approval as the first revision did. Approving
//! carries implementing on, in the writer's same conversation, with a fresh fix budget for the new
//! plan. An answer that arrives while the thread works is taken at its next stopping point: after
//! a writer's turn or after a review.
//!
//! # The lease's life
//!
//! Taken before the writer starts, and kept through every phase, through a quit, a crash and a plan
//! change, because the worktree holds the implementation's work, committed or not, and a review or
//! a fix reading it then would describe something nobody finished. The implementation's own review
//! reads under it rather than beside it: the writer is not writing while its review reads. It goes
//! at Ready to test, or when you stop the run; a plan change agreed after the hand-over takes it
//! again.
//!
//! # Picked up where it stopped
//!
//! The run's phase says what comes next, and the rounds say how far the writer got: a round with no
//! end is the writer's, part-way. So a resume, after a quit, a crash, a limit or a failure, sends
//! nothing twice: a writer cut off mid-turn is resumed in its conversation and told so, checks and
//! the review start again from the snapshot, and a decision the record already holds is not taken
//! again.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use serde::Serialize;
use wtm_core::flow::RunPart;
use wtm_core::flow::{
    AttemptedFinding, CHANGE_THE_PLAN, CheckRun, CheckSpec, CheckStatus, CheckpointBy, ContextLink,
    Field, FindingRecord, FlowDir, FlowEvent, FlowId, FlowKind, Handoff, Hold, HoldKind,
    ImplementAsk, ImplementChange, ImplementCheckpoint, ImplementOutcome, ImplementQuestion,
    ImplementScope, ImplementSpec, Implementation, InboxKind, InboxTarget, InterruptCause,
    Invocation, InvocationChange, InvocationStatus, LeaseKind, LeaseRelease, LoopFacts,
    NewImplementation, NewInboxItem, NewInvocation, NewLease, NewPlan, NewQuestion, NewReviewRun,
    NewRound, NewStep, NextRound, OutputKind, Phase, PlanContext, PlanRecord, PlanStanding,
    Question, QuestionId, Remaining, RoleProfile, RoleSpec, Round, RoundKind, ScratchLabel, Step,
    StepChange, StepStatus, StopReason, Submission, TrackerKind, WorkflowSpec, Workspace,
    affected_checks, next_round, scope_options, widened,
};
use wtm_core::model::AgentEvent;
use wtm_core::ports::{
    FlowStore, FlowStoreError, ImplementStore, InboxStore, LeaseStore, PlanStore, RuleStore,
    RunStore,
};
use wtm_core::usecase::snapshot::Snapshots;

use super::{Engine, Live, Refusal, Run, Signal, brief, missing_skill, text};
use crate::app::FlowRole;
use crate::flow_session::RoleSession;

/// How long a writer whose turn the engine interrupted, for a question it asked, gets to finish
/// the turn on its own before its session is closed anyway.
const INTERRUPT_GRACE_MS: u64 = 10_000;

/// What a writer's turn is held to while it works, as its run tools see it.
#[derive(Debug)]
pub(super) struct ImplementLive {
    pub(super) round: u32,
    pub(super) plan_revision: u32,
    pub(super) scope: ImplementScope,
    pub(super) worktree: PathBuf,
    pub(super) key: String,
}

/// What the first writer turn of a thread is for. Later turns in the same thread are fix rounds,
/// which the record says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum WriteTurn {
    /// Whatever the record says comes next.
    Next,
    /// Carry on after an interruption, a failure or a limit.
    Resume,
    /// Its questions are answered.
    Answered,
    /// You kept to the plan: these files must go back.
    Keep { paths: Vec<String> },
    /// You approved a plan revision that covers more.
    Replanned { revision: u32, added: Vec<String> },
}

/// How one writer turn ended.
#[derive(Debug)]
enum TurnEnd {
    Handed(Box<Handoff>),
    Asked(Vec<QuestionId>),
    Failed(String),
    Limit {
        resets_at: Option<u64>,
        message: String,
    },
    Cancelled,
    Quitting,
}

/// Whether the drive carries on to the next phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Next {
    Continue,
    Stop,
}

/// What an implementation's run reads while it works, resolved once per phase.
struct ImplementCtx {
    flow: FlowId,
    implementation: Implementation,
    implement: ImplementSpec,
    workflow: WorkflowSpec,
    spec: RoleSpec,
    plan: PlanRecord,
    context: Option<PlanContext>,
    tracker: TrackerKind,
    worktree_path: PathBuf,
    branch: Option<String>,
    patterns: Vec<String>,
}

impl ImplementCtx {
    fn project(&self) -> &str {
        self.implementation.project.as_str()
    }

    fn key(&self) -> &str {
        &self.implementation.key
    }
}

/// A question the implementation asked, with what it is about.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImplementQuestionView {
    pub link: ImplementQuestion,
    pub question: Question,
}

/// Where the implementation's review stands, for the Implement tab.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewSummary {
    pub flow: FlowId,
    pub phase: Phase,
    pub running: bool,
    /// Its findings still reported and not dismissed.
    pub standing: usize,
}

/// Everything the issue workspace's Implement tab shows about an issue's implementation.
///
/// Its flags are what the tab may offer now, read from the run once here, so the window never
/// works out from a phase and a hold what Rust would refuse.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(clippy::struct_excessive_bools)]
pub struct ImplementView {
    pub flow: wtm_core::flow::Flow,
    pub key: Option<String>,
    /// The writer `[workflow.implement]` names now, or `None` when the project names none.
    pub implementer: Option<String>,
    pub implementation: Option<Implementation>,
    pub rounds: Vec<Round>,
    pub checkpoints: Vec<ImplementCheckpoint>,
    /// What Verifying ran, newest last: the review run's check records.
    pub checks: Vec<CheckRun>,
    /// The checks Verifying runs, by id, in order.
    pub verifying: Vec<String>,
    pub review: Option<ReviewSummary>,
    /// The review's findings still reported, dismissed ones included.
    pub findings: Vec<FindingRecord>,
    pub questions: Vec<ImplementQuestionView>,
    /// The writer's sessions, oldest first: one provider conversation, resumed.
    pub sessions: Vec<Invocation>,
    pub step: Option<Step>,
    pub lease: Option<wtm_core::flow::Lease>,
    pub running: bool,
    /// Whether Start implementing would start it now.
    pub can_start: bool,
    /// Whether Resume would carry it on now.
    pub can_resume: bool,
    /// What happened to the run while nothing in it moved: the Mac asleep, a session stopped at
    /// launch. Never progress.
    pub notes: Vec<wtm_core::flow::FlowNote>,
}

/// The first line of `text`, trimmed, at most 72 characters: a commit's subject.
fn subject(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let mut out: String = line.chars().take(72).collect();
    if line.chars().count() > 72 {
        out.pop();
        out.push('…');
    }
    out
}

fn short(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

fn listed(paths: &[String]) -> String {
    paths
        .iter()
        .map(|p| format!("`{p}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn them(paths: &[String]) -> (&'static str, &'static str) {
    if paths.len() == 1 {
        ("it", "it was")
    } else {
        ("them", "they were")
    }
}

impl Engine {
    // ═══════════════════════════════ reading ═══════════════════════════════

    /// The implementation of `flow`, while it is open: not handed over and not stopped.
    fn open_implementation(&self, flow: FlowId) -> Option<Implementation> {
        self.db()
            .ok()?
            .implementation(flow)
            .ok()
            .flatten()
            .filter(|i| i.outcome.is_none())
    }

    /// Whether `flow`'s run holds its worktree for an implementation: one is open and the run has
    /// not ended. What recovery keeps a writer lease for.
    pub(super) fn holds_its_worktree(&self, flow: FlowId) -> bool {
        self.open_implementation(flow).is_some()
            && self.state(flow).is_ok_and(|state| state.end.is_none())
    }

    /// Whether `flow` is an implementation's thread at work, rather than a planner's or a
    /// designer's: what `shutdown` asks before deciding how to interrupt it.
    pub(super) fn is_implementing(&self, flow: FlowId) -> bool {
        self.open_implementation(flow).is_some()
            && self.state(flow).is_ok_and(|state| {
                matches!(
                    state.phase,
                    Phase::Implementing | Phase::Verifying | Phase::Reviewing | Phase::Fixing
                )
            })
    }

    /// The implementation whose review run is `review`, while it is open.
    pub(super) fn reviewing_for(&self, review: FlowId) -> Option<Implementation> {
        self.db()
            .ok()?
            .implementation_reviewed_by(review)
            .ok()
            .flatten()
            .filter(|i| i.outcome.is_none())
    }

    /// The implementer a workflow names, with its config and its role's spec.
    pub(super) fn implementer_of(workflow: &WorkflowSpec) -> Option<(ImplementSpec, RoleSpec)> {
        let implement = workflow.implement.clone()?;
        let spec = workflow.role.get(&implement.writer)?.clone();
        Some((implement, spec))
    }

    /// Everything about an issue run's implementation.
    ///
    /// # Errors
    ///
    /// When the run does not exist or the database cannot be read.
    pub fn implement_view(&self, flow: FlowId) -> Result<ImplementView, String> {
        let running = self.is_running(flow);
        let db = self.db()?;
        let state = db
            .flow(flow)
            .map_err(text)?
            .ok_or_else(|| format!("no run {flow}"))?;
        let planning = db.planning(flow).map_err(text)?;
        let project = planning
            .as_ref()
            .and_then(|p| self.app.project(p.project.as_str()).ok());
        let workflow = self.kept_workflow(flow, RunPart::Implement, project.as_ref());
        let configured = workflow.as_ref().and_then(Self::implementer_of);
        let implementation = db.implementation(flow).map_err(text)?;
        let rounds = db.rounds(flow).map_err(text)?;
        let checkpoints = db.implement_checkpoints(flow).map_err(text)?;
        let review_flow = implementation.as_ref().and_then(|i| i.review_flow);
        let (checks, findings, review) = match review_flow {
            Some(review) => {
                let findings = db.findings(review).map_err(text)?;
                let summary = db.flow(review).map_err(text)?.map(|f| ReviewSummary {
                    flow: review,
                    phase: f.state.phase,
                    running: self.is_running(review),
                    standing: findings
                        .iter()
                        .filter(|r| r.finding.dismissed.is_none())
                        .count(),
                });
                (db.checks(review).map_err(text)?, findings, summary)
            }
            None => (Vec::new(), Vec::new(), None),
        };
        let verifying = match (&configured, workflow.as_ref()) {
            (Some((implement, _)), Some(workflow)) => implement
                .checks(workflow)
                .into_iter()
                .map(|(id, _)| id.to_owned())
                .collect(),
            _ => Vec::new(),
        };
        let all_questions = db.questions(flow).map_err(text)?;
        let questions = db
            .implement_questions(flow)
            .map_err(text)?
            .into_iter()
            .filter_map(|link| {
                let question = all_questions
                    .iter()
                    .find(|q| q.id == link.question)?
                    .clone();
                Some(ImplementQuestionView { link, question })
            })
            .collect();
        let role = implementation
            .as_ref()
            .map(|i| i.role.clone())
            .or_else(|| configured.as_ref().map(|(c, _)| c.writer.clone()));
        let step = role.as_ref().and_then(|role| {
            db.steps(flow)
                .ok()?
                .into_iter()
                .find(|s| &s.role == role && s.profile == RoleProfile::Writer)
        });
        let sessions = match &step {
            Some(step) => db
                .invocations(flow)
                .map_err(text)?
                .into_iter()
                .filter(|i| i.step == step.id)
                .collect(),
            None => Vec::new(),
        };
        let lease = implementation.as_ref().and_then(|i| {
            db.open_leases(Some(&i.worktree))
                .ok()?
                .into_iter()
                .find(|l| l.flow == flow && l.kind == LeaseKind::Writer)
        });
        let open = state.state.end.is_none();
        let hold = state.state.hold.as_ref().map(Hold::kind);
        let phase = state.state.phase;
        let gate_open = matches!(phase, Phase::Planned | Phase::DesignReady)
            && self.implement_gate(flow)?.is_ok();
        let can_start = open
            && !running
            && configured.is_some()
            && hold.is_none()
            && (gate_open || phase == Phase::Implementing && implementation.is_none());
        let can_resume = open
            && !running
            && implementation.as_ref().is_some_and(|i| i.outcome.is_none())
            && matches!(
                phase,
                Phase::Implementing | Phase::Verifying | Phase::Reviewing | Phase::Fixing
            )
            && matches!(
                hold,
                None | Some(HoldKind::Interrupted | HoldKind::WaitingCapacity)
            );
        Ok(ImplementView {
            flow: state,
            key: planning.map(|p| p.key),
            implementer: configured.map(|(c, _)| c.writer),
            implementation,
            rounds,
            checkpoints,
            checks,
            verifying,
            review,
            findings,
            questions,
            sessions,
            step,
            lease,
            running,
            can_start,
            can_resume,
            notes: db.notes(flow).map_err(text)?,
        })
    }

    /// The issue's implementation, for the Implement tab, by project and key.
    ///
    /// # Errors
    ///
    /// When the database cannot be read.
    pub fn issue_implementation(
        &self,
        project_id: &str,
        key: &str,
    ) -> Result<Option<ImplementView>, String> {
        match self.issue_plan(project_id, key)? {
            Some(plan) => self.implement_view(plan.flow.id).map(Some),
            None => Ok(None),
        }
    }

    // ═══════════════════════════════ starting ═══════════════════════════════

    /// Move an issue's run to Implementing, through the gate that wants the approved plan and the
    /// design it needs, and start its implementer: Start implementing. Without an implementer
    /// configured the run moves there and waits, saying so.
    ///
    /// # Errors
    ///
    /// The gate's refusal, the lease's, or why the run is not where implementing follows. Nothing
    /// was recorded.
    pub fn start_implementing(self: &Arc<Self>, flow: FlowId) -> Result<(), Refusal> {
        let state = self.state(flow)?;
        if state.end.is_some() {
            return Err("this run has ended".into());
        }
        if self.is_running(flow) {
            return Err("this run is working now".into());
        }
        if state.phase == Phase::Implementing && self.open_implementation(flow).is_some() {
            return self.resume_implementation(flow);
        }
        if !matches!(
            state.phase,
            Phase::Planned | Phase::DesignReady | Phase::Implementing
        ) {
            return Err(format!(
                "implementing follows a plan, or a design, you approved; this run is at {:?}",
                state.phase
            )
            .into());
        }
        if state.phase != Phase::Implementing
            && let Err(blocked) = self.implement_gate(flow)?
        {
            return Err(format!("Not yet: {blocked}.").into());
        }
        let db = self.db()?;
        let planning = db
            .planning(flow)
            .map_err(text)?
            .ok_or("only a planned issue's run implements")?;
        let project = self.app.project(planning.project.as_str()).map_err(text)?;
        let configured = self
            .kept_workflow(flow, RunPart::Implement, Some(&project))
            .unwrap_or_default();
        if Self::implementer_of(&configured).is_none() {
            if state.phase != Phase::Implementing {
                self.record(
                    flow,
                    &FlowEvent::Advanced {
                        to: Phase::Implementing,
                    },
                )?;
            }
            self.file(&NewInboxItem {
                flow,
                kind: InboxKind::Failure,
                target: InboxTarget::Flow,
                title: format!("{}: nothing implements it yet", planning.key),
                body: format!(
                    "{}. The run waits at Implementing with its plan approved; Start implementing \
                     runs the writer once there is one.",
                    super::capitalise(&no_implementer())
                ),
            });
            self.notice.changed(flow);
            return Ok(());
        }
        self.begin_implementing(flow)
    }

    /// After a plan or a design approval opened the gate: implement, when the project names an
    /// implementer. A plan change you approved carries the open implementation on instead. What
    /// went wrong starting it is said in the inbox; the approval stands either way.
    pub(super) fn implement_after_approval(&self, flow: FlowId) {
        let Some(me) = self.me.upgrade() else { return };
        let started = match self
            .db()
            .ok()
            .and_then(|db| db.implementation(flow).ok().flatten())
        {
            Some(implementation) if implementation.outcome != Some(ImplementOutcome::Cancelled) => {
                me.carry_on_after_replan(flow, &implementation)
            }
            _ => {
                let configured = self
                    .db()
                    .ok()
                    .and_then(|db| db.planning(flow).ok().flatten())
                    .and_then(|p| self.app.project(p.project.as_str()).ok())
                    .and_then(|p| self.kept_workflow(flow, RunPart::Implement, Some(&p)))
                    .is_some_and(|w| Self::implementer_of(&w).is_some());
                if !configured {
                    return;
                }
                me.begin_implementing(flow)
            }
        };
        if let Err(refusal) = started {
            tracing::info!(%flow, %refusal, "implementing did not start by itself");
            let key = self.key_of(flow);
            self.file(&NewInboxItem {
                flow,
                kind: InboxKind::Failure,
                target: InboxTarget::Flow,
                title: format!("{key}: implementing did not start"),
                body: format!(
                    "{}. The approval stands; Start implementing in the issue's Implement tab \
                     runs the writer once that is fixed.",
                    super::capitalise(&refusal.message)
                ),
            });
        }
    }

    fn key_of(&self, flow: FlowId) -> String {
        self.db()
            .ok()
            .and_then(|db| db.planning(flow).ok().flatten())
            .map_or_else(|| "The issue".to_owned(), |p| p.key)
    }

    /// Take the worktree for `flow`'s writer, or say who has it. `true` when it was taken now,
    /// `false` when the run held it already.
    pub(super) fn take_writer_lease(
        &self,
        flow: FlowId,
        worktree: &str,
        role: &str,
    ) -> Result<bool, Refusal> {
        if let Some(lease) = self.writer_on(worktree) {
            return if lease.flow == flow {
                Ok(false)
            } else {
                Err(self.who_holds(&lease).into())
            };
        }
        let db = self.db()?;
        match db.acquire_lease(
            &NewLease {
                worktree: worktree.to_owned(),
                flow,
                kind: LeaseKind::Writer,
                holder: role.to_owned(),
            },
            self.now(),
        ) {
            Ok(_) => Ok(true),
            Err(FlowStoreError::Leased { lease }) => Err(self.who_holds(&lease).into()),
            Err(e) => Err(text(e).into()),
        }
    }

    /// Start implementing a run whose plan, and design when it needs one, you approved.
    fn begin_implementing(self: &Arc<Self>, flow: FlowId) -> Result<(), Refusal> {
        let _one = self.starting.lock();
        let state = self.state(flow)?;
        if state.end.is_some() {
            return Err("this run has ended".into());
        }
        if self.is_running(flow) {
            return Err("this run is working now".into());
        }
        if !matches!(
            state.phase,
            Phase::Planned | Phase::DesignReady | Phase::Implementing
        ) || state.hold.is_some()
        {
            return Err(format!(
                "implementing follows a plan, or a design, you approved; this run is at {:?}{}",
                state.phase,
                if state.hold.is_some() {
                    ", and waiting"
                } else {
                    ""
                }
            )
            .into());
        }
        let db = self.db()?;
        let planning = db
            .planning(flow)
            .map_err(text)?
            .ok_or("only a planned issue's run implements")?;
        let project = self.app.project(planning.project.as_str()).map_err(text)?;
        // Kept as implementing begins, below: an implementer configured since the plan was approved
        // is the one that runs.
        let workflow = self
            .kept_workflow(flow, RunPart::Implement, Some(&project))
            .unwrap_or_default();
        let (implement, spec) =
            Self::implementer_of(&workflow).ok_or_else(|| Refusal::from(no_implementer()))?;
        let Some((plan, _, PlanStanding::Approved { .. })) = self.approved_plan(flow)? else {
            return Err("approve the plan first: implementing follows an approved plan".into());
        };
        if state.phase != Phase::Implementing
            && let Err(blocked) = self.implement_gate(flow)?
        {
            return Err(format!("Not yet: {blocked}.").into());
        }
        self.host.preflight(&[spec.provider.id()])?;
        let worktree = self
            .app
            .worktree(&project, &planning.worktree)
            .map_err(|e| format!("the issue's worktree is not available: {e}"))?;
        let head = self
            .app
            .git
            .rev_parse(&worktree.path, "HEAD")
            .map_err(text)?
            .map(|c| c.as_str().to_owned())
            .ok_or("the issue's worktree has no commit to start from")?;
        let path = worktree.path.to_string_lossy().into_owned();
        let taken = self.take_writer_lease(flow, &path, &implement.writer)?;
        // From here on, anything refused gives the worktree back.
        let recorded = (|| -> Result<(), String> {
            if state.phase != Phase::Implementing {
                self.record(
                    flow,
                    &FlowEvent::Advanced {
                        to: Phase::Implementing,
                    },
                )?;
            }
            db.begin_implementation(
                &NewImplementation {
                    flow,
                    project: planning.project.clone(),
                    worktree: planning.worktree.clone(),
                    key: planning.key.clone(),
                    role: implement.writer.clone(),
                    recipe: implement.recipe.clone(),
                    base_sha: head,
                    plan_revision: plan.revision,
                    scope: ImplementScope::of(&plan.plan),
                    bounds: implement.bounds(),
                },
                self.now(),
            )
            .map_err(text)?;
            self.begin_part(flow, RunPart::Implement, &workflow)?;
            Ok(())
        })();
        if let Err(reason) = recorded {
            if taken {
                self.release_leases_of(flow, LeaseKind::Writer, LeaseRelease::Failed);
            }
            return Err(reason.into());
        }
        self.settle(flow, &[InboxKind::Failure, InboxKind::Interrupted]);
        tracing::info!(%flow, role = %implement.writer, "started implementing");
        self.notice.changed(flow);
        Ok(self.spawn_implementation(flow, WriteTurn::Next)?)
    }

    /// A plan revision you approved after a plan change: the implementation moves to it and the
    /// writer carries on in its conversation, with the new plan's own fix budget.
    fn carry_on_after_replan(
        self: &Arc<Self>,
        flow: FlowId,
        implementation: &Implementation,
    ) -> Result<(), Refusal> {
        let Some((plan, _, PlanStanding::Approved { .. })) = self.approved_plan(flow)? else {
            return Err("the plan is not approved".into());
        };
        let scope = ImplementScope::of(&plan.plan);
        let added: Vec<String> = scope
            .areas
            .iter()
            .filter(|area| !implementation.scope.areas.contains(area))
            .cloned()
            .collect();
        let db = self.db()?;
        let project = self
            .app
            .project(implementation.project.as_str())
            .map_err(text)?;
        let (_, spec) =
            Self::implementer_of(&self.run_workflow(flow, RunPart::Implement, &project)?)
                .ok_or_else(|| Refusal::from(no_implementer()))?;
        self.host.preflight(&[spec.provider.id()])?;
        let worktree = self
            .app
            .worktree(&project, &implementation.worktree)
            .map_err(|e| format!("the issue's worktree is not available: {e}"))?;
        let path = worktree.path.to_string_lossy().into_owned();
        let taken = self.take_writer_lease(flow, &path, &implementation.role)?;
        let moved = (|| -> Result<(), String> {
            db.change_implementation(
                flow,
                &ImplementChange {
                    plan: Some((plan.revision, scope)),
                    outcome: if implementation.outcome.is_some() {
                        Field::Clear
                    } else {
                        Field::Keep
                    },
                    stopped: Field::Clear,
                    remaining: Some(Vec::new()),
                    ..ImplementChange::default()
                },
                self.now(),
            )
            .map_err(text)?;
            self.record(
                flow,
                &FlowEvent::Advanced {
                    to: Phase::Implementing,
                },
            )?;
            // A new round for the wider plan, whose turn is not sent yet.
            if let Some(step) = db
                .steps(flow)
                .map_err(text)?
                .into_iter()
                .find(|s| s.role == implementation.role)
            {
                db.update_step(
                    step.id,
                    &StepChange::status(StepStatus::Pending)
                        .detail(format!("plan revision {} approved", plan.revision)),
                    self.now(),
                )
                .map_err(text)?;
            }
            Ok(())
        })();
        if let Err(reason) = moved {
            if taken {
                self.release_leases_of(flow, LeaseKind::Writer, LeaseRelease::Failed);
            }
            return Err(reason.into());
        }
        self.notice.changed(flow);
        Ok(self.spawn_implementation(
            flow,
            WriteTurn::Replanned {
                revision: plan.revision,
                added,
            },
        )?)
    }

    /// Carry an implementation on after a quit, a crash, a limit or a failure, under its lease,
    /// taken again if recovery let it go.
    ///
    /// # Errors
    ///
    /// Why it did not: it is running, waiting for your answer or approval, or has ended.
    pub fn resume_implementation(self: &Arc<Self>, flow: FlowId) -> Result<(), Refusal> {
        if self.is_running(flow) {
            return Err("its implementer is working already".into());
        }
        let state = self.state(flow)?;
        if let Some(end) = &state.end {
            return Err(format!("this run has already ended ({end:?})").into());
        }
        let Some(implementation) = self.open_implementation(flow) else {
            return self.begin_implementing(flow);
        };
        if !matches!(
            state.phase,
            Phase::Implementing | Phase::Verifying | Phase::Reviewing | Phase::Fixing
        ) {
            return Err(format!(
                "there is nothing to resume: the run is at {:?}",
                state.phase
            )
            .into());
        }
        let project = self
            .app
            .project(implementation.project.as_str())
            .map_err(text)?;
        let (_, spec) =
            Self::implementer_of(&self.run_workflow(flow, RunPart::Implement, &project)?)
                .ok_or_else(|| Refusal::from(no_implementer()))?;
        self.host.preflight(&[spec.provider.id()])?;
        let worktree = self
            .app
            .worktree(&project, &implementation.worktree)
            .map_err(|e| format!("the issue's worktree is not available: {e}"))?;
        self.take_writer_lease(flow, &worktree.path.to_string_lossy(), &implementation.role)?;
        {
            let _held = self.holds.lock();
            match state.hold.as_ref().map(Hold::kind) {
                Some(kind @ (HoldKind::Interrupted | HoldKind::WaitingCapacity)) => {
                    self.record(flow, &FlowEvent::Released { hold: kind })?;
                }
                Some(HoldKind::AwaitingAnswer) => {
                    return Err("it is waiting for your answer, which is what resumes it".into());
                }
                Some(other) => {
                    return Err(format!(
                        "this run is waiting on {other:?}, which a resume does not answer"
                    )
                    .into());
                }
                None => {}
            }
        }
        if let Some(review) = implementation.review_flow {
            self.ready_review_to_resume(review);
        }
        self.settle(
            flow,
            &[
                InboxKind::Interrupted,
                InboxKind::Failure,
                InboxKind::Capacity,
            ],
        );
        self.notice.changed(flow);
        let turn = if matches!(state.phase, Phase::Implementing | Phase::Fixing) {
            WriteTurn::Resume
        } else {
            WriteTurn::Next
        };
        Ok(self.spawn_implementation(flow, turn)?)
    }

    /// The implementation's review was cut off with it: release its interruption and put the roles
    /// it cut off back to pending, so the drive runs them again on the same snapshot.
    fn ready_review_to_resume(&self, review: FlowId) {
        let Ok(db) = self.db() else { return };
        let Ok(state) = self.state(review) else {
            return;
        };
        if state.end.is_some() {
            return;
        }
        {
            let _held = self.holds.lock();
            if let Some(kind @ (HoldKind::Interrupted | HoldKind::WaitingCapacity)) =
                state.hold.as_ref().map(Hold::kind)
            {
                let _ = self.record(review, &FlowEvent::Released { hold: kind });
            }
        }
        for step in db.steps(review).unwrap_or_default() {
            if step.status.wants_a_run() && step.status != StepStatus::Pending {
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
        }
        self.settle(
            review,
            &[
                InboxKind::Interrupted,
                InboxKind::Failure,
                InboxKind::Capacity,
            ],
        );
    }

    pub(super) fn spawn_implementation(
        self: &Arc<Self>,
        flow: FlowId,
        turn: WriteTurn,
    ) -> Result<(), String> {
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
            .name(format!("implement-{flow}"))
            .spawn(move || {
                {
                    struct Done<'a>(&'a Engine, FlowId);
                    impl Drop for Done<'_> {
                        fn drop(&mut self) {
                            self.0.runs.lock().remove(&self.1);
                            self.0.notice.changed(self.1);
                        }
                    }
                    let _done = Done(&engine, flow);
                    if let Err(reason) = engine.drive_implementation(flow, &run, turn)
                        && !engine.quitting()
                    {
                        tracing::warn!(%flow, %reason, "an implementation stopped");
                        engine.implementer_failed(flow, &reason);
                    }
                }
                // An answer that arrived while this thread was finishing waits for it to go.
                engine.implementation_next(flow);
            });
        if let Err(error) = spawned {
            self.runs.lock().remove(&flow);
            return Err(format!(
                "wtm could not start the implementer's thread: {error}"
            ));
        }
        Ok(())
    }

    // ═══════════════════════════════ driving ═══════════════════════════════

    fn implement_ctx(&self, flow: FlowId) -> Result<ImplementCtx, String> {
        let db = self.db()?;
        let implementation = db
            .implementation(flow)
            .map_err(text)?
            .ok_or_else(|| format!("run {flow} has not started implementing"))?;
        let planning = db
            .planning(flow)
            .map_err(text)?
            .ok_or_else(|| format!("run {flow} has not planned anything"))?;
        let project = self
            .app
            .project(implementation.project.as_str())
            .map_err(|e| format!("the project is no longer registered: {e}"))?;
        let worktree = self
            .app
            .worktree(&project, &implementation.worktree)
            .map_err(|e| format!("the issue's worktree is no longer there: {e}"))?;
        let workflow = self.run_workflow(flow, RunPart::Implement, &project)?;
        let implement = workflow.implement.clone().ok_or_else(no_implementer)?;
        let spec = workflow
            .role
            .get(&implementation.role)
            .cloned()
            .ok_or_else(|| {
                format!(
                    "role `{}` is no longer in your workflow config, so the implementer cannot \
                     run",
                    implementation.role
                )
            })?;
        let plan = db
            .plans(flow)
            .map_err(text)?
            .into_iter()
            .find(|p| p.revision == implementation.plan_revision)
            .ok_or_else(|| {
                format!(
                    "plan revision {} is missing from the record",
                    implementation.plan_revision
                )
            })?;
        Ok(ImplementCtx {
            flow,
            implement,
            spec,
            plan,
            context: planning.context,
            tracker: planning.tracker,
            branch: worktree.branch().map(|b| b.as_str().to_owned()),
            worktree_path: worktree.path.clone(),
            patterns: workflow.instruction_sources(),
            workflow,
            implementation,
        })
    }

    fn drive_implementation(&self, flow: FlowId, run: &Run, turn: WriteTurn) -> Result<(), String> {
        let mut turn = turn;
        loop {
            if self.quitting() || run.cancel.load(Ordering::SeqCst) {
                return Ok(());
            }
            let state = self.state(flow)?;
            if state.end.is_some() || state.hold.is_some() {
                return Ok(());
            }
            // A plan change you agreed to while the thread worked: it stops here, and the plan
            // goes back to you.
            if matches!(state.phase, Phase::Implementing | Phase::Fixing)
                && self.pending_replan(flow)?.is_some()
            {
                self.replan(flow)?;
                return Ok(());
            }
            let ctx = self.implement_ctx(flow)?;
            let next = match state.phase {
                Phase::Implementing | Phase::Fixing => {
                    let this = std::mem::replace(&mut turn, WriteTurn::Next);
                    self.write_round(&ctx, run, state.phase, this)?
                }
                Phase::Verifying => self.verify(&ctx)?,
                Phase::Reviewing => self.review_round(&ctx, run)?,
                _ => Next::Stop,
            };
            self.notice.changed(flow);
            if next == Next::Stop {
                return Ok(());
            }
        }
    }

    /// The writer's one step: its role's, made the first time.
    fn implementer_step(&self, ctx: &ImplementCtx) -> Result<Step, String> {
        let db = self.db()?;
        if let Some(step) = db
            .steps(ctx.flow)
            .map_err(text)?
            .into_iter()
            .find(|s| s.role == ctx.implementation.role)
        {
            return Ok(step);
        }
        db.add_steps(
            ctx.flow,
            &[NewStep {
                stage: 2,
                role: ctx.implementation.role.clone(),
                provider: ctx.spec.provider,
                model: ctx.spec.model.clone(),
                profile: RoleProfile::Writer,
                workspace: Workspace::Worktree,
                timeout_ms: ctx.spec.timeout_ms(),
                retries: ctx.spec.retries,
                status: StepStatus::Pending,
                stands_in_for: None,
                handover: None,
                detail: None,
            }],
            self.now(),
        )
        .map_err(text)?
        .into_iter()
        .next()
        .ok_or_else(|| "the implementer's step was not kept".to_owned())
    }

    /// One round of writing: the plan's, or a fix round's. Its writer turn, then what the writer
    /// left committed, and on to Verifying.
    fn write_round(
        &self,
        ctx: &ImplementCtx,
        run: &Run,
        phase: Phase,
        turn: WriteTurn,
    ) -> Result<Next, String> {
        let db = self.db()?;
        let rounds = db.rounds(ctx.flow).map_err(text)?;
        let plan_revision = ctx.implementation.plan_revision;
        let open = rounds.iter().rev().find(|r| r.ended_at.is_none()).cloned();
        let round = match (open, phase) {
            (Some(round), _) => round,
            (None, Phase::Implementing) => {
                let done = rounds.iter().any(|r| {
                    r.kind == RoundKind::Implement
                        && r.plan_revision == plan_revision
                        && r.ended_at.is_some()
                });
                if done {
                    // The round ended and the run did not move on: cut off in between.
                    self.record(
                        ctx.flow,
                        &FlowEvent::Advanced {
                            to: Phase::Verifying,
                        },
                    )?;
                    return Ok(Next::Continue);
                }
                db.start_round(
                    &NewRound {
                        flow: ctx.flow,
                        kind: RoundKind::Implement,
                        plan_revision,
                        findings: Vec::new(),
                        checks: Vec::new(),
                        note: None,
                    },
                    self.now(),
                )
                .map_err(text)?
            }
            (None, _) => {
                // Fixing with no round to write: the decision that would have recorded one was cut
                // off. The review decides again.
                self.record(
                    ctx.flow,
                    &FlowEvent::Advanced {
                        to: Phase::Reviewing,
                    },
                )?;
                return Ok(Next::Continue);
            }
        };
        let step = self.implementer_step(ctx)?;
        let invocations: Vec<Invocation> = db
            .invocations(ctx.flow)
            .map_err(text)?
            .into_iter()
            .filter(|i| i.step == step.id)
            .collect();
        let resume = invocations
            .iter()
            .rev()
            .find_map(|i| i.provider_session.clone());
        // Whether this round's turn was sent before: a round's writer step is set pending when the
        // round begins, so anything else means a turn of it ran — cut off, failed or limited.
        // Timestamps cannot say it: two records can share a millisecond.
        let sent_this_round = step.status != StepStatus::Pending;
        // Handed in, and cut off before the app finished with it: nothing to send, only to finish.
        if matches!(turn, WriteTurn::Next | WriteTurn::Resume)
            && step.status == StepStatus::Done
            && let Some(Submission::Handoff(handoff)) = &step.result
        {
            return self.finish_round(ctx, &step, &round, handoff);
        }
        let resumed = resume.is_some();
        let words = match turn {
            WriteTurn::Resume if sent_this_round => {
                brief::implementer_resume_turn(ctx.key(), resumed)
            }
            WriteTurn::Answered => {
                let asked: BTreeSet<QuestionId> = db
                    .implement_questions(ctx.flow)
                    .map_err(text)?
                    .into_iter()
                    .filter(|l| {
                        l.kind == ImplementAsk::Writer
                            && l.plan_revision == ctx.implementation.plan_revision
                    })
                    .map(|l| l.question)
                    .collect();
                let answered: Vec<Question> = db
                    .questions(ctx.flow)
                    .map_err(text)?
                    .into_iter()
                    .filter(|q| asked.contains(&q.id) && q.answer.is_some())
                    .collect();
                brief::implementer_answers_turn(&answered, resumed)
            }
            WriteTurn::Keep { paths } => brief::implementer_keep_turn(&paths),
            WriteTurn::Replanned { revision, added } if resumed => {
                brief::implementer_replanned_turn(revision, &added, resumed)
            }
            _ if sent_this_round => brief::implementer_resume_turn(ctx.key(), resumed),
            _ => match round.kind {
                RoundKind::Implement => brief::implementer_opening_turn(ctx.key()),
                RoundKind::Fix => self.fix_words(ctx, &round, resumed)?,
                RoundKind::Revise => brief::implementer_revise_turn(
                    round.note.as_deref().unwrap_or_default(),
                    &ctx.implementation
                        .remaining
                        .iter()
                        .map(|r| {
                            format!(
                                "F{} {} ({})",
                                r.finding,
                                r.title.trim(),
                                left_words(&r.because)
                            )
                        })
                        .collect::<Vec<_>>(),
                    resumed,
                ),
            },
        };
        let end = self.implementer_turn(ctx, run, &step, &round, resume.as_deref(), &words);
        match end {
            TurnEnd::Handed(handoff) => self.finish_round(ctx, &step, &round, &handoff),
            TurnEnd::Asked(asked) => {
                self.update_step(
                    &step,
                    &StepChange::status(StepStatus::Pending).detail(format!(
                        "asked {}; waiting for your answer",
                        if asked.len() == 1 {
                            "a question".to_owned()
                        } else {
                            format!("{} questions", asked.len())
                        }
                    )),
                );
                Ok(Next::Stop)
            }
            TurnEnd::Failed(reason) => {
                self.update_step(
                    &step,
                    &StepChange::status(StepStatus::Failed).detail(reason.clone()),
                );
                self.implementer_failed(ctx.flow, &reason);
                Ok(Next::Stop)
            }
            TurnEnd::Limit { resets_at, message } => {
                self.update_step(
                    &step,
                    &StepChange {
                        resets_at: Field::from(resets_at),
                        ..StepChange::status(StepStatus::WaitingCapacity)
                    }
                    .detail(message.clone()),
                );
                {
                    let _held = self.holds.lock();
                    if self
                        .state(ctx.flow)
                        .is_ok_and(|s| s.hold.is_none() && s.end.is_none())
                    {
                        self.record(
                            ctx.flow,
                            &FlowEvent::Held {
                                hold: Hold::WaitingCapacity {
                                    provider: step.provider.id().to_owned(),
                                    resets_at,
                                },
                            },
                        )?;
                    }
                }
                self.file(&NewInboxItem {
                    flow: ctx.flow,
                    kind: InboxKind::Capacity,
                    target: super::on(&step),
                    title: format!(
                        "{}: the implementer is paused, {} limit reached",
                        ctx.key(),
                        step.provider
                    ),
                    body: format!(
                        "{message}\n\nResume carries on in the same conversation, under the same \
                         lease. wtm never uses an API key."
                    ),
                });
                Ok(Next::Stop)
            }
            TurnEnd::Cancelled => {
                self.update_step(&step, &StepChange::status(StepStatus::Cancelled));
                Ok(Next::Stop)
            }
            TurnEnd::Quitting => Ok(Next::Stop),
        }
    }

    /// The words of a fix round's turn: its findings, as they are now, and its failing checks with
    /// the end of what they printed.
    fn fix_words(
        &self,
        ctx: &ImplementCtx,
        round: &Round,
        resumed: bool,
    ) -> Result<String, String> {
        let db = self.db()?;
        let current: Vec<FindingRecord> = match ctx.implementation.review_flow {
            Some(review) => db.findings(review).map_err(text)?,
            None => Vec::new(),
        };
        let asks: Vec<brief::FixAsk> = round
            .findings
            .iter()
            .map(|tried| {
                let found = current.iter().find(|r| r.finding.id == tried.finding);
                let place = tried.location.as_ref().map_or_else(
                    || "the whole change".to_owned(),
                    |at| format!("{}:{}-{}", at.path, at.start_line, at.end_line),
                );
                brief::FixAsk {
                    id: format!("F{}", tried.finding),
                    place,
                    title: tried.title.clone(),
                    detail: found.map_or_else(String::new, |r| r.finding.rationale.clone()),
                }
            })
            .collect();
        let checks: Vec<(String, String)> = match ctx.implementation.review_flow {
            Some(review) => {
                let dir = FlowDir::new(&self.app.config.paths().flow_dir, review);
                let records = db.checks(review).map_err(text)?;
                round
                    .checks
                    .iter()
                    .map(|check| {
                        let tail = records
                            .iter()
                            .rev()
                            .find(|r| &r.check == check)
                            .and_then(|r| r.output.as_ref())
                            .and_then(|blob| std::fs::read_to_string(dir.resolve(&blob.path)).ok())
                            .map(|out| {
                                let lines: Vec<&str> = out.lines().collect();
                                lines[lines.len().saturating_sub(40)..].join("\n")
                            })
                            .unwrap_or_default();
                        (check.clone(), tail)
                    })
                    .collect()
            }
            None => Vec::new(),
        };
        let number = db
            .rounds(ctx.flow)
            .map_err(text)?
            .iter()
            .filter(|r| r.kind == RoundKind::Fix && r.plan_revision == round.plan_revision)
            .count();
        Ok(brief::implementer_fix_turn(
            u32::try_from(number).unwrap_or(u32::MAX),
            &asks,
            &checks,
            resumed,
        ))
    }

    /// The writer handed in: commit what it left inside the plan, or ask about what is outside it;
    /// end the round; and verify.
    fn finish_round(
        &self,
        ctx: &ImplementCtx,
        step: &Step,
        round: &Round,
        handoff: &Handoff,
    ) -> Result<Next, String> {
        let db = self.db()?;
        self.update_step(
            step,
            &StepChange {
                result: Some(Submission::Handoff(handoff.clone())),
                detail: Field::Clear,
                ..StepChange::status(StepStatus::Done)
            },
        );
        let runner = self.app.runner.as_ref();
        let changed = wtm_git::checkpoint::changed(runner, &ctx.worktree_path).map_err(text)?;
        let outliers = ctx.implementation.scope.outliers(&changed);
        if !outliers.is_empty() {
            let kept_before = db
                .implement_questions(ctx.flow)
                .map_err(text)?
                .into_iter()
                .filter(|link| {
                    link.kind == ImplementAsk::Outliers
                        && link.plan_revision == ctx.implementation.plan_revision
                        && outliers.iter().all(|p| link.paths.contains(p))
                })
                .any(|link| {
                    db.question(link.question).ok().flatten().is_some_and(|q| {
                        q.answer
                            .as_ref()
                            .is_some_and(|a| a.choice != Some(CHANGE_THE_PLAN))
                    })
                });
            if kept_before {
                self.update_step(
                    step,
                    &StepChange::status(StepStatus::Failed).detail(format!(
                        "left {} outside the plan after you kept to it",
                        listed(&outliers)
                    )),
                );
                self.implementer_failed(
                    ctx.flow,
                    &format!(
                        "`{}` handed in with {} changed outside the plan, after you kept to the \
                         plan. Nothing was committed. Put {} back by hand, or change the plan, \
                         then Resume",
                        ctx.implementation.role,
                        listed(&outliers),
                        them(&outliers).0
                    ),
                );
                return Ok(Next::Stop);
            }
            self.ask_about_scope(
                ctx.flow,
                Some(step.id),
                &ctx.implementation,
                ImplementAsk::Outliers,
                &outliers,
                &[],
            );
            {
                let _held = self.holds.lock();
                if self
                    .state(ctx.flow)
                    .is_ok_and(|s| s.hold.is_none() && s.end.is_none())
                {
                    self.record(
                        ctx.flow,
                        &FlowEvent::Held {
                            hold: Hold::AwaitingAnswer,
                        },
                    )?;
                }
            }
            self.update_step(
                step,
                &StepChange::status(StepStatus::Done).detail(format!(
                    "handed in with {} outside the plan; waiting for your answer",
                    listed(&outliers)
                )),
            );
            return Ok(Next::Stop);
        }
        if !changed.is_empty() {
            let message = match round.kind {
                RoundKind::Implement => format!("{}: {}", ctx.key(), subject(&handoff.summary)),
                RoundKind::Fix => format!(
                    "{}: fix what the review found\n\n{}",
                    ctx.key(),
                    handoff.summary.trim()
                ),
                RoundKind::Revise => format!(
                    "{}: {}\n\n{}",
                    ctx.key(),
                    subject(&handoff.summary),
                    handoff.summary.trim()
                ),
            };
            let made = wtm_git::checkpoint::commit(
                runner,
                &ctx.worktree_path,
                &message,
                wtm_agent::authority::protected_path,
            )
            .map_err(|e| format!("what the writer left was not committed: {e}"))?;
            db.record_implement_checkpoint(
                ctx.flow,
                round.round,
                made.commit.as_str(),
                &message,
                &made.paths,
                CheckpointBy::App,
                self.now(),
            )
            .map_err(text)?;
            tracing::info!(flow = %ctx.flow, commit = %made.commit.short(),
                "committed what the implementer left");
        }
        let head = self
            .app
            .git
            .rev_parse(&ctx.worktree_path, "HEAD")
            .map_err(text)?
            .map(|c| c.as_str().to_owned())
            .ok_or("the issue's worktree has no HEAD")?;
        let rounds = db.rounds(ctx.flow).map_err(text)?;
        let from = rounds
            .iter()
            .rev()
            .filter(|r| r.round < round.round)
            .find_map(|r| r.head_sha.clone())
            .unwrap_or_else(|| ctx.implementation.base_sha.clone());
        let touched: Vec<String> = if from == head {
            Vec::new()
        } else {
            wtm_git::push::numstat(runner, &ctx.worktree_path, &from, &head)
                .map_err(text)?
                .into_iter()
                .map(|(path, _, _)| path)
                .collect()
        };
        db.end_round(
            ctx.flow,
            round.round,
            Some(&head),
            &touched,
            Some(handoff),
            self.now(),
        )
        .map_err(text)?;
        // After the writer's round, the remote is read for anything wtm did not put there.
        self.watch_remote(ctx.flow);
        self.record(
            ctx.flow,
            &FlowEvent::Advanced {
                to: Phase::Verifying,
            },
        )?;
        Ok(Next::Continue)
    }

    /// One writer turn: one session, opened (resumed when there is a conversation), sent `words`,
    /// and read until it hands in, asks, or stops.
    #[allow(clippy::too_many_lines, clippy::too_many_arguments)]
    fn implementer_turn(
        &self,
        ctx: &ImplementCtx,
        run: &Run,
        step: &Step,
        round: &Round,
        resume: Option<&str>,
        words: &str,
    ) -> TurnEnd {
        let Ok(db) = self.db() else {
            return TurnEnd::Quitting;
        };
        let attempt = db
            .invocations(ctx.flow)
            .map_or(0, |all| all.iter().filter(|i| i.step == step.id).count())
            + 1;
        let attempt = u32::try_from(attempt).unwrap_or(u32::MAX);
        // A writer's session counts against the run-session caps like any role's.
        let _seat = match self.take_seat(ctx.flow, run, step) {
            Ok(seat) => seat,
            Err(super::AttemptEnd::Cancelled) => return TurnEnd::Cancelled,
            Err(_) => return TurnEnd::Quitting,
        };
        let invocation = match db.start_invocation(
            &NewInvocation {
                flow: ctx.flow,
                step: step.id,
                attempt,
                role: step.role.clone(),
                provider: step.provider,
                model: step.model.clone(),
                profile: RoleProfile::Writer,
                snapshot: None,
                cwd: ctx.worktree_path.to_string_lossy().into_owned(),
            },
            self.now(),
        ) {
            Ok(invocation) => invocation,
            Err(e) => return TurnEnd::Failed(text(e)),
        };
        self.update_step(
            step,
            &StepChange {
                detail: Field::Clear,
                resets_at: Field::Clear,
                ..StepChange::status(StepStatus::Running)
            },
        );
        let knowledge = self.implementer_knowledge(ctx, attempt, invocation.id);
        let answers: Vec<Question> = db
            .questions(ctx.flow)
            .unwrap_or_default()
            .into_iter()
            .filter(|q| q.role.as_deref() == Some(step.role.as_str()) && q.answer.is_some())
            .collect();
        let design = self.approved_design(ctx.flow);
        let checks: Vec<(String, CheckSpec)> = ctx
            .spec
            .checks
            .iter()
            .filter_map(|id| Some((id.clone(), ctx.workflow.check.get(id)?.clone())))
            .collect();
        let verifying: Vec<String> = ctx
            .implement
            .checks(&ctx.workflow)
            .into_iter()
            .map(|(id, _)| id.to_owned())
            .collect();
        let brief_text = brief::implementer_brief(&brief::ImplementerFacts {
            role: &step.role,
            spec: &ctx.spec,
            key: ctx.key(),
            tracker: ctx.tracker,
            worktree_path: &ctx.worktree_path.to_string_lossy(),
            branch: ctx.branch.as_deref(),
            context: ctx.context.as_ref(),
            plan: &ctx.plan,
            scope: &ctx.implementation.scope,
            design: design.as_deref(),
            knowledge: knowledge.as_deref(),
            checks: &checks,
            verifying: &verifying,
            answers: &answers,
        });

        let (tx, rx) = mpsc::channel();
        let key = (ctx.flow, step.role.clone());
        self.live.lock().insert(
            key.clone(),
            Arc::new(Live {
                output: OutputKind::Handoff,
                tx: tx.clone(),
                step: step.id,
                invocation: invocation.id,
                cwd: ctx.worktree_path.clone(),
                // A check the writer asks for runs against whatever the worktree holds then; an
                // empty tree is never equal to one, so its record reads as stale at once.
                tree_sha: String::new(),
                checks: checks.into_iter().collect(),
                dir: FlowDir::new(&self.app.config.paths().flow_dir, ctx.flow),
                fix: None,
                planner: None,
                designer: None,
                implement: Some(Arc::new(ImplementLive {
                    round: round.round,
                    plan_revision: ctx.implementation.plan_revision,
                    scope: ctx.implementation.scope.clone(),
                    worktree: ctx.worktree_path.clone(),
                    key: ctx.key().to_owned(),
                })),
                project: ctx.project().to_owned(),
                worktree: ctx.implementation.worktree.clone(),
                start_services: ctx.workflow.start_services,
                browser: None,
            }),
        );
        let session_spec = RoleSession {
            project: ctx.project().to_owned(),
            worktree: ctx.implementation.worktree.clone(),
            role: FlowRole {
                flow: ctx.flow,
                role: step.role.clone(),
                authority: RoleProfile::Writer,
                browser: None,
                design: false,
            },
            provider: step.provider.id().to_owned(),
            model: step.model.clone(),
            effort: ctx.spec.effort.clone(),
            brief: brief_text,
            resume: resume.map(str::to_owned),
            cwd: Some(ctx.worktree_path.clone()),
            skills: ctx.spec.skill_names().map(str::to_owned).collect(),
        };
        let end = match self
            .host
            .open(&session_spec, Arc::new(super::InvocationSink { tx }))
        {
            Ok(session) => {
                let _ = db.update_invocation(
                    invocation.id,
                    &InvocationChange {
                        status: Some(InvocationStatus::Running),
                        session: Some(session.as_str().to_owned()),
                        ..InvocationChange::default()
                    },
                    self.now(),
                );
                self.keep_process(invocation.id, &session);
                self.notice.changed(ctx.flow);
                let end =
                    self.read_implementer(ctx, run, step, &session, invocation.id, words, &rx);
                if !matches!(end, TurnEnd::Quitting) {
                    self.host.close(&session);
                }
                end
            }
            Err(reason) => TurnEnd::Failed(format!("its session did not start: {reason}")),
        };
        self.live.lock().remove(&key);
        if !matches!(end, TurnEnd::Quitting) {
            let (status, detail) = match &end {
                TurnEnd::Handed(_) => (InvocationStatus::Submitted, None),
                TurnEnd::Asked(_) => (
                    InvocationStatus::Interrupted,
                    Some(
                        "wtm ended the turn after its question, and closed the session until you \
                         answer"
                            .to_owned(),
                    ),
                ),
                TurnEnd::Failed(reason) => (InvocationStatus::Failed, Some(reason.clone())),
                TurnEnd::Limit { message, .. } => {
                    (InvocationStatus::LimitReached, Some(message.clone()))
                }
                TurnEnd::Cancelled => (InvocationStatus::Cancelled, None),
                TurnEnd::Quitting => unreachable!("matched above"),
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

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn read_implementer(
        &self,
        ctx: &ImplementCtx,
        run: &Run,
        step: &Step,
        session: &wtm_core::model::SessionId,
        invocation: wtm_core::flow::InvocationId,
        words: &str,
        rx: &Receiver<Signal>,
    ) -> TurnEnd {
        let Ok(db) = self.db() else {
            return TurnEnd::Quitting;
        };
        let started = self.clock.monotonic_ms();
        let deadline = started.saturating_add(step.timeout_ms);
        let skills_deadline = started.saturating_add(self.tuning.skills_wait_ms);
        let mut skills_checked = ctx.spec.skills.is_empty();
        let mut sent = false;
        let mut nudges = self.tuning.nudges;
        let mut submitted: Option<Box<Handoff>> = None;
        let mut asked: Vec<QuestionId> = Vec::new();
        let mut interrupted_at: Option<u64> = None;
        let mut settle_at: Option<u64> = None;
        let say = |text: &str| -> Result<(), TurnEnd> {
            self.host.send(session, text).map_err(|reason| {
                TurnEnd::Failed(format!("its session would not take the turn: {reason}"))
            })
        };
        if skills_checked {
            if let Err(end) = say(words) {
                return end;
            }
            sent = true;
        }
        loop {
            match rx.recv_timeout(Duration::from_millis(self.tuning.tick_ms)) {
                Ok(Signal::Submitted(Submission::Handoff(handoff))) => {
                    submitted = Some(Box::new(handoff));
                    settle_at = Some(self.clock.monotonic_ms() + self.tuning.settle_ms);
                }
                Ok(Signal::Submitted(_) | Signal::Published(_))
                | Err(RecvTimeoutError::Timeout) => {}
                Ok(Signal::Asked(question)) => {
                    asked.push(question);
                    if interrupted_at.is_none() {
                        if let Err(reason) = self.host.interrupt(session) {
                            tracing::info!(flow = %ctx.flow, %reason, "could not interrupt the implementer's turn; closing its session");
                            return TurnEnd::Asked(asked);
                        }
                        interrupted_at = Some(self.clock.monotonic_ms());
                    }
                }
                Ok(Signal::Event(event)) => match event {
                    AgentEvent::SkillsListed { skills } if !skills_checked => {
                        if let Some(problem) = missing_skill(&step.role, &ctx.spec, &skills) {
                            return TurnEnd::Failed(problem.to_string());
                        }
                        skills_checked = true;
                        if let Err(end) = say(words) {
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
                        self.notice.changed(ctx.flow);
                    }
                    AgentEvent::TurnFinished { .. } if sent => {
                        if !asked.is_empty() {
                            return TurnEnd::Asked(asked);
                        }
                        if let Some(handoff) = submitted {
                            return TurnEnd::Handed(handoff);
                        }
                        if nudges == 0 {
                            return TurnEnd::Failed(
                                "it ended its turn without handing in or asking".to_owned(),
                            );
                        }
                        nudges -= 1;
                        if let Err(end) = say(&brief::implementer_nudge()) {
                            return end;
                        }
                    }
                    AgentEvent::LimitReached { message, resets_at } => {
                        if !asked.is_empty() {
                            return TurnEnd::Asked(asked);
                        }
                        if let Some(handoff) = submitted {
                            return TurnEnd::Handed(handoff);
                        }
                        return TurnEnd::Limit { resets_at, message };
                    }
                    AgentEvent::Failed { message } => {
                        if !asked.is_empty() {
                            return TurnEnd::Asked(asked);
                        }
                        if let Some(handoff) = submitted {
                            return TurnEnd::Handed(handoff);
                        }
                        return TurnEnd::Failed(message);
                    }
                    AgentEvent::ApprovalRequested { request, .. } => {
                        self.file(&NewInboxItem {
                            flow: ctx.flow,
                            kind: InboxKind::Approval,
                            target: super::on(step),
                            title: format!(
                                "`{}` asked for a permission its profile should not need",
                                step.role
                            ),
                            body: format!(
                                "{}\n\nA run session's profile decides what it may do, so this \
                                 prompt means the profile and the provider disagree. The \
                                 implementer waits until its timeout.",
                                super::describe(&request)
                            ),
                        });
                    }
                    _ => {}
                },
                Ok(Signal::Exited(summary)) => {
                    if !asked.is_empty() {
                        return TurnEnd::Asked(asked);
                    }
                    if let Some(handoff) = submitted {
                        return TurnEnd::Handed(handoff);
                    }
                    return TurnEnd::Failed(format!("its session ended: {summary}"));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return TurnEnd::Failed("the session's events stopped arriving".to_owned());
                }
            }
            if self.quitting() {
                return TurnEnd::Quitting;
            }
            if run.cancel.load(Ordering::SeqCst) {
                return TurnEnd::Cancelled;
            }
            let now = self.clock.monotonic_ms();
            if interrupted_at.is_some_and(|at| now >= at.saturating_add(INTERRUPT_GRACE_MS)) {
                return TurnEnd::Asked(asked);
            }
            if let Some(handoff) = submitted.take() {
                if settle_at.is_some_and(|at| now >= at) {
                    return TurnEnd::Handed(handoff);
                }
                submitted = Some(handoff);
            }
            if !skills_checked && now >= skills_deadline {
                return TurnEnd::Failed(
                    wtm_core::flow::SkillProblem::Unlisted {
                        role: step.role.clone(),
                        provider: ctx.spec.provider,
                    }
                    .to_string(),
                );
            }
            if now >= deadline {
                if let Some(handoff) = submitted {
                    return TurnEnd::Handed(handoff);
                }
                return TurnEnd::Failed(format!(
                    "it had handed nothing in after {}",
                    super::minutes(step.timeout_ms)
                ));
            }
        }
    }

    /// The repository's instructions and your rules, for a writer whose role takes them: the
    /// planner's retrieval, against the files the plan covers, recorded against this attempt.
    fn implementer_knowledge(
        &self,
        ctx: &ImplementCtx,
        attempt: u32,
        invocation: wtm_core::flow::InvocationId,
    ) -> Option<String> {
        let wants_instructions = ctx
            .spec
            .inputs
            .contains(&wtm_core::flow::RoleInput::Instructions);
        let planned: Vec<String> = ctx
            .implementation
            .scope
            .files
            .iter()
            .cloned()
            .chain(ctx.implementation.scope.areas.iter().cloned())
            .collect();
        let query = wtm_core::flow::RuleQuery::for_role(&ctx.spec, &planned);
        if !wants_instructions && query.is_none() {
            return None;
        }
        let instructions = wants_instructions
            .then(|| {
                crate::knowledge::instruction_set(
                    self.app.git.as_ref(),
                    &ctx.worktree_path,
                    &ctx.patterns,
                )
                .map(|(set, _)| set)
                .map_err(|error| {
                    tracing::warn!(flow = %ctx.flow, error = %error.message,
                        "could not read the instruction sources for the implementer");
                })
                .ok()
            })
            .flatten();
        let db = self.db().ok()?;
        let rules = query.and_then(|query| {
            let all = db.rules(&ctx.implementation.project).ok()?;
            let selection =
                wtm_core::flow::knowledge::select(&all, &ctx.implementation.project, &query);
            if let Err(error) = db.record_applications(
                &wtm_core::flow::ReviewInvocation {
                    flow: ctx.flow,
                    role: ctx.implementation.role.clone(),
                    attempt,
                },
                &selection.applied,
                self.now(),
            ) {
                tracing::warn!(flow = %ctx.flow, %error, "could not record the rules the implementer was given");
            }
            Some((selection, all))
        });
        if let Some(set) = &instructions {
            let _ = db.update_invocation(
                invocation,
                &InvocationChange {
                    instruction_sources: Some(
                        set.documents.iter().map(|d| d.path.clone()).collect(),
                    ),
                    ..InvocationChange::default()
                },
                self.now(),
            );
        }
        Some(wtm_core::flow::knowledge::brief(
            instructions.as_ref(),
            rules
                .as_ref()
                .map(|(selection, all)| (selection, all.as_slice())),
        ))
    }

    // ═══════════════════════════════ verifying ═══════════════════════════════

    /// The implementation's review run: the issue's own work item reviewed, made the first time.
    fn implementation_review(&self, ctx: &ImplementCtx) -> Result<FlowId, String> {
        let db = self.db()?;
        if let Some(review) = ctx.implementation.review_flow
            && self.state(review).is_ok_and(|s| s.end.is_none())
            && db.review(review).map_err(text)?.is_some()
        {
            return Ok(review);
        }
        let item = db
            .flow(ctx.flow)
            .map_err(text)?
            .ok_or_else(|| format!("no run {}", ctx.flow))?
            .work_item;
        let now = self.now();
        let flow = match db.start_flow(item, FlowKind::ReviewMine, now) {
            Ok(flow) => flow.id,
            Err(FlowStoreError::FlowAlreadyOpen { existing }) => {
                // A review of the issue you ran yourself before implementing. One that finished is
                // kept as done, as Run Review does with one you did not act on; one still working
                // or waiting is yours to finish first.
                let open = db
                    .flow(existing)
                    .map_err(text)?
                    .ok_or_else(|| format!("flow {existing} has gone"))?;
                let done = !self.is_running(existing)
                    && open.state.hold.is_none()
                    && open.state.phase == Phase::Consolidated;
                if !done {
                    return Err(format!(
                        "a review of this issue's worktree (run {existing}) is still open; let it \
                         finish or stop it, then Resume"
                    ));
                }
                db.record(existing, &FlowEvent::Finished, now)
                    .map_err(text)?;
                self.settle(existing, &super::ALL_KINDS);
                self.notice.changed(existing);
                db.start_flow(item, FlowKind::ReviewMine, now)
                    .map_err(text)?
                    .id
            }
            Err(e) => return Err(text(e)),
        };
        let base = self.resolve_base(&ctx.worktree_path, None)?;
        db.create_review(
            &NewReviewRun {
                flow,
                project: ctx.implementation.project.clone(),
                worktree: ctx.implementation.worktree.clone(),
                worktree_path: ctx.worktree_path.to_string_lossy().into_owned(),
                recipe: ctx.implementation.recipe.clone(),
                base,
            },
            now,
        )
        .map_err(text)?;
        // Its review is part of the issue's run, so it reads the recipe the run started with, not
        // the one your config names now.
        db.capture_workflow(flow, RunPart::Review, &ctx.workflow, now)
            .map_err(text)?;
        db.change_implementation(
            ctx.flow,
            &ImplementChange {
                review_flow: Some(flow),
                ..ImplementChange::default()
            },
            now,
        )
        .map_err(text)?;
        self.notice.changed(flow);
        Ok(flow)
    }

    /// Verifying: a snapshot of the new tree on the review run, then the checks, run by the app.
    /// The first round of a plan runs every configured check; a fix round runs the ones its
    /// changes affect and the ones it was asked to make pass.
    fn verify(&self, ctx: &ImplementCtx) -> Result<Next, String> {
        let db = self.db()?;
        let review_flow = self.implementation_review(ctx)?;
        let review = db
            .review(review_flow)
            .map_err(text)?
            .ok_or("the implementation's review has no record")?;
        let dir = FlowDir::new(&self.app.config.paths().flow_dir, review_flow);
        let snapshots = Snapshots {
            git: Arc::clone(&self.app.git),
            files: Arc::clone(&self.app.files),
        };
        let label = ScratchLabel::new(&format!(
            "verify-{}",
            self.scratch.fetch_add(1, Ordering::SeqCst)
        ))
        .ok_or("a scratch label")?;
        let snapshot = snapshots
            .take(&ctx.worktree_path, &review.base, &dir, &label)
            .map_err(|e| format!("the snapshot could not be taken: {e}"))?;
        let current = review
            .snapshot
            .and_then(|id| db.snapshot(id).ok().flatten());
        if current.as_ref() != Some(&snapshot) {
            db.record_snapshot(review_flow, &snapshot, self.now())
                .map_err(text)?;
        }
        let configured = ctx.implement.checks(&ctx.workflow);
        let rounds = db.rounds(ctx.flow).map_err(text)?;
        let names: Vec<String> = match rounds.last() {
            // After a fix or a revision, only the checks its changes can affect run again.
            Some(round) if matches!(round.kind, RoundKind::Fix | RoundKind::Revise) => {
                let mut names = affected_checks(configured.iter().copied(), &round.changed);
                for asked in &round.checks {
                    if !names.contains(asked) && configured.iter().any(|(id, _)| id == asked) {
                        names.push(asked.clone());
                    }
                }
                names
            }
            _ => configured.iter().map(|(id, _)| (*id).to_owned()).collect(),
        };
        let round = rounds.last().map_or(1, |r| r.round);
        let mut blocked = false;
        for name in &names {
            let Some(spec) = ctx.workflow.check.get(name) else {
                continue;
            };
            let place = super::checks::CheckPlace {
                flow: review_flow,
                project: ctx.project().to_owned(),
                step: None,
                invocation: None,
                role: None,
                cwd: ctx.worktree_path.clone(),
                tree_sha: snapshot.tree_sha.clone(),
                dir: dir.clone(),
            };
            let gated = if spec.needs.is_empty() {
                Vec::new()
            } else {
                let mut gated = self.gate(ctx.project(), &ctx.implementation.worktree, &spec.needs);
                if !wtm_core::flow::gate_passes(&gated)
                    && ctx.workflow.start_policy(name) == wtm_core::flow::StartServices::Auto
                {
                    gated = self.bring_up(ctx.project(), &ctx.implementation.worktree, &spec.needs);
                }
                gated
            };
            let (saved, _) = self.gated_run(
                name,
                spec,
                &place,
                gated,
                Some(&format!("verifying {}, round {round}", ctx.key())),
            )?;
            blocked |= saved.status == CheckStatus::Blocked;
            self.notice.changed(ctx.flow);
        }
        if blocked {
            self.file_start(review_flow);
        }
        self.record(
            ctx.flow,
            &FlowEvent::Advanced {
                to: Phase::Reviewing,
            },
        )?;
        Ok(Next::Continue)
    }

    // ═══════════════════════════════ reviewing ═══════════════════════════════

    /// Reviewing: the recipe reads the snapshot — every role the first time, then the roles whose
    /// files a fix changed — and then the loop decides: another fix round, or the hand-over.
    fn review_round(&self, ctx: &ImplementCtx, run: &Run) -> Result<Next, String> {
        let db = self.db()?;
        let review_flow = self.implementation_review(ctx)?;
        let state = self.state(review_flow)?;
        let first = db.steps(review_flow).map_err(text)?.is_empty();
        if !first && state.phase == Phase::Consolidated && state.hold.is_none() {
            let affected: BTreeSet<String> =
                self.staleness(review_flow)?.affected.into_iter().collect();
            if !affected.is_empty() {
                self.rerun_affected(review_flow, &affected)?;
            }
        }
        let wants_a_run = first
            || db
                .steps(review_flow)
                .map_err(text)?
                .iter()
                .any(|s| s.status.wants_a_run());
        if wants_a_run || self.state(review_flow)?.phase != Phase::Consolidated {
            let child = Arc::new(Run::default());
            {
                let mut runs = self.runs.lock();
                if runs.contains_key(&review_flow) {
                    return Err(format!(
                        "its review (run {review_flow}) is running somewhere else"
                    ));
                }
                runs.insert(review_flow, Arc::clone(&child));
            }
            self.notice.changed(review_flow);
            // The review's thread is this one. Stopping the run stops it too (`stop_implementation`).
            let driven = self.drive_inner(review_flow, &child);
            self.runs.lock().remove(&review_flow);
            self.notice.changed(review_flow);
            if self.quitting() || run.cancel.load(Ordering::SeqCst) {
                return Ok(Next::Stop);
            }
            if let Err(reason) = driven {
                self.fail(review_flow, &reason);
                return Err(format!("its review could not run: {reason}"));
            }
        }
        let review_state = self.state(review_flow)?;
        if review_state.end.is_some()
            || review_state.hold.is_some()
            || review_state.phase != Phase::Consolidated
        {
            return Err(format!(
                "its review (run {review_flow}) did not finish: it is {:?}",
                review_state.status()
            ));
        }
        if self.pending_replan(ctx.flow)?.is_some() {
            self.replan(ctx.flow)?;
            return Ok(Next::Stop);
        }
        self.decide(ctx, review_flow)
    }

    /// Put the roles a fix affected back to pending on the review run, as Rerun affected does.
    fn rerun_affected(&self, review_flow: FlowId, roles: &BTreeSet<String>) -> Result<(), String> {
        let db = self.db()?;
        let review = db
            .review(review_flow)
            .map_err(text)?
            .ok_or("the implementation's review has no record")?;
        let project = self.app.project(review.project.as_str()).map_err(text)?;
        let workflow = self.run_workflow(review_flow, RunPart::Review, &project)?;
        let worktree = self
            .app
            .worktree(&project, &review.worktree)
            .map_err(|e| format!("the worktree is no longer there: {e}"))?;
        let snapshot = review
            .snapshot
            .and_then(|id| db.snapshot(id).ok().flatten())
            .ok_or("the implementation's review has no snapshot")?;
        let known: Option<BTreeSet<String>> = self
            .app
            .git
            .tree_changes(&worktree.path, &snapshot.base_sha, &snapshot.tree_sha)
            .ok()
            .filter(|listed| !listed.truncated)
            .map(|listed| listed.paths.into_iter().collect());
        let replanned =
            wtm_core::flow::plan(&workflow, &review.recipe, known.as_ref()).map_err(text)?;
        let skipped: BTreeSet<&str> = replanned
            .stages
            .iter()
            .flat_map(|stage| &stage.skipped)
            .map(|role| role.id.as_str())
            .collect();
        for step in db
            .steps(review_flow)
            .map_err(text)?
            .iter()
            .filter(|s| roles.contains(&s.role))
        {
            let change = if skipped.contains(step.role.as_str()) {
                StepChange::status(StepStatus::Skipped).detail("none of its paths changed")
            } else {
                StepChange {
                    detail: Field::Clear,
                    resets_at: Field::Clear,
                    skill_problem: Field::Clear,
                    ..StepChange::status(StepStatus::Pending)
                }
            };
            db.update_step(step.id, &change, self.now()).map_err(text)?;
            let _ = db.resolve_inbox(
                review_flow,
                InboxKind::Failure,
                Some(&InboxTarget::Step {
                    step: step.id,
                    role: step.role.clone(),
                }),
                self.now(),
            );
        }
        self.record(
            review_flow,
            &FlowEvent::Advanced {
                to: Phase::Reviewing,
            },
        )?;
        Ok(())
    }

    /// What follows a review: another fix round, or the hand-over. Asks once about findings the
    /// plan does not cover.
    fn decide(&self, ctx: &ImplementCtx, review_flow: FlowId) -> Result<Next, String> {
        let db = self.db()?;
        let findings = db.findings(review_flow).map_err(text)?;
        let configured: Vec<String> = ctx
            .implement
            .checks(&ctx.workflow)
            .into_iter()
            .map(|(id, _)| id.to_owned())
            .collect();
        let failing = failing_checks(&db.checks(review_flow).map_err(text)?, &configured);
        let rounds = db.rounds(ctx.flow).map_err(text)?;
        let next = next_round(&LoopFacts {
            findings: &findings,
            failing: &failing,
            scope: &ctx.implementation.scope,
            rounds: &rounds,
            plan_revision: ctx.implementation.plan_revision,
            bounds: ctx.implementation.bounds,
        });
        let outside = next.outside_plan();
        if !outside.is_empty() {
            let mut paths: Vec<String> = Vec::new();
            for (_, of) in &outside {
                for path in of {
                    if !paths.contains(path) {
                        paths.push(path.clone());
                    }
                }
            }
            let ids: Vec<wtm_core::flow::FindingId> = outside.iter().map(|(id, _)| *id).collect();
            self.ask_about_scope(
                ctx.flow,
                None,
                &ctx.implementation,
                ImplementAsk::Findings,
                &paths,
                &ids,
            );
        }
        match next {
            NextRound::Fix {
                findings: fixing,
                checks,
                ..
            } => {
                let attempted: Vec<AttemptedFinding> = findings
                    .iter()
                    .filter(|r| fixing.contains(&r.finding.id))
                    .map(AttemptedFinding::of)
                    .collect();
                let round = db
                    .start_round(
                        &NewRound {
                            flow: ctx.flow,
                            kind: RoundKind::Fix,
                            plan_revision: ctx.implementation.plan_revision,
                            findings: attempted,
                            checks,
                            note: None,
                        },
                        self.now(),
                    )
                    .map_err(text)?;
                tracing::info!(flow = %ctx.flow, round = round.round,
                    findings = round.findings.len(), checks = round.checks.len(),
                    "starting a fix round");
                // The round's turn is not sent yet: what `write_round` reads to know it.
                let step = self.implementer_step(ctx)?;
                self.update_step(
                    &step,
                    &StepChange::status(StepStatus::Pending).detail(format!(
                        "fix round: {} finding{} and {} check{} to fix",
                        round.findings.len(),
                        if round.findings.len() == 1 { "" } else { "s" },
                        round.checks.len(),
                        if round.checks.len() == 1 { "" } else { "s" },
                    )),
                );
                self.record(ctx.flow, &FlowEvent::Advanced { to: Phase::Fixing })?;
                Ok(Next::Continue)
            }
            NextRound::Stop {
                why,
                remaining,
                failing,
            } => {
                self.ready_to_test(ctx, review_flow, &why, &remaining, &failing)?;
                Ok(Next::Stop)
            }
        }
    }

    /// Ready to test: the record says what was left and why, the lease goes, and one item says so.
    fn ready_to_test(
        &self,
        ctx: &ImplementCtx,
        review_flow: FlowId,
        why: &StopReason,
        remaining: &[Remaining],
        failing: &[wtm_core::flow::FailingCheck],
    ) -> Result<(), String> {
        let db = self.db()?;
        db.change_implementation(
            ctx.flow,
            &ImplementChange {
                outcome: Field::Set(ImplementOutcome::Ready),
                remaining: Some(remaining.to_vec()),
                stopped: Field::Set(why.clone()),
                ..ImplementChange::default()
            },
            self.now(),
        )
        .map_err(text)?;
        self.release_leases_of(ctx.flow, LeaseKind::Writer, LeaseRelease::Completed);
        self.record(
            ctx.flow,
            &FlowEvent::Advanced {
                to: Phase::ReadyToTest,
            },
        )?;
        self.settle(
            ctx.flow,
            &[
                InboxKind::Failure,
                InboxKind::Interrupted,
                InboxKind::Capacity,
            ],
        );
        let rounds = db.rounds(ctx.flow).map_err(text)?;
        let fixes = rounds
            .iter()
            .filter(|r| {
                r.kind == RoundKind::Fix && r.plan_revision == ctx.implementation.plan_revision
            })
            .count();
        let commits = db.implement_checkpoints(ctx.flow).map_err(text)?.len();
        let mut latest: BTreeMap<String, CheckRun> = BTreeMap::new();
        for run in db.checks(review_flow).map_err(text)? {
            latest.insert(run.check.clone(), run);
        }
        let checks: Vec<String> = latest
            .values()
            .map(|run| {
                format!(
                    "{} {}",
                    run.check,
                    match run.status {
                        CheckStatus::Passed => "passed",
                        CheckStatus::Failed => "failed",
                        CheckStatus::TimedOut => "timed out",
                        CheckStatus::Error => "could not run",
                        CheckStatus::Skipped => "skipped",
                        CheckStatus::Blocked => "blocked: environment not started",
                    }
                )
            })
            .collect();
        let mut body = format!(
            "{commits} commit{} by the app on `{}`, {fixes} fix round{}. Checks: {}.",
            if commits == 1 { "" } else { "s" },
            ctx.branch.as_deref().unwrap_or("the issue's branch"),
            if fixes == 1 { "" } else { "s" },
            if checks.is_empty() {
                "none configured".to_owned()
            } else {
                checks.join(", ")
            }
        );
        match why {
            StopReason::Clean => body.push_str(" The review left nothing standing."),
            StopReason::NothingFixable => {}
            StopReason::Bounded { rounds } => {
                use std::fmt::Write as _;
                let _ = write!(
                    body,
                    " The fix loop stopped at its bound of {rounds} round{}.",
                    if *rounds == 1 { "" } else { "s" }
                );
            }
        }
        if !remaining.is_empty() || !failing.is_empty() {
            use std::fmt::Write as _;
            let _ = write!(
                body,
                " Left for you: {}.",
                remaining
                    .iter()
                    .map(|r| format!(
                        "F{} {} ({})",
                        r.finding,
                        r.title.trim(),
                        left_words(&r.because)
                    ))
                    .chain(failing.iter().map(|f| format!(
                        "check {} ({})",
                        f.check,
                        left_words(&f.because)
                    )))
                    .collect::<Vec<_>>()
                    .join("; ")
            );
        }
        let drafts = self
            .app
            .project(ctx.project())
            .ok()
            .and_then(|p| p.workflow.and_then(|w| w.forge))
            .is_some();
        body.push_str(if drafts {
            " The task branch is pushed and its draft pull request made or updated; nothing is \
             marked ready, moved or merged without you."
        } else {
            " Nothing has left this Mac."
        });
        self.file(&NewInboxItem {
            flow: ctx.flow,
            kind: InboxKind::ReviewReady,
            target: InboxTarget::Flow,
            title: format!("{} is ready to test", ctx.key()),
            body,
        });
        tracing::info!(flow = %ctx.flow, ?why, left = remaining.len(), "an implementation is ready to test");
        self.notice.changed(ctx.flow);
        // The plan makes the draft automatic: at the first hand-over and after each revision.
        if let Some(me) = self.me.upgrade() {
            me.draft_after_handover(ctx.flow);
        }
        Ok(())
    }

    // ═══════════════════════════════ the run tools ═══════════════════════════════

    /// `flow_checkpoint` from an implementation's writer: the scope check, then the app's commit.
    pub(super) fn implement_checkpoint(
        &self,
        role: &FlowRole,
        live: &Live,
        implement: &ImplementLive,
        message: &str,
    ) -> Result<String, String> {
        let db = self.db()?;
        let runner = self.app.runner.as_ref();
        let changed = wtm_git::checkpoint::changed(runner, &implement.worktree).map_err(text)?;
        if changed.is_empty() {
            return Err("Nothing has changed since the last checkpoint.".to_owned());
        }
        let outliers = implement.scope.outliers(&changed);
        if !outliers.is_empty() {
            if let Some(implementation) = db.implementation(role.flow).map_err(text)? {
                self.ask_about_scope(
                    role.flow,
                    Some(live.step),
                    &implementation,
                    ImplementAsk::Outliers,
                    &outliers,
                    &[],
                );
            }
            let (it, it_was) = them(&outliers);
            return Err(format!(
                "Not committed: you changed {} outside what the approved plan covers. The person \
                 has been asked whether the plan should cover {it}; a wider plan needs their \
                 approval. Until then, put {it} back as {it_was} and checkpoint again, or leave \
                 {it} and say why in your handoff.",
                listed(&outliers)
            ));
        }
        let made = wtm_git::checkpoint::commit(
            runner,
            &implement.worktree,
            message,
            wtm_agent::authority::protected_path,
        )
        .map_err(|e| e.to_string())?;
        let id = made.commit.as_str().to_owned();
        db.record_implement_checkpoint(
            role.flow,
            implement.round,
            &id,
            message,
            &made.paths,
            CheckpointBy::Writer,
            self.now(),
        )
        .map_err(text)?;
        self.app.flow_ledger.note_checkpoint(role, &id);
        tracing::info!(flow = %role.flow, commit = %made.commit.short(),
            files = made.paths.len(), "committed an implementation checkpoint");
        self.notice.changed(role.flow);
        Ok(format!(
            "Committed {} with {} file{}.",
            short(&id),
            made.paths.len(),
            if made.paths.len() == 1 { "" } else { "s" }
        ))
    }

    /// The writer asked something: keep the question, hold the run for it, and tell the role its
    /// turn is over, as a planner's question does. Its role thread interrupts the turn.
    pub(super) fn implement_ask(
        &self,
        role: &FlowRole,
        live: &Live,
        implement: &ImplementLive,
        ask: &crate::flow_tools::Ask,
    ) -> Result<String, String> {
        let db = self.db()?;
        let question = NewQuestion {
            flow: role.flow,
            step: Some(live.step),
            role: Some(role.role.clone()),
            text: ask.question.trim().to_owned(),
            why: ask.why.trim().to_owned(),
            unblocks: ask.unblocks.trim().to_owned(),
            blocks: Some(Phase::Verifying),
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
        let title = format!("{}: {}", implement.key, question.text);
        let (asked, filed) = db.ask(&question, &title, self.now()).map_err(text)?;
        db.link_implement_question(&ImplementQuestion {
            question: asked.id,
            flow: role.flow,
            kind: ImplementAsk::Writer,
            paths: Vec::new(),
            findings: Vec::new(),
            plan_revision: implement.plan_revision,
        })
        .map_err(text)?;
        {
            let _held = self.holds.lock();
            if self
                .state(role.flow)
                .is_ok_and(|s| s.hold.is_none() && s.end.is_none())
            {
                self.record(
                    role.flow,
                    &FlowEvent::Held {
                        hold: Hold::AwaitingAnswer,
                    },
                )?;
            }
        }
        if let Some(filed) = filed {
            self.notice.filed(&filed);
        }
        self.notice.changed(role.flow);
        if live.tx.send(Signal::Asked(asked.id)).is_err() {
            return Err(super::ended());
        }
        Ok(format!(
            "Filed as question {}. Your turn ends now: wtm closes this session and continues this \
             same conversation with the person's answer as your next message, which may be hours \
             from now. Do not guess the answer; end your turn.",
            asked.id
        ))
    }

    // ═══════════════════════════════ the plan's scope ═══════════════════════════════

    /// Ask, once per set of files, whether the plan should cover files it does not: what a writer
    /// changed, or where a review found problems. `None` when it was asked already, or could not
    /// be.
    fn ask_about_scope(
        &self,
        flow: FlowId,
        step: Option<wtm_core::flow::StepId>,
        implementation: &Implementation,
        kind: ImplementAsk,
        paths: &[String],
        findings: &[wtm_core::flow::FindingId],
    ) -> Option<QuestionId> {
        let db = self.db().ok()?;
        let asked = db.implement_questions(flow).ok()?.into_iter().find(|link| {
            link.kind == kind
                && link.plan_revision == implementation.plan_revision
                && paths.iter().all(|p| link.paths.contains(p))
                && db
                    .question(link.question)
                    .ok()
                    .flatten()
                    .is_some_and(|q| q.answer.is_none())
        });
        if let Some(link) = asked {
            return Some(link.question);
        }
        let mut covered: Vec<String> = implementation
            .scope
            .areas
            .iter()
            .map(|a| format!("`{a}`"))
            .collect();
        covered.extend(implementation.scope.files.iter().map(|f| format!("`{f}`")));
        let covered = if covered.is_empty() {
            "nothing named".to_owned()
        } else {
            covered.join(", ")
        };
        let (it, _) = them(paths);
        let (text, why, unblocks, blocks) = match kind {
            ImplementAsk::Findings => (
                format!(
                    "The review found problems outside the approved plan, in {}. Change the plan \
                     to include {it}?",
                    listed(paths)
                ),
                format!(
                    "Fixing {} would change files plan revision {} does not cover ({covered}). \
                     wtm fixes only what the plan covers; a plan that covers more goes back to you \
                     for approval, and implementing carries on once you approve it. Either way \
                     they are listed with what is left.",
                    findings
                        .iter()
                        .map(|f| format!("F{f}"))
                        .collect::<Vec<_>>()
                        .join(", "),
                    implementation.plan_revision
                ),
                "Fixing them in this work.".to_owned(),
                None,
            ),
            _ => (
                format!(
                    "`{}` changed {} outside the approved plan. Change the plan to include {it}?",
                    implementation.role,
                    listed(paths)
                ),
                format!(
                    "Plan revision {} covers {covered}. wtm commits nothing outside it; a plan \
                     that covers more goes back to you for approval, and implementing carries on \
                     once you approve it.",
                    implementation.plan_revision
                ),
                "Committing those files with the work, then verifying and reviewing it.".to_owned(),
                Some(Phase::Verifying),
            ),
        };
        let question = NewQuestion {
            flow,
            step,
            role: Some(implementation.role.clone()),
            text,
            why,
            unblocks,
            blocks,
            context: paths
                .iter()
                .map(|path| ContextLink::File {
                    path: path.clone(),
                    line: None,
                    note: None,
                })
                .collect(),
            recommendation: None,
            options: scope_options(kind, paths.len()),
        };
        let title = format!("{}: {} outside the plan", implementation.key, listed(paths));
        match db.ask(&question, &title, self.now()) {
            Ok((asked, filed)) => {
                if let Err(error) = db.link_implement_question(&ImplementQuestion {
                    question: asked.id,
                    flow,
                    kind,
                    paths: paths.to_vec(),
                    findings: findings.to_vec(),
                    plan_revision: implementation.plan_revision,
                }) {
                    tracing::warn!(%flow, %error, "could not keep what a question is about");
                }
                if let Some(filed) = filed {
                    self.notice.filed(&filed);
                }
                self.notice.changed(flow);
                Some(asked.id)
            }
            Err(error) => {
                tracing::warn!(%flow, %error, "could not ask about files outside the plan");
                None
            }
        }
    }

    /// The files you agreed the plan should cover, and why, that no plan revision covers yet.
    fn pending_replan(&self, flow: FlowId) -> Result<Option<(Vec<String>, String)>, String> {
        let db = self.db()?;
        let Some(implementation) = db.implementation(flow).map_err(text)? else {
            return Ok(None);
        };
        if implementation.outcome == Some(ImplementOutcome::Cancelled) {
            return Ok(None);
        }
        let mut paths: Vec<String> = Vec::new();
        let mut why: Vec<String> = Vec::new();
        for link in db.implement_questions(flow).map_err(text)? {
            if link.kind == ImplementAsk::Writer
                || link.plan_revision != implementation.plan_revision
            {
                continue;
            }
            let Some(question) = db.question(link.question).map_err(text)? else {
                continue;
            };
            let changing = question
                .answer
                .as_ref()
                .is_some_and(|a| a.choice == Some(CHANGE_THE_PLAN));
            if !changing {
                continue;
            }
            for path in &link.paths {
                if !implementation.scope.covers(path) && !paths.contains(path) {
                    paths.push(path.clone());
                }
            }
            if let Some(answer) = &question.answer
                && !answer.text.trim().is_empty()
            {
                why.push(answer.text.trim().to_owned());
            }
        }
        if paths.is_empty() {
            return Ok(None);
        }
        Ok(Some((paths, why.join(" "))))
    }

    /// Record the plan revision your answers ask for, and send the run back to Planned for your
    /// approval of it.
    fn replan(&self, flow: FlowId) -> Result<(), String> {
        let Some((paths, why)) = self.pending_replan(flow)? else {
            return Ok(());
        };
        let db = self.db()?;
        let implementation = db
            .implementation(flow)
            .map_err(text)?
            .ok_or("this run has no implementation")?;
        let plans = db.plans(flow).map_err(text)?;
        let latest = plans.last().ok_or("this run has no plan")?;
        // Pending approval already: widen the revision that waits rather than stack another.
        let waiting = latest.revision > implementation.plan_revision;
        let next = widened(
            &latest.plan,
            &paths,
            if why.is_empty() {
                "You said the plan should cover them."
            } else {
                &why
            },
        );
        let recorded = db
            .record_plan(
                &NewPlan {
                    flow,
                    step: None,
                    invocation: None,
                    plan: next,
                    issue: latest.issue.clone(),
                },
                self.now(),
            )
            .map_err(text)?;
        if let Some(open) = db
            .rounds(flow)
            .map_err(text)?
            .into_iter()
            .rev()
            .find(|r| r.ended_at.is_none())
        {
            let head = self
                .app
                .git
                .rev_parse(std::path::Path::new(&implementation.worktree), "HEAD")
                .ok()
                .flatten()
                .map(|c| c.as_str().to_owned());
            db.end_round(flow, open.round, head.as_deref(), &[], None, self.now())
                .map_err(text)?;
        }
        {
            let _held = self.holds.lock();
            let state = self.state(flow)?;
            if let Some(hold) = &state.hold
                && !(waiting && hold.kind() == HoldKind::AwaitingApproval)
            {
                self.record(flow, &FlowEvent::Released { hold: hold.kind() })?;
            }
            if state.phase != Phase::Planned {
                self.record(flow, &FlowEvent::Advanced { to: Phase::Planned })?;
            }
            if self.state(flow)?.hold.is_none() {
                self.record(
                    flow,
                    &FlowEvent::Held {
                        hold: Hold::AwaitingApproval,
                    },
                )?;
            }
        }
        self.file(&NewInboxItem {
            flow,
            kind: InboxKind::Approval,
            target: InboxTarget::Flow,
            title: format!(
                "{}: plan revision {} covers {}; approve it to carry on",
                implementation.key,
                recorded.revision,
                listed(&paths)
            ),
            body: format!(
                "You said the plan should cover {}. Plan revision {} does, and says why in its \
                 risks. Implementing waits for your approval of it, as it did for the first; \
                 approving carries the writer on in its same conversation, with the work so far \
                 kept. The writer still holds the worktree.",
                listed(&paths),
                recorded.revision
            ),
        });
        tracing::info!(%flow, revision = recorded.revision, "a plan change went back for approval");
        self.notice.changed(flow);
        Ok(())
    }

    /// After an answer to a question this implementation asked: a plan change, keeping to the plan,
    /// or the answers the writer waits for.
    pub(super) fn on_implement_answer(&self, question: &Question) {
        let Ok(db) = self.db() else { return };
        let Ok(links) = db.implement_questions(question.flow) else {
            return;
        };
        let Some(link) = links.into_iter().find(|l| l.question == question.id) else {
            return;
        };
        let Some(me) = self.me.upgrade() else { return };
        let flow = question.flow;
        let changing = question
            .answer
            .as_ref()
            .is_some_and(|a| a.choice == Some(CHANGE_THE_PLAN));
        match link.kind {
            ImplementAsk::Writer => me.implementation_next(flow),
            _ if changing => {
                // While the thread works it takes the change at its next stop; otherwise now.
                if !self.is_running(flow)
                    && let Err(reason) = self.replan(flow)
                {
                    tracing::warn!(%flow, %reason, "could not send a plan change for approval");
                }
            }
            ImplementAsk::Outliers => {
                let held = self
                    .state(flow)
                    .is_ok_and(|s| matches!(s.hold, Some(Hold::AwaitingAnswer)));
                if held
                    && !self.is_running(flow)
                    && self.release_answered(flow)
                    && let Err(reason) =
                        me.spawn_implementation(flow, WriteTurn::Keep { paths: link.paths })
                {
                    tracing::warn!(%flow, %reason, "could not tell the writer to keep to the plan");
                }
            }
            ImplementAsk::Findings => {}
        }
    }

    /// After a writer turn, or an answer: when every question the writer waits for is answered,
    /// and nothing else holds the run, the answers go to it.
    fn implementation_next(self: &Arc<Self>, flow: FlowId) {
        if self.quitting() || self.is_running(flow) {
            return;
        }
        let Ok(state) = self.state(flow) else { return };
        if state.end.is_some() || !matches!(state.hold, Some(Hold::AwaitingAnswer)) {
            return;
        }
        if !matches!(state.phase, Phase::Implementing | Phase::Fixing) {
            return;
        }
        let Ok(db) = self.db() else { return };
        let links = db.implement_questions(flow).unwrap_or_default();
        let waiting = links.iter().any(|link| {
            link.kind == ImplementAsk::Writer
                && db
                    .question(link.question)
                    .ok()
                    .flatten()
                    .is_some_and(|q| q.answer.is_none())
        });
        let outliers_waiting = links.iter().any(|link| {
            link.kind == ImplementAsk::Outliers
                && db
                    .question(link.question)
                    .ok()
                    .flatten()
                    .is_some_and(|q| q.answer.is_none())
        });
        let asked_by_writer = links.iter().any(|l| l.kind == ImplementAsk::Writer);
        if waiting || outliers_waiting || !asked_by_writer {
            return;
        }
        if self.release_answered(flow)
            && let Err(reason) = self.spawn_implementation(flow, WriteTurn::Answered)
        {
            tracing::warn!(%flow, %reason, "could not resume the implementer with its answers");
        }
    }

    // ═══════════════════════════════ stopping and recovery ═══════════════════════════════

    /// The implementer stopped for good this turn: say so where you will see it. The run stays
    /// where it was, under its lease, and Resume carries it on.
    fn implementer_failed(&self, flow: FlowId, reason: &str) {
        let key = self.key_of(flow);
        self.file(&NewInboxItem {
            flow,
            kind: InboxKind::Failure,
            target: InboxTarget::Flow,
            title: format!("{key}: implementing stopped"),
            body: format!(
                "{}. Every commit it made is kept, and the writer still holds the worktree: Resume \
                 carries it on in the same conversation; stopping the run lets the worktree go.",
                super::capitalise(reason)
            ),
        });
        self.notice.changed(flow);
    }

    /// Stop an implementation with its run: its thread and its review's are told, its review run is
    /// cancelled, and the worktree goes. What it committed stays.
    pub(super) fn stop_implementation(&self, flow: FlowId) {
        let Ok(db) = self.db() else { return };
        let Ok(Some(implementation)) = db.implementation(flow) else {
            return;
        };
        if implementation.outcome == Some(ImplementOutcome::Cancelled) {
            return;
        }
        if let Some(run) = self.runs.lock().get(&flow) {
            run.cancel.store(true, Ordering::SeqCst);
        }
        if let Some(review) = implementation.review_flow {
            if let Some(run) = self.runs.lock().get(&review) {
                run.cancel.store(true, Ordering::SeqCst);
            }
            if self.state(review).is_ok_and(|s| s.end.is_none()) {
                {
                    let _held = self.holds.lock();
                    let _ = self.record(review, &FlowEvent::Cancelled);
                }
                for step in db.steps(review).unwrap_or_default() {
                    if !step.status.is_settled() {
                        let _ = db.update_step(
                            step.id,
                            &StepChange::status(StepStatus::Cancelled),
                            self.now(),
                        );
                    }
                }
                self.settle(review, &super::ALL_KINDS);
            }
        }
        let _ = db.change_implementation(
            flow,
            &ImplementChange {
                outcome: Field::Set(ImplementOutcome::Cancelled),
                ..ImplementChange::default()
            },
            self.now(),
        );
        self.release_leases_of(flow, LeaseKind::Writer, LeaseRelease::Cancelled);
        self.notice.changed(flow);
    }

    /// The app is quitting with an implementation working, or found at launch with one that has
    /// no process: hold it `Interrupted`, unless it waits for an answer or an approval, which needs
    /// no process.
    pub(super) fn interrupt_implementation(&self, flow: FlowId, cause: InterruptCause) {
        let Ok(db) = self.db() else { return };
        let now = self.now();
        let Ok(Some(implementation)) = db.implementation(flow) else {
            return;
        };
        if let Some(step) = db
            .steps(flow)
            .unwrap_or_default()
            .into_iter()
            .find(|s| s.role == implementation.role)
        {
            for invocation in db.invocations(flow).unwrap_or_default() {
                if invocation.step == step.id && invocation.status.is_live() {
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
            if matches!(
                step.status,
                StepStatus::Running | StepStatus::WaitingCapacity
            ) {
                let _ = db.update_step(
                    step.id,
                    &StepChange {
                        resets_at: Field::Clear,
                        ..StepChange::status(StepStatus::Interrupted)
                    },
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
            match state.hold.as_ref().map(Hold::kind) {
                Some(
                    HoldKind::AwaitingAnswer | HoldKind::AwaitingApproval | HoldKind::Interrupted,
                ) => {
                    self.notice.changed(flow);
                    return;
                }
                Some(kind) => {
                    let _ = self.record(flow, &FlowEvent::Released { hold: kind });
                }
                None => {}
            }
            if let Err(e) = self.record(
                flow,
                &FlowEvent::Held {
                    hold: Hold::Interrupted { cause },
                },
            ) {
                tracing::warn!(%flow, error = %e, "could not record an implementation's interruption");
                return;
            }
        }
        let _ = db.resolve_inbox(flow, InboxKind::Capacity, None, now);
        self.file(&NewInboxItem {
            flow,
            kind: InboxKind::Interrupted,
            target: InboxTarget::Flow,
            title: match cause {
                InterruptCause::Quit => {
                    format!(
                        "{}: wtm quit while it was being implemented",
                        implementation.key
                    )
                }
                _ => format!(
                    "{}: wtm stopped while it was being implemented",
                    implementation.key
                ),
            },
            body:
                "Nothing was sent again. The writer still holds the issue's worktree, and anything \
                   it had not committed is still there. Resume carries it on where it stopped: the \
                   writer's same conversation, the checks, or the review."
                    .to_owned(),
        });
        self.notice.changed(flow);
    }

    /// At launch: an implementation caught mid-phase with no process. Returns the runs it held.
    pub(super) fn recover_implementations(&self) -> Result<Vec<FlowId>, String> {
        let db = self.db()?;
        let mut held = Vec::new();
        for implementation in db.implementations().map_err(text)? {
            let flow = implementation.flow;
            if implementation.outcome.is_some() || self.is_running(flow) {
                continue;
            }
            let Some(state) = db.flow(flow).map_err(text)?.map(|f| f.state) else {
                continue;
            };
            let mid = state.end.is_none()
                && matches!(
                    state.phase,
                    Phase::Implementing | Phase::Verifying | Phase::Reviewing | Phase::Fixing
                )
                && matches!(
                    state.hold.as_ref().map(Hold::kind),
                    None | Some(HoldKind::WaitingCapacity)
                );
            if mid {
                self.interrupt_implementation(flow, InterruptCause::Crash);
                held.push(flow);
            }
        }
        Ok(held)
    }
}

/// The configured checks whose newest record ran and did not pass. A blocked one did not run, and
/// one that could not start at all is the machine's, not the code's: neither is the writer's to fix.
fn failing_checks(records: &[CheckRun], configured: &[String]) -> Vec<String> {
    let mut latest: BTreeMap<&str, &CheckRun> = BTreeMap::new();
    for run in records {
        latest.insert(run.check.as_str(), run);
    }
    configured
        .iter()
        .filter(|id| {
            latest.get(id.as_str()).is_some_and(|run| {
                matches!(run.status, CheckStatus::Failed | CheckStatus::TimedOut)
            })
        })
        .cloned()
        .collect()
}

fn left_words(because: &wtm_core::flow::LeftBecause) -> String {
    use wtm_core::flow::LeftBecause;
    match because {
        LeftBecause::OutsidePlan { paths } => format!("outside the plan: {}", listed(paths)),
        LeftBecause::AttemptsSpent { attempts } => format!(
            "still there after {attempts} attempt{}",
            if *attempts == 1 { "" } else { "s" }
        ),
        LeftBecause::RoundsSpent => "the rounds were spent".to_owned(),
        LeftBecause::Conflict => "a conflict for you to settle".to_owned(),
        LeftBecause::Suggestion => "a suggestion".to_owned(),
    }
}

pub(super) fn no_implementer() -> String {
    "this project names no implementer: add `[workflow.implement]` with a `writer` role and a review \
     `recipe` to your own config"
        .to_owned()
}
