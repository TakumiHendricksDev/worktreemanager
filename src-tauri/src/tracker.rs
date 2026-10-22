//! The Work view's sprint queue: a project's issues from its tracker, as the last sync mirrored
//! them, each beside the runs of the worktrees that carry its key.
//!
//! # Two gates before any tracker call
//!
//! The Work view must be on, and the project's own config must name a tracker with
//! `[workflow] tracker = "jira"`, for the reasons `reviews.rs` gives for the forge: ARCHITECTURE §1
//! promises a project without a workflow causes no tracker call, and §6a that the egress is opt-in
//! per project. A repository cannot open the second gate for you; `wtm-config` refuses `tracker` in
//! a committed `wtm.toml`.
//!
//! # Sync, and only sync, reads the queue
//!
//! [`queue`] reads the mirror and the worktree listing and calls no tracker. [`sync`] reads the
//! tracker through `wtm_core::usecase::sync_queue`, which is handed the tracker and the mirror and
//! nothing else, so no sync can start a run (its module docs hold that argument). The window asks
//! for a sync when Work opens on a project, when it comes back into focus, when the Sync button is
//! pressed and after the app's own actions; never on a timer (ARCHITECTURE §8).
//!
//! # An issue is a work item by its key
//!
//! A queue row is identified as a work item is, by `(tracker, key)`. The row carries the work
//! item's id when one exists, the worktree Start Work bound it to, and the worktrees whose branch
//! or directory carries the key (`view::extract_issue_key`). Its run is the most urgent of the
//! issue's own run (Start Work's Implement run) and those worktrees' newest reviews, and decides
//! the row's section. Nothing here creates a work item or records a link: that is Start Work's,
//! which resolves a worktree for the issue and keeps the choice (plan §4.4, `flow/start.rs`).
//! Recording links from a listing here would let a sync, or merely looking, decide which worktree
//! an issue's history belongs to.
//!
//! # No writes
//!
//! Every tracker call here is a read through [`Tracker`](wtm_core::ports::Tracker). Moving an
//! issue is the outbox's, through the app's [`TrackerWriter`](wtm_core::ports::TrackerWriter),
//! and only from Start or a drop on the board (`flow/moves.rs`).

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::Serialize;
use wtm_core::flow::{
    FlowId, FlowState, Issue, IssueDetail, MirroredQueue, QueueSection, Sprint, StartJob,
    StartJobState, StatusColumn, StatusRole, SyncFailure, TrackerAccount, TrackerKind, UnixMs,
    WorkItemId,
};
use wtm_core::model::{Project, Worktree};
use wtm_core::ports::{FlowStore, InboxStore, RunStore, StartStore, TrackerMirror};
use wtm_core::usecase::{SyncError, SyncReason, SyncReport, SyncRequest, read_issue, sync_queue};

use crate::app::App;
use crate::commands::{AppState, Reply, blocking};
use crate::view::{ErrorView, extract_issue_key};

/// What the queue's tracker sections and the board are drawn from.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrackerQueueView {
    /// The tracker the project's workflow names. `None`: the queue has no tracker sections, and the
    /// review slice's queue is all there is.
    pub tracker: Option<TrackerKind>,
    /// `[workflow.queue] query`. `None` with a tracker named: the queue says what to add.
    pub query: Option<String>,
    pub board: Option<u64>,
    /// The board's columns: the project's mapped statuses, in role order.
    pub columns: Vec<StatusColumn>,
    /// The board's active sprints as last read.
    pub sprints: Vec<Sprint>,
    pub issues: Vec<IssueRowView>,
    /// When the last successful sync read the queue. `None` before the first.
    pub read_at: Option<UnixMs>,
    /// The last attempt, when it failed after that.
    pub failure: Option<SyncFailure>,
    /// The mirror holds another query's results: the config changed since the last sync.
    pub outdated: bool,
}

impl TrackerQueueView {
    fn without_tracker(tracker: Option<TrackerKind>) -> Self {
        Self {
            tracker,
            query: None,
            board: None,
            columns: Vec::new(),
            sprints: Vec::new(),
            issues: Vec::new(),
            read_at: None,
            failure: None,
            outdated: false,
        }
    }
}

/// One issue in the queue: its team state beside its execution state, never mixed.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueRowView {
    pub tracker: TrackerKind,
    pub issue: Issue,
    pub sprint: Option<u64>,
    /// The board column its status sits in. `None`: a status the project has not mapped, which the
    /// board shows in a column of its own.
    pub column: Option<usize>,
    pub role: Option<StatusRole>,
    /// The work item for `(tracker, key)`, when one has been started.
    pub work_item: Option<WorkItemId>,
    /// Worktrees, by id, whose branch or directory carries the key.
    pub worktrees: Vec<String>,
    /// The most urgent of those worktrees' newest runs.
    pub run: Option<IssueRunView>,
    pub section: QueueSection,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueRunView {
    pub flow: FlowId,
    pub worktree: String,
    pub state: FlowState,
    /// Inbox items the run has open: what puts an issue in Needs you after its run has settled.
    pub open_items: usize,
    /// For the issue's own run, how far Start Work has got with its worktree. `None` for a review.
    pub start: Option<StartJobState>,
}

/// A sync's report and the queue it left.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncedView {
    pub queue: TrackerQueueView,
    /// `None` when the sync failed; the queue's `failure` says why.
    pub report: Option<SyncReport>,
}

/// One issue read whole, with where it sits on the board.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueDetailView {
    pub tracker: TrackerKind,
    pub detail: IssueDetail,
    pub read_at: UnixMs,
    pub column: Option<usize>,
    pub role: Option<StatusRole>,
    /// Who the CLI read it as, when it was read just now.
    pub account: Option<TrackerAccount>,
}

/// The project and its tracker, if both gates are open. See the module docs.
pub(crate) fn tracker_project(
    app: &App,
    project_id: &str,
) -> Result<(Project, TrackerKind), ErrorView> {
    if !app.work_view_enabled() {
        return Err(ErrorView::new(
            "trackerOff",
            "Issues are read only from the Work view, and it is off.",
        ));
    }
    let project = app.project(project_id)?;
    match project.workflow.as_ref().and_then(|w| w.tracker) {
        Some(tracker) => Ok((project, tracker)),
        None => Err(ErrorView::new(
            "trackerOff",
            "This project's workflow names no tracker, so wtm reads no issues for it. Add \
             `tracker = \"jira\"` to its [workflow] table in ~/.config/wtm/config.toml.",
        )),
    }
}

fn mirror(app: &App) -> Result<Arc<wtm_appdb::AppDb>, ErrorView> {
    app.appdb.get()
}

fn store_error(err: &wtm_core::ports::FlowStoreError) -> ErrorView {
    ErrorView::new("flowStore", err.to_string())
}

/// The project's queue from the mirror. No tracker call.
pub fn queue(app: &App, project_id: &str) -> Result<TrackerQueueView, ErrorView> {
    let project = app.project(project_id)?;
    let tracker = project.workflow.as_ref().and_then(|w| w.tracker);
    // A project with no tracker has no tracker sections, and asking should not create the
    // database just to say so.
    let (Some(tracker), true) = (tracker, app.work_view_enabled()) else {
        return Ok(TrackerQueueView::without_tracker(tracker));
    };
    let db = mirror(app)?;
    let mirrored = db
        .mirrored_queue(&project.id)
        .map_err(|err| store_error(&err))?;
    view(app, &project, tracker, db.as_ref(), mirrored)
}

fn view(
    app: &App,
    project: &Project,
    tracker: TrackerKind,
    db: &wtm_appdb::AppDb,
    mirrored: Option<MirroredQueue>,
) -> Result<TrackerQueueView, ErrorView> {
    let workflow = project.workflow.clone().unwrap_or_default();
    let spec = workflow.queue.as_ref();
    let (reading, failure) = mirrored.map_or((None, None), |m| (m.reading, m.failure));
    let outdated = match (&reading, spec) {
        (Some(reading), Some(spec)) => {
            reading.query != spec.query.trim() || reading.board != spec.board
        }
        _ => false,
    };

    let (linked, listed) = linked_worktrees(app, project);
    let items: BTreeMap<String, WorkItemId> = db
        .work_items(&project.id)
        .map_err(|err| store_error(&err))?
        .into_iter()
        .filter_map(|item| {
            let issue = item.issue?;
            (issue.tracker == tracker.as_str()).then_some((issue.key, item.id))
        })
        .collect();

    let jobs: BTreeMap<String, StartJob> = db
        .start_jobs(Some(&project.id))
        .map_err(|err| store_error(&err))?
        .into_iter()
        .map(|job| (job.key.clone(), job))
        .collect();

    let mut issues = Vec::new();
    let (sprints, read_at) = match reading {
        Some(reading) => {
            for queued in reading.issues {
                let job = jobs.get(&queued.issue.key);
                let mut worktrees: Vec<String> = linked
                    .get(&queued.issue.key)
                    .map(|found| found.iter().map(|w| w.id.as_str().to_owned()).collect())
                    .unwrap_or_default();
                // The worktree Start Work bound it to comes first, carrying the key or not.
                if let Some(job) = job
                    && listed.contains(&job.worktree)
                {
                    worktrees.retain(|w| *w != job.worktree);
                    worktrees.insert(0, job.worktree.clone());
                }
                let run = newest_urgent_run(db, project, &worktrees, job)?;
                let section = run
                    .as_ref()
                    .map_or(QueueSection::NotStarted, |(_, section)| *section);
                issues.push(IssueRowView {
                    tracker,
                    column: workflow.statuses.column_of(&queued.issue.status.name),
                    role: workflow.statuses.role_of(&queued.issue.status.name),
                    work_item: items.get(&queued.issue.key).copied(),
                    worktrees,
                    run: run.map(|(run, _)| run),
                    section,
                    sprint: queued.sprint,
                    issue: queued.issue,
                });
            }
            (reading.sprints, Some(reading.read_at))
        }
        None => (Vec::new(), None),
    };

    Ok(TrackerQueueView {
        tracker: Some(tracker),
        query: spec.map(|spec| spec.query.trim().to_owned()),
        board: spec.and_then(|spec| spec.board),
        columns: workflow.statuses.columns(),
        sprints,
        issues,
        read_at,
        failure,
        outdated,
    })
}

/// The project's worktrees by the issue key their branch or directory carries, and the ids of all
/// of them. Bare and prunable entries are left out, as the queue leaves them out of My worktrees.
fn linked_worktrees(
    app: &App,
    project: &Project,
) -> (BTreeMap<String, Vec<Worktree>>, Vec<String>) {
    let worktrees = match app.git.list_worktrees(&project.root) {
        Ok(worktrees) => worktrees,
        Err(error) => {
            // The queue is still worth showing without its links; the worktree list says why it
            // failed where it is drawn.
            tracing::warn!(%error, "could not list worktrees for the sprint queue");
            return (BTreeMap::new(), Vec::new());
        }
    };
    let mut linked: BTreeMap<String, Vec<Worktree>> = BTreeMap::new();
    let mut listed = Vec::new();
    for worktree in worktrees {
        if worktree.is_bare || worktree.prunable.is_some() {
            continue;
        }
        listed.push(worktree.id.as_str().to_owned());
        if let Some(key) = extract_issue_key(&worktree) {
            linked.entry(key).or_default().push(worktree);
        }
    }
    (linked, listed)
}

/// The issue's own run and the newest review of each of its worktrees, and the most urgent of
/// those by section. The issue's own run wins a tie: it is the one about the issue.
fn newest_urgent_run(
    db: &wtm_appdb::AppDb,
    project: &Project,
    worktrees: &[String],
    job: Option<&StartJob>,
) -> Result<Option<(IssueRunView, QueueSection)>, ErrorView> {
    let mut best: Option<(IssueRunView, QueueSection)> = None;
    if let Some(job) = job
        && let Some(flow) = db.flow(job.flow).map_err(|err| store_error(&err))?
    {
        let open_items = db
            .inbox(Some(flow.id), None)
            .map_err(|err| store_error(&err))?
            .len();
        best = Some((
            IssueRunView {
                flow: flow.id,
                worktree: job.worktree.clone(),
                state: flow.state.clone(),
                open_items,
                start: Some(job.state),
            },
            QueueSection::of(Some(&flow.state), open_items),
        ));
    }
    for worktree in worktrees {
        let Some(review) = db
            .reviews_of(&project.id, worktree)
            .map_err(|err| store_error(&err))?
            .into_iter()
            .next()
        else {
            continue;
        };
        let Some(flow) = db.flow(review.flow).map_err(|err| store_error(&err))? else {
            continue;
        };
        let open_items = db
            .inbox(Some(flow.id), None)
            .map_err(|err| store_error(&err))?
            .len();
        let section = QueueSection::of(Some(&flow.state), open_items);
        if best.as_ref().is_none_or(|(_, held)| section < *held) {
            best = Some((
                IssueRunView {
                    flow: flow.id,
                    worktree: worktree.clone(),
                    state: flow.state.clone(),
                    open_items,
                    start: None,
                },
                section,
            ));
        }
    }
    Ok(best)
}

/// Read the project's queue from its tracker into the mirror, then show it.
///
/// A tracker that fails is not an error here: the mirror records the failure beside the last good
/// reading, and the view returned says both. Only a gate, a missing queue or the database itself
/// is an error.
pub fn sync(app: &App, project_id: &str, reason: SyncReason) -> Result<SyncedView, ErrorView> {
    let (project, tracker) = tracker_project(app, project_id)?;
    let spec = project
        .workflow
        .as_ref()
        .and_then(|w| w.queue.clone())
        .ok_or_else(|| {
            ErrorView::new(
                "noQueue",
                "This project's workflow has no [workflow.queue], so there is nothing to sync. \
                 Add a `query` to it in ~/.config/wtm/config.toml.",
            )
        })?;
    let db = mirror(app)?;
    let request = SyncRequest {
        project: project.id.clone(),
        tracker,
        cwd: &project.root,
        queue: &spec,
        reason,
        now: app.clock.now_unix_ms(),
    };
    let report = match sync_queue(app.tracker.as_ref(), db.as_ref(), &request) {
        Ok(report) => {
            tracing::info!(
                ?reason,
                issues = report.issues,
                moved = report.changes.moved.len(),
                "synced the sprint queue"
            );
            Some(report)
        }
        Err(SyncError::Tracker(error)) => {
            tracing::warn!(?reason, %error, "the sprint queue did not sync");
            None
        }
        Err(SyncError::Store(error)) => return Err(store_error(&error)),
    };
    let mirrored = db
        .mirrored_queue(&project.id)
        .map_err(|err| store_error(&err))?;
    Ok(SyncedView {
        queue: view(app, &project, tracker, db.as_ref(), mirrored)?,
        report,
    })
}

/// One issue: as last read whole from the mirror, or read now when `fresh`.
///
/// `None` when it is not fresh and has never been read whole.
pub fn issue(
    app: &App,
    project_id: &str,
    key: &str,
    fresh: bool,
) -> Result<Option<IssueDetailView>, ErrorView> {
    let (project, tracker) = tracker_project(app, project_id)?;
    let workflow = project.workflow.clone().unwrap_or_default();
    let db = mirror(app)?;
    let (detail, read_at, account) = if fresh {
        let now = app.clock.now_unix_ms();
        let (account, detail) = read_issue(
            app.tracker.as_ref(),
            db.as_ref(),
            tracker,
            &project.root,
            key,
            &workflow.fields,
            now,
        )
        .map_err(|err| match err {
            SyncError::Tracker(error) => ErrorView::from(error),
            SyncError::Store(error) => store_error(&error),
        })?;
        (detail, now, Some(account))
    } else {
        match db
            .mirrored_issue(tracker, key)
            .map_err(|err| store_error(&err))?
        {
            Some((detail, at)) => (detail, at, None),
            None => return Ok(None),
        }
    };
    Ok(Some(IssueDetailView {
        tracker,
        column: workflow.statuses.column_of(&detail.issue.status.name),
        role: workflow.statuses.role_of(&detail.issue.status.name),
        detail,
        read_at,
        account,
    }))
}

#[tauri::command]
pub async fn tracker_queue(app: AppState<'_>, project_id: String) -> Reply<TrackerQueueView> {
    let app = Arc::clone(&app);
    blocking(move || queue(&app, &project_id)).await
}

#[tauri::command]
pub async fn sync_tracker(
    app: AppState<'_>,
    project_id: String,
    reason: SyncReason,
) -> Reply<SyncedView> {
    let app = Arc::clone(&app);
    blocking(move || sync(&app, &project_id, reason)).await
}

#[tauri::command]
pub async fn tracker_issue(
    app: AppState<'_>,
    project_id: String,
    key: String,
    fresh: bool,
) -> Reply<Option<IssueDetailView>> {
    let app = Arc::clone(&app);
    blocking(move || issue(&app, &project_id, &key, fresh)).await
}
