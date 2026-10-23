//! Local fixes of a teammate's pull request: the writer lease, the writer's run, and the app's
//! commits of what it wrote.
//!
//! # In order
//!
//! 1. **Start** (`start_fix`, the window's IPC). You pick findings from a finished review and a
//!    writer role. The scope is the picked findings' files. The review worktree must be clean and
//!    at a commit wtm put there, so the fix's diff is the writer's alone. Then the writer lease is
//!    taken, in the store's transaction: refused while a review reads the worktree or another
//!    writer holds it. Only then is anything recorded: the fix, the writer's step, and the run
//!    moving to Fixing.
//! 2. **Write.** The writer runs as a role does (`run_role`), with its R4 writer profile, in the
//!    review worktree itself. It cannot commit; `flow_checkpoint` asks the app to, and the app
//!    commits only after the scope check ([`Engine::fix_checkpoint`]). A changed file outside the
//!    scope is refused and becomes a question in the inbox; allowing it lets it into the fix.
//! 3. **Ready.** When the writer hands in its handoff, the app commits whatever it left within
//!    scope, measures the diff from the head the review read to the fix's last commit, records its
//!    hash, lets the lease go, and moves the run to `FixReady`. You read the diff before anything
//!    leaves the Mac; where it goes is a route you approve (`routes.rs`).
//!
//! # The lease's life
//!
//! The writer lease is the fix's, not the thread's: it is taken before the writer starts and let
//! go when the fix is ready, fails or is stopped. A quit or a crash mid-fix leaves it held, because
//! the worktree may hold the writer's uncommitted edits and a review reading them would describe
//! something nobody finished; Resume carries on under the same lease. Recovery at launch lets go
//! only what nothing can come back for: a lease whose run has ended or is no longer fixing, and
//! every reviewer lease, since no review thread survives the process (`reconcile_leases`).
//!
//! # A review never runs beside it
//!
//! A review drive holds a reviewer lease on its worktree while it runs (`spawn`), so a fix cannot
//! start under a running review, and a review cannot start, rerun or resume under a writer. The
//! store decides both in the transaction that grants the lease.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use serde::{Deserialize, Serialize};
use wtm_core::flow::RunPart;
use wtm_core::flow::{
    ALLOW_OUTLIERS, ContextLink, FindingId, FindingRecord, Fix, FixChange, FixId, FixScope,
    FixState, FlowEvent, FlowId, FlowKind, Hold, HoldKind, InboxKind, InboxTarget, Lease, LeaseId,
    LeaseKind, LeaseRelease, NewFix, NewInboxItem, NewLease, NewQuestion, NewStep, OutlierQuestion,
    Phase, Question, RoleProfile, RoleProvider, Step, StepChange, StepStatus, Submission,
    WorkflowSpec, diff_hash,
};
use wtm_core::ports::{
    FixStore, FlowStore, FlowStoreError, InboxStore, LeaseStore, PullReviewStore, RunStore,
};
use wtm_core::usecase::snapshot::Snapshots;
use wtm_git::push::{self, DiffStyle};

use super::{Engine, Refusal, Run, RunCtx, StageCheckout, text};
use crate::app::FlowRole;
use crate::reviews;
use crate::view::ErrorView;

/// A writer role your config has, as the outcome sheet offers it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WriterChoice {
    pub id: String,
    pub provider: RoleProvider,
    pub model: Option<String>,
    pub objective: String,
}

/// The writer roles in `workflow`, by id.
#[must_use]
pub fn writer_choices(workflow: Option<&WorkflowSpec>) -> Vec<WriterChoice> {
    workflow
        .into_iter()
        .flat_map(|workflow| &workflow.role)
        .filter(|(_, spec)| spec.profile == RoleProfile::Writer)
        .map(|(id, spec)| WriterChoice {
            id: id.clone(),
            provider: spec.provider,
            model: spec.model.clone(),
            objective: spec.objective.trim().to_owned(),
        })
        .collect()
}

/// What Fix it locally asks for.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartFix {
    pub findings: Vec<FindingId>,
    pub role: String,
}

/// What a fix's writer is held to while it works, as its role thread and its run tools see it.
#[derive(Debug)]
pub struct FixLive {
    pub id: FixId,
    pub scope: FixScope,
    /// The findings it was asked to fix.
    pub findings: Vec<FindingRecord>,
    /// The review worktree it writes in.
    pub worktree: PathBuf,
    /// The pull request's number, for commit messages and items.
    pub number: Option<u64>,
}

/// One file of a fix's diff.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FixFile {
    pub path: String,
    /// `None` for a binary file.
    pub added: Option<u32>,
    pub removed: Option<u32>,
}

/// A fix's diff, for you to read before choosing where it goes.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FixDiffView {
    pub fix: Fix,
    /// The head the review read, which the fix is measured from.
    pub base: String,
    /// The fix's last commit.
    pub head: String,
    /// What a route's approval pins: the hash of `git diff --binary base head`.
    pub hash: String,
    pub files: Vec<FixFile>,
    /// The diff to read, with context; cut at [`PATCH_LIMIT`] bytes.
    pub patch: String,
    pub truncated: bool,
    /// Files changed in the worktree and not committed: what a stopped writer left, or files
    /// outside the scope waiting on your answer.
    pub uncommitted: Vec<String>,
    /// Of those, the ones outside the fix's files and not allowed in.
    pub outliers: Vec<String>,
}

/// How much of a fix's diff the window is sent to read. The hash covers all of it.
pub const PATCH_LIMIT: usize = 400_000;

fn refused(message: impl Into<String>) -> ErrorView {
    ErrorView::new("flow", message)
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

/// The refusal for a worktree someone else holds, naming them.
pub(super) fn leased(lease: &Lease) -> String {
    match lease.kind {
        LeaseKind::Writer => format!(
            "A local fix is being written in this worktree by `{}` (run {}). It has the worktree \
             to itself until it is done or stopped.",
            lease.holder, lease.flow
        ),
        LeaseKind::Reviewer => format!(
            "A review (run {}) is reading this worktree, and a writer never edits a tree a review \
             is reading. Fix it once that review has finished.",
            lease.flow
        ),
    }
}

impl Engine {
    // ═══════════════════════════════ leases ═══════════════════════════════

    /// The reviewer lease a review drive takes for as long as it runs. `None` for a flow with no
    /// review record, which reads no worktree.
    pub(super) fn take_reviewer_lease(&self, flow: FlowId) -> Result<Option<LeaseId>, String> {
        let db = self.db()?;
        let Some(review) = db.review(flow).map_err(text)? else {
            return Ok(None);
        };
        match db.acquire_lease(
            &NewLease {
                worktree: review.worktree_path,
                flow,
                kind: LeaseKind::Reviewer,
                holder: review.recipe,
            },
            self.now(),
        ) {
            Ok(lease) => Ok(Some(lease.id)),
            Err(FlowStoreError::Leased { lease }) => Err(self.who_holds(&lease)),
            Err(e) => Err(text(e)),
        }
    }

    /// The refusal for a worktree `lease` holds, naming who and for what: an issue being
    /// implemented holds its worktree as a local fix does, for longer.
    pub(super) fn who_holds(&self, lease: &Lease) -> String {
        let implementing = lease.kind == LeaseKind::Writer
            && self
                .db()
                .ok()
                .and_then(|db| {
                    use wtm_core::ports::ImplementStore as _;
                    db.implementation(lease.flow).ok().flatten()
                })
                .is_some();
        if implementing {
            format!(
                "`{}` is implementing an issue in this worktree (run {}). It has the worktree to \
                 itself until the issue is ready to test or its run is stopped.",
                lease.holder, lease.flow
            )
        } else {
            leased(lease)
        }
    }

    pub(super) fn release_lease(&self, id: LeaseId, why: LeaseRelease) {
        let Ok(db) = self.db() else { return };
        match db.release_lease(id, why, self.now()) {
            Ok(Some(lease)) => {
                tracing::info!(lease = %id, flow = %lease.flow, kind = ?lease.kind, ?why,
                    "released a worktree lease");
                self.notice.changed(lease.flow);
            }
            Ok(None) => {}
            Err(e) => tracing::warn!(lease = %id, error = %e, "could not release a lease"),
        }
    }

    /// Release every open lease `flow` holds of `kind`.
    pub(super) fn release_leases_of(&self, flow: FlowId, kind: LeaseKind, why: LeaseRelease) {
        let Ok(db) = self.db() else { return };
        for lease in db.open_leases(None).unwrap_or_default() {
            if lease.flow == flow && lease.kind == kind {
                self.release_lease(lease.id, why);
            }
        }
    }

    /// The open writer lease on `worktree_path`, if a fix holds it.
    pub(super) fn writer_on(&self, worktree_path: &str) -> Option<Lease> {
        if !self.app.files.exists(&self.app.config.paths().flow_db) {
            return None;
        }
        self.db()
            .ok()?
            .open_leases(Some(worktree_path))
            .ok()?
            .into_iter()
            .find(|lease| lease.kind == LeaseKind::Writer)
    }

    /// Refuse to start reading `worktree_path` while a writer holds it. The store refuses it again
    /// when the reviewer lease is taken; this says so before anything is recorded.
    pub(super) fn refuse_if_written(&self, worktree_path: &str) -> Result<(), Refusal> {
        match self.writer_on(worktree_path) {
            Some(lease) => Err(self.who_holds(&lease).into()),
            None => Ok(()),
        }
    }

    /// At launch: let go of every lease nothing can come back for. A writer lease stays while its
    /// run is still fixing or implementing, interrupted or not, because the worktree holds its
    /// edits; everything else that is open belonged to a thread of a process that is gone.
    pub(super) fn reconcile_leases(&self) {
        let Ok(db) = self.db() else { return };
        for lease in db.open_leases(None).unwrap_or_default() {
            if self.is_running(lease.flow) {
                continue;
            }
            let fixing = self
                .state(lease.flow)
                .is_ok_and(|state| state.end.is_none() && state.phase == Phase::Fixing);
            if lease.kind == LeaseKind::Writer && (fixing || self.holds_its_worktree(lease.flow)) {
                continue;
            }
            tracing::info!(lease = %lease.id, flow = %lease.flow, kind = ?lease.kind,
                "recovery released a lease nothing can come back for");
            self.release_lease(lease.id, LeaseRelease::Recovered);
        }
    }

    // ═══════════════════════════════ starting ═══════════════════════════════

    /// Start a local fix of `req.findings` by the writer role `req.role`, in the review's worktree.
    ///
    /// # Errors
    ///
    /// When the review is not a finished review of a teammate's pull request, the role is not a
    /// writer, a pick names no file, the worktree is not clean or not at a commit wtm put there,
    /// its provider would not bill a subscription, or the worktree is leased. Nothing was recorded.
    pub fn start_fix(self: &Arc<Self>, flow: FlowId, req: &StartFix) -> Result<FixId, ErrorView> {
        let _one = self.starting.lock();
        if self.is_running(flow) {
            return Err(refused(
                "This review is still running; fix its findings once it has finished.",
            ));
        }
        let db = self.db().map_err(refused)?;
        let state = db
            .flow(flow)
            .map_err(|e| reviews::store_error(&e))?
            .ok_or_else(|| refused(format!("no run {flow}")))?;
        if self.pull_request_item(&state).is_none() {
            return Err(refused(
                "Local fixes are for a review of a teammate's pull request.",
            ));
        }
        if let Some(end) = &state.state.end {
            return Err(refused(format!(
                "This review has ended ({end:?}); run it again to fix anything."
            )));
        }
        if let Some(hold) = &state.state.hold {
            return Err(refused(format!(
                "This review is waiting ({:?}); resume or stop it first.",
                hold.kind()
            )));
        }
        if !matches!(state.state.phase, Phase::Consolidated | Phase::FixReady) {
            return Err(refused("Only a finished review can be fixed."));
        }
        let review = db
            .review(flow)
            .map_err(|e| reviews::store_error(&e))?
            .ok_or_else(|| refused(format!("run {flow} is not a review")))?;
        let snapshot = review
            .snapshot
            .and_then(|id| db.snapshot(id).ok().flatten())
            .ok_or_else(|| refused("This review has not read a head yet."))?;
        let project = self
            .app
            .project(review.project.as_str())
            .map_err(ErrorView::from)?;
        let workflow = self
            .run_workflow(flow, RunPart::Review, &project)
            .map_err(refused)?;
        let spec = workflow.role.get(&req.role).ok_or_else(|| {
            refused(format!(
                "There is no role `{}` in your workflow config.",
                req.role
            ))
        })?;
        if spec.profile != RoleProfile::Writer {
            return Err(refused(format!(
                "`{}` is a reviewer. A fix is written by a role with `profile = \"writer\"`.",
                req.role
            )));
        }
        let findings = db.findings(flow).map_err(|e| reviews::store_error(&e))?;
        let scope = FixScope::of(&findings, &req.findings).map_err(|e| refused(e.0))?;
        let worktree = self
            .app
            .worktree(&project, &review.worktree)
            .map_err(|e| refused(format!("The review worktree is not available: {e}")))?;

        let dirty = wtm_git::checkpoint::changed(self.app.runner.as_ref(), &worktree.path)
            .map_err(ErrorView::from)?;
        if !dirty.is_empty() {
            return Err(refused(format!(
                "The review worktree has changes nobody committed ({}). A fix starts from a clean \
                 tree, so its diff is the writer's alone: commit or discard them first.",
                listed(&dirty)
            )));
        }
        let head = self
            .app
            .git
            .rev_parse(&worktree.path, "HEAD")
            .map_err(ErrorView::from)?
            .map(|c| c.as_str().to_owned())
            .unwrap_or_default();
        let fixes = db.fixes(flow).map_err(|e| reviews::store_error(&e))?;
        let ours = head == snapshot.head_sha
            || fixes
                .iter()
                .any(|fix| fix.head_sha.as_deref() == Some(head.as_str()));
        if !ours {
            return Err(refused(format!(
                "The review worktree is at {}, which is neither the head this review read ({}) \
                 nor a fix wtm made. Re-review the pull request first.",
                short(&head),
                short(&snapshot.head_sha)
            )));
        }
        self.host
            .preflight(&[spec.provider.id()])
            .map_err(|r| refused(r.message))?;

        let now = self.now();
        let lease = db
            .acquire_lease(
                &NewLease {
                    worktree: review.worktree_path.clone(),
                    flow,
                    kind: LeaseKind::Writer,
                    holder: req.role.clone(),
                },
                now,
            )
            .map_err(|e| match e {
                FlowStoreError::Leased { lease } => ErrorView::new("leased", leased(&lease))
                    .with_detail(serde_json::json!({
                        "flow": lease.flow,
                        "kind": lease.kind,
                        "holder": lease.holder,
                    })),
                other => reviews::store_error(&other),
            })?;
        // From here on, anything refused gives the worktree back.
        let started = (|| -> Result<FixId, ErrorView> {
            let step = self.writer_step(flow, &req.role, spec)?;
            let fix = db
                .start_fix(
                    &NewFix {
                        flow,
                        step,
                        role: req.role.clone(),
                        findings: req.findings.clone(),
                        scope,
                        base_sha: snapshot.head_sha.clone(),
                    },
                    now,
                )
                .map_err(|e| reviews::store_error(&e))?;
            db.record(flow, &FlowEvent::Advanced { to: Phase::Fixing }, now)
                .map_err(|e| reviews::store_error(&e))?;
            Ok(fix.id)
        })();
        let id = match started {
            Ok(id) => id,
            Err(error) => {
                self.release_lease(lease.id, LeaseRelease::Failed);
                return Err(error);
            }
        };
        tracing::info!(%flow, fix = %id, role = %req.role, "started a local fix");
        self.notice.changed(flow);
        if let Err(reason) = self.spawn(flow) {
            self.fail_fix(flow, &reason);
            return Err(refused(reason));
        }
        Ok(id)
    }

    /// The writer's step: the role's own, run again, or a new one after the review's stages.
    fn writer_step(
        &self,
        flow: FlowId,
        role: &str,
        spec: &wtm_core::flow::RoleSpec,
    ) -> Result<wtm_core::flow::StepId, ErrorView> {
        let db = self.db().map_err(refused)?;
        let steps = db.steps(flow).map_err(|e| reviews::store_error(&e))?;
        if let Some(existing) = steps.iter().find(|s| s.role == role) {
            if existing.profile != RoleProfile::Writer {
                return Err(refused(format!(
                    "`{role}` already ran in this review as a reviewer."
                )));
            }
            db.update_step(
                existing.id,
                &StepChange {
                    failures: Some(0),
                    detail: wtm_core::flow::Field::Clear,
                    resets_at: wtm_core::flow::Field::Clear,
                    ..StepChange::status(StepStatus::Pending)
                },
                self.now(),
            )
            .map_err(|e| reviews::store_error(&e))?;
            return Ok(existing.id);
        }
        let stage = steps.iter().map(|s| s.stage + 1).max().unwrap_or(0);
        let added = db
            .add_steps(
                flow,
                &[NewStep {
                    stage,
                    role: role.to_owned(),
                    provider: spec.provider,
                    model: spec.model.clone(),
                    profile: RoleProfile::Writer,
                    workspace: wtm_core::flow::Workspace::Worktree,
                    timeout_ms: spec.timeout_ms(),
                    retries: spec.retries,
                    status: StepStatus::Pending,
                    stands_in_for: None,
                    handover: None,
                    detail: None,
                }],
                self.now(),
            )
            .map_err(|e| reviews::store_error(&e))?;
        added
            .first()
            .map(|step| step.id)
            .ok_or_else(|| refused("the writer's step was not recorded"))
    }

    // ═══════════════════════════════ driving ═══════════════════════════════

    /// The writer's run: one role, in the review worktree, held to the fix's files.
    pub(super) fn drive_fix(&self, flow: FlowId, run: &Run) -> Result<(), String> {
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
            .map_err(|e| format!("the review worktree is no longer there: {e}"))?;
        let workflow = self.run_workflow(flow, RunPart::Review, &project)?;
        let fix = db
            .fixes(flow)
            .map_err(text)?
            .into_iter()
            .rev()
            .find(|fix| fix.state == FixState::Writing)
            .ok_or("no fix of this run is being written")?;
        let snapshot_id = review.snapshot.ok_or("this review has no snapshot")?;
        let snapshot = db
            .snapshot(snapshot_id)
            .map_err(text)?
            .ok_or_else(|| format!("snapshot {snapshot_id} is missing from the record"))?;
        let step = db
            .steps(flow)
            .map_err(text)?
            .into_iter()
            .find(|s| s.id == fix.step)
            .ok_or("the writer's step is missing from the record")?;
        let findings: Vec<FindingRecord> = db
            .findings(flow)
            .map_err(text)?
            .into_iter()
            .filter(|record| fix.findings.contains(&record.finding.id))
            .collect();
        let pull_request = db
            .flow(flow)
            .ok()
            .flatten()
            .filter(|state| state.state.kind == FlowKind::ReviewPr)
            .and_then(|state| db.mirrored_pr(state.work_item).ok().flatten())
            .map(|mirror| mirror.pull_request);
        let live = Arc::new(FixLive {
            id: fix.id,
            scope: fix.scope.clone(),
            findings,
            worktree: worktree.path.clone(),
            number: pull_request.as_ref().map(|pr| pr.number),
        });
        let changed = self
            .app
            .git
            .tree_changes(&worktree.path, &snapshot.base_sha, &snapshot.tree_sha)
            .ok()
            .filter(|listed| !listed.truncated)
            .map(|listed| listed.paths);
        let ctx = RunCtx {
            flow,
            project: review.project.as_str().to_owned(),
            project_id: review.project.clone(),
            worktree: review.worktree.clone(),
            worktree_path: worktree.path.clone(),
            branch: worktree.branch().map(|b| b.as_str().to_owned()),
            recipe: review.recipe.clone(),
            base: review.base.clone(),
            concurrency: None,
            patterns: workflow.instruction_sources(),
            pull_request,
            revision: None,
            fix: Some(Arc::clone(&live)),
            dir: wtm_core::flow::FlowDir::new(&self.app.config.paths().flow_dir, flow),
            workflow,
            snapshot,
            snapshot_id,
            changed,
        };
        let snapshots = Snapshots {
            git: Arc::clone(&self.app.git),
            files: Arc::clone(&self.app.files),
        };
        self.notice.changed(flow);
        self.run_role(&ctx, run, &snapshots, &StageCheckout::default(), step);

        if self.quitting() {
            // `shutdown` recorded the interruption; the lease stays with the fix.
            return Ok(());
        }
        let ended = self.state(flow).is_ok_and(|state| state.end.is_some());
        let stopped = run.cancel.load(Ordering::SeqCst)
            || db
                .fix(fix.id)
                .ok()
                .flatten()
                .is_some_and(|f| f.state == FixState::Cancelled);
        if ended || stopped {
            self.end_stopped_fix(flow, fix.id);
            return Ok(());
        }
        let step = db
            .steps(flow)
            .map_err(text)?
            .into_iter()
            .find(|s| s.id == fix.step)
            .ok_or("the writer's step is missing from the record")?;
        match (&step.status, &step.result) {
            (StepStatus::Done, Some(Submission::Handoff(handoff))) => {
                self.finish_fix(flow, &step, &live, Some(handoff.summary.trim()));
            }
            (StepStatus::Done, _) => self.finish_fix(flow, &step, &live, None),
            (StepStatus::WaitingCapacity | StepStatus::Interrupted, _) => {}
            _ => self.fail_fix(
                flow,
                &format!(
                    "`{}` did not finish: {}",
                    step.role,
                    step.detail.as_deref().unwrap_or("it handed in nothing")
                ),
            ),
        }
        Ok(())
    }

    /// The writer handed in: commit what it left within scope, measure the diff, and give the
    /// worktree back.
    fn finish_fix(&self, flow: FlowId, step: &Step, live: &FixLive, summary: Option<&str>) {
        let Ok(db) = self.db() else { return };
        let Ok(Some(fix)) = db.fix(live.id) else {
            return;
        };
        let runner = self.app.runner.as_ref();
        let mut notes: Vec<String> = Vec::new();
        if let Ok(changed) = wtm_git::checkpoint::changed(runner, &live.worktree)
            && !changed.is_empty()
        {
            let outliers = live.scope.outliers(&changed, &fix.allowed);
            if outliers.is_empty() {
                let message = commit_message(live.number, summary);
                match wtm_git::checkpoint::commit(
                    runner,
                    &live.worktree,
                    &message,
                    wtm_agent::authority::protected_path,
                ) {
                    Ok(made) => {
                        tracing::info!(%flow, fix = %fix.id, commit = %made.commit.short(),
                            "committed what the writer left");
                    }
                    Err(e) => notes.push(format!("Its last edits were not committed: {e}.")),
                }
            } else {
                let fresh: Vec<String> = outliers
                    .iter()
                    .filter(|p| !fix.asked_about(p))
                    .cloned()
                    .collect();
                if !fresh.is_empty() {
                    self.ask_outliers(flow, step.id, &step.role, &fix, &fresh);
                }
                notes.push(format!(
                    "{} not committed: outside the files of the findings it was asked to fix. \
                     Answer the question in the inbox to let {} into the fix.",
                    listed(&outliers),
                    if outliers.len() == 1 { "it" } else { "them" }
                ));
            }
        }
        let (head, hash) = match self.measure(&live.worktree, &fix.base_sha) {
            Ok(measured) => measured,
            Err(reason) => {
                self.fail_fix(flow, &reason);
                return;
            }
        };
        if head == fix.base_sha {
            notes.insert(0, "The writer committed nothing.".to_owned());
        }
        let ready = db.change_fix(
            fix.id,
            &FixChange {
                head_sha: wtm_core::flow::Field::Set(head.clone()),
                diff_hash: wtm_core::flow::Field::Set(hash),
                detail: wtm_core::flow::Field::from((!notes.is_empty()).then(|| notes.join(" "))),
                ..FixChange::state(FixState::Ready)
            },
            self.now(),
        );
        if let Err(e) = ready {
            tracing::warn!(%flow, error = %e, "could not record a fix as ready");
        }
        self.release_leases_of(flow, LeaseKind::Writer, LeaseRelease::Completed);
        let _ = self.record(
            flow,
            &FlowEvent::Advanced {
                to: Phase::FixReady,
            },
        );
        let number = live
            .number
            .map_or_else(String::new, |number| format!(" of #{number}"));
        let (title, body) = if head == fix.base_sha {
            (
                format!("The local fix{number} committed nothing"),
                notes.join(" "),
            )
        } else {
            (
                format!("Your local fix{number} is ready: read the diff"),
                format!(
                    "`{}` fixed {} finding{} and the app committed it as {}. Nothing has left \
                     this Mac: read the diff, then choose where it goes.{}",
                    step.role,
                    fix.findings.len(),
                    if fix.findings.len() == 1 { "" } else { "s" },
                    short(&head),
                    if notes.is_empty() {
                        String::new()
                    } else {
                        format!(" {}", notes.join(" "))
                    }
                ),
            )
        };
        self.file(&NewInboxItem {
            flow,
            kind: InboxKind::ReviewReady,
            target: InboxTarget::Step {
                step: step.id,
                role: step.role.clone(),
            },
            title,
            body,
        });
        self.notice.changed(flow);
    }

    /// The fix's head and the hash of its diff from `base`.
    fn measure(&self, worktree: &Path, base: &str) -> Result<(String, String), String> {
        let head = self
            .app
            .git
            .rev_parse(worktree, "HEAD")
            .map_err(text)?
            .map(|c| c.as_str().to_owned())
            .ok_or("the review worktree has no HEAD")?;
        let diff = push::diff(
            self.app.runner.as_ref(),
            worktree,
            base,
            &head,
            DiffStyle::Binary,
        )
        .map_err(text)?;
        Ok((head, diff_hash(&diff)))
    }

    /// The fix could not run or the writer failed for good: say so, and give the worktree back.
    pub(super) fn fail_fix(&self, flow: FlowId, reason: &str) {
        let Ok(db) = self.db() else { return };
        if let Some(fix) = db
            .fixes(flow)
            .unwrap_or_default()
            .into_iter()
            .rev()
            .find(|f| f.state == FixState::Writing)
        {
            let _ = db.change_fix(
                fix.id,
                &FixChange::state(FixState::Failed).detail(reason.to_owned()),
                self.now(),
            );
        }
        self.release_leases_of(flow, LeaseKind::Writer, LeaseRelease::Failed);
        self.leave_fixing(flow);
        self.file(&NewInboxItem {
            flow,
            kind: InboxKind::Failure,
            target: InboxTarget::Flow,
            title: "The local fix did not finish".to_owned(),
            body: format!(
                "{} Whatever it committed is in the review worktree; nothing left this Mac.",
                super::capitalise(reason)
            ),
        });
        self.notice.changed(flow);
    }

    /// Move a run out of Fixing, releasing a hold that keeps it there.
    fn leave_fixing(&self, flow: FlowId) {
        let _held = self.holds.lock();
        let Ok(state) = self.state(flow) else { return };
        if state.end.is_some() || state.phase != Phase::Fixing {
            return;
        }
        if let Some(hold) = &state.hold {
            let _ = self.record(flow, &FlowEvent::Released { hold: hold.kind() });
        }
        let _ = self.record(
            flow,
            &FlowEvent::Advanced {
                to: Phase::FixReady,
            },
        );
    }

    /// Stop the fix being written. Its writer is ended and what it committed stays; what it had
    /// not committed is left in the worktree for you to look at.
    ///
    /// # Errors
    ///
    /// When no fix of the run is being written.
    pub fn cancel_fix(&self, flow: FlowId) -> Result<(), ErrorView> {
        let db = self.db().map_err(refused)?;
        let fix = db
            .fixes(flow)
            .map_err(|e| reviews::store_error(&e))?
            .into_iter()
            .rev()
            .find(|f| f.state == FixState::Writing)
            .ok_or_else(|| refused("No fix of this review is being written."))?;
        db.change_fix(
            fix.id,
            &FixChange::state(FixState::Cancelled).detail("You stopped it."),
            self.now(),
        )
        .map_err(|e| reviews::store_error(&e))?;
        let running = self.runs.lock().get(&flow).cloned();
        match running {
            // The writer's thread ends the attempt and gives the worktree back as it leaves.
            Some(run) => run.cancel.store(true, Ordering::SeqCst),
            None => self.end_stopped_fix(flow, fix.id),
        }
        self.notice.changed(flow);
        Ok(())
    }

    /// A stopped fix's end: the lease let go, the run out of Fixing, its interruption settled.
    fn end_stopped_fix(&self, flow: FlowId, fix: FixId) {
        if let Ok(db) = self.db()
            && let Ok(Some(found)) = db.fix(fix)
            && found.state == FixState::Writing
        {
            let _ = db.change_fix(
                fix,
                &FixChange::state(FixState::Cancelled).detail("It was stopped."),
                self.now(),
            );
        }
        self.release_leases_of(flow, LeaseKind::Writer, LeaseRelease::Cancelled);
        self.leave_fixing(flow);
        self.settle(
            flow,
            &[
                InboxKind::Interrupted,
                InboxKind::Capacity,
                InboxKind::Failure,
            ],
        );
        self.notice.changed(flow);
    }

    /// The review is being stopped: a fix it is writing stops with it.
    pub(super) fn stop_fix_record(&self, flow: FlowId) {
        let Ok(db) = self.db() else { return };
        for fix in db.fixes(flow).unwrap_or_default() {
            if fix.state == FixState::Writing {
                let _ = db.change_fix(
                    fix.id,
                    &FixChange::state(FixState::Cancelled).detail("The review was stopped."),
                    self.now(),
                );
            }
        }
        self.release_leases_of(flow, LeaseKind::Writer, LeaseRelease::Cancelled);
    }

    /// Run the writer again after a quit, a crash or a limit, on top of what it left, under the
    /// fix's own lease — taken again if recovery let it go.
    pub(super) fn resume_fix(self: &Arc<Self>, flow: FlowId) -> Result<(), Refusal> {
        let db = self.db()?;
        let review = db
            .review(flow)
            .map_err(text)?
            .ok_or_else(|| format!("run {flow} is not a review"))?;
        let state = self.state(flow)?;
        let fix = db
            .fixes(flow)
            .map_err(text)?
            .into_iter()
            .rev()
            .find(|f| f.state == FixState::Writing)
            .ok_or("no fix of this review is being written")?;
        let project = self.app.project(review.project.as_str()).map_err(text)?;
        let provider = self
            .run_workflow(flow, RunPart::Review, &project)?
            .role
            .get(&fix.role)
            .map(|spec| spec.provider)
            .ok_or_else(|| {
                format!(
                    "the writer role `{}` is not in your config any more",
                    fix.role
                )
            })?;
        self.host.preflight(&[provider.id()])?;
        if self.writer_on(&review.worktree_path).is_none() {
            db.acquire_lease(
                &NewLease {
                    worktree: review.worktree_path.clone(),
                    flow,
                    kind: LeaseKind::Writer,
                    holder: fix.role.clone(),
                },
                self.now(),
            )
            .map_err(|e| match e {
                FlowStoreError::Leased { lease } => Refusal::from(leased(&lease)),
                other => Refusal::from(text(other)),
            })?;
        }
        {
            let _held = self.holds.lock();
            match state.hold.as_ref().map(Hold::kind) {
                Some(kind @ (HoldKind::Interrupted | HoldKind::WaitingCapacity)) => {
                    self.record(flow, &FlowEvent::Released { hold: kind })?;
                }
                Some(other) => {
                    return Err(format!(
                        "this fix is waiting on {other:?}, which a resume does not answer"
                    )
                    .into());
                }
                None => {}
            }
        }
        let _ = db.update_step(
            fix.step,
            &StepChange {
                status: Some(StepStatus::Pending),
                resets_at: wtm_core::flow::Field::Clear,
                ..StepChange::default()
            },
            self.now(),
        );
        self.settle(
            flow,
            &[
                InboxKind::Interrupted,
                InboxKind::Failure,
                InboxKind::Capacity,
            ],
        );
        self.notice.changed(flow);
        Ok(self.spawn(flow)?)
    }

    // ═══════════════════════════════ the checkpoint ═══════════════════════════════

    /// `flow_checkpoint` from a fix's writer: the scope check, then the app's commit.
    pub(super) fn fix_checkpoint(
        &self,
        role: &FlowRole,
        live: &super::Live,
        fix: &FixLive,
        message: &str,
    ) -> Result<String, String> {
        let db = self.db()?;
        let runner = self.app.runner.as_ref();
        let changed =
            wtm_git::checkpoint::changed(runner, &fix.worktree).map_err(|e| e.to_string())?;
        if changed.is_empty() {
            return Err("Nothing has changed since the last checkpoint.".to_owned());
        }
        let current = db
            .fix(fix.id)
            .map_err(text)?
            .ok_or("this fix is no longer recorded")?;
        let outliers = fix.scope.outliers(&changed, &current.allowed);
        if !outliers.is_empty() {
            let fresh: Vec<String> = outliers
                .iter()
                .filter(|p| !current.asked_about(p))
                .cloned()
                .collect();
            if !fresh.is_empty() {
                self.ask_outliers(role.flow, live.step, &role.role, &current, &fresh);
            }
            let scope: Vec<String> = fix.scope.paths.iter().cloned().collect();
            return Err(format!(
                "Not committed: you changed {} outside the files of the findings you were asked to \
                 fix ({}). The person has been asked whether to let {} into the fix. Until they \
                 answer, put {} back as {} were, and checkpoint again; or leave {} and say why in \
                 your handoff.",
                listed(&outliers),
                listed(&scope),
                if outliers.len() == 1 { "it" } else { "them" },
                if outliers.len() == 1 { "it" } else { "them" },
                if outliers.len() == 1 { "it" } else { "they" },
                if outliers.len() == 1 { "it" } else { "them" },
            ));
        }
        let made = wtm_git::checkpoint::commit(
            runner,
            &fix.worktree,
            message,
            wtm_agent::authority::protected_path,
        )
        .map_err(|e| e.to_string())?;
        let id = made.commit.as_str().to_owned();
        let _ = db.change_fix(
            fix.id,
            &FixChange {
                head_sha: wtm_core::flow::Field::Set(id.clone()),
                ..FixChange::default()
            },
            self.now(),
        );
        self.app.flow_ledger.note_checkpoint(role, &id);
        tracing::info!(flow = %role.flow, fix = %fix.id, commit = %made.commit.short(),
            files = made.paths.len(), "committed a fix checkpoint");
        self.notice.changed(role.flow);
        Ok(format!(
            "Committed {} with {} file{}.",
            short(&id),
            made.paths.len(),
            if made.paths.len() == 1 { "" } else { "s" }
        ))
    }

    /// Ask you about files a writer changed outside its fix's scope.
    fn ask_outliers(
        &self,
        flow: FlowId,
        step: wtm_core::flow::StepId,
        role: &str,
        fix: &Fix,
        paths: &[String],
    ) {
        let Ok(db) = self.db() else { return };
        let scope: Vec<String> = fix.scope.paths.iter().cloned().collect();
        let question = NewQuestion {
            flow,
            step: Some(step),
            role: Some(role.to_owned()),
            text: format!(
                "The writer changed {} outside this fix's files. Let {} into the fix?",
                listed(paths),
                if paths.len() == 1 { "it" } else { "them" }
            ),
            why: format!(
                "A fix is held to the files of the findings you picked ({}), and wtm commits \
                 nothing outside them unless you allow it.",
                listed(&scope)
            ),
            unblocks: "Committing those files with the fix.".to_owned(),
            blocks: None,
            context: paths
                .iter()
                .map(|path| ContextLink::File {
                    path: path.clone(),
                    line: None,
                    note: None,
                })
                .collect(),
            recommendation: None,
            options: vec![
                "Allow them in this fix".to_owned(),
                "Keep the fix to the findings' files".to_owned(),
            ],
        };
        let title = format!("`{role}` changed files outside the fix: {}", listed(paths));
        match db.ask(&question, &title, self.now()) {
            Ok((asked, filed)) => {
                if let Some(filed) = filed {
                    self.notice.filed(&filed);
                }
                let _ = db.change_fix(
                    fix.id,
                    &FixChange {
                        asked: Some(OutlierQuestion {
                            question: asked.id,
                            paths: paths.to_vec(),
                        }),
                        ..FixChange::default()
                    },
                    self.now(),
                );
                self.notice.changed(flow);
            }
            Err(e) => tracing::warn!(%flow, error = %e, "could not ask about files outside a fix"),
        }
    }

    /// An answer to a question: when it lets outliers into a fix, they are allowed, and a fix
    /// whose writer has finished commits them now.
    pub(super) fn on_outlier_answer(&self, question: &Question) {
        let Ok(db) = self.db() else { return };
        let Some((fix, asked)) = db
            .fixes(question.flow)
            .unwrap_or_default()
            .into_iter()
            .rev()
            .find_map(|fix| {
                let asked = fix
                    .asked
                    .iter()
                    .find(|a| a.question == question.id)
                    .cloned()?;
                Some((fix, asked))
            })
        else {
            return;
        };
        let allowed = question.answer.as_ref().and_then(|a| a.choice) == Some(ALLOW_OUTLIERS);
        if !allowed {
            return;
        }
        if let Err(e) = db.change_fix(
            fix.id,
            &FixChange {
                allowed: asked.paths.clone(),
                ..FixChange::default()
            },
            self.now(),
        ) {
            tracing::warn!(flow = %question.flow, error = %e, "could not allow files into a fix");
            return;
        }
        if fix.state == FixState::Ready
            && !self.is_running(question.flow)
            && let Err(reason) = self.commit_rest(question.flow, fix.id)
        {
            tracing::warn!(flow = %question.flow, %reason, "could not commit allowed files");
        }
    }

    /// Commit what a finished fix left that is now within scope, and measure the fix again. Under a
    /// writer lease of its own for the moment it takes, since it changes the worktree.
    fn commit_rest(&self, flow: FlowId, fix: FixId) -> Result<Fix, String> {
        let db = self.db()?;
        let current = db.fix(fix).map_err(text)?.ok_or("the fix has gone")?;
        let review = db
            .review(flow)
            .map_err(text)?
            .ok_or_else(|| format!("run {flow} is not a review"))?;
        let worktree = PathBuf::from(&review.worktree_path);
        let runner = self.app.runner.as_ref();
        let changed = wtm_git::checkpoint::changed(runner, &worktree).map_err(text)?;
        if changed.is_empty() {
            return Ok(current);
        }
        let outliers = current.scope.outliers(&changed, &current.allowed);
        if !outliers.is_empty() {
            return Err(format!("{} are still outside the fix", listed(&outliers)));
        }
        let lease = db
            .acquire_lease(
                &NewLease {
                    worktree: review.worktree_path.clone(),
                    flow,
                    kind: LeaseKind::Writer,
                    holder: "wtm".to_owned(),
                },
                self.now(),
            )
            .map_err(|e| match e {
                FlowStoreError::Leased { lease } => leased(&lease),
                other => text(other),
            })?;
        let committed = (|| {
            wtm_git::checkpoint::commit(
                runner,
                &worktree,
                "Add the files you allowed into the fix",
                wtm_agent::authority::protected_path,
            )
            .map_err(text)?;
            let (head, hash) = self.measure(&worktree, &current.base_sha)?;
            db.change_fix(
                fix,
                &FixChange {
                    head_sha: wtm_core::flow::Field::Set(head),
                    diff_hash: wtm_core::flow::Field::Set(hash),
                    detail: wtm_core::flow::Field::Clear,
                    ..FixChange::default()
                },
                self.now(),
            )
            .map_err(text)
        })();
        self.release_lease(lease.id, LeaseRelease::Completed);
        self.notice.changed(flow);
        committed
    }

    // ═══════════════════════════════ reading ═══════════════════════════════

    /// The current fix's diff, for reading before choosing a route.
    ///
    /// # Errors
    ///
    /// When the run has no fix, or git cannot read it.
    pub fn fix_diff(&self, flow: FlowId) -> Result<FixDiffView, ErrorView> {
        let db = self.db().map_err(refused)?;
        let fix = db
            .fixes(flow)
            .map_err(|e| reviews::store_error(&e))?
            .pop()
            .ok_or_else(|| refused("This review has no local fix."))?;
        let review = db
            .review(flow)
            .map_err(|e| reviews::store_error(&e))?
            .ok_or_else(|| refused(format!("run {flow} is not a review")))?;
        let worktree = PathBuf::from(&review.worktree_path);
        let runner = self.app.runner.as_ref();
        let head = match &fix.head_sha {
            Some(head) => head.clone(),
            None => self
                .app
                .git
                .rev_parse(&worktree, "HEAD")
                .map_err(ErrorView::from)?
                .map_or_else(|| fix.base_sha.clone(), |c| c.as_str().to_owned()),
        };
        let binary = push::diff(runner, &worktree, &fix.base_sha, &head, DiffStyle::Binary)
            .map_err(ErrorView::from)?;
        let mut patch = push::diff(runner, &worktree, &fix.base_sha, &head, DiffStyle::Reading)
            .map_err(ErrorView::from)?;
        let truncated = patch.len() > PATCH_LIMIT;
        if truncated {
            let mut end = PATCH_LIMIT;
            while !patch.is_char_boundary(end) {
                end -= 1;
            }
            patch.truncate(end);
        }
        let files = push::numstat(runner, &worktree, &fix.base_sha, &head)
            .map_err(ErrorView::from)?
            .into_iter()
            .map(|(path, added, removed)| FixFile {
                path,
                added,
                removed,
            })
            .collect();
        let uncommitted =
            wtm_git::checkpoint::changed(runner, &worktree).map_err(ErrorView::from)?;
        let outliers = fix.scope.outliers(&uncommitted, &fix.allowed);
        Ok(FixDiffView {
            base: fix.base_sha.clone(),
            hash: diff_hash(&binary),
            head,
            files,
            patch,
            truncated,
            uncommitted,
            outliers,
            fix,
        })
    }

    /// The diff hash of the run's newest ready fix: what a route of it covers now.
    pub(super) fn fix_diff_now(&self, flow: FlowId) -> Option<String> {
        self.db()
            .ok()?
            .fixes(flow)
            .ok()?
            .into_iter()
            .rev()
            .find(|fix| fix.state == FixState::Ready)?
            .diff_hash
    }
}

/// The message of the commit the app makes for what a writer left.
fn commit_message(number: Option<u64>, summary: Option<&str>) -> String {
    let subject = match number {
        Some(number) => format!("Fix review findings on #{number}"),
        None => "Fix review findings".to_owned(),
    };
    match summary.filter(|s| !s.is_empty()) {
        Some(summary) => format!("{subject}\n\n{summary}"),
        None => subject,
    }
}
