//! Start Work: an issue from the sprint queue becomes one Implement run with one worktree, and,
//! when the project says so, moves to its "started" status.
//!
//! # In order
//!
//! 1. **Resolve** (the caller's thread, nothing recorded). Read the work item's bound worktree,
//!    the project's worktrees and the keys they carry, the work item's pull request, and the New
//!    Worktree form's own plan for the issue: the branches its `existing_branch_match` offers and
//!    the new worktree it would make. [`wtm_core::flow::resolve`] decides from those. More than
//!    one candidate comes back as a preview, and nothing is recorded until one is picked.
//! 2. **Record**, under one lock and the store's uniqueness rules: the work item (one per issue),
//!    the Implement flow (one open per work item), the approval and operation for the move, and the
//!    job (one per work item) with the place it makes, before anything is made. A second Start
//!    that arrives meanwhile finds the first's job and focuses its run.
//! 3. **Make** (a thread of its own). An existing worktree is bound as it is. Anything else goes
//!    through `CreatePipeline::execute`, exactly as the New Worktree form's Create does, with the
//!    key in the form field `[workflow.start] issue_field` names, and the setup's terminal kept for
//!    the issue workspace to show.
//! 4. **Move** the issue through the outbox (`moves.rs`): read first, adopted when already there,
//!    stopped when it moved elsewhere.
//!
//! The run then stays in Preparing with its worktree ready, which the issue workspace calls "Ready
//! to plan". Investigating and planning are the planner's, a later item, and nothing here starts
//! them.
//!
//! # A crash in the middle
//!
//! The job is written before `git worktree add`, with the path it makes. At launch a job still
//! creating or setting up belongs to a process that is gone: it is marked interrupted, its run is
//! held `Interrupted(crash)`, and the inbox says so. Resume looks at what is there. A worktree at
//! the job's path gets the pipeline's Retry setup — `run_setup`, which is also how the form
//! adopts a worktree made elsewhere — or is used as it is, your choice; no worktree there is
//! created again from the same plan. Never a second worktree beside the first.
//!
//! # What this never does
//!
//! It never cancels, removes or moves a worktree, and nothing on the board reaches here except
//! a drop on the start column. A backward drag changes the team status only, through
//! [`Engine::move_issue`], and offers to pause the run in the inbox; pausing is a hold on the
//! flow, and the worktree is untouched by that too.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use wtm_core::flow::{
    BranchOffer, Candidate, DropMeaning, Flow, FlowEvent, FlowId, FlowKind, Hold, HoldKind,
    InboxItem, InboxKind, InboxTarget, InterruptCause, IssueRef, MoveCause, MoveIssuePayload,
    MoveRequest, NewInboxItem, NewStartJob, NewWorkItem, NewWorktree, Phase, Place,
    PublicationState, PullRequestHeadRef, ResolutionInput, Resolved, StartJob, StartJobChange,
    StartJobState, StartMove, StayReason, TrackerKind, WorkItem, WorkItemId, WorkKey, WorkLink,
    drop_meaning, resolve, start_move, start_status,
};
use wtm_core::model::{
    CreateOutcome, ExistingBranchBehavior, FieldKind, PlanPreview, PreflightSeverity, Project,
    ProjectId,
};
use wtm_core::ports::exec::CancelToken;
use wtm_core::ports::progress::{NullProgress, ProgressEvent, ProgressSink};
use wtm_core::ports::{
    FlowStore, FlowStoreError, InboxStore, OutboxStore, PullReviewStore, StartStore, TrackerMirror,
};

use super::{Engine, Refusal, text};

/// Rows and columns for a setup's terminal before a window has sized it, as Home's jobs use.
const SETUP_ROWS: u16 = 24;
const SETUP_COLS: u16 = 100;

/// Start Work's own state in the engine.
#[derive(Debug, Default)]
pub(super) struct Starts {
    /// Held across recording a start, so two Starts of one issue record one between them. Not the
    /// review engine's `starting` lock: resolving reads the form's lookups, which may ask a
    /// tracker, and a review should not wait on that.
    recording: Mutex<()>,
    /// Work items whose job a thread of this process is working on now.
    busy: Mutex<BTreeSet<WorkItemId>>,
}

/// What the Start button, ⌘↵ and a drop on the start column ask for.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartWork {
    pub project: String,
    pub key: String,
    /// The candidate picked from a preview, by its id. `None` the first time.
    #[serde(default)]
    pub choice: Option<String>,
    /// A card dropped on the start column, which moves the issue from wherever it is.
    #[serde(default)]
    pub by_drop: bool,
    /// You confirmed moving it out of review.
    #[serde(default)]
    pub confirmed: bool,
}

/// Where an issue's work would happen, and what starting it would do: what the Start sheet shows.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartPreview {
    pub key: String,
    /// The issue's team status as last read.
    pub status: String,
    pub resolved: Resolved,
    /// Why the form cannot plan a new worktree from the issue alone, when it cannot.
    pub problems: Vec<String>,
    /// What starting would do to the team status.
    pub start_move: StartMove,
    /// The issue's open run, which a Start focuses rather than starting another.
    pub open: Option<FlowId>,
}

/// What a Start did.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StartOutcome {
    /// A run was started, with this place for its worktree.
    #[serde(rename_all = "camelCase")]
    Started { flow: FlowId, candidate: Candidate },
    /// The issue's run was under way already: nothing new, it is the one to show.
    #[serde(rename_all = "camelCase")]
    Focused { flow: FlowId },
    /// More than one place: pick one. Nothing was recorded.
    Choose { preview: StartPreview },
    /// Starting would move the issue out of review, which needs your confirmation.
    Confirm { from: String, to: String },
}

/// What a drop on another column did.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DropOutcome {
    /// The move is queued and being sent.
    Moving { flow: FlowId },
    /// It is in that column already.
    Stayed,
    /// Out of review: confirm first.
    Confirm { from: String, to: String },
    /// The start column: that is a Start, and the window should ask for one.
    Start,
}

/// One move of an issue, as the issue workspace shows it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveView {
    pub approval: wtm_core::flow::ApprovalId,
    pub request: MoveRequest,
    pub operation: wtm_core::flow::Operation,
    pub state: PublicationState,
}

/// Everything the issue workspace shows about an issue's start and run.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueStartView {
    pub work_item: Option<WorkItemId>,
    /// The open Implement run, else the newest one.
    pub flow: Option<Flow>,
    pub job: Option<StartJob>,
    /// A thread of this app is making or setting up its worktree now.
    pub working: bool,
    /// The moves Start and the board asked for, newest last. Approvals whose one move was taken
    /// over by a later approval of the same move are left out.
    pub moves: Vec<MoveView>,
    pub inbox: Vec<InboxItem>,
    /// What a Start would do to the team status now.
    pub start_move: StartMove,
    /// The configured start column, when one is mapped.
    pub start_status: Option<String>,
}

/// Reports a creation's progress into its job: the setup's terminal once it exists.
struct SetupProgress {
    engine: Arc<Engine>,
    item: WorkItemId,
    flow: FlowId,
}

impl ProgressSink for SetupProgress {
    fn emit(&self, event: ProgressEvent) {
        if let ProgressEvent::SessionStarted { session } = event {
            let mut change = StartJobChange::to(StartJobState::SettingUp);
            change.setup_session = wtm_core::flow::Field::Set(session);
            self.engine.change_job(self.item, &change);
            self.engine.notice.changed(self.flow);
        }
    }
}

/// The issue as the mirror last read it: its status and summary.
struct Known {
    status: String,
    summary: String,
}

/// The form field Start Work fills with the issue's key: the one `[workflow.start]` names, else the
/// form's first required text field.
fn issue_field(project: &Project) -> Result<String, String> {
    let named = project
        .workflow
        .as_ref()
        .and_then(|w| w.start.issue_field.clone());
    if let Some(named) = named {
        return if project.fields.iter().any(|f| f.key == named) {
            Ok(named)
        } else {
            Err(format!(
                "[workflow.start] issue_field names `{named}`, and the New Worktree form has no \
                 such field"
            ))
        };
    }
    project
        .fields
        .iter()
        .find(|f| f.required && f.kind == FieldKind::Text)
        .map(|f| f.key.clone())
        .ok_or_else(|| {
            "the New Worktree form has no required text field for the issue's key; name the one \
             to use in [workflow.start] issue_field"
                .to_owned()
        })
}

/// Why a plan cannot be used as it stands, in the form's own words.
fn blockers(preview: &PlanPreview) -> Vec<String> {
    preview
        .preflight
        .iter()
        .filter(|item| item.severity == PreflightSeverity::Error)
        .map(|item| item.message.clone())
        .collect()
}

fn same_path(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    // git reports a worktree by the path it resolved, which on macOS can be /private/var for the
    // /var the form planned. Both are asked of the filesystem before they are called different.
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

impl Engine {
    // ═══════════════════════════════ reading ═══════════════════════════════

    fn workflow_of(project: &Project) -> wtm_core::flow::WorkflowSpec {
        project.workflow.clone().unwrap_or_default()
    }

    /// The issue's status and summary as the mirror last read them.
    fn known(&self, project: &Project, tracker: TrackerKind, key: &str) -> Result<Known, String> {
        let db = self.db()?;
        let queued = db
            .mirrored_queue(&ProjectId::from_root(&project.root))
            .map_err(text)?
            .and_then(|m| m.reading)
            .and_then(|r| r.issues.into_iter().find(|q| q.issue.key == key))
            .map(|q| q.issue);
        let issue = match queued {
            Some(issue) => Some(issue),
            None => db
                .mirrored_issue(tracker, key)
                .map_err(text)?
                .map(|(detail, _)| detail.issue),
        };
        issue
            .map(|issue| Known {
                status: issue.status.name,
                summary: issue.summary,
            })
            .ok_or_else(|| {
                format!(
                    "wtm has not read {key} from the tracker yet; sync the queue, then start it"
                )
            })
    }

    fn work_item_of(&self, tracker: TrackerKind, key: &str) -> Result<Option<WorkItem>, String> {
        self.db()?
            .find_work_item(&WorkKey::Issue(IssueRef {
                tracker: tracker.as_str().to_owned(),
                key: key.to_owned(),
            }))
            .map_err(text)
    }

    fn open_implement(&self, item: WorkItemId) -> Result<Option<Flow>, String> {
        Ok(self
            .db()?
            .open_flows(item)
            .map_err(text)?
            .into_iter()
            .find(|f| f.state.kind == FlowKind::Implement))
    }

    /// The form's plan for the issue, adopting `adopt` when given. Runs the form's lookups, as its
    /// own preview does; changes nothing.
    fn plan_for(
        &self,
        project: &Project,
        key: &str,
        adopt: Option<&str>,
    ) -> Result<(wtm_core::usecase::CreateRequest, PlanPreview), Vec<String>> {
        let field = issue_field(project).map_err(|e| vec![e])?;
        let values = BTreeMap::from([(field, key.to_owned())]);
        let req = crate::commands::create_request(
            &self.app,
            project.id.as_str(),
            &values,
            adopt.map(str::to_owned),
            Vec::new(),
            SETUP_ROWS,
            SETUP_COLS,
        )
        .map_err(|e| vec![e.message])?;
        match self
            .app
            .create_pipeline()
            .preview(&req, &NullProgress, &CancelToken::new())
        {
            Ok(preview) => Ok((req, preview)),
            Err(wtm_core::error::WtmError::Validation(problems)) => Err(problems
                .into_iter()
                .map(|p| format!("{}: {}", p.field, p.message))
                .collect()),
            Err(other) => Err(vec![other.to_string()]),
        }
    }

    /// Everything resolution reads, then its answer. Changes nothing.
    fn resolve_issue(
        &self,
        project: &Project,
        key: &str,
        item: Option<&WorkItem>,
    ) -> Result<(Resolved, Vec<String>), String> {
        let db = self.db()?;
        let bound = match item {
            Some(item) => db
                .links(item.id)
                .map_err(text)?
                .into_iter()
                .rev()
                .find_map(|l| match l.link {
                    WorkLink::Worktree { path } => Some(path),
                    _ => None,
                }),
            None => None,
        };
        let worktrees: Vec<Place> = self
            .app
            .git
            .list_worktrees(&project.root)
            .map_err(text)?
            .into_iter()
            .filter(|w| !w.is_bare && w.prunable.is_none())
            .map(|w| Place {
                id: w.id.as_str().to_owned(),
                branch: w.branch().map(|b| b.as_str().to_owned()),
                key: crate::view::extract_issue_key(&w),
            })
            .collect();
        let pull_request = match item.and_then(|i| i.pull_request.as_ref().map(|pr| (i.id, pr))) {
            Some((id, pr)) => db
                .mirrored_pr(id)
                .map_err(text)?
                .map(|m| PullRequestHeadRef {
                    number: pr.number,
                    branch: m.pull_request.head.branch,
                }),
            None => None,
        };
        let (offers, new, problems) = match self.plan_for(project, key, None) {
            Ok((_, preview)) => {
                let offers = preview
                    .branch_choices
                    .iter()
                    .map(|c| BranchOffer {
                        branch: c.branch.as_str().to_owned(),
                        remote_only: c.remote_only,
                        directory: c.directory.to_string_lossy().into_owned(),
                    })
                    .collect();
                let problems = blockers(&preview);
                let new = problems.is_empty().then(|| NewWorktree {
                    branch: preview
                        .plan
                        .branch_plan
                        .branch()
                        .map(|b| b.as_str().to_owned()),
                    directory: preview.plan.directory.to_string_lossy().into_owned(),
                    base: preview.plan.base_ref.clone(),
                });
                (offers, new, problems)
            }
            Err(problems) => (Vec::new(), None, problems),
        };
        let prefer_offers = {
            let matchers: Vec<_> = project
                .create
                .existing_branch_match
                .iter()
                .filter(|m| m.behavior != ExistingBranchBehavior::Ignore)
                .collect();
            !matchers.is_empty()
                && matchers
                    .iter()
                    .all(|m| m.behavior == ExistingBranchBehavior::Prefer)
        };
        let input = ResolutionInput {
            key: key.to_owned(),
            bound,
            worktrees,
            pull_request,
            offers,
            prefer_offers,
            new,
        };
        Ok((resolve(&input), problems))
    }

    /// What a Start of `key` would find, and do. Changes nothing.
    ///
    /// # Errors
    ///
    /// When the project has no tracker or the Work view is off, the issue has never been read, or
    /// the repository cannot be listed.
    pub fn preview_start(
        &self,
        project_id: &str,
        key: &str,
        by_drop: bool,
    ) -> Result<StartPreview, String> {
        let (project, tracker) =
            crate::tracker::tracker_project(&self.app, project_id).map_err(|e| e.message)?;
        let known = self.known(&project, tracker, key)?;
        let item = self.work_item_of(tracker, key)?;
        let open = match &item {
            Some(item) => self.open_implement(item.id)?.map(|f| f.id),
            None => None,
        };
        let (resolved, problems) = self.resolve_issue(&project, key, item.as_ref())?;
        let workflow = Self::workflow_of(&project);
        Ok(StartPreview {
            key: key.to_owned(),
            start_move: start_move(
                &workflow.statuses,
                workflow.start.move_issue,
                &known.status,
                by_drop,
            ),
            status: known.status,
            resolved,
            problems,
            open,
        })
    }

    // ═══════════════════════════════ starting ═══════════════════════════════

    /// Start work on an issue: see the module docs for what that records and makes.
    ///
    /// # Errors
    ///
    /// Why nothing was started, in a sentence. Nothing was recorded.
    pub fn start_work(self: &Arc<Self>, req: &StartWork) -> Result<StartOutcome, Refusal> {
        let (project, tracker) =
            crate::tracker::tracker_project(&self.app, &req.project).map_err(|e| e.message)?;
        let workflow = Self::workflow_of(&project);
        let known = self.known(&project, tracker, &req.key)?;
        let wanted = start_move(
            &workflow.statuses,
            workflow.start.move_issue,
            &known.status,
            req.by_drop,
        );

        // A run under way is the answer whatever else is true, and costs no resolution.
        if let Some(item) = self.work_item_of(tracker, &req.key)?
            && let Some(flow) = self.open_implement(item.id)?
            && self.db()?.start_job_of(flow.id).map_err(text)?.is_some()
        {
            return self.focus(&project, tracker, &req.key, &flow, &wanted, req);
        }

        let item = self.work_item_of(tracker, &req.key)?;
        let (resolved, problems) = self.resolve_issue(&project, &req.key, item.as_ref())?;
        let candidate = match (&resolved, req.choice.as_deref()) {
            (_, Some(id)) => resolved.find(id).cloned().ok_or_else(|| {
                Refusal::from(
                    "what you picked is no longer one of the places this issue's work could \
                     happen; look again",
                )
            })?,
            (Resolved::One { candidate }, None) => candidate.clone(),
            (Resolved::Choose { .. }, None) => {
                let open = item
                    .as_ref()
                    .map(|i| self.open_implement(i.id))
                    .transpose()?
                    .flatten()
                    .map(|f| f.id);
                return Ok(StartOutcome::Choose {
                    preview: StartPreview {
                        key: req.key.clone(),
                        status: known.status,
                        resolved,
                        problems,
                        start_move: wanted,
                        open,
                    },
                });
            }
            (Resolved::Nothing, None) => {
                return Err(format!(
                    "no worktree carries {} and the New Worktree form cannot plan one from the \
                     issue alone: {}",
                    req.key,
                    problems.join("; ")
                )
                .into());
            }
        };
        if let StartMove::Move {
            from,
            to,
            leaving_review: true,
        } = &wanted
            && !req.confirmed
        {
            return Ok(StartOutcome::Confirm {
                from: from.clone(),
                to: to.clone(),
            });
        }

        // What making it would run, planned before anything is recorded, so a plan the form
        // refuses leaves nothing behind.
        let creation = if candidate.worktree().is_some() {
            None
        } else {
            let one_at_a_time = project
                .setup
                .as_ref()
                .is_some_and(|s| s.concurrency == wtm_core::model::Concurrency::OneGlobally);
            if one_at_a_time && self.app.creating_in(project.id.as_str()) > 0 {
                return Err(format!(
                    "another worktree is being created in {}, whose setup allows one at a time; \
                     start this when it has finished",
                    project.display_name()
                )
                .into());
            }
            let (req, preview) = self
                .plan_for(&project, &req.key, candidate.adopts())
                .map_err(|p| Refusal::from(p.join("; ")))?;
            let refused = blockers(&preview);
            if !refused.is_empty() {
                return Err(format!(
                    "the New Worktree form would not make it: {}",
                    refused.join("; ")
                )
                .into());
            }
            Some((req, preview))
        };

        let recorded = {
            let _one = self.starts.recording.lock();
            self.record_start(
                &project,
                tracker,
                &req.key,
                &known,
                &candidate,
                creation.as_ref(),
            )?
        };
        let (item, flow) = match recorded {
            Recorded::Started { item, flow } => (item, flow),
            Recorded::Focused { flow } => {
                let flow = self
                    .db()?
                    .flow(flow)
                    .map_err(text)?
                    .ok_or_else(|| format!("run {flow} has gone"))?;
                return self.focus(&project, tracker, &req.key, &flow, &wanted, req);
            }
        };

        if let StartMove::Move { from, to, .. } = wanted
            && let Err(error) = self.queue_move(
                flow,
                &MoveRequest {
                    payload: MoveIssuePayload {
                        tracker,
                        key: req.key.clone(),
                        from,
                        to,
                    },
                    cause: MoveCause::Start,
                },
            )
        {
            // The run is started either way; the move's absence is said where it would show.
            tracing::warn!(%flow, %error, "could not queue Start's move");
        }
        self.notice.changed(flow);
        if let Some((create, _)) = creation {
            self.spawn_create(item, flow, create);
        }
        Ok(StartOutcome::Started { flow, candidate })
    }

    /// A Start of an issue whose run is under way: show that run, and do what a drop on the start
    /// column asks of the status, which a repeat of the button does not.
    fn focus(
        self: &Arc<Self>,
        _project: &Project,
        tracker: TrackerKind,
        key: &str,
        flow: &Flow,
        wanted: &StartMove,
        req: &StartWork,
    ) -> Result<StartOutcome, Refusal> {
        if req.by_drop
            && let StartMove::Move {
                from,
                to,
                leaving_review,
            } = wanted
        {
            if *leaving_review && !req.confirmed {
                return Ok(StartOutcome::Confirm {
                    from: from.clone(),
                    to: to.clone(),
                });
            }
            self.queue_move(
                flow.id,
                &MoveRequest {
                    payload: MoveIssuePayload {
                        tracker,
                        key: key.to_owned(),
                        from: from.clone(),
                        to: to.clone(),
                    },
                    cause: MoveCause::Drop,
                },
            )?;
        }
        Ok(StartOutcome::Focused { flow: flow.id })
    }

    /// Record the work item, the flow, the job and the link, or find the run another Start made.
    fn record_start(
        &self,
        project: &Project,
        tracker: TrackerKind,
        key: &str,
        known: &Known,
        candidate: &Candidate,
        creation: Option<&(wtm_core::usecase::CreateRequest, PlanPreview)>,
    ) -> Result<Recorded, Refusal> {
        let db = self.db()?;
        let now = self.now();
        let project_id = ProjectId::from_root(&project.root);
        let issue = IssueRef {
            tracker: tracker.as_str().to_owned(),
            key: key.to_owned(),
        };
        let item = match db.create_work_item(
            &NewWorkItem {
                project: project_id.clone(),
                title: known.summary.clone(),
                issue: Some(issue),
                pull_request: None,
            },
            now,
        ) {
            Ok(item) => item.id,
            Err(FlowStoreError::WorkItemExists { existing }) => existing,
            Err(e) => return Err(text(e).into()),
        };
        let flow = match db.start_flow(item, FlowKind::Implement, now) {
            Ok(flow) => flow.id,
            // Open already. With a job, another Start won; without one, the Start that made it
            // stopped between the two, and this one carries on with its run.
            Err(FlowStoreError::FlowAlreadyOpen { existing }) => {
                if db.start_job_of(existing).map_err(text)?.is_some() {
                    return Ok(Recorded::Focused { flow: existing });
                }
                existing
            }
            Err(e) => return Err(text(e).into()),
        };
        let (worktree, branch, state) = match (candidate.worktree(), creation) {
            (Some(place), _) => (place.id.clone(), place.branch.clone(), StartJobState::Ready),
            (None, Some((_, preview))) => (
                preview.plan.directory.to_string_lossy().into_owned(),
                preview
                    .plan
                    .branch_plan
                    .branch()
                    .map(|b| b.as_str().to_owned()),
                StartJobState::Creating,
            ),
            (None, None) => return Err("there is no plan for making its worktree".into()),
        };
        match db.record_start_job(
            &NewStartJob {
                work_item: item,
                flow,
                project: project_id,
                key: key.to_owned(),
                candidate: candidate.clone(),
                worktree: worktree.clone(),
                branch: branch.clone(),
                state,
            },
            now,
        ) {
            Ok(_) => {}
            Err(FlowStoreError::StartUnderway { flow, .. }) => {
                return Ok(Recorded::Focused { flow });
            }
            Err(e) => return Err(text(e).into()),
        }
        // Bound now, not once it exists: the choice is made, and the next resolution of this
        // issue starts from it.
        db.link(item, &WorkLink::Worktree { path: worktree }, now)
            .map_err(text)?;
        if let Some(branch) = branch {
            db.link(item, &WorkLink::Branch { name: branch }, now)
                .map_err(text)?;
        }
        self.record(
            flow,
            &FlowEvent::Advanced {
                to: Phase::Preparing,
            },
        )?;
        Ok(Recorded::Started { item, flow })
    }

    // ═══════════════════════════════ making the worktree ═══════════════════════════════

    pub(super) fn change_job(&self, item: WorkItemId, change: &StartJobChange) -> Option<StartJob> {
        let db = self.db().ok()?;
        match db.change_start_job(item, change, self.now()) {
            Ok(job) => Some(job),
            Err(error) => {
                tracing::warn!(%item, %error, "could not record a start job's change");
                None
            }
        }
    }

    /// Whether a thread of this process is making `item`'s worktree. Claims it when not.
    fn claim_job(&self, item: WorkItemId) -> bool {
        self.starts.busy.lock().insert(item)
    }

    fn release_job(&self, item: WorkItemId) {
        self.starts.busy.lock().remove(&item);
    }

    fn spawn_create(
        self: &Arc<Self>,
        item: WorkItemId,
        flow: FlowId,
        req: wtm_core::usecase::CreateRequest,
    ) {
        if !self.claim_job(item) {
            return;
        }
        let engine = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name(format!("start-{item}"))
            .spawn(move || {
                engine.create(item, flow, &req);
                engine.release_job(item);
                engine.notice.changed(flow);
            });
        if let Err(error) = spawned {
            self.release_job(item);
            self.job_failed(
                item,
                flow,
                &format!("could not start making the worktree: {error}"),
            );
        }
    }

    /// `git worktree add` and the project's setup, as the New Worktree form's Create runs them.
    fn create(
        self: &Arc<Self>,
        item: WorkItemId,
        flow: FlowId,
        req: &wtm_core::usecase::CreateRequest,
    ) {
        let progress = SetupProgress {
            engine: Arc::clone(self),
            item,
            flow,
        };
        let _creating = self.app.start_creating(req.project.id.as_str());
        let result = self.app.create_pipeline().execute(
            req,
            &progress,
            self.host.setup_sink(),
            &CancelToken::new(),
        );
        self.app.reap_hosts();
        if self.quitting() {
            // `shutdown` has written what the record should say.
            return;
        }
        match result {
            Ok(CreateOutcome::Created { worktree, .. }) => {
                let path = worktree.id.as_str().to_owned();
                self.settle_job(item, flow, &path, StartJobChange::to(StartJobState::Ready));
            }
            Ok(CreateOutcome::SetupFailed {
                worktree, outcome, ..
            }) => {
                let path = worktree.id.as_str().to_owned();
                self.settle_job(
                    item,
                    flow,
                    &path,
                    StartJobChange::to(StartJobState::SetupFailed).detail(outcome.describe()),
                );
            }
            Ok(CreateOutcome::Cancelled { worktree, .. }) => match worktree {
                Some(worktree) => {
                    let path = worktree.id.as_str().to_owned();
                    self.settle_job(
                        item,
                        flow,
                        &path,
                        StartJobChange::to(StartJobState::SetupFailed)
                            .detail("its setup was cancelled"),
                    );
                }
                None => self.job_failed(item, flow, "making the worktree was cancelled"),
            },
            Err(error) => self.job_failed(item, flow, &error.to_string()),
        }
    }

    /// The worktree exists: record where git put it, and what its setup did.
    fn settle_job(&self, item: WorkItemId, flow: FlowId, path: &str, mut change: StartJobChange) {
        change.worktree = Some(path.to_owned());
        let Some(job) = self.change_job(item, &change) else {
            return;
        };
        if let Ok(db) = self.db()
            && !db
                .links(item)
                .unwrap_or_default()
                .iter()
                .any(|l| matches!(&l.link, WorkLink::Worktree { path: linked } if linked == path))
        {
            let _ = db.link(
                item,
                &WorkLink::Worktree {
                    path: path.to_owned(),
                },
                self.now(),
            );
        }
        match job.state {
            StartJobState::Ready => {
                self.settle(flow, &[InboxKind::Failure, InboxKind::Interrupted]);
            }
            StartJobState::SetupFailed => self.file(&NewInboxItem {
                flow,
                kind: InboxKind::Failure,
                target: InboxTarget::Flow,
                title: format!("{}: its worktree's setup failed", job.key),
                body: format!(
                    "The worktree is at {} and was kept, because setup may have made things in \
                     it. {} Retry setup runs it again; Use as it is carries on without it.",
                    job.worktree,
                    job.detail
                        .as_deref()
                        .map_or_else(String::new, |d| format!("Setup {d}."))
                ),
            }),
            _ => {}
        }
        self.notice.changed(flow);
    }

    /// Nothing was made. The run ends failed: there is nothing to resume, and Start again plans
    /// afresh.
    fn job_failed(&self, item: WorkItemId, flow: FlowId, reason: &str) {
        let job = self.change_job(
            item,
            &StartJobChange::to(StartJobState::Failed).detail(reason),
        );
        let key = job.map_or_else(|| "The issue".to_owned(), |j| j.key);
        if let Err(error) = self.record(
            flow,
            &FlowEvent::Failed {
                reason: reason.to_owned(),
            },
        ) {
            tracing::warn!(%flow, %error, "could not record a failed start");
        }
        self.file(&NewInboxItem {
            flow,
            kind: InboxKind::Failure,
            target: InboxTarget::Flow,
            title: format!("{key}: its worktree could not be made"),
            body: format!("{reason}. Nothing was created; Start plans it again."),
        });
        self.notice.changed(flow);
    }

    // ═══════════════════════════════ resuming ═══════════════════════════════

    /// Whether `flow` is an issue's run that Start Work prepares.
    pub(super) fn is_start(&self, flow: FlowId) -> bool {
        self.db()
            .ok()
            .and_then(|db| db.start_job_of(flow).ok().flatten())
            .is_some()
    }

    /// Carry on with an issue's start after a crash or a failed setup: Retry setup on the worktree
    /// at the job's path, or use it as it is (`as_is`), or make it again when it is not there.
    ///
    /// # Errors
    ///
    /// When the run has ended, is being made now, or is held for something a resume does not
    /// answer.
    pub fn resume_start(self: &Arc<Self>, flow: FlowId, as_is: bool) -> Result<(), Refusal> {
        let db = self.db()?;
        let job = db
            .start_job_of(flow)
            .map_err(text)?
            .ok_or_else(|| format!("run {flow} is not an issue's start"))?;
        let state = self.state(flow)?;
        if let Some(end) = &state.end {
            return Err(format!("this run has already ended ({end:?}); Start it again").into());
        }
        if self.starts.busy.lock().contains(&job.work_item) {
            return Err("its worktree is being made right now".into());
        }
        if !matches!(
            job.state,
            StartJobState::Interrupted | StartJobState::SetupFailed
        ) {
            return Err(format!(
                "its worktree is {:?}; there is nothing to resume",
                job.state
            )
            .into());
        }
        {
            let _held = self.holds.lock();
            match state.hold.as_ref().map(Hold::kind) {
                Some(HoldKind::Interrupted) => {
                    self.record(
                        flow,
                        &FlowEvent::Released {
                            hold: HoldKind::Interrupted,
                        },
                    )?;
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
        self.settle(flow, &[InboxKind::Interrupted, InboxKind::Failure]);
        let project = self.app.project(job.project.as_str()).map_err(text)?;
        let existing = self
            .app
            .git
            .list_worktrees(&project.root)
            .map_err(text)?
            .into_iter()
            .find(|w| same_path(w.id.as_str(), &job.worktree));
        match existing {
            Some(worktree) if as_is || project.setup.is_none() => {
                let path = worktree.id.as_str().to_owned();
                self.settle_job(
                    job.work_item,
                    flow,
                    &path,
                    StartJobChange::to(StartJobState::Ready),
                );
                Ok(())
            }
            Some(worktree) => {
                self.spawn_setup(job.work_item, flow, project, worktree);
                Ok(())
            }
            None if as_is => Err(format!(
                "there is no worktree at {} to use; Resume makes it again",
                job.worktree
            )
            .into()),
            None => {
                // Cut off before `git worktree add` finished: the same plan, made again.
                let (req, preview) = self
                    .plan_for(&project, &job.key, job.candidate.adopts())
                    .map_err(|p| Refusal::from(p.join("; ")))?;
                let refused = blockers(&preview);
                if !refused.is_empty() {
                    return Err(format!(
                        "the New Worktree form would not make it again: {}",
                        refused.join("; ")
                    )
                    .into());
                }
                self.change_job(job.work_item, &StartJobChange::to(StartJobState::Creating));
                self.spawn_create(job.work_item, flow, req);
                Ok(())
            }
        }
    }

    /// Retry setup on the job's worktree: `CreatePipeline::run_setup`, the form's own remedy and
    /// adopt path.
    fn spawn_setup(
        self: &Arc<Self>,
        item: WorkItemId,
        flow: FlowId,
        project: Project,
        worktree: wtm_core::model::Worktree,
    ) {
        if !self.claim_job(item) {
            return;
        }
        self.change_job(item, &StartJobChange::to(StartJobState::SettingUp));
        self.notice.changed(flow);
        let engine = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name(format!("start-setup-{item}"))
            .spawn(move || {
                let progress = SetupProgress {
                    engine: Arc::clone(&engine),
                    item,
                    flow,
                };
                let ambient = crate::commands::ambient_context(&engine.app, &project);
                let result = engine.app.create_pipeline().run_setup(
                    &wtm_core::usecase::SetupRequest {
                        project: &project,
                        worktree: &worktree,
                        ambient: &ambient,
                        extra_args: &[],
                        rows: SETUP_ROWS,
                        cols: SETUP_COLS,
                    },
                    engine.host.setup_sink(),
                    &progress,
                );
                engine.app.reap_hosts();
                if !engine.quitting() {
                    let path = worktree.id.as_str().to_owned();
                    let change = match result {
                        Ok((_, outcome)) if outcome.is_success() => {
                            StartJobChange::to(StartJobState::Ready)
                        }
                        Ok((_, outcome)) => StartJobChange::to(StartJobState::SetupFailed)
                            .detail(outcome.describe()),
                        Err(error) => {
                            StartJobChange::to(StartJobState::SetupFailed).detail(error.to_string())
                        }
                    };
                    engine.settle_job(item, flow, &path, change);
                }
                engine.release_job(item);
                engine.notice.changed(flow);
            });
        if let Err(error) = spawned {
            self.release_job(item);
            tracing::warn!(%item, %error, "could not start a setup");
        }
    }

    /// Stop an issue's run. Its worktree, its branch and any move already sent stay as they are.
    ///
    /// # Errors
    ///
    /// When the run has already ended.
    pub fn cancel_start(&self, flow: FlowId) -> Result<(), String> {
        {
            let _held = self.holds.lock();
            self.record(flow, &FlowEvent::Cancelled)?;
        }
        self.settle(flow, &super::ALL_KINDS);
        self.notice.changed(flow);
        Ok(())
    }

    // ═══════════════════════════════ pausing ═══════════════════════════════

    /// Hold an issue's run `Paused`, as the inbox offers after the issue was dragged backward.
    /// Touches nothing but the run's hold.
    ///
    /// # Errors
    ///
    /// When the run has ended or is held already.
    pub fn pause_run(&self, flow: FlowId) -> Result<(), String> {
        {
            let _held = self.holds.lock();
            self.record(flow, &FlowEvent::Held { hold: Hold::Paused })?;
        }
        self.settle_offer(flow);
        self.notice.changed(flow);
        Ok(())
    }

    /// Release a run you paused.
    ///
    /// # Errors
    ///
    /// When it is not paused.
    pub fn unpause_run(&self, flow: FlowId) -> Result<(), String> {
        {
            let _held = self.holds.lock();
            self.record(
                flow,
                &FlowEvent::Released {
                    hold: HoldKind::Paused,
                },
            )?;
        }
        self.notice.changed(flow);
        Ok(())
    }

    /// Resolve the "Pause the run?" offer, whichever way it was answered.
    fn settle_offer(&self, flow: FlowId) {
        if let Ok(db) = self.db() {
            let _ = db.resolve_inbox(
                flow,
                InboxKind::Approval,
                Some(&InboxTarget::Flow),
                self.now(),
            );
        }
    }

    // ═══════════════════════════════ the board ═══════════════════════════════

    /// A card dropped on another column than the start column: move the issue there, and only
    /// that. Backward, the inbox offers to pause its run; the run and its worktree are untouched.
    ///
    /// An issue with no run in wtm is not moved from here: wtm writes to an issue only for work it
    /// is doing, and that work begins with Start.
    ///
    /// # Errors
    ///
    /// Why nothing was moved.
    pub fn move_issue(
        self: &Arc<Self>,
        project_id: &str,
        key: &str,
        to: &str,
        confirmed: bool,
    ) -> Result<DropOutcome, Refusal> {
        let (project, tracker) =
            crate::tracker::tracker_project(&self.app, project_id).map_err(|e| e.message)?;
        let workflow = Self::workflow_of(&project);
        let known = self.known(&project, tracker, key)?;
        let (backward, leaving_review) = match drop_meaning(&workflow.statuses, &known.status, to) {
            DropMeaning::Stay => return Ok(DropOutcome::Stayed),
            DropMeaning::Start => return Ok(DropOutcome::Start),
            DropMeaning::Move {
                backward,
                leaving_review,
                ..
            } => (backward, leaving_review),
        };
        if workflow.statuses.column_of(to).is_none() {
            return Err(format!("{to} is not one of the board's columns").into());
        }
        if leaving_review && !confirmed {
            return Ok(DropOutcome::Confirm {
                from: known.status,
                to: to.to_owned(),
            });
        }
        let flow = match self.work_item_of(tracker, key)? {
            Some(item) => self.open_implement(item.id)?,
            None => None,
        }
        .ok_or_else(|| {
            Refusal::from(format!(
                "wtm moves {key} only once you have started it here; start it, or move it in \
                 the tracker"
            ))
        })?;
        self.queue_move(
            flow.id,
            &MoveRequest {
                payload: MoveIssuePayload {
                    tracker,
                    key: key.to_owned(),
                    from: known.status.clone(),
                    to: to.to_owned(),
                },
                cause: MoveCause::Drop,
            },
        )?;
        if backward && flow.state.hold.is_none() {
            self.file(&NewInboxItem {
                flow: flow.id,
                kind: InboxKind::Approval,
                target: InboxTarget::Flow,
                title: format!("{key} moved back to {to}. Pause its run?"),
                body: "Moving an issue backward changes only its team status: its run and its \
                       worktree are as they were. Pause the run if the work should stop for now; \
                       resuming carries on where it was."
                    .to_owned(),
            });
        }
        self.notice.changed(flow.id);
        Ok(DropOutcome::Moving { flow: flow.id })
    }

    // ═══════════════════════════════ the issue workspace ═══════════════════════════════

    /// Everything the issue workspace shows about `key`'s start, run and moves.
    ///
    /// # Errors
    ///
    /// When the gates are closed or the database cannot be read.
    pub fn issue_start(&self, project_id: &str, key: &str) -> Result<IssueStartView, String> {
        let (project, tracker) =
            crate::tracker::tracker_project(&self.app, project_id).map_err(|e| e.message)?;
        let workflow = Self::workflow_of(&project);
        let status = self.known(&project, tracker, key).ok().map(|k| k.status);
        let start_move = status.as_deref().map_or(
            StartMove::Stay {
                reason: StayReason::NotToDo,
            },
            |status| start_move(&workflow.statuses, workflow.start.move_issue, status, false),
        );
        let start_status = start_status(&workflow.statuses).map(str::to_owned);
        let Some(item) = self.work_item_of(tracker, key)? else {
            return Ok(IssueStartView {
                work_item: None,
                flow: None,
                job: None,
                working: false,
                moves: Vec::new(),
                inbox: Vec::new(),
                start_move,
                start_status,
            });
        };
        let db = self.db()?;
        let job = db.start_job(item.id).map_err(text)?;
        let flow = match self.open_implement(item.id)? {
            Some(flow) => Some(flow),
            None => match &job {
                Some(job) => db.flow(job.flow).map_err(text)?,
                None => None,
            },
        };
        let (moves, inbox) = match &flow {
            Some(flow) => (
                self.move_views(flow.id)?,
                db.inbox(Some(flow.id), None).map_err(text)?,
            ),
            None => (Vec::new(), Vec::new()),
        };
        Ok(IssueStartView {
            working: self.starts.busy.lock().contains(&item.id),
            work_item: Some(item.id),
            flow,
            job,
            moves,
            inbox,
            start_move,
            start_status,
        })
    }

    pub(super) fn move_views(&self, flow: FlowId) -> Result<Vec<MoveView>, String> {
        Ok(self
            .db()?
            .issue_moves(flow)
            .map_err(text)?
            .into_iter()
            .filter_map(|send| {
                let state = send.state(self.activity(send.approval.id));
                let operation = send.operation()?.clone();
                Some(MoveView {
                    approval: send.approval.id,
                    request: send.request,
                    operation,
                    state,
                })
            })
            .collect())
    }

    // ═══════════════════════════════ quitting and launching ═══════════════════════════════

    /// The app is quitting: a worktree being made or set up is interrupted, honestly, before the
    /// setup's process goes with the app.
    pub(super) fn shutdown_starts(&self) {
        let busy: Vec<WorkItemId> = self.starts.busy.lock().iter().copied().collect();
        for item in busy {
            self.interrupt_start(item, InterruptCause::Quit);
        }
    }

    /// At launch: a job still creating or setting up was cut off by a crash. Returns the runs it
    /// held.
    pub(super) fn recover_starts(&self) -> Result<Vec<FlowId>, String> {
        let db = self.db()?;
        let mut held = Vec::new();
        for job in db.start_jobs(None).map_err(text)? {
            if !job.state.is_busy() || self.starts.busy.lock().contains(&job.work_item) {
                continue;
            }
            let open = db
                .flow(job.flow)
                .map_err(text)?
                .is_some_and(|f| f.state.end.is_none());
            if open {
                self.interrupt_start(job.work_item, InterruptCause::Crash);
                held.push(job.flow);
            }
        }
        Ok(held)
    }

    fn interrupt_start(&self, item: WorkItemId, cause: InterruptCause) {
        let words = match cause {
            InterruptCause::Quit => "wtm quit",
            InterruptCause::Crash | InterruptCause::Sleep => "wtm stopped",
        };
        let doing = match self
            .db()
            .ok()
            .and_then(|db| db.start_job(item).ok().flatten())
        {
            Some(job) if job.state == StartJobState::SettingUp => "set up",
            _ => "made",
        };
        let Some(job) = self.change_job(
            item,
            &StartJobChange::to(StartJobState::Interrupted)
                .detail(format!("{words} while its worktree was being {doing}")),
        ) else {
            return;
        };
        {
            let _held = self.holds.lock();
            if self
                .state(job.flow)
                .is_ok_and(|s| s.end.is_none() && s.hold.is_none())
                && let Err(error) = self.record(
                    job.flow,
                    &FlowEvent::Held {
                        hold: Hold::Interrupted { cause },
                    },
                )
            {
                tracing::warn!(flow = %job.flow, %error, "could not hold an interrupted start");
            }
        }
        self.file(&NewInboxItem {
            flow: job.flow,
            kind: InboxKind::Interrupted,
            target: InboxTarget::Flow,
            title: format!("{}: {words} while making its worktree", job.key),
            body: format!(
                "The worktree was to be at {}. Resume looks there: it runs the project's setup \
                 again in the worktree it finds, or makes the worktree again when there is none. \
                 Nothing else is made.",
                job.worktree
            ),
        });
        self.notice.changed(job.flow);
    }
}

/// What recording a start came to.
enum Recorded {
    Started {
        item: WorkItemId,
        flow: FlowId,
    },
    /// Another Start recorded it first.
    Focused {
        flow: FlowId,
    },
}
