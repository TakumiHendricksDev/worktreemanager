//! Where a local fix goes: the routes on offer, the preview of exactly what each sends, the
//! approval of its destination and diff, and the executor that sends it.
//!
//! # Approved by destination and diff
//!
//! A route is planned from a fresh read of the pull request ([`Engine::route_options`]), shown
//! exactly — the remote and its URL, the refspec, the argv that pushes it, the pull request's text
//! with its marker, or each suggestion as it posts — and approved by the hash of that plan. The
//! approval pins the remote, the ref, the diff's hash, the fix's commit and their branch's head
//! (`wtm_core::flow::route`). Approving builds the plan again from a new read and refuses a
//! different hash, as a review's publish does (`publish.rs`).
//!
//! # Read, then send
//!
//! The executor follows the review outbox's order (`outbox.rs`). For a push: read the remote branch
//! first, and adopt a branch already at the fix; then check the approval against their branch's
//! head now and the fix's diff now; then decide from the remote branch ([`before_push`]) — push
//! from where the approval expected it, stop and ask from anywhere else — and only then mark the
//! operation executing and push, which is never a force. For the follow-up pull request: look for
//! one from the fix's branch into theirs, by its marker or its branches, and adopt it; otherwise
//! open it. A lost answer is settled by reading again, and a crash is checked at launch, never
//! re-sent.
//!
//! # Suggestions are a review
//!
//! Suggestion comments post the way a review does, through `outbox.rs`'s two writes and its
//! executor; this module builds them and records their approval with the route's pins as well.

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use wtm_core::flow::{
    Activity, ApprovalId, Attention, BeforePush, FixRef, FixState, FlowId, InboxKind, InboxTarget,
    IntegrationBranches, Marker, NewApproval, NewInboxItem, OpenPullRequestPayload, Operation,
    OperationChange, OperationPayload, OperationResult, OperationState, Phase, PublicationState,
    PushPayload, RouteFacts, RouteKind, RoutePlan, RouteSend, SuggestionContext, SuggestionRoute,
    TheirBranch, before_push, follow_up, parse_unified, push_to_their_branch, signed, suggestions,
};
use wtm_core::ports::{FixStore, FlowStore, FlowStoreError, ForgeError, OutboxStore, RunStore};
use wtm_git::push::{self, DiffStyle, Pushed};

use super::Engine;
use super::publish::{PublicationView, PublishPreview, preview_of};
use crate::reviews::{self, ReadNow};
use crate::view::ErrorView;

/// The error kind a refused route approval carries when their branch moved past the fix's base.
pub const ROUTE_MOVED: &str = "routeMoved";

/// What the outcome sheet asks a route for.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteRequest {
    pub kind: RouteKind,
    /// The follow-up pull request's title. Unused by the other routes.
    #[serde(default)]
    pub title: String,
    /// The follow-up pull request's body, or the suggestions' summary.
    #[serde(default)]
    pub body: String,
}

/// One route as the sheet offers it: open or not, and why.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteOption {
    pub kind: RouteKind,
    /// Why it is closed, in a sentence. `None` when it is open.
    pub unavailable: Option<String>,
    /// Exactly where it goes, when it is open.
    pub destination: Option<String>,
    /// The text it starts from: a follow-up's title and body, or the suggestions' summary.
    pub title: String,
    pub body: String,
}

/// Every route for the run's fix, from a fresh read.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteOptions {
    pub fix: FixRef,
    pub their: TheirBranch,
    pub account: Option<String>,
    pub routes: Vec<RouteOption>,
}

/// Exactly what a route sends, and the hash an approval of it pins.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutePreview {
    pub kind: RouteKind,
    pub hash: String,
    /// The push route's plan. `None` for suggestions.
    pub plan: Option<RoutePlan>,
    /// The commands and requests it makes, in order, as they run.
    pub steps: Vec<String>,
    /// The follow-up pull request's body as it is sent, marker included.
    pub body: Option<String>,
    /// The suggestions as they post. `None` for a push route.
    pub suggestions: Option<PublishPreview>,
    pub diff_hash: String,
    /// Their branch's head by this read.
    pub head: String,
}

/// A push route's send as the window shows it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteSendView {
    #[serde(flatten)]
    pub route: RouteSend,
    pub state: PublicationState,
    /// The follow-up pull request, once there is one.
    pub url: Option<String>,
}

/// What approving a route started.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ApprovedRoute {
    Route { route: Box<RouteSendView> },
    Suggestions { publication: Box<PublicationView> },
}

fn refused(message: impl Into<String>) -> ErrorView {
    ErrorView::new("flow", message)
}

fn short(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

/// What a route is planned from, read fresh.
struct Ctx {
    read: ReadNow,
    fix: FixRef,
    picks: Vec<wtm_core::flow::FindingId>,
    remotes: Vec<wtm_core::ports::RemoteUrl>,
    integration: IntegrationBranches,
    worktree: PathBuf,
}

impl Ctx {
    fn facts(&self) -> RouteFacts<'_> {
        RouteFacts {
            pr: &self.read.pull_request,
            permission: self.read.permission,
            login: self.read.account.login.as_deref(),
            remotes: &self.remotes,
            integration: &self.integration,
            fix: &self.fix,
        }
    }

    fn head(&self) -> &str {
        self.read.pull_request.head.sha.as_str()
    }
}

/// What a route builds into.
enum Built {
    Push(Box<RoutePlan>),
    Suggestions(Box<SuggestionRoute>),
}

impl Built {
    fn hash(&self) -> String {
        match self {
            Self::Push(plan) => plan.hash(),
            Self::Suggestions(route) => route.hash(),
        }
    }
}

/// The follow-up pull request's text before you edit it.
fn default_follow_up(
    number: u64,
    branch: &str,
    findings: &[wtm_core::flow::FindingRecord],
) -> (String, String) {
    let mut body = format!("Fixes from my review of #{number}, made on top of `{branch}`:\n");
    for record in findings {
        let place = record.finding.location.as_ref().map_or_else(
            || "the whole change".to_owned(),
            |at| format!("`{}:{}`", at.path, at.start_line),
        );
        let _ = write!(body, "\n- {} ({place})", record.finding.title.trim());
    }
    let _ = write!(
        body,
        "\n\nMerge this into `{branch}` to take the fixes into #{number}."
    );
    (format!("Fixes for #{number}"), body)
}

fn default_summary(number: u64) -> String {
    format!(
        "Suggested changes for #{number}, from a local fix of what my review found. Each one can \
         be applied from its comment."
    )
}

impl Engine {
    /// Read everything a route of `flow`'s fix is planned from.
    fn route_ctx(&self, flow: FlowId) -> Result<Ctx, ErrorView> {
        if self.is_running(flow) {
            return Err(refused(
                "This review is still running; choose where the fix goes once it has finished.",
            ));
        }
        let db = self.db().map_err(refused)?;
        let state = db
            .flow(flow)
            .map_err(|e| reviews::store_error(&e))?
            .ok_or_else(|| refused(format!("no run {flow}")))?;
        let item = self
            .pull_request_item(&state)
            .ok_or_else(|| refused("Only a fix of a teammate's pull request has routes."))?;
        if state.state.phase != Phase::FixReady || state.state.end.is_some() {
            return Err(refused("There is no finished local fix to send."));
        }
        let fix = db
            .fixes(flow)
            .map_err(|e| reviews::store_error(&e))?
            .pop()
            .filter(|fix| fix.state == FixState::Ready)
            .ok_or_else(|| {
                refused("The last local fix did not finish, so there is nothing to send.")
            })?;
        let (Some(head), Some(diff_hash)) = (fix.head_sha.clone(), fix.diff_hash.clone()) else {
            return Err(refused("The local fix has no commit to send."));
        };
        if head == fix.base_sha {
            return Err(refused(
                "The local fix committed nothing, so there is nothing to send.",
            ));
        }
        let review = db
            .review(flow)
            .map_err(|e| reviews::store_error(&e))?
            .ok_or_else(|| refused(format!("run {flow} is not a review")))?;
        let read = reviews::read_as(&self.app, review.project.as_str(), item.id)?;
        let remotes = self.app.git.remote_urls(&read.project.root)?;
        let configured = read
            .project
            .workflow
            .as_ref()
            .map(|w| w.integration_branches.clone())
            .unwrap_or_default();
        let integration = IntegrationBranches::new(&configured, &read.pull_request.base.branch);
        Ok(Ctx {
            fix: FixRef {
                base: fix.base_sha.clone(),
                head,
                diff_hash,
            },
            picks: fix.findings.clone(),
            worktree: PathBuf::from(&review.worktree_path),
            read,
            remotes,
            integration,
        })
    }

    fn build(&self, flow: FlowId, ctx: &Ctx, req: &RouteRequest) -> Result<Built, ErrorView> {
        let pr = &ctx.read.pull_request;
        match req.kind {
            RouteKind::FollowUp => follow_up(&ctx.facts(), &req.title, &req.body)
                .map(|plan| Built::Push(Box::new(plan)))
                .map_err(|e| refused(e.0)),
            RouteKind::PushToBranch => push_to_their_branch(&ctx.facts())
                .map(|plan| Built::Push(Box::new(plan)))
                .map_err(|e| refused(e.0)),
            RouteKind::Suggestions => {
                if ctx.head() != ctx.fix.base {
                    return Err(ErrorView::new(
                        ROUTE_MOVED,
                        format!(
                            "#{} has new commits since the fix was made from {}: suggestions on \
                             {} would be on lines that have moved. Re-review the changes, or send \
                             the fix as a follow-up pull request.",
                            pr.number,
                            short(&ctx.fix.base),
                            short(&ctx.fix.base)
                        ),
                    )
                    .with_detail(
                        serde_json::json!({ "reviewed": ctx.fix.base, "current": ctx.head() }),
                    ));
                }
                let db = self.db().map_err(refused)?;
                let findings = db.findings(flow).map_err(|e| reviews::store_error(&e))?;
                let runner = self.app.runner.as_ref();
                let diff = push::diff(
                    runner,
                    &ctx.worktree,
                    &ctx.fix.base,
                    &ctx.fix.head,
                    DiffStyle::Suggestions,
                )?;
                let files = parse_unified(&diff);
                let base = pr
                    .base
                    .sha
                    .as_ref()
                    .map_or_else(|| pr.base.branch.clone(), |sha| sha.as_str().to_owned());
                let lines = self.app.forge.changed_lines(
                    &ctx.read.project.root,
                    &pr.base.repo,
                    &base,
                    &ctx.fix.base,
                )?;
                let worktree = ctx.worktree.clone();
                let commit = ctx.fix.base.clone();
                let original = move |path: &str, line: u32| {
                    push::line_at(runner, &worktree, &commit, path, line)
                        .ok()
                        .flatten()
                };
                let summary = if req.body.trim().is_empty() {
                    default_summary(pr.number)
                } else {
                    req.body.clone()
                };
                let review = suggestions(
                    &SuggestionContext {
                        repo: &pr.base.repo,
                        number: pr.number,
                        commit_id: &ctx.fix.base,
                        findings: &findings,
                        picks: &ctx.picks,
                        lines: &lines,
                        diff: &files,
                    },
                    &original,
                    &summary,
                )
                .map_err(|e| refused(e.0))?;
                Ok(Built::Suggestions(Box::new(SuggestionRoute {
                    review,
                    their: TheirBranch {
                        repo: pr.head.repo.clone(),
                        branch: pr.head.branch.clone(),
                        head: ctx.head().to_owned(),
                    },
                    fix: ctx.fix.clone(),
                })))
            }
        }
    }

    /// Every route for the run's fix, open or not, from a fresh read of the pull request.
    ///
    /// # Errors
    ///
    /// When there is no finished fix to send, or the forge cannot be read.
    pub fn route_options(&self, flow: FlowId) -> Result<RouteOptions, ErrorView> {
        let ctx = self.route_ctx(flow)?;
        let pr = &ctx.read.pull_request;
        let findings: Vec<_> = self
            .db()
            .map_err(refused)?
            .findings(flow)
            .map_err(|e| reviews::store_error(&e))?
            .into_iter()
            .filter(|r| ctx.picks.contains(&r.finding.id))
            .collect();
        let (title, body) = default_follow_up(pr.number, &pr.head.branch, &findings);
        let mut routes = Vec::new();
        for kind in [
            RouteKind::FollowUp,
            RouteKind::Suggestions,
            RouteKind::PushToBranch,
        ] {
            let (title, body) = match kind {
                RouteKind::FollowUp => (title.clone(), body.clone()),
                RouteKind::Suggestions => (String::new(), default_summary(pr.number)),
                RouteKind::PushToBranch => (String::new(), String::new()),
            };
            let req = RouteRequest {
                kind,
                title: title.clone(),
                body: body.clone(),
            };
            let (unavailable, destination) = match self.build(flow, &ctx, &req) {
                Ok(Built::Push(plan)) => (None, Some(destination_line(&plan))),
                Ok(Built::Suggestions(route)) => (
                    None,
                    Some(format!(
                        "{} suggestion comment{} and {} in the summary, on {}#{} at {}",
                        route.review.comments.len(),
                        if route.review.comments.len() == 1 {
                            ""
                        } else {
                            "s"
                        },
                        route.review.folded.len(),
                        route.review.repo,
                        route.review.number,
                        short(&route.review.commit_id)
                    )),
                ),
                Err(error) => (Some(error.message), None),
            };
            routes.push(RouteOption {
                kind,
                unavailable,
                destination,
                title,
                body,
            });
        }
        Ok(RouteOptions {
            their: TheirBranch {
                repo: pr.head.repo.clone(),
                branch: pr.head.branch.clone(),
                head: ctx.head().to_owned(),
            },
            account: ctx.read.account.login.clone(),
            fix: ctx.fix.clone(),
            routes,
        })
    }

    /// Exactly what `req` would send, and the hash approving it takes.
    ///
    /// # Errors
    ///
    /// When the route is closed, as [`Self::route_options`] says.
    pub fn preview_route(
        &self,
        flow: FlowId,
        req: &RouteRequest,
    ) -> Result<RoutePreview, ErrorView> {
        let ctx = self.route_ctx(flow)?;
        let built = self.build(flow, &ctx, req)?;
        let hash = built.hash();
        Ok(match built {
            Built::Push(plan) => {
                let ops = plan.operations(flow);
                let mut steps = Vec::new();
                let mut body = None;
                for op in &ops {
                    match &op.payload {
                        OperationPayload::PushBranch(p) => {
                            steps.push(format!(
                                "git ls-remote --refs -- {} {}",
                                p.destination.remote_arg(),
                                p.destination.reference
                            ));
                            steps.push(
                                push::push_argv(p.destination.remote_arg(), &p.refspec()).join(" "),
                            );
                        }
                        OperationPayload::OpenPullRequest(p) => {
                            let marker = Marker::of(&op.key);
                            steps.push(format!(
                                "gh api --method POST repos/{}/pulls (title, head {}, base {}, \
                                 body below, maintainer_can_modify true)",
                                p.pull_request.repo.full_name(),
                                p.pull_request.head,
                                p.pull_request.base
                            ));
                            body = Some(signed(&p.pull_request.body, &marker.tag()));
                        }
                        _ => {}
                    }
                }
                RoutePreview {
                    kind: plan.kind,
                    hash,
                    diff_hash: plan.fix.diff_hash.clone(),
                    head: ctx.head().to_owned(),
                    plan: Some(*plan),
                    steps,
                    body,
                    suggestions: None,
                }
            }
            Built::Suggestions(route) => {
                let pr = &ctx.read.pull_request;
                let preview = preview_of(
                    route.review.clone(),
                    flow,
                    ctx.read.account.login.clone(),
                    false,
                    ctx.head(),
                    &pr.url,
                );
                RoutePreview {
                    kind: RouteKind::Suggestions,
                    hash,
                    diff_hash: route.fix.diff_hash.clone(),
                    head: ctx.head().to_owned(),
                    plan: None,
                    steps: vec![
                        format!(
                            "gh api --method POST repos/{}/pulls/{}/reviews (a pending review of {} \
                             suggestion{})",
                            route.review.repo.full_name(),
                            route.review.number,
                            route.review.comments.len(),
                            if route.review.comments.len() == 1 {
                                ""
                            } else {
                                "s"
                            }
                        ),
                        format!(
                            "gh api --method POST repos/{}/pulls/{}/reviews/<id>/events (submitted \
                             as a comment)",
                            route.review.repo.full_name(),
                            route.review.number
                        ),
                    ],
                    body: None,
                    suggestions: Some(preview),
                }
            }
        })
    }

    /// Approve exactly what the preview showed, and start sending it.
    ///
    /// # Errors
    ///
    /// When what would be sent is not what was previewed, the route closed meanwhile, or an
    /// earlier send of it is mid-write.
    pub fn approve_route(
        self: &Arc<Self>,
        flow: FlowId,
        req: &RouteRequest,
        previewed: &str,
    ) -> Result<ApprovedRoute, ErrorView> {
        let ctx = self.route_ctx(flow)?;
        let built = self.build(flow, &ctx, req)?;
        if built.hash() != previewed {
            return Err(refused(
                "What would be sent has changed since you previewed it: their branch, the fix, \
                 your remotes or your text. Preview it again.",
            ));
        }
        let db = self.db().map_err(refused)?;
        match built {
            Built::Push(plan) => {
                let approval =
                    NewApproval::route(flow, &plan).map_err(|e| refused(e.to_string()))?;
                for earlier in db.routes(flow).map_err(|e| reviews::store_error(&e))? {
                    if earlier.plan.kind == plan.kind {
                        if self.activity(earlier.approval.id) == Some(Activity::Posting) {
                            return Err(refused(
                                "An earlier send of this route is being sent. Wait for it to \
                                 finish.",
                            ));
                        }
                        self.replace_route(&earlier);
                    }
                }
                let approved = db
                    .approve(&approval, &plan.operations(flow), self.now())
                    .map_err(|e| match e {
                        FlowStoreError::StillSending { .. } => refused(
                            "An earlier send of exactly this was being sent when wtm stopped, \
                             and the remote has not been checked yet. Retry that one instead.",
                        ),
                        other => reviews::store_error(&other),
                    })?;
                tracing::info!(%flow, approval = %approved.approval.id, kind = ?plan.kind,
                    refspec = %plan.refspec(), "approved a route for a local fix");
                self.notice.changed(flow);
                self.send_route(approved.approval.id);
                Ok(ApprovedRoute::Route {
                    route: Box::new(self.route_view(approved.approval.id)?),
                })
            }
            Built::Suggestions(route) => {
                if self.publishing_in(flow) {
                    return Err(refused(
                        "A publish of this review is being sent. Wait for it to finish.",
                    ));
                }
                let approval =
                    NewApproval::suggestions(flow, &route).map_err(|e| refused(e.to_string()))?;
                for earlier in db
                    .publications(flow)
                    .map_err(|e| reviews::store_error(&e))?
                {
                    self.replace(&earlier);
                }
                let approved = db
                    .approve(&approval, &route.review.operations(flow), self.now())
                    .map_err(|e| match e {
                        FlowStoreError::StillSending { .. } => refused(
                            "An earlier publish of exactly these suggestions was being sent when \
                             wtm stopped. Retry that one instead.",
                        ),
                        other => reviews::store_error(&other),
                    })?;
                tracing::info!(%flow, approval = %approved.approval.id,
                    "approved posting a local fix as suggestions");
                self.notice.changed(flow);
                self.send(approved.approval.id);
                Ok(ApprovedRoute::Suggestions {
                    publication: Box::new(self.publication_view(approved.approval.id)?),
                })
            }
        }
    }

    /// Send what is left of a route that failed or stopped.
    ///
    /// # Errors
    ///
    /// When it waits for a new approval because their branch moved, or it does not exist.
    pub fn retry_route(self: &Arc<Self>, approval: ApprovalId) -> Result<RouteSendView, ErrorView> {
        let route = self.route_of(approval)?;
        for op in &route.operations {
            match (&op.state, &op.attention) {
                (OperationState::NeedsAttention, Some(Attention::HeadMoved { .. })) => {
                    return Err(refused(
                        "Their branch moved after you approved this, so it needs a new approval \
                         rather than a retry.",
                    ));
                }
                (OperationState::NeedsAttention, _) => {
                    self.change(op.id, &OperationChange::to(OperationState::Pending));
                }
                _ => {}
            }
        }
        self.send_route(approval);
        self.route_view(approval)
    }

    /// Stop a route where it is. What was pushed stays pushed.
    ///
    /// # Errors
    ///
    /// When it does not exist, or is being sent right now.
    pub fn cancel_route(&self, approval: ApprovalId) -> Result<RouteSendView, ErrorView> {
        if self.activity(approval) == Some(Activity::Posting) {
            return Err(refused(
                "It is being sent right now. Wait a moment and look again.",
            ));
        }
        let route = self.route_of(approval)?;
        self.replace_route(&route);
        self.notice.changed(route.approval.flow);
        self.route_view(approval)
    }

    fn replace_route(&self, route: &RouteSend) {
        for op in &route.operations {
            if matches!(
                op.state,
                OperationState::Pending | OperationState::Failed | OperationState::NeedsAttention
            ) {
                self.change(op.id, &OperationChange::to(OperationState::Cancelled));
            }
        }
        self.settle_publication(route.approval.flow, route.approval.id);
    }

    fn route_of(&self, approval: ApprovalId) -> Result<RouteSend, ErrorView> {
        self.db()
            .map_err(refused)?
            .route(approval)
            .map_err(|e| reviews::store_error(&e))?
            .ok_or_else(|| refused(format!("no route {approval}")))
    }

    /// One route's send as the window shows it.
    ///
    /// # Errors
    ///
    /// When it does not exist or the database cannot be read.
    pub fn route_view(&self, approval: ApprovalId) -> Result<RouteSendView, ErrorView> {
        Ok(self.route_view_of(self.route_of(approval)?))
    }

    fn route_view_of(&self, route: RouteSend) -> RouteSendView {
        let state = route.state(self.activity(route.approval.id));
        let url = route
            .operations
            .iter()
            .rev()
            .find_map(|op| op.result.as_ref().and_then(OperationResult::url))
            .map(str::to_owned);
        RouteSendView { route, state, url }
    }

    /// A run's push routes, oldest first.
    ///
    /// Without the approvals left with no operation: approving the same push again moves its one
    /// write to the new approval, and the earlier approval would otherwise be drawn as a route that
    /// was never sent, with a Retry that has nothing to send. The click-through found exactly that,
    /// beside the card of the push that went.
    pub(super) fn route_views(&self, flow: FlowId) -> Result<Vec<RouteSendView>, String> {
        let db = self.db()?;
        Ok(db
            .routes(flow)
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|route| !route.operations.is_empty())
            .map(|route| self.route_view_of(route))
            .collect())
    }

    // ═══════════════════════════════ the executor ═══════════════════════════════

    /// Send what is left of a route, on a thread of its own. `false` when this process is
    /// sending or checking it already.
    pub(super) fn send_route(self: &Arc<Self>, approval: ApprovalId) -> bool {
        if !self.claim(approval, Activity::Posting) {
            return false;
        }
        let engine = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name(format!("route-{approval}"))
            .spawn(move || {
                engine.send_route_all(approval);
                engine.release(approval);
                if let Ok(Some(route)) = engine.db().map(|db| db.route(approval).ok().flatten()) {
                    engine.notice.changed(route.approval.flow);
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "could not start sending a route");
            self.release(approval);
            return false;
        }
        true
    }

    fn send_route_all(&self, approval: ApprovalId) {
        loop {
            let Some(route) = self
                .db()
                .ok()
                .and_then(|db| db.route(approval).ok().flatten())
            else {
                return;
            };
            let Some(op) = route.next().cloned() else {
                self.settle_publication(route.approval.flow, approval);
                tracing::info!(%approval, kind = ?route.plan.kind, "sent a local fix's route");
                return;
            };
            let next = match op.state {
                OperationState::Cancelled
                | OperationState::NeedsAttention
                | OperationState::Done => false,
                OperationState::Executing => matches!(
                    self.settle_route_unsent(&route, &op),
                    Some(OperationState::Done | OperationState::Failed)
                ),
                OperationState::Pending | OperationState::Failed => {
                    self.send_route_one(&route, &op)
                }
            };
            if !next {
                return;
            }
        }
    }

    /// The pull request and your sign-in, read now, for a route of `flow`.
    fn read_for(&self, flow: FlowId) -> Result<ReadNow, String> {
        let db = self.db()?;
        let state = db
            .flow(flow)
            .map_err(|e| e.to_string())?
            .ok_or("the review is gone")?;
        let review = db
            .review(flow)
            .map_err(|e| e.to_string())?
            .ok_or("the review is gone")?;
        reviews::read_as(&self.app, review.project.as_str(), state.work_item).map_err(|e| e.message)
    }

    /// One operation of a route. `true` to go on to the next.
    fn send_route_one(&self, route: &RouteSend, op: &Operation) -> bool {
        let read = match self.read_for(route.approval.flow) {
            Ok(read) => read,
            Err(error) => {
                self.route_failed(route, op, &format!("GitHub could not be read: {error}"));
                return false;
            }
        };
        let root = read.project.root.clone();
        match &op.payload {
            OperationPayload::PushBranch(payload) => {
                self.push_one(route, op, payload, &read, &root)
            }
            OperationPayload::OpenPullRequest(payload) => {
                if !self.still_approved(route, op, &read) {
                    return false;
                }
                self.open_one(route, op, payload, &root)
            }
            _ => {
                self.route_failed(route, op, "this is a review's write, not a route's");
                false
            }
        }
    }

    /// Whether the route's approval still covers it, with their branch where the forge says it is
    /// now and the fix as it is now. Stops it and asks again when not.
    fn still_approved(&self, route: &RouteSend, op: &Operation, read: &ReadNow) -> bool {
        let head = read.pull_request.head.sha.as_str();
        let diff = self
            .fix_diff_now(route.approval.flow)
            .unwrap_or_else(|| route.plan.fix.diff_hash.clone());
        match route.approval.check(&route.plan.binding_now(head, &diff)) {
            Ok(()) => true,
            Err(stale) => {
                tracing::info!(approval = %route.approval.id, changes = stale.changes.len(),
                    "a route stopped: what it covered changed after it was approved");
                self.moved(route, op, &route.plan.their.head, head);
                false
            }
        }
    }

    /// Their branch is not where the approval saw it: stop, send nothing more, and ask again.
    fn moved(&self, route: &RouteSend, op: &Operation, approved: &str, current: &str) {
        self.change(
            op.id,
            &OperationChange::stopped(Attention::HeadMoved {
                approved: approved.to_owned(),
                current: current.to_owned(),
            }),
        );
        let number = route.plan.pull_request.number;
        self.file(&NewInboxItem {
            flow: route.approval.flow,
            kind: InboxKind::Approval,
            target: InboxTarget::Approval {
                approval: route.approval.id,
            },
            title: format!(
                "Sending your fix of #{number} stopped: their branch moved after you approved"
            ),
            body: format!(
                "You approved {} from {}, and `{}` is at {} now. {} Re-review the changes, or send \
                 the fix as a follow-up pull request.",
                route_words(route.plan.kind),
                short(approved),
                route.plan.their.branch,
                if current.is_empty() {
                    "nothing"
                } else {
                    short(current)
                },
                if route.done() == 0 {
                    "Nothing was pushed."
                } else {
                    "The fix's branch is pushed; nothing more was sent."
                }
            ),
        });
    }

    fn push_one(
        &self,
        route: &RouteSend,
        op: &Operation,
        payload: &PushPayload,
        read: &ReadNow,
        root: &std::path::Path,
    ) -> bool {
        let runner = self.app.runner.as_ref();
        let remote = payload.destination.remote_arg();
        let reference = &payload.destination.reference;
        if self.fault == Some(super::outbox::Fault::StallBeforePush) {
            tracing::warn!(
                "WTM_DEV_PUBLISH_FAULT: holding a push before it reads the remote branch"
            );
            for _ in 0..super::outbox::PUSH_STALL_STEPS {
                if self.quitting() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        }
        let now = match push::remote_head(runner, root, remote, reference) {
            Ok(now) => now.map(|c| c.as_str().to_owned()),
            Err(error) => {
                self.route_failed(
                    route,
                    op,
                    &format!("The remote could not be read, so nothing was pushed: {error}."),
                );
                return false;
            }
        };
        // Already there is adopted before anything else is asked: a push whose answer was lost
        // has moved their branch to the fix, and that is this route having happened, not a move.
        if now.as_deref() == Some(payload.commit.as_str()) {
            return self.pushed(op, payload, true);
        }
        if !self.still_approved(route, op, read) {
            return false;
        }
        match before_push(&payload.expect, &payload.commit, now.as_deref()) {
            BeforePush::AlreadyThere => return self.pushed(op, payload, true),
            BeforePush::Moved { expected, current } => {
                match expected {
                    Some(expected) => {
                        self.moved(route, op, &expected, current.as_deref().unwrap_or_default());
                    }
                    None => self.route_refused(
                        route,
                        op,
                        &format!(
                            "`{}` already exists on {} at {}, and wtm pushes a new branch only \
                             where there is none.",
                            reference.strip_prefix("refs/heads/").unwrap_or(reference),
                            payload.destination.url,
                            short(current.as_deref().unwrap_or_default())
                        ),
                    ),
                }
                return false;
            }
            BeforePush::Send => {}
        }
        if self.change(op.id, &OperationChange::sending()).is_none() {
            return false;
        }
        match push::push(runner, root, remote, &payload.refspec()) {
            Ok(Pushed::Updated | Pushed::UpToDate) => self.pushed(op, payload, false),
            Ok(Pushed::Rejected { reason }) => {
                // Refused by the remote: nothing changed there. Read it to say why.
                let now = push::remote_head(runner, root, remote, reference)
                    .ok()
                    .flatten()
                    .map(|c| c.as_str().to_owned());
                if now.as_deref() == Some(payload.commit.as_str()) {
                    return self.pushed(op, payload, true);
                }
                match &payload.expect {
                    wtm_core::flow::Expect::At { sha } => {
                        self.moved(route, op, sha, now.as_deref().unwrap_or_default());
                    }
                    wtm_core::flow::Expect::Absent => self.route_refused(
                        route,
                        op,
                        &format!("The remote refused the push: {reason}"),
                    ),
                }
                false
            }
            Err(error) => {
                // A push whose outcome is unknown is settled by reading the remote again.
                let now = push::remote_head(runner, root, remote, reference);
                match now {
                    Ok(Some(at)) if at.as_str() == payload.commit => self.pushed(op, payload, true),
                    Ok(_) => {
                        self.route_failed(route, op, &format!("The push did not happen: {error}"));
                        false
                    }
                    Err(read) => {
                        self.route_failed(
                            route,
                            op,
                            &format!(
                                "{error}. The remote could not be read afterwards to see whether \
                                 it arrived ({read}); Retry checks before pushing anything."
                            ),
                        );
                        false
                    }
                }
            }
        }
    }

    fn pushed(&self, op: &Operation, payload: &PushPayload, adopted: bool) -> bool {
        if adopted {
            tracing::info!(op = %op.id, "found the fix already on the remote branch; not pushing it again");
        }
        self.change(
            op.id,
            &OperationChange::done(OperationResult::Pushed {
                url: payload.destination.url.clone(),
                reference: payload.destination.reference.clone(),
                commit: payload.commit.clone(),
                adopted,
            }),
        )
        .is_some()
    }

    fn open_one(
        &self,
        route: &RouteSend,
        op: &Operation,
        payload: &OpenPullRequestPayload,
        root: &std::path::Path,
    ) -> bool {
        let marker = Marker::of(&op.key);
        let pr = &payload.pull_request;
        let head = head_label(pr);
        let writer = &self.app.pr_writer;
        let find = |found: &[wtm_core::ports::RemotePullRequest]| {
            found
                .iter()
                .find(|p| marker.signs(&p.body))
                .or_else(|| {
                    found
                        .iter()
                        .find(|p| p.base == pr.base && p.state == wtm_core::flow::PrState::Open)
                })
                .cloned()
        };
        match writer.pull_requests_from(root, &pr.repo, &head) {
            Ok(found) => {
                if let Some(existing) = find(&found) {
                    return self.opened(op, &existing, true);
                }
            }
            Err(error) => {
                self.route_failed(
                    route,
                    op,
                    &format!("GitHub could not be read, so nothing was opened: {error}."),
                );
                return false;
            }
        }
        if self.change(op.id, &OperationChange::sending()).is_none() {
            return false;
        }
        let body = signed(&pr.body, &marker.tag());
        match writer.open_pull_request(root, &pr.repo, &pr.base, &pr.head, &pr.title, &body) {
            Ok(opened) => self.opened(op, &opened, false),
            Err(error) => {
                let again = writer.pull_requests_from(root, &pr.repo, &head);
                if let Ok(found) = &again
                    && let Some(existing) = find(found)
                {
                    return self.opened(op, &existing, true);
                }
                match error {
                    ForgeError::Rejected { reason, .. } => self.route_refused(route, op, &reason),
                    other => self.route_failed(route, op, &other.to_string()),
                }
                false
            }
        }
    }

    fn opened(
        &self,
        op: &Operation,
        found: &wtm_core::ports::RemotePullRequest,
        adopted: bool,
    ) -> bool {
        self.change(
            op.id,
            &OperationChange::done(OperationResult::PullRequest {
                number: found.number,
                url: found.url.clone(),
                adopted,
            }),
        )
        .is_some()
    }

    fn route_failed(&self, route: &RouteSend, op: &Operation, error: &str) {
        self.change(op.id, &OperationChange::failed(error));
        let number = route.plan.pull_request.number;
        self.file(&NewInboxItem {
            flow: route.approval.flow,
            kind: InboxKind::Failure,
            target: InboxTarget::Approval {
                approval: route.approval.id,
            },
            title: format!("Your fix of #{number} was not sent"),
            body: format!(
                "{error} {} Retry reads the remote first, so nothing is sent twice.",
                route_progress(route)
            ),
        });
    }

    fn route_refused(&self, route: &RouteSend, op: &Operation, message: &str) {
        self.change(
            op.id,
            &OperationChange::stopped(Attention::Refused {
                message: message.to_owned(),
            }),
        );
        let number = route.plan.pull_request.number;
        self.file(&NewInboxItem {
            flow: route.approval.flow,
            kind: InboxKind::Failure,
            target: InboxTarget::Approval {
                approval: route.approval.id,
            },
            title: format!("Sending your fix of #{number} was refused"),
            body: format!("{message} {}", route_progress(route)),
        });
    }

    /// Settle a route's operation a crash left executing, by reading the remote. Sends nothing.
    pub(super) fn settle_route_unsent(
        &self,
        route: &RouteSend,
        op: &Operation,
    ) -> Option<OperationState> {
        let read = match self.read_for(route.approval.flow) {
            Ok(read) => read,
            Err(error) => {
                return self
                    .change(
                        op.id,
                        &OperationChange::failed(format!(
                            "wtm stopped while sending this, and GitHub could not be read to see \
                             whether it arrived: {error}. Retry checks before sending anything."
                        )),
                    )
                    .map(|op| op.state);
            }
        };
        let root = read.project.root;
        let gone = "wtm stopped while sending this, and the remote shows it did not arrive. Retry \
                    sends it.";
        let change = match &op.payload {
            OperationPayload::PushBranch(p) => match push::remote_head(
                self.app.runner.as_ref(),
                &root,
                p.destination.remote_arg(),
                &p.destination.reference,
            ) {
                Ok(Some(at)) if at.as_str() == p.commit => {
                    OperationChange::done(OperationResult::Pushed {
                        url: p.destination.url.clone(),
                        reference: p.destination.reference.clone(),
                        commit: p.commit.clone(),
                        adopted: true,
                    })
                }
                Ok(_) => OperationChange::failed(gone),
                Err(error) => OperationChange::failed(format!(
                    "wtm stopped while pushing this, and the remote could not be read: {error}."
                )),
            },
            OperationPayload::OpenPullRequest(p) => {
                let marker = Marker::of(&op.key);
                match self.app.pr_writer.pull_requests_from(
                    &root,
                    &p.pull_request.repo,
                    &head_label(&p.pull_request),
                ) {
                    Ok(found) => match found.iter().find(|pr| marker.signs(&pr.body)) {
                        Some(pr) => OperationChange::done(OperationResult::PullRequest {
                            number: pr.number,
                            url: pr.url.clone(),
                            adopted: true,
                        }),
                        None => OperationChange::failed(gone),
                    },
                    Err(error) => OperationChange::failed(format!(
                        "wtm stopped while opening this, and GitHub could not be read: {error}."
                    )),
                }
            }
            _ => OperationChange::failed("this is a review's write, not a route's"),
        };
        tracing::info!(op = %op.id, to = ?change.state, "settled a route's write a crash left mid-write");
        self.change(op.id, &change).map(|op| op.state)
    }

    /// At launch, for an operation a crash left executing: settle it if it is a route's. `false`
    /// when it is not, which leaves it to the review's recovery.
    pub(super) fn recover_route(&self, approval: ApprovalId, op: &Operation) -> bool {
        let Ok(db) = self.db() else { return false };
        let Ok(Some(route)) = db.route(approval) else {
            return false;
        };
        self.settle_route_unsent(&route, op);
        if let Ok(Some(now)) = db.route(approval)
            && now.state(None) != PublicationState::Published
        {
            let number = now.plan.pull_request.number;
            self.file(&NewInboxItem {
                flow: now.approval.flow,
                kind: InboxKind::Failure,
                target: InboxTarget::Approval { approval },
                title: format!("wtm stopped while sending your fix of #{number}"),
                body: format!(
                    "It was checked at launch. {} Retry checks again and sends only what is \
                     missing.",
                    route_progress(&now)
                ),
            });
        }
        true
    }
}

/// `owner:branch`, which is how GitHub's list of pull requests is asked about a head.
fn head_label(pr: &wtm_core::flow::FollowUpPr) -> String {
    if pr.head.contains(':') {
        pr.head.clone()
    } else {
        format!("{}:{}", pr.repo.owner, pr.head)
    }
}

fn route_words(kind: RouteKind) -> &'static str {
    match kind {
        RouteKind::FollowUp => "a follow-up pull request",
        RouteKind::Suggestions => "suggestions",
        RouteKind::PushToBranch => "a push to their branch",
    }
}

/// What of a route has reached the remote, in a sentence.
fn route_progress(route: &RouteSend) -> &'static str {
    match (route.plan.kind, route.done()) {
        (_, 0) => "Nothing was pushed.",
        (RouteKind::FollowUp, _) => "The fix's branch is pushed; the pull request is not open yet.",
        _ => "The fix is pushed.",
    }
}

/// Exactly where a push route goes, in a line.
fn destination_line(plan: &RoutePlan) -> String {
    let remote = plan.push.remote.as_deref().map_or_else(
        || format!("{} (by URL)", plan.push.url),
        |name| format!("`{name}` ({})", plan.push.url),
    );
    match &plan.open {
        Some(open) => format!(
            "Push {} to {remote} as `{}`, then open a pull request in {}: `{}` into `{}`",
            short(&plan.fix.head),
            plan.push.reference,
            open.repo,
            open.head,
            open.base
        ),
        None => format!(
            "Push {} to {remote} as `{}`, a fast-forward from {}",
            short(&plan.fix.head),
            plan.push.reference,
            short(&plan.their.head)
        ),
    }
}
