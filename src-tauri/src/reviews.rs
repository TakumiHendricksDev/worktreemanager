//! The Work view's Reviews queue: pull requests you add by URL or number, what the forge says about
//! them, and the worktree a review of one reads.
//!
//! # Two gates before any forge call
//!
//! The Work view must be on, and the project's own config must name a forge with
//! `[workflow] forge = "github"`. ARCHITECTURE §1 promises that a project without a workflow causes
//! no forge call, and §6a that the egress is opt-in per project; checking both here, at the only
//! entry points that reach [`App::forge`], is what keeps those promises true whatever the webview
//! asks for. A repository cannot set the second gate for you: `wtm-config` refuses `forge` in a
//! committed `wtm.toml`.
//!
//! # The queue is work items
//!
//! Adding a pull request makes a pull-request-backed work item in the flow store, which keeps one
//! per `(repo, number)`. Adding the same one twice is not an error: the store names the item that
//! already exists, and that is the one returned, so a second paste lands on the first row instead
//! of beside it. A review worktree is recorded against the item as a link, because the plan makes a
//! worktree something a work item uses, not what it is.
//!
//! # Read on demand, and remembered
//!
//! Nothing here polls. A pull request is read when it is added and whenever its row is opened or
//! refreshed — which the window also does when it comes back into focus — and its head is read
//! again, and pinned, when a worktree is prepared: §8's polling ban, and §6a's "sync runs on
//! demand". Each reading is kept as the work item's mirror (`wtm_core::flow::pull_review`), which is
//! how a review of it learns, without asking the forge again, that the author has pushed since.
//!
//! # Yours, beside the forge's
//!
//! "I'm reviewing this" is a flag in wtm's own store, set and cleared here. It is not GitHub's
//! requested reviewers and it changes nothing there.
//!
//! # No writes
//!
//! Every forge call here is a read through [`Forge`](wtm_core::ports::Forge), which has no method
//! that writes. Publishing a review is a later item with approvals of its own.

use std::path::Path;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use wtm_core::flow::{
    BranchProtection, ChecksState, ForgeAccount, ForgeKind, MirroredPr, NewWorkItem, Permission,
    PrTarget, PullRequest, RepoSlug, WorkItem, WorkItemId, WorkLink, default_host, host_for,
    remote_for,
};
use wtm_core::model::{CreateOutcome, PreflightSeverity, Project};
use wtm_core::ports::PullReviewStore;
use wtm_core::ports::flow::{FlowStore, FlowStoreError};
use wtm_core::ports::git::RemoteUrl;
use wtm_core::usecase::{
    ReuseReason, ReviewCheckout, ReviewFetch, ReviewOutcome, ReviewPreview, ReviewRequest,
    ReviewSetup, ReviewTarget,
};

use crate::app::App;
use crate::commands::{AppState, Reply, ambient_context, blocking};
use crate::view::{ErrorView, PreflightView, preflight_view};

/// What the Reviews section lists.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewQueueView {
    /// The forge the project's workflow names. `None` is the reason the section offers no way to
    /// add a pull request, and says why.
    pub forge: Option<ForgeKind>,
    pub items: Vec<ReviewItemView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewItemView {
    pub id: WorkItemId,
    /// `owner/name`.
    pub repo: String,
    pub number: u64,
    pub title: String,
    /// The worktree most recently used to review it, by path. It may have been removed since,
    /// which the view finds out from the worktree listing it already has.
    pub worktree: Option<String>,
    /// When you said "I'm reviewing this", if you did.
    pub reviewing_since: Option<u64>,
}

/// One pull request, as the forge describes it now.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewDetailView {
    pub item: ReviewItemView,
    pub pull_request: PullRequest,
    pub checks_state: ChecksState,
    pub permission: Permission,
    pub can_push: bool,
    /// Whether the base branch is one of the workflow's integration branches.
    pub base_integration: bool,
    /// The base branch's protection, read only when it is an integration branch: that is the
    /// branch a merge would land on, and the one protection is configured for.
    pub base_protection: Option<BranchProtection>,
    pub account: ForgeAccount,
}

/// The review screen for a review worktree.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewPreviewView {
    pub number: u64,
    pub head: String,
    pub fetch: Option<ReviewFetch>,
    pub head_present: bool,
    pub target: ReviewTargetView,
    pub setup: ReviewSetup,
    pub preflight: Vec<PreflightView>,
    pub warnings: Vec<String>,
    /// No error stands, waivable or not.
    pub can_create: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ReviewTargetView {
    Reuse {
        worktree_id: String,
        path: String,
        reason: ReuseReason,
        at_head: bool,
    },
    Create {
        directory: String,
        branch: Option<String>,
        git_argv: Vec<String>,
    },
}

/// What preparing did.
#[derive(Debug, Clone, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ReviewOutcomeView {
    Reused {
        worktree_id: String,
        path: String,
        at_head: bool,
    },
    Created {
        worktree_id: String,
        path: String,
        setup_session: Option<String>,
    },
    /// The worktree exists and setup failed; the form's remedies apply, from the worktree view.
    SetupFailed {
        worktree_id: String,
        path: String,
        session: String,
    },
    Cancelled {
        worktree_id: Option<String>,
    },
}

/// A review worktree request from the webview.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewWorktreeArgs {
    pub item_id: WorkItemId,
    /// The head the view showed. Preparing refuses a different one.
    pub expected_head: Option<String>,
    #[serde(default)]
    pub checkout: ReviewCheckout,
    #[serde(default)]
    pub prepare_environment: bool,
    #[serde(default)]
    pub acknowledged: Vec<String>,
    #[serde(default = "default_rows")]
    pub rows: u16,
    #[serde(default = "default_cols")]
    pub cols: u16,
}

fn default_rows() -> u16 {
    24
}

fn default_cols() -> u16 {
    100
}

/// The project, if both gates are open. See the module docs.
pub(crate) fn forge_project(app: &App, project_id: &str) -> Result<Project, ErrorView> {
    if !app.work_view_enabled() {
        return Err(ErrorView::new(
            "forgeOff",
            "Pull requests are read only from the Work view, and it is off.",
        ));
    }
    let project = app.project(project_id)?;
    match project
        .workflow
        .as_ref()
        .and_then(|workflow| workflow.forge)
    {
        Some(ForgeKind::Github) => Ok(project),
        None => Err(ErrorView::new(
            "forgeOff",
            "This project's workflow names no forge, so wtm reads no pull requests for it. \
             Add `forge = \"github\"` to its [workflow] table in ~/.config/wtm/config.toml.",
        )),
    }
}

pub(crate) fn store(app: &App) -> Result<Arc<dyn FlowStore>, ErrorView> {
    app.flow_store()
        .map_err(|err| ErrorView::new("flowStore", err.to_string()))
}

pub(crate) fn store_error(err: &FlowStoreError) -> ErrorView {
    ErrorView::new("flowStore", err.to_string())
}

/// The work item, if it is one of this project's pull requests.
pub(crate) fn project_item(
    store: &dyn FlowStore,
    project: &Project,
    item_id: WorkItemId,
) -> Result<WorkItem, ErrorView> {
    store
        .work_item(item_id)
        .map_err(|err| store_error(&err))?
        .filter(|item| item.project == project.id && item.pull_request.is_some())
        .ok_or_else(|| {
            ErrorView::new(
                "unknownReview",
                format!("Review {item_id} is not in this project's queue."),
            )
        })
}

fn item_view(
    app: &App,
    store: &dyn FlowStore,
    item: &WorkItem,
) -> Result<ReviewItemView, ErrorView> {
    let pull_request = item
        .pull_request
        .as_ref()
        .ok_or_else(|| ErrorView::new("unknownReview", "That work item has no pull request."))?;
    let worktree = store
        .links(item.id)
        .map_err(|err| store_error(&err))?
        .into_iter()
        .rev()
        .find_map(|recorded| match recorded.link {
            WorkLink::Worktree { path } => Some(path),
            _ => None,
        });
    let reviewing_since = pull_reviews(app)?
        .claim(item.id)
        .map_err(|err| store_error(&err))?
        .map(|claim| claim.since);
    Ok(ReviewItemView {
        id: item.id,
        repo: pull_request.repo.clone(),
        number: pull_request.number,
        title: item.title.clone(),
        worktree,
        reviewing_since,
    })
}

/// The store's pull-request half: the same database as [`store`].
pub(crate) fn pull_reviews(app: &App) -> Result<Arc<dyn PullReviewStore>, ErrorView> {
    app.appdb
        .open()
        .map(|db| db as Arc<dyn PullReviewStore>)
        .map_err(|err| ErrorView::new("flowStore", err.to_string()))
}

/// Keep what the forge just said as the item's mirror. Losing it costs only the "new commits"
/// line until the next read, so a failure is logged rather than failing the read it came from.
fn remember(app: &App, item: WorkItemId, pull_request: &PullRequest, permission: Permission) {
    let mirror = MirroredPr {
        item,
        pull_request: pull_request.clone(),
        permission,
        read_at: app.clock.now_unix_ms(),
    };
    if let Err(err) = pull_reviews(app)
        .and_then(|store| store.mirror_pr(&mirror).map_err(|err| store_error(&err)))
    {
        tracing::warn!(message = %err.message, "could not keep a pull request's reading");
    }
}

/// Read a queued pull request and your permission on it from the forge now, and keep the reading.
/// The read Run Review and Re-review changes decide on.
pub(crate) fn read_now(
    app: &App,
    project_id: &str,
    item_id: WorkItemId,
) -> Result<(Project, WorkItem, PullRequest, Permission), ErrorView> {
    read_as(app, project_id, item_id)
        .map(|read| (read.project, read.item, read.pull_request, read.permission))
}

/// A pull request read now, with who the forge's CLI is signed in as.
pub(crate) struct ReadNow {
    pub project: Project,
    pub item: WorkItem,
    pub pull_request: PullRequest,
    pub permission: Permission,
    pub account: ForgeAccount,
}

/// [`read_now`], keeping the account the sign-in check reported: publishing needs to know whose
/// pull request is yours, and whose pending review is.
pub(crate) fn read_as(
    app: &App,
    project_id: &str,
    item_id: WorkItemId,
) -> Result<ReadNow, ErrorView> {
    let project = forge_project(app, project_id)?;
    let store = store(app)?;
    let item = project_item(store.as_ref(), &project, item_id)?;
    let remotes = app.git.remote_urls(&project.root)?;
    let target = target_of(&item, &remotes)?;
    let host = target
        .repo
        .as_ref()
        .map_or_else(|| default_host(&remotes), |repo| repo.host.clone());
    let account = app.forge.check(&project.root, &host)?;
    let pull_request = app.forge.pull_request(&project.root, &target)?;
    let permission = app
        .forge
        .permission(&project.root, &pull_request.base.repo)?;
    remember(app, item.id, &pull_request, permission);
    Ok(ReadNow {
        project,
        item,
        pull_request,
        permission,
        account,
    })
}

/// The stored pull request as something to ask the forge about. See [`host_for`] for where the
/// host comes from.
pub(crate) fn target_of(item: &WorkItem, remotes: &[RemoteUrl]) -> Result<PrTarget, ErrorView> {
    let reference = item
        .pull_request
        .as_ref()
        .ok_or_else(|| ErrorView::new("unknownReview", "That work item has no pull request."))?;
    let (owner, name) = reference.repo.split_once('/').ok_or_else(|| {
        ErrorView::new(
            "unknownReview",
            format!("`{}` is not an owner/name", reference.repo),
        )
    })?;
    Ok(PrTarget {
        repo: Some(RepoSlug::new(host_for(owner, name, remotes), owner, name)),
        number: reference.number,
    })
}

/// Everything the detail panel shows, read fresh.
fn detail(
    app: &App,
    project: &Project,
    item: ReviewItemView,
    pull_request: PullRequest,
    account: ForgeAccount,
) -> Result<ReviewDetailView, ErrorView> {
    let root = project.root.as_path();
    let permission = app.forge.permission(root, &pull_request.base.repo)?;
    remember(app, item.id, &pull_request, permission);
    let base_integration = project
        .workflow
        .as_ref()
        .is_some_and(|w| w.is_integration_branch(&pull_request.base.branch));
    let base_protection = if base_integration {
        Some(app.forge.branch_protection(
            root,
            &pull_request.base.repo,
            &pull_request.base.branch,
        )?)
    } else {
        None
    };
    Ok(ReviewDetailView {
        item,
        checks_state: pull_request.checks.state(),
        permission,
        can_push: permission.can_push(),
        base_integration,
        base_protection,
        account,
        pull_request,
    })
}

/// The project's Reviews queue. Reads only the local store: no forge call.
pub fn queue(app: &App, project_id: &str) -> Result<ReviewQueueView, ErrorView> {
    let project = app.project(project_id)?;
    let forge = project
        .workflow
        .as_ref()
        .and_then(|workflow| workflow.forge);
    // A project with no forge has nothing in its queue, and asking should not create the database
    // just to say so.
    if forge.is_none() || !app.work_view_enabled() {
        return Ok(ReviewQueueView {
            forge,
            items: Vec::new(),
        });
    }
    let store = store(app)?;
    let items = store
        .work_items(&project.id)
        .map_err(|err| store_error(&err))?
        .iter()
        // A teammate's pull request, not an issue's own: the one an issue's run drafted is its
        // Implement tab's to follow.
        .filter(|item| item.pull_request.is_some() && item.issue.is_none())
        .map(|item| item_view(app, store.as_ref(), item))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ReviewQueueView { forge, items })
}

#[tauri::command]
pub async fn review_queue(app: AppState<'_>, project_id: String) -> Reply<ReviewQueueView> {
    let app = Arc::clone(&app);
    blocking(move || queue(&app, &project_id)).await
}

/// Add a pull request to the queue by URL or number, and return what the forge says about it.
///
/// Refused for a pull request on a repository none of the project's remotes point at: there would
/// be nowhere to fetch it from, and finding that out at Prepare is later than it needs to be.
pub fn add(app: &App, project_id: &str, input: &str) -> Result<ReviewDetailView, ErrorView> {
    let project = forge_project(app, project_id)?;
    let target = PrTarget::parse(input).map_err(|err| ErrorView::new("validation", err.0))?;
    let remotes = app.git.remote_urls(&project.root)?;
    let host = target
        .repo
        .as_ref()
        .map_or_else(|| default_host(&remotes), |repo| repo.host.clone());

    let account = app.forge.check(&project.root, &host)?;
    let pull_request = app.forge.pull_request(&project.root, &target)?;
    if remote_for(&pull_request.base.repo, &remotes).is_none() {
        return Err(ErrorView::new(
            "forge",
            format!(
                "#{} is a pull request on {}, and no remote of this project points there.",
                pull_request.number, pull_request.base.repo
            ),
        ));
    }

    let store = store(app)?;
    let new = NewWorkItem {
        project: project.id.clone(),
        title: pull_request.title.clone(),
        issue: None,
        pull_request: Some(pull_request.reference()),
    };
    let item = match store.create_work_item(&new, app.clock.now_unix_ms()) {
        Ok(item) => item,
        Err(FlowStoreError::WorkItemExists { existing }) => store
            .work_item(existing)
            .map_err(|err| store_error(&err))?
            .filter(|item| item.project == project.id)
            .ok_or_else(|| {
                ErrorView::new(
                    "forge",
                    format!(
                        "{}#{} is already in another project's queue.",
                        pull_request.base.repo, pull_request.number
                    ),
                )
            })?,
        Err(other) => return Err(store_error(&other)),
    };
    let item = item_view(app, store.as_ref(), &item)?;
    detail(app, &project, item, pull_request, account)
}

#[tauri::command]
pub async fn add_review(
    app: AppState<'_>,
    project_id: String,
    input: String,
) -> Reply<ReviewDetailView> {
    let app = Arc::clone(&app);
    blocking(move || add(&app, &project_id, &input)).await
}

/// Read a queued pull request again.
pub fn refresh(
    app: &App,
    project_id: &str,
    item_id: WorkItemId,
) -> Result<ReviewDetailView, ErrorView> {
    let project = forge_project(app, project_id)?;
    let store = store(app)?;
    let item = project_item(store.as_ref(), &project, item_id)?;
    let remotes = app.git.remote_urls(&project.root)?;
    let target = target_of(&item, &remotes)?;
    let host = target
        .repo
        .as_ref()
        .map_or_else(|| default_host(&remotes), |repo| repo.host.clone());
    let account = app.forge.check(&project.root, &host)?;
    let pull_request = app.forge.pull_request(&project.root, &target)?;
    let item = item_view(app, store.as_ref(), &item)?;
    detail(app, &project, item, pull_request, account)
}

/// Set or clear "I'm reviewing this" on a queued pull request. Reads nothing from the forge and
/// writes nothing there.
pub fn set_reviewing(
    app: &App,
    project_id: &str,
    item_id: WorkItemId,
    on: bool,
) -> Result<ReviewItemView, ErrorView> {
    let project = forge_project(app, project_id)?;
    let store = store(app)?;
    let item = project_item(store.as_ref(), &project, item_id)?;
    pull_reviews(app)?
        .set_claim(item.id, on.then(|| app.clock.now_unix_ms()))
        .map_err(|err| store_error(&err))?;
    item_view(app, store.as_ref(), &item)
}

#[tauri::command]
pub async fn set_reviewing_pull_request(
    app: AppState<'_>,
    project_id: String,
    item_id: WorkItemId,
    on: bool,
) -> Reply<ReviewItemView> {
    let app = Arc::clone(&app);
    blocking(move || set_reviewing(&app, &project_id, item_id, on)).await
}

#[tauri::command]
pub async fn review_detail(
    app: AppState<'_>,
    project_id: String,
    item_id: WorkItemId,
) -> Reply<ReviewDetailView> {
    let app = Arc::clone(&app);
    blocking(move || refresh(&app, &project_id, item_id)).await
}

/// The pipeline's request for a queued pull request, with its head read again now.
fn review_request(
    app: &App,
    project_id: &str,
    args: &ReviewWorktreeArgs,
) -> Result<(ReviewRequest, Arc<dyn FlowStore>), ErrorView> {
    let project = forge_project(app, project_id)?;
    let store = store(app)?;
    let item = project_item(store.as_ref(), &project, args.item_id)?;
    let remotes = app.git.remote_urls(&project.root)?;
    let target = target_of(&item, &remotes)?;
    let pull_request = app.forge.pull_request(&project.root, &target)?;
    let ambient = ambient_context(app, &project);
    Ok((
        ReviewRequest {
            project,
            ambient,
            pull_request,
            expected_head: args
                .expected_head
                .as_deref()
                .map(wtm_core::model::CommitId::new),
            checkout: args.checkout,
            prepare_environment: args.prepare_environment,
            acknowledged: args.acknowledged.clone(),
            rows: args.rows,
            cols: args.cols,
        },
        store,
    ))
}

fn preview_view(preview: &ReviewPreview) -> ReviewPreviewView {
    let plan = &preview.plan;
    ReviewPreviewView {
        number: plan.number,
        head: plan.head.as_str().to_owned(),
        fetch: plan.fetch.clone(),
        head_present: plan.head_present,
        target: match &plan.target {
            ReviewTarget::Reuse {
                worktree,
                reason,
                at_head,
            } => ReviewTargetView::Reuse {
                worktree_id: worktree.id.as_str().to_owned(),
                path: worktree.path.to_string_lossy().into_owned(),
                reason: *reason,
                at_head: *at_head,
            },
            ReviewTarget::Create {
                directory,
                branch,
                git_argv,
            } => ReviewTargetView::Create {
                directory: directory.to_string_lossy().into_owned(),
                branch: branch.as_ref().map(|b| b.as_str().to_owned()),
                git_argv: git_argv.clone(),
            },
        },
        setup: plan.setup.clone(),
        preflight: preview.preflight.iter().map(preflight_view).collect(),
        warnings: preview.warnings.iter().map(|w| w.message.clone()).collect(),
        can_create: !preview
            .preflight
            .iter()
            .any(|item| item.severity == PreflightSeverity::Error),
    }
}

/// What preparing a review worktree would do. Mutates nothing.
pub fn preview(
    app: &App,
    project_id: &str,
    args: &ReviewWorktreeArgs,
) -> Result<ReviewPreviewView, ErrorView> {
    let (req, _) = review_request(app, project_id, args)?;
    let preview = app.create_pipeline().preview_review(
        &req,
        &wtm_core::ports::progress::NullProgress,
        &wtm_core::ports::exec::CancelToken::new(),
    )?;
    Ok(preview_view(&preview))
}

#[tauri::command]
pub async fn preview_review_worktree(
    app: AppState<'_>,
    project_id: String,
    args: ReviewWorktreeArgs,
) -> Reply<ReviewPreviewView> {
    let app = Arc::clone(&app);
    blocking(move || preview(&app, &project_id, &args)).await
}

/// Record that `path` was used for the review, unless it is already the latest worktree recorded.
fn link_worktree(app: &App, store: &dyn FlowStore, item: WorkItemId, path: &Path) {
    let path = path.to_string_lossy().into_owned();
    let latest = store.links(item).ok().and_then(|links| {
        links
            .into_iter()
            .rev()
            .find_map(|recorded| match recorded.link {
                WorkLink::Worktree { path } => Some(path),
                _ => None,
            })
    });
    if latest.as_deref() == Some(path.as_str()) {
        return;
    }
    // Losing this link loses a convenience, not the worktree: the review can be prepared again
    // and finds the same worktree by its head.
    if let Err(err) = store.link(item, &WorkLink::Worktree { path }, app.clock.now_unix_ms()) {
        tracing::warn!(%err, "could not record a review worktree against its work item");
    }
}

/// Fetch, add and, only if asked, run setup; or hand back the worktree that already holds it.
///
/// Re-plans first, like `create_worktree`, and reads the head again: a push since the preview is a
/// preflight error, not a worktree of a commit nobody looked at.
pub fn prepare(
    app: &App,
    project_id: &str,
    args: &ReviewWorktreeArgs,
    progress: &dyn wtm_core::ports::progress::ProgressSink,
    sink: Arc<dyn wtm_core::ports::pty::PtySink>,
) -> Result<ReviewOutcomeView, ErrorView> {
    let (req, store) = review_request(app, project_id, args)?;
    let _creating = app.start_creating(project_id);

    let outcome = app.create_pipeline().execute_review(
        &req,
        progress,
        sink,
        &wtm_core::ports::exec::CancelToken::new(),
    )?;
    let id = |w: &wtm_core::model::Worktree| w.id.as_str().to_owned();
    let path = |w: &wtm_core::model::Worktree| w.path.to_string_lossy().into_owned();
    let view = match &outcome {
        ReviewOutcome::Reused { worktree, at_head } => ReviewOutcomeView::Reused {
            worktree_id: id(worktree),
            path: path(worktree),
            at_head: *at_head,
        },
        ReviewOutcome::Created {
            outcome:
                CreateOutcome::Created {
                    worktree,
                    setup_session,
                },
        } => ReviewOutcomeView::Created {
            worktree_id: id(worktree),
            path: path(worktree),
            setup_session: setup_session.as_ref().map(|s| s.as_str().to_owned()),
        },
        ReviewOutcome::Created {
            outcome:
                CreateOutcome::SetupFailed {
                    worktree, session, ..
                },
        } => ReviewOutcomeView::SetupFailed {
            worktree_id: id(worktree),
            path: path(worktree),
            session: session.as_str().to_owned(),
        },
        ReviewOutcome::Created {
            outcome: CreateOutcome::Cancelled { worktree, .. },
        } => ReviewOutcomeView::Cancelled {
            worktree_id: worktree.as_ref().map(id),
        },
    };
    let used = match &outcome {
        ReviewOutcome::Reused { worktree, .. } => Some(worktree),
        ReviewOutcome::Created { outcome } => outcome.worktree(),
    };
    if let Some(worktree) = used {
        link_worktree(app, store.as_ref(), args.item_id, &worktree.path);
    }
    Ok(view)
}

#[tauri::command]
pub async fn create_review_worktree(
    handle: tauri::AppHandle,
    app: AppState<'_>,
    project_id: String,
    args: ReviewWorktreeArgs,
) -> Reply<ReviewOutcomeView> {
    let app = Arc::clone(&app);
    blocking(move || {
        let progress = crate::pty_bridge::ProgressBridge::new(handle.clone());
        let sink = crate::pty_bridge::EventSink::new(handle);
        prepare(&app, &project_id, &args, &progress, sink)
    })
    .await
}
