//! The designer: an issue's run from "Planned" through "Designing" to "Design ready", and the gate
//! in front of Implementing.
//!
//! # In order
//!
//! 1. **Start.** Approving a plan that says the work needs a design (and not approving it with the
//!    design skipped) moves the run to Designing, and the project's designer (`[workflow]
//!    designer`) starts by itself. **Request a design** in the Design tab does the same for any
//!    planned issue whose plan you approved. With no designer configured the run waits at
//!    Designing and nothing is spent; Start designer runs it once there is one.
//! 2. **Design**: one headless session under the role's read-only profile (§6h), in the issue's
//!    worktree, given the issue and the approved plan in its brief. Its run tools are a reviewer's
//!    plus `prototype_prepare` and `prototype_publish`, and on its token those act on this run's
//!    design: prepare reads the project's component and token evidence from the worktree, and
//!    publish keeps a self-contained HTML document and its reuse notes under wtm's data directory
//!    (ARCHITECTURE §6g), never in the source tree. Each publication is the run's next numbered
//!    revision, kept with its fingerprint, brief, the feedback it answers, reuse notes and sources.
//! 3. **Design ready.** The run moves to `DesignReady` and holds `AwaitingApproval`, and one inbox item
//!    says so. The Design tab lists every revision, previews any of them in the existing sandboxed
//!    review pane, and keeps your comments per revision.
//! 4. **Your decision.** Approve records `Approval(design, revision, fingerprint, sources)` and
//!    releases the hold. Request changes sends your note and that revision's comments into the same
//!    conversation, and the next revision replaces the approval of any earlier one.
//! 5. **Implementing** waits behind [`Engine::check_implementing`], which every route into it goes
//!    through (`Engine::record`): an approved plan, and, when the plan asked for a design or a design
//!    was started anyway, an approved revision whose artifact and sources are unchanged.
//!
//! # Why a designer's prototype belongs to its run
//!
//! A chat's prototype belongs to the chat that made it, found through the live session. A designer
//! is a headless session that is closed after every turn and resumed hours later with a new token,
//! so the only thing that lasts as long as its design is the run. The prototype record names the
//! run, the run's design record names the prototype, and a token reaches the prototype only through
//! the run it is scoped to.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use wtm_core::flow::RunPart;
use wtm_core::flow::{
    ApprovalSubject, Blocked, Design, DesignApproval, DesignComment, DesignDecision, DesignNow,
    DesignOrigin, DesignRevision, DesignStanding, DesignVerdict, Field, Flow, FlowEvent, FlowId,
    FlowKind, GateFacts, Hold, HoldKind, InboxKind, InboxTarget, InterruptCause, Invocation,
    InvocationChange, InvocationStatus, NewApproval, NewDesign, NewDesignRevision, NewInboxItem,
    NewInvocation, NewStep, OutputKind, Phase, PlanApproval, PlanRecord, PlanStanding, PlanVerdict,
    RoleSpec, SourcePin, Step, StepChange, StepStatus, TrackerKind, WorkflowSpec, Workspace,
    implementing_gate,
};
use wtm_core::model::AgentEvent;
use wtm_core::ports::{
    DesignStore, FlowStore, InboxStore, OutboxStore, PlanStore, RuleStore, RunStore, TrackerMirror,
};

use super::{Engine, Live, Refusal, Run, Signal, brief, missing_skill, text};
use crate::app::{FlowRole, SessionScope};
use crate::flow_session::RoleSession;
use crate::handoff::CodeCall;
use crate::prototype_commands::{self as lifecycle, Owner};

/// What one designer turn is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DesignTurn {
    /// The first: design from the approved plan.
    Begin,
    /// You asked for changes to a revision: your note and its comments, as sent.
    Changes { revision: u32, feedback: String },
    /// Carry on after an interruption, a failure or a limit.
    Resume,
}

/// How one designer turn ended.
#[derive(Debug)]
enum TurnEnd {
    Published(u32),
    Failed(String),
    Limit {
        resets_at: Option<u64>,
        message: String,
    },
    Cancelled,
    Quitting,
}

/// A designer turn that is running, as the prototype tools find it.
#[derive(Debug, Default)]
pub(super) struct DesignerLive {
    /// What this turn answers, kept with the revision it publishes.
    feedback: Option<String>,
    /// The approved plan revision it designs.
    plan_revision: u32,
    /// The revision it published this turn: one per turn.
    published: Mutex<Option<u32>>,
}

/// One revision as the Design tab shows it: the record, and whether it still reads as published.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RevisionView {
    pub revision: DesignRevision,
    /// Whether its artifact still reads back with the fingerprint it was published with.
    pub readable: bool,
    /// Its sources whose content in the worktree differs from when it was drawn, by path.
    pub changed_sources: Vec<String>,
    /// The directory its HTML and reuse notes are in, under wtm's data directory.
    pub artifact: Option<String>,
}

/// Whether the run may start implementing now, and if not, why.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GateView {
    pub ready: bool,
    pub blocked: Option<Blocked>,
    /// `blocked`, as the sentence the window shows.
    pub reason: Option<String>,
}

/// Everything the issue workspace's Design tab shows about an issue's design.
///
/// Its flags are what the tab may offer now, each read from the run's state once here, so the
/// window never works out from a phase and a hold what Rust would refuse.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(clippy::struct_excessive_bools)]
pub struct DesignView {
    pub flow: Flow,
    pub key: Option<String>,
    /// The role `[workflow] designer` names now, or `None` when the project names none.
    pub designer: Option<String>,
    pub design: Option<Design>,
    /// Oldest first: every revision, however many came after it.
    pub revisions: Vec<RevisionView>,
    pub comments: Vec<DesignComment>,
    pub decisions: Vec<DesignDecision>,
    pub standing: DesignStanding,
    /// The approved plan's revision, when a plan is approved.
    pub plan_revision: Option<u32>,
    /// Whether the approved plan said the work needs a design.
    pub design_needed: bool,
    /// Whether the plan was approved with the design skipped.
    pub skip_design: bool,
    /// Whether Implementing waits for an approved design.
    pub required: bool,
    pub gate: GateView,
    /// The designer's sessions, oldest first: one provider conversation, resumed.
    pub sessions: Vec<Invocation>,
    pub step: Option<Step>,
    pub running: bool,
    /// Whether Request a design would start one now.
    pub can_request: bool,
    /// Whether Start designer (or Resume designer) would run it now.
    pub can_start: bool,
    /// Whether the newest revision waits for Approve or Request changes.
    pub can_decide: bool,
    /// Whether Start implementing is offered: the run is where implementing would follow.
    pub can_implement: bool,
}

/// What a designer's run reads while it works, resolved once per turn.
struct DesignCtx {
    flow: FlowId,
    design: Design,
    plan: PlanRecord,
    context: Option<wtm_core::flow::PlanContext>,
    tracker: TrackerKind,
    worktree_path: PathBuf,
    branch: Option<String>,
    spec: RoleSpec,
    /// The repository's instruction sources the project names, or the default ones.
    patterns: Vec<String>,
}

/// The fingerprint a source that can no longer be read is measured with: it differs from every
/// fingerprint a readable file has, so a removed or excluded source makes an approval stale.
const MISSING: &str = "missing";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Prepare {
    brief: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    sources: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Publish {
    id: String,
    revision: String,
    html: String,
    reuse: String,
}

impl Engine {
    // ═══════════════════════════════ reading ═══════════════════════════════

    /// Whether `flow` is an issue's run with a design record.
    pub(super) fn is_designing(&self, flow: FlowId) -> bool {
        self.db()
            .ok()
            .and_then(|db| db.design(flow).ok().flatten())
            .is_some()
    }

    /// Whether a designer turn of `flow` is running now.
    pub(super) fn is_designer_running(&self, flow: FlowId) -> bool {
        self.live
            .lock()
            .iter()
            .any(|((of, _), live)| *of == flow && live.designer.is_some())
    }

    /// The designer role a workflow names, with its spec, when it names one.
    fn designer_of(workflow: &WorkflowSpec) -> Option<(String, RoleSpec)> {
        let id = workflow.designer.clone()?;
        let spec = workflow.role.get(&id)?.clone();
        Some((id, spec))
    }

    /// The plan you approved, and what the approval was of, while the newest decision is an
    /// approval.
    pub(super) fn approved_plan(
        &self,
        flow: FlowId,
    ) -> Result<Option<(PlanRecord, PlanApproval, PlanStanding)>, String> {
        let db = self.db()?;
        let plans = db.plans(flow).map_err(text)?;
        let decisions = db.plan_decisions(flow).map_err(text)?;
        let issue_now = match db.planning(flow).map_err(text)? {
            Some(planning) => db
                .mirrored_issue(planning.tracker, &planning.key)
                .map_err(text)?
                .map(|(detail, _)| wtm_core::flow::IssueText::from_detail(&detail)),
            None => None,
        };
        let standing = self.standing_of(flow, &plans, &decisions, issue_now.as_ref())?;
        let Some(PlanVerdict::Approved { approval, .. }) = decisions.last().map(|d| &d.verdict)
        else {
            return Ok(None);
        };
        let Some(ApprovalSubject::Plan(subject)) = db
            .approved(*approval)
            .map_err(text)?
            .map(|approved| approved.subject)
        else {
            return Ok(None);
        };
        let Some(plan) = plans.into_iter().find(|p| p.revision == subject.revision) else {
            return Ok(None);
        };
        Ok(Some((plan, *subject, standing)))
    }

    /// The newest revision's artifact and sources as they read now, which an approval is measured
    /// against, with what differs, for the window.
    fn design_now(&self, design: &Design, latest: &DesignRevision) -> (DesignNow, RevisionView) {
        let (readable, artifact) = self.read_revision(design, latest);
        let root = lifecycle::root_of(&self.app, design.project.as_str(), &design.worktree).ok();
        let sources: Vec<SourcePin> = latest
            .sources
            .iter()
            .map(|pin| SourcePin {
                path: pin.path.clone(),
                fingerprint: root
                    .as_deref()
                    .and_then(|root| crate::prototype_context::read(root, &pin.path).ok())
                    .map_or_else(|| MISSING.to_owned(), |source| source.fingerprint),
            })
            .collect();
        let changed_sources = latest
            .sources
            .iter()
            .zip(&sources)
            .filter(|(then, now)| then.fingerprint != now.fingerprint)
            .map(|(then, _)| then.path.clone())
            .collect();
        (
            DesignNow {
                latest: latest.revision,
                fingerprint: readable.then(|| latest.fingerprint.clone()),
                sources,
            },
            RevisionView {
                revision: latest.clone(),
                readable,
                changed_sources,
                artifact,
            },
        )
    }

    /// Whether a revision's artifact reads back as it was published, and where it is.
    fn read_revision(&self, design: &Design, revision: &DesignRevision) -> (bool, Option<String>) {
        let Ok(prototype) = lifecycle::run_record(
            &self.app,
            revision.flow.0,
            design.project.as_str(),
            &design.worktree,
            &revision.prototype,
        ) else {
            return (false, None);
        };
        let artifact = Path::new(&prototype.directory)
            .join("reviews")
            .join(&revision.review)
            .to_string_lossy()
            .into_owned();
        let readable =
            crate::prototypes::artifact(&prototype, &revision.review).is_ok_and(|(html, reuse)| {
                crate::prototype_context::fingerprint(&format!("{html}\0{reuse}"))
                    == revision.fingerprint
            });
        (readable, Some(artifact))
    }

    /// Where the run's design stands now, and whether one is started.
    fn design_standing(&self, flow: FlowId) -> Result<(DesignStanding, Vec<RevisionView>), String> {
        let db = self.db()?;
        let Some(design) = db.design(flow).map_err(text)? else {
            return Ok((DesignStanding::None, Vec::new()));
        };
        let revisions = db.design_revisions(flow).map_err(text)?;
        let decisions = db.design_decisions(flow).map_err(text)?;
        let Some(latest) = revisions.last() else {
            return Ok((DesignStanding::None, Vec::new()));
        };
        let (now, latest_view) = self.design_now(&design, latest);
        let approval = decisions
            .iter()
            .rev()
            .find_map(|d| match d.verdict {
                DesignVerdict::Approved { approval } => Some(approval),
                DesignVerdict::ChangesRequested { .. } => None,
            })
            .map(|approval| db.approved(approval).map_err(text))
            .transpose()?
            .flatten()
            .and_then(|approved| match approved.subject {
                ApprovalSubject::Design(subject) => Some((approved.approval, *subject)),
                _ => None,
            });
        let standing = wtm_core::flow::design::standing(
            Some(latest),
            &decisions,
            approval.as_ref().map(|(a, s)| (a, s)),
            &now,
        );
        let mut views: Vec<RevisionView> = revisions[..revisions.len() - 1]
            .iter()
            .map(|revision| {
                let (readable, artifact) = self.read_revision(&design, revision);
                RevisionView {
                    revision: revision.clone(),
                    readable,
                    changed_sources: Vec::new(),
                    artifact,
                }
            })
            .collect();
        views.push(latest_view);
        Ok((standing, views))
    }

    /// Whether the run may enter Implementing now: the plan and the design it needs, approved and
    /// still covering what would be built. `Ok(Ok(()))` for a run of another kind, which this gate
    /// is not about.
    pub(super) fn implement_gate(&self, flow: FlowId) -> Result<Result<(), Blocked>, String> {
        let state = self.state(flow)?;
        if state.kind != FlowKind::Implement {
            return Ok(Ok(()));
        }
        let (plan, design_needed) =
            if let Some((_, subject, standing)) = self.approved_plan(flow)? {
                (standing, subject.design_needed)
            } else {
                let db = self.db()?;
                let plans = db.plans(flow).map_err(text)?;
                let decisions = db.plan_decisions(flow).map_err(text)?;
                (self.standing_of(flow, &plans, &decisions, None)?, false)
            };
        let designing =
            self.is_designing(flow) || matches!(state.phase, Phase::Designing | Phase::DesignReady);
        let (design, _) = self.design_standing(flow)?;
        Ok(implementing_gate(&GateFacts {
            plan: &plan,
            design_needed,
            designing,
            design: &design,
        }))
    }

    /// The gate in front of Implementing, as `Engine::record` holds it.
    pub(super) fn check_implementing(&self, flow: FlowId) -> Result<(), String> {
        match self.implement_gate(flow)? {
            Ok(()) => Ok(()),
            Err(blocked) => Err(blocked.to_string()),
        }
    }

    /// Everything about an issue run's design.
    ///
    /// # Errors
    ///
    /// When the run does not exist or the database cannot be read.
    pub fn design_view(&self, flow: FlowId) -> Result<DesignView, String> {
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
        let designer = self
            .kept_workflow(flow, RunPart::Design, project.as_ref())
            .as_ref()
            .and_then(Self::designer_of)
            .map(|(id, _)| id);
        let design = db.design(flow).map_err(text)?;
        let comments = db.design_comments(flow).map_err(text)?;
        let decisions = db.design_decisions(flow).map_err(text)?;
        let (standing, revisions) = self.design_standing(flow)?;
        let approved = self.approved_plan(flow)?;
        let (plan_revision, design_needed, skip_design, plan_approved) = match &approved {
            Some((plan, subject, standing)) => (
                Some(plan.revision),
                subject.design_needed,
                subject.skip_design,
                matches!(standing, PlanStanding::Approved { .. }),
            ),
            None => (None, false, false, false),
        };
        let phase = state.state.phase;
        let designing = design.is_some() || matches!(phase, Phase::Designing | Phase::DesignReady);
        let required = designing || (design_needed && !skip_design);
        let gate = match self.implement_gate(flow)? {
            Ok(()) => GateView {
                ready: true,
                blocked: None,
                reason: None,
            },
            Err(blocked) => GateView {
                ready: false,
                reason: Some(blocked.to_string()),
                blocked: Some(blocked),
            },
        };
        let step = design.as_ref().and_then(|design| {
            db.steps(flow)
                .ok()?
                .into_iter()
                .find(|s| s.role == design.role)
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
        let open = state.state.end.is_none();
        let hold = state.state.hold.as_ref().map(Hold::kind);
        let can_request = open
            && !running
            && hold.is_none()
            && phase == Phase::Planned
            && plan_approved
            && designer.is_some();
        let can_start = open
            && !running
            && phase == Phase::Designing
            && designer.is_some()
            && matches!(
                hold,
                None | Some(HoldKind::Interrupted | HoldKind::WaitingCapacity)
            );
        let can_decide = open
            && !running
            && phase == Phase::DesignReady
            && !revisions.is_empty()
            && matches!(hold, None | Some(HoldKind::AwaitingApproval));
        let can_implement =
            open && !running && matches!(phase, Phase::Planned | Phase::DesignReady);
        if let DesignStanding::Stale {
            approval, revision, ..
        } = &standing
        {
            let key = planning
                .as_ref()
                .map_or_else(String::new, |p| p.key.clone());
            self.file(&NewInboxItem {
                flow,
                kind: InboxKind::Approval,
                target: InboxTarget::Approval {
                    approval: *approval,
                },
                title: format!("{key}: the approved design no longer covers what would be built"),
                body: format!(
                    "Your approval of design revision {revision} covered its artifact and the \
                     project files it was drawn from. One of them changed since. The Design tab \
                     shows which; ask for a fresh revision and approve that."
                ),
            });
        }
        Ok(DesignView {
            flow: state,
            key: planning.map(|p| p.key),
            designer,
            design,
            revisions,
            comments,
            decisions,
            standing,
            plan_revision,
            design_needed,
            skip_design,
            required,
            gate,
            sessions,
            step,
            running,
            can_request,
            can_start,
            can_decide,
            can_implement,
        })
    }

    /// The issue's design, for the Design tab, by project and key.
    ///
    /// # Errors
    ///
    /// When the gates are closed or the database cannot be read.
    pub fn issue_design(&self, project_id: &str, key: &str) -> Result<Option<DesignView>, String> {
        match self.issue_plan(project_id, key)? {
            Some(plan) => self.design_view(plan.flow.id).map(Some),
            None => Ok(None),
        }
    }

    /// The approved design of the issue whose work `flow` reviews, for a role that takes a design:
    /// as [`brief::design_section`] writes it, while its approval still holds.
    pub(super) fn approved_design_for(&self, flow: FlowId) -> Option<String> {
        let db = self.db().ok()?;
        let item = db.flow(flow).ok()??.work_item;
        let implement = db
            .open_flows(item)
            .ok()?
            .into_iter()
            .find(|f| f.state.kind == FlowKind::Implement)?;
        self.approved_design(implement.id)
    }

    /// An issue run's approved design, as its implementer is given it, while the approval still
    /// covers the newest revision, its artifact and its sources. `None` when there is none.
    #[must_use]
    pub fn approved_design(&self, flow: FlowId) -> Option<String> {
        let (standing, revisions) = self.design_standing(flow).ok()?;
        let DesignStanding::Approved { revision, .. } = standing else {
            return None;
        };
        let view = revisions
            .into_iter()
            .find(|v| v.revision.revision == revision)?;
        Some(brief::design_section(
            &view.revision,
            view.artifact.as_deref()?,
        ))
    }

    // ═══════════════════════════════ starting ═══════════════════════════════

    /// After a plan approval: design when the plan said the work needs it and you did not skip it.
    /// The approval stands either way; what went wrong starting the designer is said in the inbox.
    pub(super) fn design_after_plan(&self, flow: FlowId, design_needed: bool, skip_design: bool) {
        if !design_needed || skip_design {
            return;
        }
        let Some(me) = self.me.upgrade() else { return };
        if let Err(refusal) = me.begin_designing(flow, DesignOrigin::Plan, None) {
            tracing::info!(%flow, %refusal, "the designer did not start by itself");
            let key = self
                .db()
                .ok()
                .and_then(|db| db.planning(flow).ok().flatten())
                .map_or_else(|| "The issue".to_owned(), |p| p.key);
            self.file(&NewInboxItem {
                flow,
                kind: InboxKind::Failure,
                target: InboxTarget::Flow,
                title: format!("{key} needs a design, and the designer did not start"),
                body: format!(
                    "{}. The run waits at Designing; Start designer in the issue's Design tab runs \
                     it once that is fixed. Nothing is implemented before a design is approved.",
                    super::capitalise(&refusal.message)
                ),
            });
        }
    }

    /// Request a design for a planned issue whose plan you approved: the same step as a plan that
    /// asked for one, with your words in the designer's brief.
    ///
    /// # Errors
    ///
    /// Why it did not start: the run is not at "Planned, approved", no designer is configured, or a
    /// provider would not bill a subscription. Nothing was recorded.
    pub fn request_design(self: &Arc<Self>, flow: FlowId, request: &str) -> Result<(), Refusal> {
        let request = request.trim();
        self.begin_designing(
            flow,
            DesignOrigin::You,
            (!request.is_empty()).then(|| request.to_owned()),
        )
    }

    fn begin_designing(
        self: &Arc<Self>,
        flow: FlowId,
        origin: DesignOrigin,
        request: Option<String>,
    ) -> Result<(), Refusal> {
        let state = self.state(flow)?;
        if state.end.is_some() {
            return Err("this run has ended".into());
        }
        if self.is_running(flow) {
            return Err("this run is working already".into());
        }
        if state.phase != Phase::Planned || state.hold.is_some() {
            return Err(format!(
                "a design follows an approved plan, and this run is not at \"Planned, approved\" \
                 ({:?})",
                state.phase
            )
            .into());
        }
        match self.approved_plan(flow)? {
            Some((_, _, PlanStanding::Approved { .. })) => {}
            _ => {
                return Err(
                    "approve the plan first: a design is drawn for an approved plan".into(),
                );
            }
        }
        let db = self.db()?;
        let planning = db
            .planning(flow)
            .map_err(text)?
            .ok_or("only a planned issue's run designs")?;
        let project = self.app.project(planning.project.as_str()).map_err(text)?;
        // Kept as designing begins, below, so a designer configured since Start Work is the one.
        let workflow = self
            .kept_workflow(flow, RunPart::Design, Some(&project))
            .unwrap_or_default();
        let configured = Self::designer_of(&workflow);
        // A plan that asked for a design moves on without a designer: the run waits at Designing,
        // where nothing can be implemented, rather than at Planned, where the gate would be the
        // only thing saying so. A request of yours has nothing to wait for and is refused.
        let Some((role, spec)) = configured else {
            if origin == DesignOrigin::You {
                return Err(no_designer().into());
            }
            self.record(
                flow,
                &FlowEvent::Advanced {
                    to: Phase::Designing,
                },
            )?;
            return Err(no_designer().into());
        };
        self.host.preflight(&[spec.provider.id()])?;
        db.begin_design(
            &NewDesign {
                flow,
                project: planning.project.clone(),
                worktree: planning.worktree.clone(),
                key: planning.key.clone(),
                role,
                origin,
                request,
            },
            self.now(),
        )
        .map_err(text)?;
        self.begin_part(flow, RunPart::Design, &workflow)?;
        self.record(
            flow,
            &FlowEvent::Advanced {
                to: Phase::Designing,
            },
        )?;
        self.notice.changed(flow);
        Ok(self.spawn_designer(flow, DesignTurn::Begin)?)
    }

    /// Run the designer of a run waiting at Designing: the first time once a designer is
    /// configured, or again after an interruption, a failure or a limit, in the same conversation.
    ///
    /// # Errors
    ///
    /// Why it did not start. Nothing was recorded.
    pub fn start_designer(self: &Arc<Self>, flow: FlowId) -> Result<(), Refusal> {
        if self.is_running(flow) {
            return Err("its designer is working already".into());
        }
        let state = self.state(flow)?;
        if let Some(end) = &state.end {
            return Err(format!("this run has already ended ({end:?})").into());
        }
        if state.phase != Phase::Designing {
            return Err("there is nothing to design: the run is not at Designing".into());
        }
        let db = self.db()?;
        let planning = db
            .planning(flow)
            .map_err(text)?
            .ok_or("only a planned issue's run designs")?;
        let project = self.app.project(planning.project.as_str()).map_err(text)?;
        let workflow = self
            .kept_workflow(flow, RunPart::Design, Some(&project))
            .unwrap_or_default();
        let (role, spec) =
            Self::designer_of(&workflow).ok_or_else(|| Refusal::from(no_designer()))?;
        self.host.preflight(&[spec.provider.id()])?;
        let origin = match self.approved_plan(flow)? {
            Some((_, subject, _)) if subject.design_needed && !subject.skip_design => {
                DesignOrigin::Plan
            }
            _ => DesignOrigin::You,
        };
        {
            let _held = self.holds.lock();
            match state.hold.as_ref().map(Hold::kind) {
                Some(kind @ (HoldKind::Interrupted | HoldKind::WaitingCapacity)) => {
                    self.record(flow, &FlowEvent::Released { hold: kind })?;
                }
                Some(other) => {
                    return Err(format!(
                        "this run is waiting on {other:?}, which starting the designer does not \
                         answer"
                    )
                    .into());
                }
                None => {}
            }
        }
        db.begin_design(
            &NewDesign {
                flow,
                project: planning.project.clone(),
                worktree: planning.worktree.clone(),
                key: planning.key.clone(),
                role,
                origin,
                request: None,
            },
            self.now(),
        )
        .map_err(text)?;
        self.begin_part(flow, RunPart::Design, &workflow)?;
        self.settle(
            flow,
            &[
                InboxKind::Interrupted,
                InboxKind::Failure,
                InboxKind::Capacity,
            ],
        );
        let begun = self.designer_sessions(flow).is_empty();
        Ok(self.spawn_designer(
            flow,
            if begun {
                DesignTurn::Begin
            } else {
                DesignTurn::Resume
            },
        )?)
    }

    fn designer_sessions(&self, flow: FlowId) -> Vec<Invocation> {
        let Ok(db) = self.db() else {
            return Vec::new();
        };
        let Some(design) = db.design(flow).ok().flatten() else {
            return Vec::new();
        };
        let Some(step) = db
            .steps(flow)
            .ok()
            .and_then(|steps| steps.into_iter().find(|s| s.role == design.role))
        else {
            return Vec::new();
        };
        db.invocations(flow)
            .unwrap_or_default()
            .into_iter()
            .filter(|i| i.step == step.id)
            .collect()
    }

    fn spawn_designer(self: &Arc<Self>, flow: FlowId, turn: DesignTurn) -> Result<(), String> {
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
            .name(format!("design-{flow}"))
            .spawn(move || {
                struct Done<'a>(&'a Engine, FlowId);
                impl Drop for Done<'_> {
                    fn drop(&mut self) {
                        self.0.runs.lock().remove(&self.1);
                        self.0.notice.changed(self.1);
                    }
                }
                let _done = Done(&engine, flow);
                if let Err(reason) = engine.drive_designer(flow, &run, &turn)
                    && !engine.quitting()
                {
                    tracing::warn!(%flow, %reason, "a designer turn failed");
                    engine.designer_failed(flow, &reason);
                }
            });
        if let Err(error) = spawned {
            self.runs.lock().remove(&flow);
            return Err(format!(
                "wtm could not start the designer's thread: {error}"
            ));
        }
        Ok(())
    }

    // ═══════════════════════════════ driving ═══════════════════════════════

    fn design_ctx(&self, flow: FlowId) -> Result<DesignCtx, String> {
        let db = self.db()?;
        let design = db
            .design(flow)
            .map_err(text)?
            .ok_or_else(|| format!("run {flow} has not started designing"))?;
        let planning = db
            .planning(flow)
            .map_err(text)?
            .ok_or_else(|| format!("run {flow} has not planned anything"))?;
        let project = self
            .app
            .project(design.project.as_str())
            .map_err(|e| format!("the project is no longer registered: {e}"))?;
        let worktree = self
            .app
            .worktree(&project, &design.worktree)
            .map_err(|e| format!("the issue's worktree is no longer there: {e}"))?;
        let workflow = self.run_workflow(flow, RunPart::Design, &project)?;
        let spec = workflow.role.get(&design.role).cloned().ok_or_else(|| {
            format!(
                "role `{}` is no longer in your workflow config, so the designer cannot run",
                design.role
            )
        })?;
        let (plan, _, _) = self
            .approved_plan(flow)?
            .ok_or("the plan is no longer approved, so there is nothing to design for")?;
        Ok(DesignCtx {
            flow,
            plan,
            context: planning.context,
            tracker: planning.tracker,
            branch: worktree.branch().map(|b| b.as_str().to_owned()),
            worktree_path: worktree.path.clone(),
            patterns: workflow.instruction_sources(),
            spec,
            design,
        })
    }

    fn drive_designer(&self, flow: FlowId, run: &Run, turn: &DesignTurn) -> Result<(), String> {
        let db = self.db()?;
        let ctx = self.design_ctx(flow)?;
        let step = self.designer_step(&ctx)?;
        let resume = db
            .invocations(flow)
            .map_err(text)?
            .into_iter()
            .rev()
            .filter(|i| i.step == step.id)
            .find_map(|i| i.provider_session);
        // A resume after a request for changes that never reached the designer — the turn failed,
        // or wtm quit — sends that request again, rather than a bare "carry on".
        let pending = self.pending_feedback(flow)?;
        let (words, feedback) = match turn {
            DesignTurn::Begin => (brief::designer_opening_turn(&ctx.design.key), None),
            DesignTurn::Changes { revision, feedback } => (
                brief::designer_changes_turn(*revision, feedback, resume.is_some()),
                Some(feedback.clone()),
            ),
            DesignTurn::Resume => (
                brief::designer_resume_turn(
                    &ctx.design.key,
                    pending.as_ref().map(|(r, f)| (*r, f.as_str())),
                    resume.is_some(),
                ),
                pending.map(|(_, f)| f),
            ),
        };
        let end = self.designer_turn(&ctx, run, &step, resume.as_deref(), &words, feedback);
        self.after_design_turn(&ctx, &step, end);
        Ok(())
    }

    /// The repository's instructions and your rules, for a designer whose role takes them: the
    /// planner's retrieval (§6i), recorded against this attempt, so a revision drawn against the
    /// project's own guidance says which guidance that was.
    fn designer_knowledge(
        &self,
        ctx: &DesignCtx,
        attempt: u32,
        invocation: wtm_core::flow::InvocationId,
    ) -> Option<String> {
        let wants_instructions = ctx
            .spec
            .inputs
            .contains(&wtm_core::flow::RoleInput::Instructions);
        let query = wtm_core::flow::RuleQuery::for_role(&ctx.spec, &[]);
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
                        "could not read the instruction sources for the designer");
                })
                .ok()
            })
            .flatten();
        let db = self.db().ok()?;
        let rules = query.and_then(|query| {
            let all = db.rules(&ctx.design.project).ok()?;
            let selection = wtm_core::flow::knowledge::select(&all, &ctx.design.project, &query);
            if let Err(error) = db.record_applications(
                &wtm_core::flow::ReviewInvocation {
                    flow: ctx.flow,
                    role: ctx.design.role.clone(),
                    attempt,
                },
                &selection.applied,
                self.now(),
            ) {
                tracing::warn!(flow = %ctx.flow, %error, "could not record the rules the designer was given");
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

    /// The feedback a request for changes sent that no revision has answered yet.
    fn pending_feedback(&self, flow: FlowId) -> Result<Option<(u32, String)>, String> {
        let db = self.db()?;
        let latest = db.design_revisions(flow).map_err(text)?.last().cloned();
        let Some(latest) = latest else {
            return Ok(None);
        };
        let decision = db.design_decisions(flow).map_err(text)?.last().cloned();
        match decision {
            Some(DesignDecision {
                revision,
                verdict: DesignVerdict::ChangesRequested { note },
                ..
            }) if revision == latest.revision => {
                let comments = db.design_comments(flow).map_err(text)?;
                Ok(Some((revision, feedback_text(&note, revision, &comments))))
            }
            _ => Ok(None),
        }
    }

    /// The designer's one step: its role's, made the first time.
    fn designer_step(&self, ctx: &DesignCtx) -> Result<Step, String> {
        let db = self.db()?;
        if let Some(step) = db
            .steps(ctx.flow)
            .map_err(text)?
            .into_iter()
            .find(|s| s.role == ctx.design.role)
        {
            return Ok(step);
        }
        db.add_steps(
            ctx.flow,
            &[NewStep {
                stage: 1,
                role: ctx.design.role.clone(),
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
        .ok_or_else(|| "the designer's step was not kept".to_owned())
    }

    /// One designer turn: one session, opened (resumed when there is a conversation), sent `words`,
    /// and read until it publishes a revision or stops.
    #[allow(clippy::too_many_lines)]
    fn designer_turn(
        &self,
        ctx: &DesignCtx,
        run: &Run,
        step: &Step,
        resume: Option<&str>,
        words: &str,
        feedback: Option<String>,
    ) -> TurnEnd {
        let Ok(db) = self.db() else {
            return TurnEnd::Quitting;
        };
        let attempt = db
            .invocations(ctx.flow)
            .map_or(0, |all| all.iter().filter(|i| i.step == step.id).count())
            + 1;
        let attempt = u32::try_from(attempt).unwrap_or(u32::MAX);
        // A designer session counts against the run-session caps like any role's, and waits for a
        // place before its invocation is recorded (`capacity.rs`). The guard lives until the turn
        // ends, which is when its session is closed.
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
        let revisions = db.design_revisions(ctx.flow).unwrap_or_default();
        let knowledge = self.designer_knowledge(ctx, attempt, invocation.id);
        let brief_text = brief::designer_brief(&brief::DesignerFacts {
            role: &ctx.design.role,
            spec: &ctx.spec,
            key: &ctx.design.key,
            tracker: ctx.tracker,
            worktree_path: &ctx.worktree_path.to_string_lossy(),
            branch: ctx.branch.as_deref(),
            context: ctx.context.as_ref(),
            plan: &ctx.plan,
            request: ctx.design.request.as_deref(),
            latest: revisions.last(),
            knowledge: knowledge.as_deref(),
        });

        let (tx, rx) = mpsc::channel();
        let key = (ctx.flow, step.role.clone());
        self.live.lock().insert(
            key.clone(),
            Arc::new(Live {
                // Never compared: a designer's `flow_submit` is refused before its kind is.
                output: OutputKind::Handoff,
                tx: tx.clone(),
                step: step.id,
                invocation: invocation.id,
                cwd: ctx.worktree_path.clone(),
                tree_sha: String::new(),
                checks: std::collections::BTreeMap::new(),
                dir: wtm_core::flow::FlowDir::new(&self.app.config.paths().flow_dir, ctx.flow),
                fix: None,
                planner: None,
                designer: Some(Arc::new(DesignerLive {
                    feedback,
                    plan_revision: ctx.plan.revision,
                    published: Mutex::new(None),
                })),
                implement: None,
                project: ctx.design.project.as_str().to_owned(),
                worktree: ctx.design.worktree.clone(),
                // A designer runs no checks and drives no pane: it reads, and publishes.
                start_services: None,
                browser: None,
            }),
        );
        let session_spec = RoleSession {
            project: ctx.design.project.as_str().to_owned(),
            worktree: ctx.design.worktree.clone(),
            role: FlowRole {
                flow: ctx.flow,
                role: step.role.clone(),
                authority: step.profile,
                browser: None,
                design: true,
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
                let end = self.read_designer(ctx, run, step, &session, invocation.id, words, &rx);
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
                TurnEnd::Published(_) => (InvocationStatus::Submitted, None),
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
    fn read_designer(
        &self,
        ctx: &DesignCtx,
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
        let mut published: Option<u32> = None;
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
                Ok(Signal::Published(revision)) => {
                    published = Some(revision);
                    settle_at = Some(self.clock.monotonic_ms() + self.tuning.settle_ms);
                }
                // A designer's `flow_submit` is refused and its `flow_ask` answered in place, so
                // neither arrives; and a tick with nothing new is a tick.
                Ok(Signal::Submitted(_) | Signal::Asked(_)) | Err(RecvTimeoutError::Timeout) => {}
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
                        if let Some(revision) = published {
                            return TurnEnd::Published(revision);
                        }
                        if nudges == 0 {
                            return TurnEnd::Failed(
                                "it ended its turn without publishing a revision".to_owned(),
                            );
                        }
                        nudges -= 1;
                        if let Err(end) = say("Your turn ended without a published revision. Call \
                             `prototype_prepare`, then `prototype_publish` with the `id` and \
                             `revision` it gave you, the whole HTML document and your reuse \
                             notes, and end your turn.")
                        {
                            return end;
                        }
                    }
                    AgentEvent::LimitReached { message, resets_at } => {
                        if let Some(revision) = published {
                            return TurnEnd::Published(revision);
                        }
                        return TurnEnd::Limit { resets_at, message };
                    }
                    AgentEvent::Failed { message } => {
                        if let Some(revision) = published {
                            return TurnEnd::Published(revision);
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
                                 prompt means the profile and the provider disagree. The designer \
                                 waits until its timeout.",
                                super::describe(&request)
                            ),
                        });
                    }
                    _ => {}
                },
                Ok(Signal::Exited(summary)) => {
                    if let Some(revision) = published {
                        return TurnEnd::Published(revision);
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
            if let Some(revision) = published
                && settle_at.is_some_and(|at| now >= at)
            {
                return TurnEnd::Published(revision);
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
                if let Some(revision) = published {
                    return TurnEnd::Published(revision);
                }
                return TurnEnd::Failed(format!(
                    "it had published nothing after {}",
                    super::minutes(step.timeout_ms)
                ));
            }
        }
    }

    /// Record what a turn came to.
    fn after_design_turn(&self, ctx: &DesignCtx, step: &Step, end: TurnEnd) {
        match end {
            TurnEnd::Published(revision) => {
                // Pending, not Done: a done step holds a submission, and a designer's result is a
                // revision, kept as one. The step runs again when you ask for changes, as a
                // planner's does after a question.
                self.update_step(
                    step,
                    &StepChange::status(StepStatus::Pending).detail(format!(
                        "published revision {revision}; waiting for your decision"
                    )),
                );
                {
                    let _held = self.holds.lock();
                    if self
                        .state(ctx.flow)
                        .is_ok_and(|s| s.phase == Phase::Designing && s.end.is_none())
                    {
                        let _ = self.record(
                            ctx.flow,
                            &FlowEvent::Advanced {
                                to: Phase::DesignReady,
                            },
                        );
                    }
                    if self
                        .state(ctx.flow)
                        .is_ok_and(|s| s.hold.is_none() && s.end.is_none())
                    {
                        let _ = self.record(
                            ctx.flow,
                            &FlowEvent::Held {
                                hold: Hold::AwaitingApproval,
                            },
                        );
                    }
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
                    title: format!("{}: design revision {revision} is ready", ctx.design.key),
                    body: "Preview it in the issue's Design tab, comment on it, and approve it or \
                           ask for changes. Nothing is implemented until a design revision is \
                           approved."
                        .to_owned(),
                });
            }
            TurnEnd::Failed(reason) => {
                self.update_step(
                    step,
                    &StepChange::status(StepStatus::Failed).detail(reason.clone()),
                );
                self.designer_failed(ctx.flow, &reason);
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
                        "{}: the designer is paused, {} limit reached",
                        ctx.design.key, step.provider
                    ),
                    body: format!(
                        "{message}\n\nResume designer carries on in the same conversation. wtm \
                         never uses an API key."
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

    /// The designer stopped for good this turn: say so where you will see it. The run stays at
    /// Designing, and Resume designer runs it again in the same conversation.
    fn designer_failed(&self, flow: FlowId, reason: &str) {
        let key = self
            .db()
            .ok()
            .and_then(|db| db.planning(flow).ok().flatten())
            .map_or_else(|| "The issue".to_owned(), |p| p.key);
        self.file(&NewInboxItem {
            flow,
            kind: InboxKind::Failure,
            target: InboxTarget::Flow,
            title: format!("{key}: the designer stopped"),
            body: format!(
                "{}. Every revision it published is kept: Resume designer runs it again in the \
                 same conversation.",
                super::capitalise(reason)
            ),
        });
        self.notice.changed(flow);
    }

    // ═══════════════════════════════ the prototype tools ═══════════════════════════════

    /// `prototype_prepare` or `prototype_publish` from a designer's token: its run's design, while
    /// one of its turns is running.
    ///
    /// # Errors
    ///
    /// The refusal, as the tool result's text: no turn running, a prototype that is not its run's,
    /// arguments that do not match, a second revision in one turn, or what the prototype store
    /// refused.
    pub fn design_tool(
        &self,
        role: &FlowRole,
        scope: &SessionScope,
        call: &CodeCall,
    ) -> Result<String, String> {
        let live = self
            .live
            .lock()
            .get(&(role.flow, role.role.clone()))
            .cloned()
            .filter(|live| live.designer.is_some())
            .ok_or_else(super::ended)?;
        let designer = live.designer.as_ref().ok_or_else(super::ended)?.clone();
        let db = self.db()?;
        let design = db
            .design(role.flow)
            .map_err(text)?
            .ok_or("this run has no design")?;
        if scope.place() != Some((design.project.as_str(), design.worktree.as_str())) {
            return Err("this session is not in the worktree its run designs in".to_owned());
        }
        let current = db
            .invocations(role.flow)
            .map_err(text)?
            .into_iter()
            .find(|i| i.id == live.invocation);
        let owner = Owner {
            project: design.project.as_str().to_owned(),
            worktree: design.worktree.clone(),
            session: current
                .as_ref()
                .and_then(|i| i.session.clone())
                .unwrap_or_default(),
            provider: current
                .as_ref()
                .map_or_else(String::new, |i| i.provider.id().to_owned()),
            conversation: current.and_then(|i| i.provider_session).unwrap_or_default(),
            flow: Some(role.flow.0),
        };
        let args = if call.args.is_null() {
            serde_json::json!({})
        } else {
            call.args.clone()
        };
        match call.tool.as_str() {
            "prototype_prepare" => {
                let Prepare { brief, id, sources } =
                    serde_json::from_value(args).map_err(|error| {
                        format!(
                            "`prototype_prepare` was called with arguments that do not match its \
                             schema: {error}. Fix them and call it again."
                        )
                    })?;
                // One prototype per run, so every revision is one history: the run's own, whatever
                // id the model remembers.
                let id = match (&design.prototype, id) {
                    (Some(kept), Some(asked)) if *kept != asked => {
                        return Err(format!(
                            "This run designs in prototype `{kept}`; prepare with that id, or \
                             leave `id` out."
                        ));
                    }
                    (Some(kept), _) => Some(kept.clone()),
                    (None, Some(_)) => {
                        return Err(
                            "This run has no prototype yet: leave `id` out to start its first \
                             revision."
                                .to_owned(),
                        );
                    }
                    (None, None) => None,
                };
                let previous = id.as_deref().and_then(|id| {
                    let kept = lifecycle::run_record(
                        &self.app,
                        role.flow.0,
                        &owner.project,
                        &owner.worktree,
                        id,
                    )
                    .ok()?;
                    let review = kept.review.as_ref()?.id.clone();
                    crate::prototypes::artifact(&kept, &review).ok()
                });
                let prototype = lifecycle::prepare_for_run(
                    &self.app,
                    &owner,
                    &brief,
                    id.as_deref(),
                    sources.as_deref(),
                )
                .map_err(|e| e.message)?;
                db.set_design_prototype(role.flow, &prototype.id, self.now())
                    .map_err(text)?;
                Ok(crate::prototype_tools::prepared(
                    &prototype,
                    previous,
                    "Stop after publishing: the person reviews the revision in the issue's Design \
                     tab and approves it there.",
                ))
            }
            "prototype_publish" => {
                let Publish {
                    id,
                    revision,
                    html,
                    reuse,
                } = serde_json::from_value(args).map_err(|error| {
                    format!(
                        "`prototype_publish` was called with arguments that do not match its \
                         schema: {error}. Fix them and call it again."
                    )
                })?;
                if design.prototype.as_deref() != Some(id.as_str()) {
                    return Err(
                        "Publish under the id `prototype_prepare` gave you for this run."
                            .to_owned(),
                    );
                }
                let mut published = designer.published.lock();
                if let Some(already) = *published {
                    return Err(format!(
                        "You published revision {already} in this turn already. End your turn: \
                         the person reviews it before anything else is drawn."
                    ));
                }
                let prototype =
                    lifecycle::publish_for_run(&self.app, &owner, &id, &revision, &html, &reuse)
                        .map_err(|e| e.message)?;
                let review = prototype
                    .review
                    .as_ref()
                    .ok_or("the revision was not kept")?;
                let recorded = db
                    .record_design_revision(
                        &NewDesignRevision {
                            flow: role.flow,
                            prototype: prototype.id.clone(),
                            review: review.id.clone(),
                            fingerprint: review.fingerprint.clone(),
                            brief: review.brief.clone(),
                            feedback: designer.feedback.clone(),
                            reuse: reuse.clone(),
                            sources: prototype
                                .sources
                                .iter()
                                .map(|s| SourcePin {
                                    path: s.path.clone(),
                                    fingerprint: s.fingerprint.clone(),
                                })
                                .collect(),
                            plan_revision: designer.plan_revision,
                            step: Some(live.step),
                            invocation: Some(live.invocation),
                        },
                        self.now(),
                    )
                    .map_err(text)?;
                *published = Some(recorded.revision);
                drop(published);
                self.notice.changed(role.flow);
                if live.tx.send(Signal::Published(recorded.revision)).is_err() {
                    return Err(super::ended());
                }
                Ok(format!(
                    "Saved as revision {} of this issue's design. Stop here and end your turn: the \
                     person previews and comments on it in the issue's Design tab, and approves it \
                     or asks for changes. Publishing is not approval, and nothing is implemented \
                     until a revision is approved.",
                    recorded.revision
                ))
            }
            other => Err(format!("no prototype tool named `{other}`")),
        }
    }

    // ═══════════════════════════════ your decisions ═══════════════════════════════

    /// Comment on revision `revision` of `flow`'s design. Any revision, however old: a comment is
    /// what you said about it, and the newest one's go to the designer with Request changes.
    ///
    /// # Errors
    ///
    /// When the text is empty or the run has no such revision.
    pub fn comment_on_design(
        &self,
        flow: FlowId,
        revision: u32,
        text_: &str,
    ) -> Result<DesignComment, String> {
        let said = text_.trim();
        if said.is_empty() {
            return Err("say something about the revision".to_owned());
        }
        let comment = self
            .db()?
            .comment_on_design(flow, revision, said, self.now())
            .map_err(text)?;
        self.notice.changed(flow);
        Ok(comment)
    }

    /// Approve revision `revision`, whose fingerprint you saw as `fingerprint`.
    ///
    /// The artifact is read back and its sources are read from the worktree first: what you
    /// approve is the revision as you saw it, drawn from the project as it is.
    ///
    /// # Errors
    ///
    /// When it is not the newest revision, the run is not waiting for a decision, or the artifact or
    /// its sources changed.
    pub fn approve_design(
        &self,
        flow: FlowId,
        revision: u32,
        fingerprint: &str,
    ) -> Result<DesignDecision, String> {
        let state = self.state(flow)?;
        if state.end.is_some() {
            return Err("this run has ended".to_owned());
        }
        if self.is_running(flow) {
            return Err("its designer is working now".to_owned());
        }
        if state.phase != Phase::DesignReady
            || !matches!(state.hold, None | Some(Hold::AwaitingApproval))
        {
            return Err(format!(
                "this run is not waiting for a design decision ({:?})",
                state.phase
            ));
        }
        let db = self.db()?;
        let design = db
            .design(flow)
            .map_err(text)?
            .ok_or("this run has no design")?;
        let latest = db
            .design_revisions(flow)
            .map_err(text)?
            .last()
            .cloned()
            .ok_or("there is no revision to approve")?;
        if latest.revision != revision {
            return Err(format!(
                "revision {revision} is not the newest; revision {} replaced it. Look at that \
                 one, then decide.",
                latest.revision
            ));
        }
        if latest.fingerprint != fingerprint {
            return Err("this is not the revision you were looking at; read it again".to_owned());
        }
        let (now, view) = self.design_now(&design, &latest);
        if now.fingerprint.is_none() {
            return Err(format!(
                "revision {revision}'s artifact no longer reads as it was published; ask for a \
                 fresh revision"
            ));
        }
        if !view.changed_sources.is_empty() {
            return Err(format!(
                "the project files revision {revision} was drawn from changed since ({}); ask for \
                 a fresh revision and approve that",
                view.changed_sources.join(", ")
            ));
        }
        let (_, decision) = db
            .approve_design(
                &NewApproval::design(flow, &DesignApproval::of(&latest)),
                latest.id,
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
        self.settle_design_items(flow);
        self.notice.changed(flow);
        // The approval opens the gate in front of Implementing; implementing starts when the
        // project names an implementer (`implement.rs`).
        self.implement_after_approval(flow);
        Ok(decision)
    }

    /// Ask for changes to revision `revision`: the designer is told `note` and the revision's
    /// comments in the same conversation, and publishes the next revision.
    ///
    /// # Errors
    ///
    /// When there is nothing to send, it is not the newest revision, or the run is not waiting for
    /// a decision.
    pub fn request_design_changes(
        self: &Arc<Self>,
        flow: FlowId,
        revision: u32,
        note: &str,
    ) -> Result<DesignDecision, String> {
        let note = note.trim();
        let state = self.state(flow)?;
        if state.end.is_some() || state.phase != Phase::DesignReady {
            return Err("this run is not waiting for a design decision".to_owned());
        }
        if self.is_running(flow) {
            return Err("its designer is working now".to_owned());
        }
        let db = self.db()?;
        let latest = db
            .design_revisions(flow)
            .map_err(text)?
            .last()
            .cloned()
            .ok_or("there is no revision to change")?;
        if latest.revision != revision {
            return Err(format!(
                "revision {revision} is not the newest; ask for changes to revision {}",
                latest.revision
            ));
        }
        let comments = db.design_comments(flow).map_err(text)?;
        let has_comments = comments.iter().any(|c| c.revision == revision);
        if note.is_empty() && !has_comments {
            return Err("say what to change, or comment on the revision first".to_owned());
        }
        let recorded_note = if note.is_empty() {
            format!("See the comments on revision {revision}.")
        } else {
            note.to_owned()
        };
        let feedback = feedback_text(&recorded_note, revision, &comments);
        let decision = db
            .request_design_changes(latest.id, &recorded_note, self.now())
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
                    to: Phase::Designing,
                },
            )?;
        }
        self.settle_design_items(flow);
        self.notice.changed(flow);
        self.spawn_designer(flow, DesignTurn::Changes { revision, feedback })?;
        Ok(decision)
    }

    /// Resolve the run's design-ready item and any staleness notice about a design approval: a
    /// decision answers them.
    fn settle_design_items(&self, flow: FlowId) {
        let Ok(db) = self.db() else { return };
        self.settle(flow, &[InboxKind::Approval]);
        for item in db.inbox(Some(flow), None).unwrap_or_default() {
            if item.kind == InboxKind::Approval
                && matches!(item.target, InboxTarget::Approval { approval }
                    if db.approved(approval).ok().flatten().is_some_and(|a| matches!(a.subject, ApprovalSubject::Design(_))))
            {
                let _ = db.resolve_inbox_item(item.id, self.now());
            }
        }
    }

    // ═══════════════════════════════ holds and recovery ═══════════════════════════════

    /// The app is quitting with a designer turn running, or found at launch with one that has no
    /// process: hold it `Interrupted`.
    pub(super) fn interrupt_designer(&self, flow: FlowId, cause: InterruptCause) {
        let Ok(db) = self.db() else { return };
        let now = self.now();
        let Some(design) = db.design(flow).ok().flatten() else {
            return;
        };
        let steps = db.steps(flow).unwrap_or_default();
        let Some(step) = steps.iter().find(|s| s.role == design.role) else {
            return;
        };
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
        if step.status == StepStatus::Running {
            let _ = db.update_step(step.id, &StepChange::status(StepStatus::Interrupted), now);
        }
        {
            let _held = self.holds.lock();
            let Ok(state) = self.state(flow) else { return };
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
                tracing::warn!(%flow, error = %e, "could not record a designer's interruption");
                return;
            }
        }
        self.file(&NewInboxItem {
            flow,
            kind: InboxKind::Interrupted,
            target: InboxTarget::Flow,
            title: match cause {
                InterruptCause::Quit => {
                    format!("{}: wtm quit while it was being designed", design.key)
                }
                _ => format!("{}: wtm stopped while it was being designed", design.key),
            },
            body: "Nothing was sent again, and every revision published is kept. Resume designer \
                   continues the designer's conversation where it was cut off."
                .to_owned(),
        });
        self.notice.changed(flow);
    }

    /// At launch: a designer caught mid-turn with no process. Returns the runs it held.
    pub(super) fn recover_designers(&self) -> Result<Vec<FlowId>, String> {
        let db = self.db()?;
        let mut held = Vec::new();
        for design in db.designs().map_err(text)? {
            if self.is_running(design.flow) {
                continue;
            }
            let Some(flow) = db.flow(design.flow).map_err(text)? else {
                continue;
            };
            // Running, or about to: a crash between the run moving to Designing and its session
            // opening leaves the step pending and no hold, which nothing would ever pick up.
            let running = db.steps(design.flow).unwrap_or_default().iter().any(|s| {
                s.role == design.role
                    && matches!(s.status, StepStatus::Running | StepStatus::Pending)
            });
            if flow.state.end.is_none()
                && flow.state.hold.is_none()
                && flow.state.phase == Phase::Designing
                && running
            {
                self.interrupt_designer(design.flow, InterruptCause::Crash);
                held.push(design.flow);
            }
        }
        Ok(held)
    }
}

/// What a request for changes sends: your note, then the revision's comments.
fn feedback_text(note: &str, revision: u32, comments: &[DesignComment]) -> String {
    use std::fmt::Write as _;
    let mut out = note.trim().to_owned();
    let on: Vec<&DesignComment> = comments.iter().filter(|c| c.revision == revision).collect();
    if !on.is_empty() {
        let _ = write!(out, "\n\nComments on revision {revision}:");
        for comment in on {
            let _ = write!(out, "\n- {}", comment.text.trim());
        }
    }
    out
}

fn no_designer() -> String {
    "this project names no designer: add `designer = \"<role>\"` to its [workflow] in your own \
     config, naming a reviewer role that reads the worktree"
        .to_owned()
}
