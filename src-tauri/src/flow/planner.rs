//! The planner: an issue's run from "Ready to plan" to "Planned, approved".
//!
//! # In order
//!
//! 1. **Start**, by itself once Start Work's worktree is ready when the project names a planner
//!    (`[workflow] planner`), or from **Plan** in the issue workspace. Nothing is spent before
//!    then.
//! 2. **Gather** (the run's own thread), again before every turn that can lead to a plan. The issue
//!    is read whole from the tracker, fresh, and kept in the mirror; then its sprint and goal, its parent and links as read-only context, the
//!    repository's instruction sources and your rules (R7's retrieval, for the role's inputs and
//!    knowledge scope), and the role's skills. All of it is recorded as the context drawer lists it
//!    (`PlanContext`), before the planner reads a word, and the brief is written from that record.
//! 3. **Investigate** (Preparing → Investigating): one headless session under the role's read-only
//!    profile (§6h), in the issue's worktree, sent one turn. Its result is a plan, through
//!    `flow_submit`, checked against the plan's schema.
//! 4. **Questions.** `flow_ask` files the question, holds the run `AwaitingAnswer`, and the engine
//!    interrupts the turn and closes the session itself: nothing waits on a CLI for hours. The
//!    answer — from the inbox or the workspace, the first one counts — resumes the *same provider
//!    conversation* (`--resume` for Claude, `thread/resume` for Codex) as a new session with a new
//!    run token, its first turn being the answer. **Discuss** is the same machinery for a reply in
//!    words: the conversation is resumed for one turn that only talks, and closed again.
//! 5. **Planned**: the plan is recorded as the run's next revision, with the issue as the planner
//!    was given it, and the run holds `AwaitingApproval`.
//! 6. **Your decision.** Approve (or approve and skip design) records an approval of the plan's
//!    revision *and* the issue's revision, read again at that moment; it releases the hold and the
//!    run stops at "Planned, approved", because implementing is a later item. Request changes
//!    resumes the same conversation with your note, and the next plan is the next revision.
//!
//! # Why the session is closed while it waits
//!
//! A question may be answered in a minute or tomorrow, after a quit, a reboot, or a usage reset. A
//! CLI held open for that long is a process that can die, a sandbox and a token that stay live with
//! nothing to do, and a pane that looks busy. The provider keeps the conversation on disk either
//! way, so the run keeps only its id (the invocation's `provider_session`), and an answer after a
//! relaunch resumes it exactly as an answer a minute later does. The token is fresh each time,
//! because a token is the session's, and the session is new.
//!
//! # Stale is measured when the issue is read
//!
//! The approval pins the issue's revision (`wtm_core::flow::planning`); whether it still holds is
//! asked whenever the workspace reads the issue — opening it, Check the issue — against the issue as
//! the mirror last read it. A change files one inbox item, and the workspace shows the diff.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use serde::Serialize;
use wtm_core::flow::planning::standing;
use wtm_core::flow::{
    Answer, ApprovalSubject, ContextLink, Field, Flow, FlowEvent, FlowId, FlowKind, Hold, HoldKind,
    InboxKind, InboxTarget, InterruptCause, Invocation, InvocationChange, InvocationStatus,
    IssueText, NewApproval, NewInboxItem, NewInvocation, NewPlan, NewPlanning, NewQuestion,
    NewStep, OutputKind, Phase, PlanApproval, PlanContext, PlanDecision, PlanDependency,
    PlanRecord, PlanReport, PlanRule, PlanStanding, PlanVerdict, Planning, Question, QuestionId,
    QuestionMessage, RoleSpec, RuleQuery, Speaker, StartJobState, Step, StepChange, StepStatus,
    Submission, TrackerKind, Workspace,
};
use wtm_core::model::AgentEvent;
use wtm_core::ports::{
    FlowStore, InboxStore, OutboxStore, PlanStore, RuleStore, RunStore, StartStore, TrackerMirror,
};

use super::comments::CommentView;
use super::{Engine, Live, Refusal, Run, Signal, brief, missing_skill, text};
use crate::app::FlowRole;
use crate::flow_session::RoleSession;

/// What one planner turn is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PlanTurn {
    /// The first: gather, then plan.
    Begin,
    /// Questions it asked were answered.
    Answered,
    /// You asked about a question before answering it.
    Discuss {
        question: QuestionId,
        message: String,
    },
    /// You asked for changes to a revision.
    Changes { revision: u32, note: String },
    /// Carry on after an interruption, a failure or a limit.
    Resume,
}

/// How one planner turn ended.
#[derive(Debug)]
enum TurnEnd {
    Submitted(Box<PlanReport>),
    /// It asked; the run waits for the answer.
    Asked(Vec<QuestionId>),
    /// A discussion turn's reply.
    Replied(String),
    Failed(String),
    Limit {
        resets_at: Option<u64>,
        message: String,
    },
    Cancelled,
    Quitting,
}

/// A planner turn that is running, as the run tools find it.
#[derive(Debug, Default)]
pub(super) struct PlannerLive {
    /// A discussion turn: it replies in words, and asks and submits nothing.
    pub(super) discussing: bool,
}

/// One question as the workspace and the inbox card show it: with its discussion and what was sent.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuestionThread {
    pub question: Question,
    pub discussion: Vec<QuestionMessage>,
    pub comments: Vec<CommentView>,
    /// Whether Discuss can be asked now: a planner's open question, with its run waiting for it
    /// and nothing running.
    pub can_discuss: bool,
    /// A discussion turn is running now.
    pub replying: bool,
    /// The issue Send to Jira would post to, for an issue's run.
    pub issue: Option<String>,
}

/// Everything the issue workspace shows about an issue's planning.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanView {
    pub flow: Flow,
    /// The role `[workflow] planner` names now, or `None` when the project names none.
    pub planner: Option<String>,
    pub planning: Option<Planning>,
    /// Oldest revision first.
    pub plans: Vec<PlanRecord>,
    pub decisions: Vec<PlanDecision>,
    pub standing: PlanStanding,
    /// The issue as the mirror last read it, which staleness is measured against.
    pub issue_now: Option<IssueText>,
    /// `issue_now`'s revision, which Approve sends back as the issue you saw.
    pub issue_revision_now: Option<String>,
    /// What differs between the issue the newest plan was made from and the issue now: the plan
    /// may not answer an edit made since.
    pub issue_changes: Vec<wtm_core::flow::IssueChange>,
    pub questions: Vec<QuestionThread>,
    /// The planner's sessions, oldest first: the same provider conversation, resumed.
    pub sessions: Vec<Invocation>,
    pub step: Option<Step>,
    pub running: bool,
    /// Whether Plan would start it now.
    pub can_start: bool,
}

/// What a planner's run reads while it works, resolved once per turn.
struct PlanCtx {
    flow: FlowId,
    planning: Planning,
    project_root: PathBuf,
    worktree_path: PathBuf,
    branch: Option<String>,
    spec: RoleSpec,
    fields: std::collections::BTreeMap<String, String>,
    patterns: Vec<String>,
}

/// How long a planner whose turn the engine interrupted gets to finish the turn on its own before
/// its session is closed anyway.
const INTERRUPT_GRACE_MS: u64 = 10_000;

impl Engine {
    // ═══════════════════════════════ reading ═══════════════════════════════

    /// Whether `flow` is an issue's run that has planned, or is planning.
    pub(super) fn is_planning(&self, flow: FlowId) -> bool {
        self.db()
            .ok()
            .and_then(|db| db.planning(flow).ok().flatten())
            .is_some()
    }

    /// The planner role a project names, with its spec, when it names one.
    fn planner_of(project: &wtm_core::model::Project) -> Option<(String, RoleSpec)> {
        let workflow = project.workflow.as_ref()?;
        let id = workflow.planner.clone()?;
        let spec = workflow.role.get(&id)?.clone();
        Some((id, spec))
    }

    /// Everything about an issue run's planning.
    ///
    /// # Errors
    ///
    /// When the run does not exist or the database cannot be read.
    pub fn plan_view(&self, flow: FlowId) -> Result<PlanView, String> {
        let running = self.is_running(flow);
        let db = self.db()?;
        let state = db
            .flow(flow)
            .map_err(text)?
            .ok_or_else(|| format!("no run {flow}"))?;
        let planning = db.planning(flow).map_err(text)?;
        let job = db.start_job_of(flow).map_err(text)?;
        let project = job
            .as_ref()
            .and_then(|job| self.app.project(job.project.as_str()).ok());
        let planner = project
            .as_ref()
            .and_then(Self::planner_of)
            .map(|(id, _)| id);
        let plans = db.plans(flow).map_err(text)?;
        let decisions = db.plan_decisions(flow).map_err(text)?;
        let issue_now = match &planning {
            Some(planning) => db
                .mirrored_issue(planning.tracker, &planning.key)
                .map_err(text)?
                .map(|(detail, _)| IssueText::from_detail(&detail)),
            None => None,
        };
        let standing = self.standing_of(flow, &plans, &decisions, issue_now.as_ref())?;
        let issue_changes = match (plans.last(), &issue_now) {
            (Some(latest), Some(now)) => wtm_core::flow::planning::diff(&latest.issue, now),
            _ => Vec::new(),
        };
        let step = planning.as_ref().and_then(|planning| {
            db.steps(flow)
                .ok()?
                .into_iter()
                .find(|s| s.role == planning.role)
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
        let questions = db
            .questions(flow)
            .map_err(text)?
            .into_iter()
            .map(|q| self.thread_of(q, running))
            .collect::<Result<Vec<_>, _>>()?;
        let can_start = planner.is_some()
            && !running
            && state.state.end.is_none()
            && state.state.hold.is_none()
            && state.state.phase == Phase::Preparing
            && job
                .as_ref()
                .is_some_and(|j| j.state == StartJobState::Ready);
        Ok(PlanView {
            flow: state,
            planner,
            planning,
            plans,
            decisions,
            standing,
            issue_revision_now: issue_now.as_ref().map(IssueText::revision),
            issue_now,
            issue_changes,
            questions,
            sessions,
            step,
            running,
            can_start,
        })
    }

    fn standing_of(
        &self,
        flow: FlowId,
        plans: &[PlanRecord],
        decisions: &[PlanDecision],
        issue_now: Option<&IssueText>,
    ) -> Result<PlanStanding, String> {
        let decision = decisions.last();
        let approval = match decision.map(|d| &d.verdict) {
            Some(PlanVerdict::Approved { approval, .. }) => self
                .db()?
                .approved(*approval)
                .map_err(text)?
                .and_then(|approved| match approved.subject {
                    ApprovalSubject::Plan(subject) => Some((approved.approval, *subject)),
                    _ => None,
                }),
            _ => None,
        };
        let _ = flow;
        Ok(standing(
            plans.last(),
            decision,
            approval.as_ref().map(|(a, s)| (a, s)),
            issue_now,
        ))
    }

    fn thread_of(&self, question: Question, running: bool) -> Result<QuestionThread, String> {
        let db = self.db()?;
        let discussion = db.discussion(question.id).map_err(text)?;
        let comments = self
            .comment_views(question.flow)?
            .into_iter()
            .filter(|c| c.request.question == question.id)
            .collect();
        let held = db
            .flow(question.flow)
            .map_err(text)?
            .is_some_and(|f| matches!(f.state.hold, Some(Hold::AwaitingAnswer)));
        let planned = db.planning(question.flow).map_err(text)?.is_some();
        let can_discuss =
            planned && question.answer.is_none() && question.blocks.is_some() && held && !running;
        let issue = db
            .planning(question.flow)
            .map_err(text)?
            .map(|planning| planning.key);
        Ok(QuestionThread {
            replying: running && planned && question.answer.is_none() && held,
            question,
            discussion,
            comments,
            can_discuss,
            issue,
        })
    }

    /// One question with its discussion and what was sent, for the card.
    ///
    /// # Errors
    ///
    /// When it does not exist or the database cannot be read.
    pub fn question_thread(&self, question: QuestionId) -> Result<QuestionThread, String> {
        let asked = self
            .db()?
            .question(question)
            .map_err(text)?
            .ok_or_else(|| format!("no question {question}"))?;
        let running = self.is_running(asked.flow);
        self.thread_of(asked, running)
    }

    // ═══════════════════════════════ starting ═══════════════════════════════

    /// Start planning when the issue's worktree has just become ready and the project names a
    /// planner. Quiet when it does not: the run waits at "Ready to plan".
    pub(super) fn plan_when_ready(&self, flow: FlowId) {
        let Some(me) = self.me.upgrade() else { return };
        let configured = self
            .db()
            .ok()
            .and_then(|db| db.start_job_of(flow).ok().flatten())
            .and_then(|job| self.app.project(job.project.as_str()).ok())
            .is_some_and(|project| Self::planner_of(&project).is_some());
        if !configured {
            return;
        }
        if let Err(refusal) = me.start_planning(flow) {
            tracing::info!(%flow, %refusal, "the planner did not start by itself");
            self.file(&NewInboxItem {
                flow,
                kind: InboxKind::Failure,
                target: InboxTarget::Flow,
                title: "The planner did not start".to_owned(),
                body: format!(
                    "{}. Its worktree is ready; Plan in the issue workspace starts it once that is \
                     fixed.",
                    super::capitalise(&refusal.message)
                ),
            });
        }
    }

    /// Plan an issue's run that is ready to plan.
    ///
    /// # Errors
    ///
    /// Why it did not start, in a sentence: no planner configured, the worktree not ready, the run
    /// past planning or held, a provider that would not bill a subscription. Nothing was recorded.
    pub fn start_planning(self: &Arc<Self>, flow: FlowId) -> Result<(), Refusal> {
        let db = self.db()?;
        let state = self.state(flow)?;
        if state.end.is_some() {
            return Err("this run has ended; Start the issue again".into());
        }
        if self.is_running(flow) {
            return Err("its planner is working already".into());
        }
        if state.hold.is_some() || state.phase != Phase::Preparing {
            return Err(format!(
                "this run is past \"Ready to plan\" ({:?}); its Run section says what it waits for",
                state.phase
            )
            .into());
        }
        let job = db
            .start_job_of(flow)
            .map_err(text)?
            .ok_or("only an issue's run plans")?;
        if job.state != StartJobState::Ready {
            return Err("its worktree is not ready yet".into());
        }
        let project = self.app.project(job.project.as_str()).map_err(text)?;
        let (role, spec) = Self::planner_of(&project).ok_or_else(|| {
            Refusal::from(
                "this project names no planner: add `planner = \"<role>\"` to its [workflow] in \
                 your own config, naming a reviewer role whose output is `plan`",
            )
        })?;
        let tracker = project
            .workflow
            .as_ref()
            .and_then(|w| w.tracker)
            .unwrap_or(TrackerKind::Jira);
        self.host.preflight(&[spec.provider.id()])?;
        db.begin_planning(
            &NewPlanning {
                flow,
                project: job.project.clone(),
                worktree: job.worktree.clone(),
                key: job.key.clone(),
                tracker,
                role,
            },
            self.now(),
        )
        .map_err(text)?;
        self.notice.changed(flow);
        Ok(self.spawn_planner(flow, PlanTurn::Begin)?)
    }

    fn spawn_planner(self: &Arc<Self>, flow: FlowId, turn: PlanTurn) -> Result<(), String> {
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
            .name(format!("plan-{flow}"))
            .spawn(move || {
                struct Done<'a>(&'a Engine, FlowId);
                impl Drop for Done<'_> {
                    fn drop(&mut self) {
                        self.0.runs.lock().remove(&self.1);
                        self.0.notice.changed(self.1);
                    }
                }
                {
                    let _done = Done(&engine, flow);
                    if let Err(reason) = engine.drive_planner(flow, &run, &turn)
                        && !engine.quitting()
                    {
                        tracing::warn!(%flow, %reason, "a planner turn failed");
                        engine.planner_failed(flow, &reason);
                    }
                }
                // An answer that arrived while this turn was still closing its session waits for
                // the thread to go; it goes now.
                engine.planner_next(flow);
            });
        if let Err(error) = spawned {
            self.runs.lock().remove(&flow);
            return Err(format!("wtm could not start the planner's thread: {error}"));
        }
        Ok(())
    }

    /// After a planner turn: when every question it waits for is answered by now, and nothing
    /// else holds it, its answers go to it.
    fn planner_next(self: &Arc<Self>, flow: FlowId) {
        if self.quitting() || self.is_running(flow) {
            return;
        }
        let Ok(state) = self.state(flow) else { return };
        if state.end.is_some() || !matches!(state.hold, Some(Hold::AwaitingAnswer)) {
            return;
        }
        if self.waiting_on(flow).is_empty()
            && self.release_answered(flow)
            && let Err(reason) = self.spawn_planner(flow, PlanTurn::Answered)
        {
            tracing::warn!(%flow, %reason, "could not resume the planner with its answers");
        }
    }

    /// The questions `flow` holds for: the ones that block a phase and have no answer yet.
    fn waiting_on(&self, flow: FlowId) -> Vec<QuestionId> {
        self.db()
            .and_then(|db| db.questions(flow).map_err(text))
            .map(|questions| {
                questions
                    .into_iter()
                    .filter(|q| q.blocks.is_some() && q.answer.is_none())
                    .map(|q| q.id)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Release an `AwaitingAnswer` hold. `false` when it was not held so, or the release failed.
    fn release_answered(&self, flow: FlowId) -> bool {
        let _held = self.holds.lock();
        match self.state(flow) {
            Ok(state) if matches!(state.hold, Some(Hold::AwaitingAnswer)) => self
                .record(
                    flow,
                    &FlowEvent::Released {
                        hold: HoldKind::AwaitingAnswer,
                    },
                )
                .is_ok(),
            _ => false,
        }
    }

    // ═══════════════════════════════ driving ═══════════════════════════════

    fn plan_ctx(&self, flow: FlowId) -> Result<PlanCtx, String> {
        let db = self.db()?;
        let planning = db
            .planning(flow)
            .map_err(text)?
            .ok_or_else(|| format!("run {flow} has not started planning"))?;
        let project = self
            .app
            .project(planning.project.as_str())
            .map_err(|e| format!("the project is no longer registered: {e}"))?;
        let worktree = self
            .app
            .worktree(&project, &planning.worktree)
            .map_err(|e| format!("the issue's worktree is no longer there: {e}"))?;
        let workflow = project
            .workflow
            .clone()
            .ok_or("the project's workflow config has gone")?;
        let spec = workflow.role.get(&planning.role).cloned().ok_or_else(|| {
            format!(
                "role `{}` is no longer in your workflow config, so the planner cannot run",
                planning.role
            )
        })?;
        Ok(PlanCtx {
            flow,
            branch: worktree.branch().map(|b| b.as_str().to_owned()),
            worktree_path: worktree.path.clone(),
            project_root: project.root.clone(),
            fields: workflow.fields.clone(),
            patterns: crate::knowledge::patterns(&project).0,
            spec,
            planning,
        })
    }

    fn drive_planner(&self, flow: FlowId, run: &Run, turn: &PlanTurn) -> Result<(), String> {
        let db = self.db()?;
        let ctx = self.plan_ctx(flow)?;
        let state = self.state(flow)?;
        // Every turn that can lead to a plan reads the issue again first, so a plan is recorded
        // against the issue as the planner was last given it: an issue edited while a question
        // waited, or between a plan and a request for changes, is what the next plan answers. A
        // discussion turn plans nothing and is given nothing new.
        if !matches!(turn, PlanTurn::Discuss { .. }) {
            match self.gather(&ctx) {
                Ok(context) => {
                    db.set_plan_context(flow, &context, self.now())
                        .map_err(text)?;
                    self.notice.changed(flow);
                }
                // The first read is what the planner works from; without it there is nothing to
                // plan. A later one that fails leaves the last reading, said in the log.
                Err(error) if ctx.planning.context.is_none() => return Err(error),
                Err(error) => {
                    tracing::warn!(%flow, %error, "could not read the issue again; the planner keeps its last reading");
                }
            }
        }
        if state.phase == Phase::Preparing {
            self.record(
                flow,
                &FlowEvent::Advanced {
                    to: Phase::Investigating,
                },
            )?;
        }
        let step = self.planner_step(&ctx)?;
        let discussing = matches!(turn, PlanTurn::Discuss { .. });
        let resume = db
            .invocations(flow)
            .map_err(text)?
            .into_iter()
            .rev()
            .filter(|i| i.step == step.id)
            .find_map(|i| i.provider_session);
        if resume.is_none() && *turn != PlanTurn::Begin {
            tracing::info!(%flow, "the planner has no conversation to resume; it starts afresh");
        }
        let words = self.turn_words(&ctx, turn, resume.is_some())?;
        let end = self.planner_turn(&ctx, run, &step, resume.as_deref(), &words, discussing);
        self.after_turn(&ctx, &step, turn, end);
        Ok(())
    }

    /// The planner's one step: its role's, made the first time.
    fn planner_step(&self, ctx: &PlanCtx) -> Result<Step, String> {
        let db = self.db()?;
        if let Some(step) = db
            .steps(ctx.flow)
            .map_err(text)?
            .into_iter()
            .find(|s| s.role == ctx.planning.role)
        {
            return Ok(step);
        }
        db.add_steps(
            ctx.flow,
            &[NewStep {
                stage: 0,
                role: ctx.planning.role.clone(),
                provider: ctx.spec.provider,
                model: ctx.spec.model.clone(),
                profile: ctx.spec.profile,
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
        .ok_or_else(|| "the planner's step was not kept".to_owned())
    }

    /// Read everything the planner is given, fresh, and keep the issue in the mirror.
    fn gather(&self, ctx: &PlanCtx) -> Result<PlanContext, String> {
        let db = self.db()?;
        let detail = self
            .app
            .tracker
            .issue(&ctx.project_root, &ctx.planning.key, &ctx.fields)
            .map_err(|e| {
                format!(
                    "{} could not be read from the tracker: {e}",
                    ctx.planning.key
                )
            })?;
        if let Err(error) = db.mirror_issue(ctx.planning.tracker, &detail, self.now()) {
            tracing::warn!(key = ctx.planning.key, %error, "could not keep the issue the planner read");
        }
        // The sprint the issue is in, from the issue's own sprint field when one is mapped, else
        // the queue's active sprint when the queue holds the issue.
        let mut sprint = detail
            .sprints
            .iter()
            .find(|s| s.state == wtm_core::flow::SprintState::Active)
            .cloned();
        if sprint.is_none() {
            sprint = db
                .mirrored_queue(&ctx.planning.project)
                .ok()
                .flatten()
                .and_then(|m| m.reading)
                .and_then(|reading| {
                    let queued = reading
                        .issues
                        .iter()
                        .find(|q| q.issue.key == ctx.planning.key)?;
                    reading
                        .sprints
                        .iter()
                        .find(|s| {
                            s.state == wtm_core::flow::SprintState::Active
                                && queued.sprint == Some(s.id)
                        })
                        .cloned()
                });
        }
        let mut dependencies: Vec<PlanDependency> = Vec::new();
        if let Some(parent) = &detail.parent {
            dependencies.push(PlanDependency {
                relation: "is a child of".to_owned(),
                key: parent.key.clone(),
                summary: parent.summary.clone(),
                status: None,
            });
        }
        dependencies.extend(detail.links.iter().map(|link| PlanDependency {
            relation: link.relation.clone(),
            key: link.key.clone(),
            summary: link.summary.clone(),
            status: link.status.as_ref().map(|s| s.name.clone()),
        }));
        let (sources, rules) = self.planner_knowledge(ctx, None);
        Ok(PlanContext {
            issue: IssueText::from_detail(&detail),
            status: detail.issue.status.name.clone(),
            sprint: sprint.as_ref().map(|s| s.name.clone()),
            sprint_goal: sprint
                .and_then(|s| s.goal)
                .filter(|goal| !goal.trim().is_empty()),
            dependencies,
            instruction_sources: sources,
            rules,
            skills: ctx.spec.skill_names().map(str::to_owned).collect(),
            role: ctx.planning.role.clone(),
            provider: ctx.spec.provider,
            model: ctx.spec.model.clone(),
            read_at: self.now(),
        })
    }

    /// The instruction sources and rules the planner's role takes, and, given an attempt, the brief
    /// section that carries them, with the rules recorded against that attempt.
    fn planner_knowledge(
        &self,
        ctx: &PlanCtx,
        attempt: Option<u32>,
    ) -> (Vec<String>, Vec<PlanRule>) {
        self.planner_knowledge_section(ctx, attempt).0
    }

    #[allow(clippy::type_complexity)]
    fn planner_knowledge_section(
        &self,
        ctx: &PlanCtx,
        attempt: Option<u32>,
    ) -> ((Vec<String>, Vec<PlanRule>), Option<String>) {
        let wants_instructions = ctx
            .spec
            .inputs
            .contains(&wtm_core::flow::RoleInput::Instructions);
        let query = RuleQuery::for_role(&ctx.spec, &[]);
        if !wants_instructions && query.is_none() {
            return ((Vec::new(), Vec::new()), None);
        }
        let instructions = if wants_instructions {
            crate::knowledge::instruction_set(
                self.app.git.as_ref(),
                &ctx.worktree_path,
                &ctx.patterns,
            )
            .map(|(set, _)| set)
            .map_err(|error| {
                tracing::warn!(flow = %ctx.flow, error = %error.message,
                    "could not read the instruction sources for the planner");
            })
            .ok()
        } else {
            None
        };
        let rules = query.and_then(|query| {
            let db = self.db().ok()?;
            let all = db.rules(&ctx.planning.project).ok()?;
            let selection = wtm_core::flow::knowledge::select(&all, &ctx.planning.project, &query);
            if let Some(attempt) = attempt
                && let Err(error) = db.record_applications(
                    &wtm_core::flow::ReviewInvocation {
                        flow: ctx.flow,
                        role: ctx.planning.role.clone(),
                        attempt,
                    },
                    &selection.applied,
                    self.now(),
                )
            {
                tracing::warn!(flow = %ctx.flow, %error, "could not record the rules the planner was given");
            }
            Some((selection, all))
        });
        let section = wtm_core::flow::knowledge::brief(
            instructions.as_ref(),
            rules
                .as_ref()
                .map(|(selection, all)| (selection, all.as_slice())),
        );
        let sources = instructions
            .map(|set| set.documents.into_iter().map(|d| d.path).collect())
            .unwrap_or_default();
        let listed = rules
            .map(|(selection, all)| {
                selection
                    .applied
                    .iter()
                    .filter_map(|applied| {
                        let rule = all.iter().find(|r| r.id == applied.rule)?;
                        let revision = rule.revision(applied.revision)?;
                        Some(PlanRule {
                            rule: applied.rule,
                            revision: applied.revision,
                            kind: applied.kind,
                            text: revision.draft.text.clone(),
                            reason: applied.reason.clone(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        ((sources, listed), Some(section))
    }

    /// What the turn says.
    fn turn_words(&self, ctx: &PlanCtx, turn: &PlanTurn, resumed: bool) -> Result<String, String> {
        let key = &ctx.planning.key;
        Ok(match turn {
            PlanTurn::Begin => brief::planner_opening_turn(key),
            PlanTurn::Answered => {
                let db = self.db()?;
                let since = db
                    .invocations(ctx.flow)
                    .map_err(text)?
                    .into_iter()
                    .rev()
                    .find(|i| i.role == ctx.planning.role)
                    .map_or(0, |i| i.started_at);
                let answered: Vec<Question> = db
                    .questions(ctx.flow)
                    .map_err(text)?
                    .into_iter()
                    .filter(|q| q.answer.as_ref().is_some_and(|a| a.at >= since))
                    .collect();
                brief::planner_answers_turn(&answered, resumed)
            }
            PlanTurn::Discuss { question, message } => {
                let asked = self
                    .db()?
                    .question(*question)
                    .map_err(text)?
                    .ok_or_else(|| format!("no question {question}"))?;
                brief::planner_discuss_turn(&asked, message)
            }
            PlanTurn::Changes { revision, note } => {
                brief::planner_changes_turn(*revision, note, resumed)
            }
            PlanTurn::Resume => brief::planner_resume_turn(key, resumed),
        })
    }

    /// One planner turn: one session, opened (resumed when there is a conversation), sent `words`,
    /// and read until it hands in a plan, asks, replies or stops.
    #[allow(clippy::too_many_lines)]
    fn planner_turn(
        &self,
        ctx: &PlanCtx,
        run: &Run,
        step: &Step,
        resume: Option<&str>,
        words: &str,
        discussing: bool,
    ) -> TurnEnd {
        let Ok(db) = self.db() else {
            return TurnEnd::Quitting;
        };
        let attempt = db
            .invocations(ctx.flow)
            .map_or(0, |all| all.iter().filter(|i| i.step == step.id).count())
            + 1;
        let attempt = u32::try_from(attempt).unwrap_or(u32::MAX);
        // A planner session counts against the run-session caps like any role's, and waits for a
        // place before its invocation is recorded (`capacity.rs`).
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
                profile: step.profile,
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
        let (section_lists, section) = self.planner_knowledge_section(ctx, Some(attempt));
        if !section_lists.0.is_empty() {
            let _ = db.update_invocation(
                invocation.id,
                &InvocationChange {
                    instruction_sources: Some(section_lists.0.clone()),
                    ..InvocationChange::default()
                },
                self.now(),
            );
        }
        let context = db.planning(ctx.flow).ok().flatten().and_then(|p| p.context);
        let answers = db
            .questions(ctx.flow)
            .unwrap_or_default()
            .into_iter()
            .filter(|q| q.answer.is_some())
            .collect::<Vec<_>>();
        let plans = db.plans(ctx.flow).unwrap_or_default();
        let brief_text = brief::planner_brief(&brief::PlannerFacts {
            role: &ctx.planning.role,
            spec: &ctx.spec,
            key: &ctx.planning.key,
            tracker: ctx.planning.tracker,
            worktree_path: &ctx.worktree_path.to_string_lossy(),
            branch: ctx.branch.as_deref(),
            context: context.as_ref(),
            knowledge: section.as_deref(),
            answers: &answers,
            latest: plans.last(),
        });

        let (tx, rx) = mpsc::channel();
        let key = (ctx.flow, step.role.clone());
        self.live.lock().insert(
            key.clone(),
            Arc::new(Live {
                output: OutputKind::Plan,
                tx: tx.clone(),
                step: step.id,
                invocation: invocation.id,
                cwd: ctx.worktree_path.clone(),
                tree_sha: String::new(),
                checks: std::collections::BTreeMap::new(),
                dir: wtm_core::flow::FlowDir::new(&self.app.config.paths().flow_dir, ctx.flow),
                fix: None,
                planner: Some(Arc::new(PlannerLive { discussing })),
                project: ctx.planning.project.as_str().to_owned(),
                worktree: ctx.planning.worktree.clone(),
                // A planner runs no checks and drives no pane: it reads, and hands in a plan.
                start_services: None,
                browser: None,
            }),
        );
        let session_spec = RoleSession {
            project: ctx.planning.project.as_str().to_owned(),
            worktree: ctx.planning.worktree.clone(),
            role: FlowRole {
                flow: ctx.flow,
                role: step.role.clone(),
                authority: step.profile,
                browser: None,
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
                        // Not the id it was asked to resume: the record holds what the CLI says
                        // it continued, when it says so, so a resume that quietly started a new
                        // conversation shows as one.
                        ..InvocationChange::default()
                    },
                    self.now(),
                );
                self.notice.changed(ctx.flow);
                let end = self.read_planner(ctx, run, step, &session, invocation.id, words, &rx);
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
                TurnEnd::Submitted(_) => (InvocationStatus::Submitted, None),
                TurnEnd::Asked(asked) => (
                    InvocationStatus::Interrupted,
                    Some(format!(
                        "wtm ended the turn after {}, and closed the session until you answer",
                        questions_named(asked)
                    )),
                ),
                TurnEnd::Replied(_) => (
                    InvocationStatus::Interrupted,
                    Some("a discussion turn: wtm closed the session after its reply".to_owned()),
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
    fn read_planner(
        &self,
        ctx: &PlanCtx,
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
        let mut submitted: Option<Box<PlanReport>> = None;
        let mut asked: Vec<QuestionId> = Vec::new();
        let mut interrupted_at: Option<u64> = None;
        let mut settle_at: Option<u64> = None;
        let discussing = self
            .live
            .lock()
            .get(&(ctx.flow, step.role.clone()))
            .and_then(|live| live.planner.clone())
            .is_some_and(|p| p.discussing);
        // A discussion turn's reply: the turn's last message, as the provider streamed or sent it.
        let mut streamed = String::new();
        let mut whole: Vec<String> = Vec::new();

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
        let reply = |streamed: &str, whole: &[String]| {
            let text = if whole.is_empty() {
                streamed.to_owned()
            } else {
                whole.join("\n\n")
            };
            text.trim().to_owned()
        };

        loop {
            match rx.recv_timeout(Duration::from_millis(self.tuning.tick_ms)) {
                Ok(Signal::Submitted(Submission::Plan(plan))) => {
                    submitted = Some(Box::new(plan));
                    settle_at = Some(self.clock.monotonic_ms() + self.tuning.settle_ms);
                }
                // Only a plan is accepted from a planner (`on_tool`), so nothing else arrives; and
                // a tick with nothing new is a tick.
                Ok(Signal::Submitted(_)) | Err(RecvTimeoutError::Timeout) => {}
                Ok(Signal::Asked(question)) => {
                    asked.push(question);
                    if interrupted_at.is_none() {
                        // The engine ends the turn itself: the CLI is told to stop, and if it has
                        // not finished in a few seconds its session is closed anyway.
                        if let Err(reason) = self.host.interrupt(session) {
                            tracing::info!(flow = %ctx.flow, %reason, "could not interrupt the planner's turn; closing its session");
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
                    AgentEvent::TurnStarted { .. } => {
                        streamed.clear();
                        whole.clear();
                    }
                    AgentEvent::ToolStarted { .. } => {
                        // What came before a tool call was the model thinking aloud; the reply is
                        // what it says after the last one.
                        streamed.clear();
                        whole.clear();
                    }
                    AgentEvent::MessageDelta { text } => streamed.push_str(&text),
                    AgentEvent::Message { text } => whole.push(text),
                    AgentEvent::TurnFinished { .. } if sent => {
                        if !asked.is_empty() {
                            return TurnEnd::Asked(asked);
                        }
                        if let Some(plan) = submitted {
                            return TurnEnd::Submitted(plan);
                        }
                        if discussing {
                            let said = reply(&streamed, &whole);
                            if !said.is_empty() {
                                return TurnEnd::Replied(said);
                            }
                        }
                        if nudges == 0 {
                            return TurnEnd::Failed(if discussing {
                                "it ended the discussion turn without saying anything".to_owned()
                            } else {
                                "it ended its turn without handing in a plan or asking".to_owned()
                            });
                        }
                        nudges -= 1;
                        let nudge = if discussing {
                            "Your turn ended without a reply. Answer the person's message in plain \
                             words now, then end your turn."
                                .to_owned()
                        } else {
                            brief::nudge(OutputKind::Plan)
                        };
                        if let Err(end) = say(&nudge) {
                            return end;
                        }
                    }
                    AgentEvent::LimitReached { message, resets_at } => {
                        if !asked.is_empty() {
                            return TurnEnd::Asked(asked);
                        }
                        if let Some(plan) = submitted {
                            return TurnEnd::Submitted(plan);
                        }
                        return TurnEnd::Limit { resets_at, message };
                    }
                    AgentEvent::Failed { message } => {
                        if !asked.is_empty() {
                            return TurnEnd::Asked(asked);
                        }
                        if let Some(plan) = submitted {
                            return TurnEnd::Submitted(plan);
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
                                 prompt means the profile and the provider disagree. The planner \
                                 waits until its timeout.",
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
                    if let Some(plan) = submitted {
                        return TurnEnd::Submitted(plan);
                    }
                    if discussing {
                        let said = reply(&streamed, &whole);
                        if !said.is_empty() {
                            return TurnEnd::Replied(said);
                        }
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
            if submitted.is_some() && settle_at.is_some_and(|at| now >= at) {
                return TurnEnd::Submitted(submitted.unwrap_or_else(|| unreachable!()));
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
                if let Some(plan) = submitted {
                    return TurnEnd::Submitted(plan);
                }
                return TurnEnd::Failed(format!(
                    "it had no plan after {}",
                    super::minutes(step.timeout_ms)
                ));
            }
        }
    }

    /// Record what a turn came to.
    fn after_turn(&self, ctx: &PlanCtx, step: &Step, turn: &PlanTurn, end: TurnEnd) {
        let Ok(db) = self.db() else { return };
        match end {
            TurnEnd::Submitted(plan) => {
                let issue = db
                    .planning(ctx.flow)
                    .ok()
                    .flatten()
                    .and_then(|p| p.context)
                    .map(|c| c.issue);
                let Some(issue) = issue else {
                    self.planner_failed(ctx.flow, "the issue the planner was given was not kept");
                    return;
                };
                let invocation = db
                    .invocations(ctx.flow)
                    .ok()
                    .and_then(|all| all.into_iter().rev().find(|i| i.step == step.id))
                    .map(|i| i.id);
                let record = match db.record_plan(
                    &NewPlan {
                        flow: ctx.flow,
                        step: Some(step.id),
                        invocation,
                        plan: (*plan).clone(),
                        issue,
                    },
                    self.now(),
                ) {
                    Ok(record) => record,
                    Err(error) => {
                        self.planner_failed(
                            ctx.flow,
                            &format!("the plan could not be kept: {error}"),
                        );
                        return;
                    }
                };
                self.update_step(
                    step,
                    &StepChange {
                        result: Some(Submission::Plan(*plan)),
                        detail: Field::Clear,
                        ..StepChange::status(StepStatus::Done)
                    },
                );
                {
                    let _held = self.holds.lock();
                    if self
                        .state(ctx.flow)
                        .is_ok_and(|s| s.phase == Phase::Investigating)
                    {
                        let _ = self.record(ctx.flow, &FlowEvent::Advanced { to: Phase::Planned });
                    }
                    let _ = self.record(
                        ctx.flow,
                        &FlowEvent::Held {
                            hold: Hold::AwaitingApproval,
                        },
                    );
                }
                self.settle(
                    ctx.flow,
                    &[
                        InboxKind::Failure,
                        InboxKind::Interrupted,
                        InboxKind::Capacity,
                    ],
                );
                self.file(&NewInboxItem {
                    flow: ctx.flow,
                    kind: InboxKind::Approval,
                    target: InboxTarget::Flow,
                    title: format!(
                        "{}: plan revision {} is ready for your approval",
                        ctx.planning.key, record.revision
                    ),
                    body: format!(
                        "{}\n\nApprove it, approve it and skip design, or ask for changes. Nothing \
                         is implemented until a plan is approved.",
                        record.plan.summary.trim()
                    ),
                });
            }
            TurnEnd::Asked(asked) => {
                self.update_step(
                    step,
                    &StepChange::status(StepStatus::Pending).detail(format!(
                        "waiting for your answer to {}",
                        questions_named(&asked)
                    )),
                );
            }
            TurnEnd::Replied(said) => {
                if let PlanTurn::Discuss { question, .. } = turn
                    && let Err(error) = db.discuss(*question, Speaker::Agent, &said, self.now())
                {
                    tracing::warn!(%question, %error, "could not keep the planner's reply");
                }
                self.update_step(
                    step,
                    &StepChange::status(StepStatus::Pending)
                        .detail("waiting for your answer".to_owned()),
                );
            }
            TurnEnd::Failed(reason) => {
                if matches!(turn, PlanTurn::Discuss { .. }) {
                    // A discussion that failed leaves the question as it was: the run still waits
                    // for its answer, and Discuss can be asked again.
                    self.update_step(
                        step,
                        &StepChange::status(StepStatus::Pending)
                            .detail("waiting for your answer".to_owned()),
                    );
                    self.file(&NewInboxItem {
                        flow: ctx.flow,
                        kind: InboxKind::Failure,
                        target: InboxTarget::Flow,
                        title: format!("{}: the planner did not reply", ctx.planning.key),
                        body: format!(
                            "{}. The question is still open; answer it, or Discuss again.",
                            super::capitalise(&reason)
                        ),
                    });
                } else {
                    self.update_step(
                        step,
                        &StepChange::status(StepStatus::Failed).detail(reason.clone()),
                    );
                    self.planner_failed(ctx.flow, &reason);
                }
            }
            TurnEnd::Limit { resets_at, message } => {
                self.update_step(
                    step,
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
                        let _ = self.record(
                            ctx.flow,
                            &FlowEvent::Held {
                                hold: Hold::WaitingCapacity {
                                    provider: step.provider.id().to_owned(),
                                    resets_at,
                                },
                            },
                        );
                    }
                }
                self.file(&NewInboxItem {
                    flow: ctx.flow,
                    kind: InboxKind::Capacity,
                    target: super::on(step),
                    title: format!(
                        "{}: the planner is paused, {} limit reached",
                        ctx.planning.key, step.provider
                    ),
                    body: format!(
                        "{message}\n\nResume carries on in the same conversation. wtm never uses \
                         an API key."
                    ),
                });
            }
            TurnEnd::Cancelled => {
                self.update_step(step, &StepChange::status(StepStatus::Cancelled));
            }
            TurnEnd::Quitting => {}
        }
        self.notice.changed(ctx.flow);
    }

    /// The planner stopped for good this turn: say so where you will see it. The run stays where it
    /// was, and Resume runs the planner again in the same conversation.
    fn planner_failed(&self, flow: FlowId, reason: &str) {
        let key = self
            .db()
            .ok()
            .and_then(|db| db.planning(flow).ok().flatten())
            .map_or_else(|| "The issue".to_owned(), |p| p.key);
        self.file(&NewInboxItem {
            flow,
            kind: InboxKind::Failure,
            target: InboxTarget::Flow,
            title: format!("{key}: the planner stopped"),
            body: format!(
                "{}. Nothing it found was lost: Resume runs it again in the same conversation.",
                super::capitalise(reason)
            ),
        });
        self.notice.changed(flow);
    }

    // ═══════════════════════════════ the run tools ═══════════════════════════════

    /// A planner asked something: keep the question, hold the run for it, and tell the role its
    /// turn is over. The role thread interrupts the turn when it hears.
    pub(super) fn planner_ask(
        &self,
        role: &FlowRole,
        live: &Live,
        planner: &PlannerLive,
        ask: &crate::flow_tools::Ask,
    ) -> Result<String, String> {
        if planner.discussing {
            return Err(
                "This turn is a discussion of a question you already asked: reply in words, and \
                 do not ask another question until the person answers."
                    .to_owned(),
            );
        }
        let db = self.db()?;
        let question = NewQuestion {
            flow: role.flow,
            step: Some(live.step),
            role: Some(role.role.clone()),
            text: ask.question.trim().to_owned(),
            why: ask.why.trim().to_owned(),
            unblocks: ask.unblocks.trim().to_owned(),
            blocks: Some(Phase::Planned),
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
        let key = db
            .planning(role.flow)
            .ok()
            .flatten()
            .map_or_else(String::new, |p| format!("{}: ", p.key));
        let title = format!("{key}{}", question.text);
        let (asked, filed) = db.ask(&question, &title, self.now()).map_err(text)?;
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
             from now. Do not guess the answer and do not keep working; end your turn.",
            asked.id
        ))
    }

    // ═══════════════════════════════ answering ═══════════════════════════════

    /// After an answer to `question` was recorded: a planner's question that was the last one its
    /// run waited for resumes the planner with the answers.
    pub(super) fn on_planner_answer(&self, question: &Question) {
        if question.blocks.is_none() || !self.is_planning(question.flow) {
            return;
        }
        let Some(me) = self.me.upgrade() else { return };
        // The thread that asked may still be closing its session; it picks the answers up as it
        // goes (`planner_next`). Otherwise they go now.
        me.planner_next(question.flow);
    }

    /// Talk a planner's open question over with it before answering: your message, and its reply in
    /// the same conversation. The run keeps waiting for the answer.
    ///
    /// # Errors
    ///
    /// When the question is answered, not a planner's, or its planner is working now.
    pub fn discuss(self: &Arc<Self>, question: QuestionId, message: &str) -> Result<(), String> {
        let message = message.trim();
        if message.is_empty() {
            return Err("say something to discuss".to_owned());
        }
        let thread = self.question_thread(question)?;
        if !thread.can_discuss {
            return Err(if thread.question.answer.is_some() {
                "this question is answered; the planner has carried on".to_owned()
            } else if thread.replying || self.is_running(thread.question.flow) {
                "the planner is replying now".to_owned()
            } else {
                "only a planner's open question can be discussed, while its run waits for it"
                    .to_owned()
            });
        }
        self.db()?
            .discuss(question, Speaker::You, message, self.now())
            .map_err(text)?;
        self.notice.changed(thread.question.flow);
        self.spawn_planner(
            thread.question.flow,
            PlanTurn::Discuss {
                question,
                message: message.to_owned(),
            },
        )
    }

    // ═══════════════════════════════ deciding ═══════════════════════════════

    fn latest_plan(&self, flow: FlowId, revision: u32) -> Result<PlanRecord, String> {
        let plans = self.db()?.plans(flow).map_err(text)?;
        let latest = plans.last().ok_or("this run has no plan yet")?;
        if latest.revision != revision {
            return Err(format!(
                "revision {revision} is not the newest plan; revision {} is",
                latest.revision
            ));
        }
        Ok(latest.clone())
    }

    /// Read the issue whole from the tracker again and keep it, as the workspace's Check the issue
    /// does: what an approval is measured against. Returns its text.
    ///
    /// # Errors
    ///
    /// When the run has not planned or the tracker cannot be read.
    pub fn read_planned_issue(&self, flow: FlowId) -> Result<IssueText, String> {
        let db = self.db()?;
        let planning = db
            .planning(flow)
            .map_err(text)?
            .ok_or("this run has not planned anything")?;
        let (project, tracker) =
            crate::tracker::tracker_project(&self.app, planning.project.as_str())
                .map_err(|e| e.message)?;
        let fields = project.workflow.unwrap_or_default().fields;
        let detail = self
            .app
            .tracker
            .issue(&project.root, &planning.key, &fields)
            .map_err(|e| format!("{} could not be read: {e}", planning.key))?;
        db.mirror_issue(tracker, &detail, self.now())
            .map_err(text)?;
        let issue = IssueText::from_detail(&detail);
        self.note_stale(flow);
        Ok(issue)
    }

    /// File one item when the run's approved plan no longer covers the issue as read now.
    pub(super) fn note_stale(&self, flow: FlowId) {
        let Ok(view) = self.plan_view(flow) else {
            return;
        };
        if let PlanStanding::Stale {
            approval, revision, ..
        } = &view.standing
        {
            let key = view
                .planning
                .as_ref()
                .map_or_else(String::new, |p| p.key.clone());
            self.file(&NewInboxItem {
                flow,
                kind: InboxKind::Approval,
                target: InboxTarget::Approval {
                    approval: *approval,
                },
                title: format!("{key} changed after you approved its plan"),
                body: format!(
                    "Your approval of plan revision {revision} covered the issue as it read then. \
                     The issue's Plan tab shows what changed. Approve again for the issue as it \
                     is, or ask for changes."
                ),
            });
            self.notice.changed(flow);
        }
    }

    /// Approve plan `revision` for the issue whose revision you saw, `issue_revision`.
    ///
    /// The issue is read again first. If it changed while you were reading, nothing is approved:
    /// what you approve is the plan for the issue as you saw it, and the window shows the change.
    ///
    /// # Errors
    ///
    /// When the revision is not the newest, the run is not waiting for a decision, or the issue
    /// changed.
    pub fn approve_plan(
        &self,
        flow: FlowId,
        revision: u32,
        issue_revision: &str,
        skip_design: bool,
    ) -> Result<PlanDecision, String> {
        let state = self.state(flow)?;
        if state.end.is_some() {
            return Err("this run has ended".to_owned());
        }
        if state.phase != Phase::Planned
            || !matches!(state.hold, None | Some(Hold::AwaitingApproval))
        {
            return Err(format!(
                "this run is not waiting for a plan decision ({:?})",
                state.phase
            ));
        }
        let plan = self.latest_plan(flow, revision)?;
        let now = self.read_planned_issue(flow)?;
        if now.revision() != issue_revision {
            return Err(format!(
                "{} changed while you were reading the plan; it has been read again. Look at \
                 what changed, then decide.",
                now.key
            ));
        }
        let subject = PlanApproval {
            revision,
            issue: now.clone(),
            skip_design,
            design_needed: plan.plan.design_needed,
        };
        let db = self.db()?;
        let (_, decision) = db
            .approve_plan(
                &NewApproval::plan(flow, &subject, &now.revision()),
                plan.id,
                skip_design,
                self.now(),
            )
            .map_err(text)?;
        {
            let _held = self.holds.lock();
            if matches!(self.state(flow)?.hold, Some(Hold::AwaitingApproval)) {
                self.record(
                    flow,
                    &FlowEvent::Released {
                        hold: HoldKind::AwaitingApproval,
                    },
                )?;
            }
        }
        self.settle(flow, &[InboxKind::Approval]);
        for item in db.inbox(Some(flow), None).unwrap_or_default() {
            if item.kind == InboxKind::Approval
                && matches!(item.target, InboxTarget::Approval { approval } if self.is_plan_approval(approval))
            {
                let _ = db.resolve_inbox_item(item.id, self.now());
            }
        }
        self.notice.changed(flow);
        Ok(decision)
    }

    fn is_plan_approval(&self, approval: wtm_core::flow::ApprovalId) -> bool {
        self.db()
            .ok()
            .and_then(|db| db.approved(approval).ok().flatten())
            .is_some_and(|a| matches!(a.subject, ApprovalSubject::Plan(_)))
    }

    /// Ask for changes to plan `revision`: the planner is told `note` in the same conversation and
    /// hands in the next revision.
    ///
    /// # Errors
    ///
    /// When the note is empty, the revision is not the newest, or the run is not waiting for a
    /// decision.
    pub fn request_plan_changes(
        self: &Arc<Self>,
        flow: FlowId,
        revision: u32,
        note: &str,
    ) -> Result<PlanDecision, String> {
        let note = note.trim();
        if note.is_empty() {
            return Err("say what to change".to_owned());
        }
        let state = self.state(flow)?;
        if state.end.is_some() || state.phase != Phase::Planned {
            return Err("this run is not waiting for a plan decision".to_owned());
        }
        if self.is_running(flow) {
            return Err("its planner is working now".to_owned());
        }
        let plan = self.latest_plan(flow, revision)?;
        let decision = self
            .db()?
            .request_plan_changes(plan.id, note, self.now())
            .map_err(text)?;
        {
            let _held = self.holds.lock();
            if matches!(self.state(flow)?.hold, Some(Hold::AwaitingApproval)) {
                self.record(
                    flow,
                    &FlowEvent::Released {
                        hold: HoldKind::AwaitingApproval,
                    },
                )?;
            }
            self.record(
                flow,
                &FlowEvent::Advanced {
                    to: Phase::Investigating,
                },
            )?;
        }
        self.settle(flow, &[InboxKind::Approval]);
        self.notice.changed(flow);
        self.spawn_planner(
            flow,
            PlanTurn::Changes {
                revision,
                note: note.to_owned(),
            },
        )?;
        Ok(decision)
    }

    // ═══════════════════════════════ holds and recovery ═══════════════════════════════

    /// Carry on planning after an interruption, a failure or a limit, in the same conversation.
    pub(super) fn resume_planner(self: &Arc<Self>, flow: FlowId) -> Result<(), Refusal> {
        if self.is_running(flow) {
            return Err("its planner is working already".into());
        }
        let state = self.state(flow)?;
        if let Some(end) = &state.end {
            return Err(format!("this run has already ended ({end:?})").into());
        }
        if state.phase != Phase::Investigating {
            return Err(
                "there is nothing to resume: the planner is not part-way through a plan".into(),
            );
        }
        let ctx = self.plan_ctx(flow)?;
        self.host.preflight(&[ctx.spec.provider.id()])?;
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
        self.settle(
            flow,
            &[
                InboxKind::Interrupted,
                InboxKind::Failure,
                InboxKind::Capacity,
            ],
        );
        Ok(self.spawn_planner(flow, PlanTurn::Resume)?)
    }

    /// The app is quitting with a planner turn running, or found at launch with one that has no
    /// process: hold it `Interrupted`, unless it waits for an answer, which needs no process.
    pub(super) fn interrupt_planner(&self, flow: FlowId, cause: InterruptCause) {
        let Ok(db) = self.db() else { return };
        let now = self.now();
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
        let planning = db.planning(flow).ok().flatten();
        if let Some(planning) = &planning
            && let Some(step) = db
                .steps(flow)
                .unwrap_or_default()
                .into_iter()
                .find(|s| s.role == planning.role && s.status == StepStatus::Running)
        {
            let _ = db.update_step(step.id, &StepChange::status(StepStatus::Interrupted), now);
        }
        {
            let _held = self.holds.lock();
            let Ok(state) = self.state(flow) else { return };
            // Waiting for an answer needs no process, and neither did a discussion turn cut off:
            // the question is still open, and Discuss can be asked again.
            if state.end.is_some() || state.hold.is_some() {
                self.notice.changed(flow);
                return;
            }
            if let Err(e) = self.record(
                flow,
                &FlowEvent::Held {
                    hold: Hold::Interrupted { cause },
                },
            ) {
                tracing::warn!(%flow, error = %e, "could not record a planner's interruption");
                return;
            }
        }
        let key = planning.map_or_else(|| "The issue".to_owned(), |p| p.key);
        self.file(&NewInboxItem {
            flow,
            kind: InboxKind::Interrupted,
            target: InboxTarget::Flow,
            title: match cause {
                InterruptCause::Quit => format!("{key}: wtm quit while it was being planned"),
                _ => format!("{key}: wtm stopped while it was being planned"),
            },
            body: "Nothing was sent again. Resume continues the planner's conversation where it \
                   was cut off."
                .to_owned(),
        });
        self.notice.changed(flow);
    }

    /// At launch: a planner caught mid-turn with no process. Returns the runs it held.
    pub(super) fn recover_planners(&self) -> Result<Vec<FlowId>, String> {
        let db = self.db()?;
        let mut held = Vec::new();
        for planning in db.plannings().map_err(text)? {
            if self.is_running(planning.flow) {
                continue;
            }
            let Some(flow) = db.flow(planning.flow).map_err(text)? else {
                continue;
            };
            let failed = db
                .steps(planning.flow)
                .unwrap_or_default()
                .iter()
                .any(|s| s.role == planning.role && s.status == StepStatus::Failed);
            if flow.state.end.is_none()
                && flow.state.hold.is_none()
                && flow.state.phase == Phase::Investigating
                && !failed
            {
                self.interrupt_planner(planning.flow, InterruptCause::Crash);
                held.push(planning.flow);
            }
        }
        Ok(held)
    }

    /// The issue's planning, for the Plan tab, by project and key.
    ///
    /// # Errors
    ///
    /// When the gates are closed or the database cannot be read.
    pub fn issue_plan(&self, project_id: &str, key: &str) -> Result<Option<PlanView>, String> {
        let (_, tracker) =
            crate::tracker::tracker_project(&self.app, project_id).map_err(|e| e.message)?;
        if !self.app.files.exists(&self.app.config.paths().flow_db) {
            return Ok(None);
        }
        let db = self.db()?;
        let Some(item) = db
            .find_work_item(&wtm_core::flow::WorkKey::Issue(wtm_core::flow::IssueRef {
                tracker: tracker.as_str().to_owned(),
                key: key.to_owned(),
            }))
            .map_err(text)?
        else {
            return Ok(None);
        };
        let flow = match db
            .open_flows(item.id)
            .map_err(text)?
            .into_iter()
            .find(|f| f.state.kind == FlowKind::Implement)
        {
            Some(flow) => flow.id,
            None => match db.start_job(item.id).map_err(text)? {
                Some(job) => job.flow,
                None => return Ok(None),
            },
        };
        self.plan_view(flow).map(Some)
    }
}

/// "question 3", "questions 3 and 4".
fn questions_named(asked: &[QuestionId]) -> String {
    let ids: BTreeSet<i64> = asked.iter().map(|q| q.0).collect();
    let ids: Vec<String> = ids.into_iter().map(|id| id.to_string()).collect();
    match ids.as_slice() {
        [] => "its question".to_owned(),
        [one] => format!("question {one}"),
        [rest @ .., last] => format!("questions {} and {last}", rest.join(", ")),
    }
}

/// The answer as the planner is told it.
pub(super) fn answer_words(question: &Question, answer: &Answer) -> String {
    let picked = answer
        .choice
        .and_then(|choice| question.options.get(choice))
        .map(|option| format!("\"{}\"", option.trim()));
    let said = Some(answer.text.trim())
        .filter(|text| !text.is_empty())
        .map(|text| format!("\"{text}\""));
    match (picked, said) {
        (Some(picked), Some(said)) => format!("{picked}, adding {said}"),
        (Some(only), None) | (None, Some(only)) => only,
        (None, None) => "(no words)".to_owned(),
    }
}
