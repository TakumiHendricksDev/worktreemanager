//! A teammate's pull request, reviewed: Run Review on its review worktree, following it to new
//! heads, and keeping the result to yourself.
//!
//! # Compared with the pull request's base
//!
//! A review of your own work is measured from wherever your branch left the default branch. A pull
//! request says what it merges into, so its review is measured from there instead: the base
//! branch's remote-tracking ref, fetched first, because a stale one would put commits already on
//! the base into the change and the reviewers would review them too. The fetch updates that one
//! remote-tracking branch and nothing else, the base fetch the create pipeline already makes.
//!
//! # Following new heads
//!
//! The head a review read is its snapshot's. The pull request's head is what the forge said when it
//! was last read (the mirror, `reviews.rs`), so a push by the author makes the review stale the next
//! time the window reads the pull request: on focus, on Refresh, or when it is opened. Which
//! findings that touches is git's answer when the new commits are already here and "all of them"
//! when they are not, since nothing narrower can be known without fetching.
//!
//! Re-review changes then, in this order: reads the pull request again and refuses a head other
//! than the one you were shown; checks the run could rerun at all, before anything moves; brings
//! the review worktree to the new head (`advance_review`, which moves only a clean review checkout
//! at the head the review read); records what the review said at the head it leaves; fetches the
//! base; and reruns the roles whose files changed, each given its own earlier findings to verify.
//! If anything before the move refuses, nothing has happened. After it, the worst case is a review
//! that is visibly stale with Rerun affected offered, which is the state a rerun would fix anyway.
//!
//! # Private, and nothing else yet
//!
//! Keep private notes records an outcome in wtm and writes nowhere else. Publishing and local fixes
//! are drawn in the outcome sheet and disabled: they write to GitHub, and every such write waits for
//! the outbox and its approvals (M1b).

use std::path::Path;
use std::sync::Arc;

use serde::Serialize;
use wtm_core::WtmError;
use wtm_core::flow::{
    CheckRun, EarlierFate, FindingRecord, Flow, FlowId, FlowKind, MirroredPr, NewOutcome,
    NewReviewedHead, Outcome, PullRequest, PushToBranch, ReviewClaim, ReviewedHead, Snapshot,
    WorkItem, WorkItemId, base_ref, fate, push_to_branch,
};
use wtm_core::model::{CommitId, Project, ProjectId};
use wtm_core::ports::{FlowStore, PullReviewStore, RunStore};
use wtm_core::usecase::{AdvanceOutcome, AdvanceRequest};

use super::{Engine, Refusal, StartReview, text};
use crate::app::App;
use crate::reviews;
use crate::view::ErrorView;

/// What the Reviews tab shows about the pull request a review is of.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestReview {
    pub item: WorkItemId,
    /// `owner/name` of the base repository.
    pub repo: String,
    pub number: u64,
    /// What the forge said the last time wtm read it. `None` before the first read.
    pub mirror: Option<MirroredPr>,
    /// "I'm reviewing this", if you said so.
    pub reviewing: Option<ReviewClaim>,
    /// The head the review's latest snapshot read.
    pub reviewed_head: Option<String>,
    /// The pull request's head by the last reading, when it is not the head reviewed.
    pub moved_to: Option<String>,
    /// Whether a fix could go to the pull request's own branch, by the last reading.
    pub push_to_branch: Option<PushToBranch>,
    /// Heads the review moved on from, oldest first, each with what it said there.
    pub earlier: Vec<EarlierHead>,
    /// What you decided, oldest first. The last is the current decision.
    pub outcomes: Vec<Outcome>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EarlierHead {
    /// Nested rather than flattened: its own `findings` and `checks` are the ids, and these are
    /// the records.
    pub head: ReviewedHead,
    pub findings: Vec<EarlierFinding>,
    pub checks: Vec<CheckRun>,
}

/// A finding as it stood at an earlier head, and what has become of it since.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EarlierFinding {
    #[serde(flatten)]
    pub record: FindingRecord,
    pub fate: EarlierFate,
}

/// What Re-review changes did.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Rereviewed {
    pub from: String,
    pub to: String,
    /// The roles run again. Empty when the new commits changed nothing any role reads, in which
    /// case the review now stands at the new head as it was.
    pub roles: Vec<String>,
}

/// A refusal before anything moved, as the window draws one.
fn refused(message: impl Into<String>) -> ErrorView {
    ErrorView::new("flow", message)
}

/// A refusal from the pipeline, said in its own sentences rather than as "1 preflight check(s)
/// failed".
fn pipeline_error(error: WtmError) -> ErrorView {
    match &error {
        WtmError::Preflight(items) => refused(
            items
                .iter()
                .map(|item| match &item.hint {
                    Some(hint) => format!("{} {hint}", item.message),
                    None => item.message.clone(),
                })
                .collect::<Vec<_>>()
                .join(" "),
        ),
        _ => ErrorView::from(error),
    }
}

fn refusal_view(refusal: Refusal) -> ErrorView {
    if refusal.billing.is_empty() {
        refused(refusal.message)
    } else {
        ErrorView::new("billing", refusal.message)
            .with_detail(serde_json::json!({ "providers": refusal.billing }))
    }
}

/// The worktree a pull request's review reads: the newest one recorded against it that still
/// exists.
fn review_worktree(
    app: &App,
    project: &Project,
    store: &dyn FlowStore,
    item: WorkItemId,
) -> Result<wtm_core::model::Worktree, ErrorView> {
    let listed = app.git.list_worktrees(&project.root)?;
    let links = store
        .links(item)
        .map_err(|err| reviews::store_error(&err))?;
    links
        .iter()
        .rev()
        .filter_map(|recorded| match &recorded.link {
            wtm_core::flow::WorkLink::Worktree { path } => Some(path.as_str()),
            _ => None,
        })
        .find_map(|path| {
            listed
                .iter()
                .find(|w| w.prunable.is_none() && w.path == Path::new(path))
                .cloned()
        })
        .ok_or_else(|| {
            ErrorView::new(
                "noReviewWorktree",
                "This pull request has no review worktree yet. Prepare one first.",
            )
        })
}

/// Fetch the base branch a review of `pr` is measured from, and name it. A failed fetch is a
/// warning when the ref is already here, as the create pipeline's base fetch is.
fn fetch_base(app: &App, project: &Project, pr: &PullRequest) -> Result<String, ErrorView> {
    let remotes = app.git.remote_urls(&project.root)?;
    let (remote, base) = base_ref(pr, &remotes).ok_or_else(|| {
        refused(format!(
            "No remote of this repository points at {}, so there is no base to compare #{} with.",
            pr.base.repo, pr.number
        ))
    })?;
    if let Err(err) = app.git.fetch(&project.root, &remote, &pr.base.branch) {
        tracing::warn!(%err, %base, "could not fetch a pull request's base; using what is here");
    }
    if app.git.rev_parse(&project.root, &base)?.is_none() {
        return Err(refused(format!(
            "`{base}` is not here and could not be fetched, so there is nothing to compare #{} \
             with.",
            pr.number
        )));
    }
    Ok(base)
}

/// Run Review on a queued pull request: bring its review worktree to the head, then review it
/// against the pull request's base.
///
/// # Errors
///
/// When the Work view or the project's forge is off, the head moved since `expected_head` was
/// shown, there is no review worktree, it cannot be moved to the head, or the engine refuses.
pub fn start(
    app: &App,
    engine: &Arc<Engine>,
    project_id: &str,
    item_id: WorkItemId,
    recipe: &str,
    expected_head: Option<&str>,
) -> Result<FlowId, ErrorView> {
    let (project, item, pr, _) = reviews::read_now(app, project_id, item_id)?;
    if let Some(expected) = expected_head
        && expected != pr.head.sha.as_str()
    {
        return Err(refused(format!(
            "#{} moved from {} to {} since it was shown. Look at it again first.",
            pr.number,
            CommitId::new(expected).short(),
            pr.head.sha.short()
        )));
    }
    let store = reviews::store(app)?;
    let worktree = review_worktree(app, &project, store.as_ref(), item.id)?;
    let reviewed = engine.last_reviewed_head(&project, &worktree);
    app.create_pipeline()
        .advance_review(&AdvanceRequest {
            project: project.clone(),
            pull_request: pr.clone(),
            worktree: worktree.clone(),
            reviewed: reviewed.map(CommitId::new),
        })
        .map_err(pipeline_error)?;
    let base = fetch_base(app, &project, &pr)?;
    engine
        .start(&StartReview {
            project: project_id.to_owned(),
            worktree: worktree.id.as_str().to_owned(),
            recipe: recipe.to_owned(),
            base: Some(base),
        })
        .map_err(refusal_view)
}

/// Re-review changes: follow the pull request to its new head and rerun the roles it affects.
///
/// # Errors
///
/// When the review is not of one of this project's pull requests, the head has not moved or moved
/// again since `expected_head` was shown, the run could not rerun now, or the review worktree
/// cannot be moved. Nothing has moved when it refuses.
pub fn rereview(
    app: &App,
    engine: &Arc<Engine>,
    project_id: &str,
    flow: FlowId,
    expected_head: Option<&str>,
) -> Result<Rereviewed, ErrorView> {
    let db = app.appdb.get()?;
    let state = db
        .flow(flow)
        .map_err(|err| reviews::store_error(&err))?
        .ok_or_else(|| refused(format!("no run {flow}")))?;
    let review = db
        .review(flow)
        .map_err(|err| reviews::store_error(&err))?
        .ok_or_else(|| refused(format!("run {flow} is not a review")))?;
    let (project, _, pr, _) = reviews::read_now(app, project_id, state.work_item)?;
    if let Some(expected) = expected_head
        && expected != pr.head.sha.as_str()
    {
        return Err(refused(format!(
            "#{} moved again, from {} to {}, since it was shown. Look at it again first.",
            pr.number,
            CommitId::new(expected).short(),
            pr.head.sha.short()
        )));
    }
    let snapshot_id = review
        .snapshot
        .ok_or_else(|| refused("This review has not read a head yet."))?;
    let snapshot = db
        .snapshot(snapshot_id)
        .map_err(|err| reviews::store_error(&err))?
        .ok_or_else(|| refused(format!("snapshot {snapshot_id} is missing")))?;
    if snapshot.head_sha == pr.head.sha.as_str() {
        return Err(refused(format!(
            "#{} is still at {}, the head this review read: there is nothing new to review.",
            pr.number,
            pr.head.sha.short()
        )));
    }
    engine.can_rerun(flow).map_err(refusal_view)?;

    let worktree = app.worktree(&project, &review.worktree)?;
    let moved = app
        .create_pipeline()
        .advance_review(&AdvanceRequest {
            project: project.clone(),
            pull_request: pr.clone(),
            worktree,
            reviewed: Some(CommitId::new(&snapshot.head_sha)),
        })
        .map_err(pipeline_error)?;
    if let AdvanceOutcome::Moved { from, to, .. } = &moved {
        tracing::info!(%flow, from = ?from.as_ref().map(CommitId::short), to = %to.short(),
            "moved a review worktree to its pull request's new head");
    }
    engine.leave_head(flow, &snapshot, snapshot_id, pr.head.sha.as_str())?;
    let base = fetch_base(app, &project, &pr)?;
    if base != review.base {
        tracing::info!(%flow, was = %review.base, now = %base, "a pull request's base changed");
    }
    let roles = engine.rerun_at_new_head(flow).map_err(refusal_view)?;
    Ok(Rereviewed {
        from: snapshot.head_sha,
        to: pr.head.sha.as_str().to_owned(),
        roles,
    })
}

impl Engine {
    /// The work item a run is of, when it is a pull request's.
    fn pull_request_item(&self, flow: &Flow) -> Option<WorkItem> {
        if flow.state.kind != FlowKind::ReviewPr {
            return None;
        }
        let db = self.db().ok()?;
        db.work_item(flow.work_item)
            .ok()
            .flatten()
            .filter(|item| item.pull_request.is_some())
    }

    /// The pull request's head by the last reading, when it is not the head `snapshot` read.
    pub(super) fn moved_head(&self, flow: FlowId, snapshot: &Snapshot) -> Option<String> {
        let db = self.db().ok()?;
        let state = db.flow(flow).ok().flatten()?;
        let item = self.pull_request_item(&state)?;
        let mirror = db.mirrored_pr(item.id).ok().flatten()?;
        let head = mirror.pull_request.head.sha.as_str();
        (head != snapshot.head_sha).then(|| head.to_owned())
    }

    /// The base a pull request's review is measured from, by its last reading: what a Run Review
    /// started from the worktree bar uses, so both entry points compare with the same thing.
    pub(super) fn pull_request_base(
        &self,
        project: &Project,
        worktree_path: &str,
    ) -> Option<String> {
        if !self.app.files.exists(&self.app.config.paths().flow_db) {
            return None;
        }
        let db = self.db().ok()?;
        let id = ProjectId::from_root(&project.root);
        let item = db
            .work_item_for_worktree(&id, worktree_path)
            .ok()
            .flatten()?;
        let item = db.work_item(item).ok().flatten()?;
        item.pull_request.as_ref()?;
        let mirror = db.mirrored_pr(item.id).ok().flatten()?;
        let remotes = self.app.git.remote_urls(&project.root).ok()?;
        let (_, base) = base_ref(&mirror.pull_request, &remotes)?;
        self.app
            .git
            .rev_parse(&project.root, &base)
            .ok()
            .flatten()
            .map(|_| base)
    }

    /// The head the newest review of `worktree` read, if it has been reviewed.
    fn last_reviewed_head(
        &self,
        project: &Project,
        worktree: &wtm_core::model::Worktree,
    ) -> Option<String> {
        if !self.app.files.exists(&self.app.config.paths().flow_db) {
            return None;
        }
        let db = self.db().ok()?;
        let id = ProjectId::from_root(&project.root);
        let review = db
            .reviews_of(&id, worktree.id.as_str())
            .ok()?
            .into_iter()
            .next()?;
        db.snapshot(review.snapshot?)
            .ok()
            .flatten()
            .map(|s| s.head_sha)
    }

    /// Record what the review said at the head it is leaving, before the rerun replaces it.
    fn leave_head(
        &self,
        flow: FlowId,
        snapshot: &Snapshot,
        snapshot_id: wtm_core::flow::SnapshotId,
        next_head: &str,
    ) -> Result<ReviewedHead, ErrorView> {
        let db = self.db().map_err(refused)?;
        let findings = db
            .findings(flow)
            .map_err(|err| reviews::store_error(&err))?
            .iter()
            .map(|record| record.finding.id)
            .collect();
        let checks = db
            .checks(flow)
            .map_err(|err| reviews::store_error(&err))?
            .iter()
            .filter(|run| run.tree_sha == snapshot.tree_sha)
            .map(|run| run.id)
            .collect();
        let left = db
            .record_reviewed_head(
                &NewReviewedHead {
                    flow,
                    head_sha: snapshot.head_sha.clone(),
                    snapshot: snapshot_id,
                    findings,
                    checks,
                    next_head: next_head.to_owned(),
                },
                self.now(),
            )
            .map_err(|err| reviews::store_error(&err))?;
        self.notice.changed(flow);
        Ok(left)
    }

    /// The revision a run's current snapshot follows, when it is a pull request's re-review: the
    /// head it moved on from, and the head it is at.
    pub(super) fn revision(&self, flow: FlowId, snapshot: &Snapshot) -> Option<(String, String)> {
        let db = self.db().ok()?;
        db.reviewed_heads(flow)
            .ok()?
            .into_iter()
            .rev()
            .find(|left| left.head.next_head == snapshot.head_sha)
            .map(|left| (left.head.head_sha, snapshot.head_sha.clone()))
    }

    /// Keep a review's result private: notes and the findings as they stand, pinned to the head it
    /// read. Writes nothing outside wtm.
    ///
    /// # Errors
    ///
    /// When the run is still working, has no snapshot, or there is nothing to keep.
    pub fn keep_private(&self, flow: FlowId, notes: &str) -> Result<Outcome, String> {
        if self.is_running(flow) {
            return Err("this review is still running; keep notes once it has finished".to_owned());
        }
        let db = self.db()?;
        let review = db
            .review(flow)
            .map_err(text)?
            .ok_or_else(|| format!("run {flow} is not a review"))?;
        let state = db
            .flow(flow)
            .map_err(text)?
            .ok_or_else(|| format!("no run {flow}"))?;
        if self.pull_request_item(&state).is_none() {
            return Err("only a review of a teammate's pull request has an outcome".to_owned());
        }
        let snapshot = review
            .snapshot
            .and_then(|id| db.snapshot(id).ok().flatten())
            .ok_or("this review has not read a head yet")?;
        let findings = db
            .findings(flow)
            .map_err(text)?
            .iter()
            .map(|record| record.finding.id)
            .collect();
        let outcome =
            NewOutcome::private(flow, &snapshot.head_sha, notes, findings).map_err(text)?;
        let kept = db.record_outcome(&outcome, self.now()).map_err(text)?;
        self.notice.changed(flow);
        Ok(kept)
    }

    /// The pull request a run is of, as the Reviews tab shows it. `None` for a review of your own
    /// work.
    pub(super) fn pull_request_view(
        &self,
        state: &Flow,
        snapshot: Option<&Snapshot>,
        findings: &[FindingRecord],
        checks: &[CheckRun],
    ) -> Result<Option<PullRequestReview>, String> {
        let Some(item) = self.pull_request_item(state) else {
            return Ok(None);
        };
        let Some(reference) = item.pull_request.clone() else {
            return Ok(None);
        };
        let db = self.db()?;
        let mirror = db.mirrored_pr(item.id).map_err(text)?;
        let reviewed_head = snapshot.map(|s| s.head_sha.clone());
        let moved_to = match (&mirror, &reviewed_head) {
            (Some(mirror), Some(reviewed)) if mirror.pull_request.head.sha.as_str() != reviewed => {
                Some(mirror.pull_request.head.sha.as_str().to_owned())
            }
            _ => None,
        };
        // Each head is judged by the one that followed it: what the round at the next head said,
        // and for the last one left, the review as it stands. Judged against the newest instead, a
        // finding addressed two pushes later would read as addressed at every head before.
        let left_heads = db.reviewed_heads(state.id).map_err(text)?;
        let mut recorded = Vec::with_capacity(left_heads.len());
        for left in &left_heads {
            let mut at_head = Vec::new();
            for id in &left.head.findings {
                if let Some(record) = db.finding(*id).map_err(text)? {
                    at_head.push(record);
                }
            }
            recorded.push(at_head);
        }
        let mut earlier = Vec::new();
        for (index, left) in left_heads.into_iter().enumerate() {
            let next = recorded.get(index + 1).map_or(findings, Vec::as_slice);
            let at_head = recorded[index]
                .iter()
                .map(|record| EarlierFinding {
                    fate: fate(record, next),
                    record: record.clone(),
                })
                .collect();
            let ran = checks
                .iter()
                .filter(|run| left.head.checks.contains(&run.id))
                .cloned()
                .collect();
            earlier.push(EarlierHead {
                head: left,
                findings: at_head,
                checks: ran,
            });
        }
        Ok(Some(PullRequestReview {
            item: item.id,
            repo: reference.repo,
            number: reference.number,
            push_to_branch: mirror
                .as_ref()
                .map(|m| push_to_branch(&m.pull_request, m.permission)),
            reviewing: db.claim(item.id).map_err(text)?,
            outcomes: db.outcomes(state.id).map_err(text)?,
            mirror,
            reviewed_head,
            moved_to,
            earlier,
        }))
    }
}
