//! The issue's own pull request, written through the outbox: the draft each hand-over makes, "Mark
//! ready + Needs Review", following it, and the Merge sheet (`wtm_core::flow::own_pr` for the rules).
//!
//! # In order
//!
//! 1. **The draft, at each hand-over.** Ready to test pushes the task branch's commit to the
//!    project's remote and creates the draft pull request, or updates the one the branch has, under
//!    an approval the app grants itself because the plan made the draft automatic. It pins the
//!    remote, the ref, the diff and the commit; the push is a fast-forward of where the remote
//!    branch is, and a remote branch at a commit this worktree does not have stops it.
//! 2. **Ready + Needs Review**, from its sheet: one approval of yours over two operations, the pull
//!    request marked ready and the issue moved to the mapped status, bound to the head you saw.
//!    The run is Publishing while they go, and In review once both are done. Each is read before it
//!    is sent, so Retry after a partial success sends only what is missing.
//! 3. **Following it**, on demand and when the window comes back, never on a timer: its checks,
//!    reviews and merge state, kept as the work item's reading. A review someone else leaves asking
//!    for changes, or with something to say, is kept the first time it is read and is news once;
//!    a revision from it is one click, and takes it in.
//! 4. **The Merge sheet** is the only way to merge: an approval of exactly one head and one method,
//!    sent as `gh pr merge --match-head-commit`, so GitHub refuses a head that moved after you
//!    looked. Merged, the run is done.
//!
//! # Every write is read first, and a crash is checked, never re-sent
//!
//! As for every outbox write (`outbox.rs`): each operation reads the remote before it is marked
//! executing and sent, adopts a result already there, and is settled by reading at launch when a
//! crash left it executing ([`Engine::recover_own`]).
//!
//! # After a writer's round, what the remote holds
//!
//! [`Engine::watch_remote`] reads the task branch and the integration branches with `git ls-remote`
//! and says, in the Inbox, when either holds something wtm did not put there.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use wtm_core::flow::moves::{arrived, step};
use wtm_core::flow::publish::{Marker, signed};
use wtm_core::flow::route::{BeforePush, before_push, diff_hash};
use wtm_core::flow::{
    Activity, Approval, ApprovalId, Attention, Binding, Destination, DraftFacts, DraftPlan, Expect,
    FeedbackComment, FlowEvent, FlowId, ForgeKind, Implementation, InboxKind, InboxTarget,
    IntegrationBranches, MergeMethod, MergeRequest, MirroredPr, MoveIssuePayload, MoveStep,
    NewApproval, NewInboxItem, Operation, OperationChange, OperationPayload, OperationResult,
    OperationState, OwnRequest, OwnSend, Phase, Pinned, PrFeedback, PrState, PrTarget, PullRequest,
    PullRequestRef, ReadyRequest, RepoSlug, StatusRole, TaskRemote, TrackerKind, Unexplained,
    WorkItemId, WorkLink, feedback_note, is_feedback, unexplained,
};
use wtm_core::ports::{
    FlowStore, FlowStoreError, ImplementStore, InboxStore, OutboxStore, PullReviewStore, StartStore,
};
use wtm_git::push::{self, DiffStyle, Pushed};

use super::{Engine, Refusal, text};

/// How often, at most, the window's focus reads the pull request again. On demand is any time.
pub const FOCUS_GAP_MS: u64 = 60_000;

/// Where the issue's own pull request is, read from its run, project and worktree.
struct OwnTarget {
    flow: FlowId,
    key: String,
    /// The issue's summary, the pull request's title after the key.
    title: String,
    item: WorkItemId,
    root: PathBuf,
    worktree: PathBuf,
    branch: String,
    base: String,
    remote: String,
    url: String,
    repo: RepoSlug,
    integration: IntegrationBranches,
    tracker: TrackerKind,
    implementation: Implementation,
}

/// One approval of the issue's own pull request, as the window shows it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OwnSendView {
    #[serde(flatten)]
    pub send: OwnSend,
    pub state: wtm_core::flow::PublicationState,
}

/// The issue's own pull request: where it is, what the forge last said, what was sent, and the
/// feedback it got.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(clippy::struct_excessive_bools)]
pub struct OwnPrView {
    pub flow: FlowId,
    /// Why nothing is drafted, when nothing can be: no forge, no remote, an integration branch.
    pub unavailable: Option<String>,
    pub branch: Option<String>,
    pub base: Option<String>,
    pub repo: Option<String>,
    pub pull_request: Option<PullRequestRef>,
    pub url: Option<String>,
    /// The forge's last reading of it.
    pub reading: Option<MirroredPr>,
    /// Drafts, Ready + Needs Review, merges, oldest first.
    pub sends: Vec<OwnSendView>,
    pub feedback: Vec<PrFeedback>,
    pub can_publish: bool,
    pub can_merge: bool,
    pub can_sync: bool,
    /// Feedback no revision has taken in yet, and a revision may be started from it.
    pub can_revise_feedback: bool,
}

/// What "Mark ready + Needs Review" will do, read fresh for its sheet.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadySheet {
    pub flow: FlowId,
    pub key: String,
    pub repo: String,
    pub number: u64,
    pub url: String,
    pub draft: bool,
    /// The pull request's head now: what the approval binds.
    pub head: String,
    /// The worktree's `HEAD`: the hand-over the checks and the review read.
    pub handed_over: String,
    pub tracker_from: Option<String>,
    pub tracker_to: Option<String>,
    /// What stands in the way, in sentences. Approve is offered only when empty.
    pub problems: Vec<String>,
}

/// What the Merge sheet shows, read fresh.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MergeSheet {
    pub flow: FlowId,
    pub key: String,
    pub repo: String,
    pub number: u64,
    pub url: String,
    pub branch: String,
    pub base: String,
    pub head: String,
    pub pull_request: PullRequest,
    pub methods: Vec<MergeMethod>,
    pub problems: Vec<String>,
}

fn short(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

/// The branch a base ref names, without the remote it was read from: `origin/develop` is
/// `develop`, `develop` is itself.
fn base_branch(base: &str, remotes: &[String]) -> String {
    remotes
        .iter()
        .find_map(|remote| base.strip_prefix(&format!("{remote}/")))
        .unwrap_or(base)
        .to_owned()
}

impl Engine {
    // ═══════════════════════════════ where ═══════════════════════════════

    /// Whether the project's workflow names a forge to publish to.
    fn forge_of(project: &wtm_core::model::Project) -> Option<ForgeKind> {
        project.workflow.as_ref().and_then(|w| w.forge)
    }

    fn own_target(&self, flow: FlowId) -> Result<OwnTarget, String> {
        let db = self.db()?;
        let implementation = db
            .implementation(flow)
            .map_err(text)?
            .ok_or("this run has implemented nothing")?;
        let state = db.flow(flow).map_err(text)?.ok_or("the run has gone")?;
        let item = db
            .work_item(state.work_item)
            .map_err(text)?
            .ok_or("the work item has gone")?;
        let (project, tracker) =
            crate::tracker::tracker_project(&self.app, implementation.project.as_str())
                .map_err(|e| e.message)?;
        if Self::forge_of(&project).is_none() {
            return Err(
                "the project's workflow names no forge, so nothing leaves this Mac; add \
                        `forge = \"github\"` to its [workflow] in your own config"
                    .to_owned(),
            );
        }
        let worktree = self
            .app
            .worktree(&project, &implementation.worktree)
            .map_err(|e| format!("the issue's worktree is not available: {e}"))?;
        let branch = worktree
            .branch()
            .map(|b| b.as_str().to_owned())
            .ok_or("the issue's worktree is not on a branch, so there is nothing to push")?;
        let remotes = self.app.git.remote_urls(&project.root).map_err(text)?;
        let remote = remotes
            .iter()
            .find(|r| r.name == "origin")
            .or_else(|| remotes.first())
            .ok_or("the repository has no remote to push the task branch to")?;
        let repo = RepoSlug::parse_url(&remote.url).ok_or_else(|| {
            format!(
                "`{}` is not a repository wtm knows how to open a pull request on",
                remote.url
            )
        })?;
        let names: Vec<String> = remotes.iter().map(|r| r.name.clone()).collect();
        let started = db.start_job_of(flow).map_err(text)?;
        let configured = project
            .workflow
            .as_ref()
            .map(|w| w.integration_branches.clone())
            .unwrap_or_default();
        let base = match started.map(|job| job.candidate) {
            Some(wtm_core::flow::Candidate::New { base, .. }) => base_branch(&base, &names),
            _ => configured
                .iter()
                .filter(|b| !b.contains('*'))
                .chain(["main".to_owned(), "master".to_owned()].iter())
                .find(|b| {
                    self.app
                        .git
                        .rev_parse(&project.root, &format!("{}/{b}", remote.name))
                        .ok()
                        .flatten()
                        .is_some()
                })
                .cloned()
                .unwrap_or_else(|| "main".to_owned()),
        };
        Ok(OwnTarget {
            flow,
            key: implementation.key.clone(),
            title: item.title.clone(),
            item: item.id,
            root: project.root.clone(),
            worktree: worktree.path.clone(),
            integration: IntegrationBranches::new(&configured, &base),
            branch,
            base,
            remote: remote.name.clone(),
            url: remote.url.clone(),
            repo,
            tracker,
            implementation,
        })
    }

    /// The pull request the run's drafts opened or adopted, newest first.
    fn own_number(&self, flow: FlowId) -> Option<(u64, String)> {
        let db = self.db().ok()?;
        db.own_sends(flow)
            .ok()?
            .iter()
            .rev()
            .flat_map(|send| send.operations.iter().rev())
            .find_map(|op| match &op.result {
                Some(OperationResult::PullRequest { number, url, .. }) => {
                    Some((*number, url.clone()))
                }
                _ => None,
            })
    }

    /// The commit the outbox last pushed to the task branch.
    fn last_pushed(&self, flow: FlowId) -> Option<String> {
        self.db()
            .ok()?
            .own_sends(flow)
            .ok()?
            .iter()
            .rev()
            .flat_map(|send| send.operations.iter().rev())
            .find_map(|op| match &op.result {
                Some(OperationResult::Pushed { commit, .. }) => Some(commit.clone()),
                _ => None,
            })
    }

    /// Whether this process is sending one of `flow`'s own writes, or one is left executing.
    pub(super) fn publishing_now(&self, flow: FlowId) -> bool {
        let Ok(db) = self.db() else { return false };
        db.own_sends(flow).unwrap_or_default().iter().any(|send| {
            self.activity(send.approval.id).is_some()
                || send
                    .operations
                    .iter()
                    .any(|op| op.state == OperationState::Executing)
        })
    }

    /// Whether `commit` is in the worktree's history: here, and an ancestor of `HEAD`.
    fn ours(&self, t: &OwnTarget, commit: &str) -> bool {
        self.app
            .git
            .merge_base(&t.worktree, commit, "HEAD")
            .ok()
            .flatten()
            .is_some_and(|base| base.as_str() == commit)
    }

    // ═══════════════════════════════ the draft ═══════════════════════════════

    /// At a hand-over: push the task branch and create or update its draft, on a thread of its own.
    /// What stops it is said in the Inbox; a project with no forge drafts nothing and says nothing.
    pub(super) fn draft_after_handover(self: &Arc<Self>, flow: FlowId) {
        let configured = self
            .db()
            .ok()
            .and_then(|db| db.implementation(flow).ok().flatten())
            .and_then(|i| self.app.project(i.project.as_str()).ok())
            .is_some_and(|p| Self::forge_of(&p).is_some());
        if !configured {
            return;
        }
        let engine = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name(format!("draft-{flow}"))
            .spawn(move || {
                if let Err(reason) = engine.draft(flow) {
                    tracing::info!(%flow, %reason, "the hand-over's draft was not sent");
                    let key = engine.key_of_run(flow);
                    engine.file(&NewInboxItem {
                        flow,
                        kind: InboxKind::Failure,
                        target: InboxTarget::Flow,
                        title: format!("{key}: its draft pull request was not sent"),
                        body: format!(
                            "{}. The hand-over stands, and nothing left this Mac. The next \
                             hand-over drafts it again.",
                            super::capitalise(&reason)
                        ),
                    });
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "could not start drafting a pull request");
        }
    }

    fn key_of_run(&self, flow: FlowId) -> String {
        self.db()
            .ok()
            .and_then(|db| db.implementation(flow).ok().flatten())
            .map_or_else(|| "The issue".to_owned(), |i| i.key)
    }

    /// Plan, approve and send the hand-over's draft. `None` when there is nothing to send: the
    /// remote branch and the pull request already show this hand-over.
    fn draft(self: &Arc<Self>, flow: FlowId) -> Result<Option<ApprovalId>, String> {
        let t = self.own_target(flow)?;
        let runner = self.app.runner.as_ref();
        let head = self
            .app
            .git
            .rev_parse(&t.worktree, "HEAD")
            .map_err(text)?
            .map(|c| c.as_str().to_owned())
            .ok_or("the issue's worktree has no commit to push")?;
        let reference = format!("refs/heads/{}", t.branch);
        let now = push::remote_head(runner, &t.worktree, &t.remote, &reference)
            .map_err(|e| format!("the remote could not be read, so nothing was pushed: {e}"))?
            .map(|c| c.as_str().to_owned());
        let pushed = self.last_pushed(flow);
        let found = unexplained(
            &TaskRemote {
                branch: &t.branch,
                pushed: pushed.as_deref(),
                now: now.as_deref(),
                now_is_ours: now.as_deref().is_some_and(|now| self.ours(&t, now)),
            },
            &BTreeSet::new(),
            &[],
        );
        if !found.is_empty() {
            return Err(unexplained_words(&t, &found));
        }
        let expect = match &now {
            None => Expect::Absent,
            Some(now) => Expect::At { sha: now.clone() },
        };
        let base_commit = self
            .app
            .git
            .merge_base(&t.worktree, &format!("{}/{}", t.remote, t.base), &head)
            .ok()
            .flatten()
            .map_or_else(
                || t.implementation.base_sha.clone(),
                |c| c.as_str().to_owned(),
            );
        let binary = push::diff(runner, &t.worktree, &base_commit, &head, DiffStyle::Binary)
            .map_err(text)?;
        let label = format!("{}:{}", t.repo.owner, t.branch);
        let existing = self
            .app
            .pr_writer
            .pull_requests_from(&t.root, &t.repo, &label)
            .map_err(|e| format!("GitHub could not be read, so nothing was drafted: {e}"))?
            .into_iter()
            .find(|p| p.state == PrState::Open && p.base == t.base)
            .map(|p| p.number);
        let body = self.draft_body(&t)?;
        let title = format!("{}: {}", t.key, t.title.trim());
        let plan = DraftPlan::new(
            DraftFacts {
                key: &t.key,
                destination: Destination {
                    remote: Some(t.remote.clone()),
                    url: t.url.clone(),
                    repo: t.repo.clone(),
                    reference,
                },
                branch: &t.branch,
                base: &t.base,
                commit: &head,
                expect,
                diff_hash: &diff_hash(&binary),
                title: &title,
                body: &body,
                pull_request: existing,
            },
            &t.integration,
        )
        .map_err(|e| e.0)?;
        let db = self.db()?;
        let approval = NewApproval::draft(flow, &plan).map_err(text)?;
        let approved = match db.approve(&approval, &plan.operations(flow), self.now()) {
            Ok(approved) => approved,
            Err(FlowStoreError::StillSending { .. }) => {
                return Err("an earlier draft is still being sent".to_owned());
            }
            Err(e) => return Err(text(e)),
        };
        if approved
            .operations
            .iter()
            .all(|op| op.state == OperationState::Done)
        {
            return Ok(None);
        }
        let id = approved.approval.id;
        self.notice.changed(flow);
        self.send_own(id);
        Ok(Some(id))
    }

    /// The draft's description: what the hand-over panel shows, as text a reviewer can read on the
    /// pull request.
    fn draft_body(&self, t: &OwnTarget) -> Result<String, String> {
        use std::fmt::Write as _;
        let handoff = self.handoff_view(t.flow)?;
        let db = self.db()?;
        let rounds = db.rounds(t.flow).map_err(text)?;
        let mut out = String::new();
        let _ = writeln!(out, "## What was done\n");
        for round in rounds.iter().filter(|r| r.handoff.is_some()) {
            if let Some(handoff) = &round.handoff {
                let _ = writeln!(out, "- {}", handoff.summary.trim().replace('\n', " "));
            }
        }
        if !handoff.coverage.is_empty() {
            let _ = writeln!(out, "\n## Acceptance criteria\n");
            for criterion in &handoff.coverage {
                let said = match &criterion.claim {
                    Some(claim) => format!(
                        "the implementer says {}: {}",
                        if claim.met { "met" } else { "not met" },
                        claim.evidence.trim()
                    ),
                    None => "the implementer said nothing about it".to_owned(),
                };
                let _ = writeln!(
                    out,
                    "{}. {} — {said}",
                    criterion.criterion,
                    criterion.text.trim()
                );
            }
        }
        if !handoff.checks.is_empty() {
            let _ = writeln!(out, "\n## Checks run by wtm\n");
            for check in &handoff.checks {
                let _ = writeln!(
                    out,
                    "- `{}`: {}",
                    check.run.check,
                    serde_json::to_value(check.run.status)
                        .ok()
                        .and_then(|v| v.as_str().map(str::to_owned))
                        .unwrap_or_default()
                        .replace('_', " ")
                );
            }
        }
        if !t.implementation.remaining.is_empty() {
            let _ = writeln!(out, "\n## Left for review\n");
            for left in &t.implementation.remaining {
                let _ = writeln!(out, "- {}", left.title.trim());
            }
        }
        let _ = write!(
            out,
            "\n_Drafted by wtm from `{}` at `{}`; this description is rewritten at each hand-over._",
            t.branch,
            short(&handoff.head_sha)
        );
        Ok(out)
    }

    // ═══════════════════════════════ the executor ═══════════════════════════════

    /// Send one of the issue's own writes on a thread of its own. `false` when this process is
    /// sending it already.
    pub(super) fn send_own(self: &Arc<Self>, approval: ApprovalId) -> bool {
        if !self.claim(approval, Activity::Posting) {
            return false;
        }
        let engine = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name(format!("own-pr-{approval}"))
            .spawn(move || {
                let flow = engine.own_once(approval);
                engine.release(approval);
                if let Some(flow) = flow {
                    engine.notice.changed(flow);
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "could not start sending to the pull request");
            self.release(approval);
            return false;
        }
        true
    }

    /// Each operation not done, in order, each waiting for the one before.
    fn own_once(&self, approval: ApprovalId) -> Option<FlowId> {
        let db = self.db().ok()?;
        let send = db.own_send(approval).ok().flatten()?;
        let flow = send.approval.flow;
        let t = match self.own_target(flow) {
            Ok(t) => t,
            Err(error) => {
                if let Some(op) = send.next() {
                    self.own_failed(&send, op, &format!("{error}. Nothing was sent."));
                }
                return Some(flow);
            }
        };
        for op in &send.operations {
            let op = db.operation(op.id).ok().flatten()?;
            match op.state {
                OperationState::Done => continue,
                OperationState::Executing => {
                    self.settle_own(&send, &op, &t);
                    return Some(flow);
                }
                OperationState::Cancelled | OperationState::NeedsAttention => return Some(flow),
                OperationState::Pending | OperationState::Failed => {}
            }
            let went = match &op.payload {
                OperationPayload::PushBranch(payload) => self.own_push(&send, &op, payload, &t),
                OperationPayload::CreateDraftPullRequest(payload) => {
                    self.own_open(&send, &op, payload, &t)
                }
                OperationPayload::UpdatePullRequest(payload) => {
                    self.own_update(&send, &op, payload, &t)
                }
                OperationPayload::MarkReady(payload) => self.own_ready(&send, &op, payload, &t),
                OperationPayload::MoveIssue(payload) => self.own_move(&send, &op, payload, &t),
                OperationPayload::Merge(payload) => self.own_merge(&send, &op, payload, &t),
                other => {
                    self.own_failed(
                        &send,
                        &op,
                        &format!(
                            "a {:?} is not a write to the issue's own pull request",
                            other.kind()
                        ),
                    );
                    false
                }
            };
            if !went {
                return Some(flow);
            }
        }
        self.own_finished(approval);
        Some(flow)
    }

    /// Every write of `approval` is done: the run moves on as it says.
    fn own_finished(&self, approval: ApprovalId) {
        let Ok(db) = self.db() else { return };
        let Ok(Some(send)) = db.own_send(approval) else {
            return;
        };
        if send
            .operations
            .iter()
            .any(|op| op.state != OperationState::Done)
        {
            return;
        }
        let flow = send.approval.flow;
        self.settle_own_items(&send);
        match &send.request {
            OwnRequest::Draft(plan) => {
                if let Some((number, url)) = self.own_number(flow) {
                    self.link_pull_request(flow, &plan.destination.repo, number);
                    tracing::info!(%flow, number, url, "drafted the issue's pull request");
                }
            }
            OwnRequest::Ready(_) => {
                if self
                    .state(flow)
                    .is_ok_and(|s| s.phase == Phase::Publishing && s.end.is_none())
                    && let Err(error) = self.record(
                        flow,
                        &FlowEvent::Advanced {
                            to: Phase::InReview,
                        },
                    )
                {
                    tracing::warn!(%flow, %error, "published, and the run did not move to In review");
                }
            }
            OwnRequest::Merge(request) => {
                self.merged(flow, request.merge.number);
            }
        }
        // What GitHub says now, so the tab does not show the pull request as it was before these
        // writes until the next Sync.
        if !matches!(send.request, OwnRequest::Draft(_))
            && let Err(error) = self.sync_pull_request(flow)
        {
            tracing::info!(%flow, %error, "could not read the pull request after writing to it");
        }
        self.notice.changed(flow);
    }

    /// The pull request is merged: the run is done, and nothing about it waits for you.
    fn merged(&self, flow: FlowId, number: u64) {
        if self.state(flow).is_ok_and(|s| s.end.is_none()) {
            if let Err(error) = self.record(flow, &FlowEvent::Finished) {
                tracing::warn!(%flow, %error, "merged, and the run did not finish");
            }
            self.settle(
                flow,
                &[
                    InboxKind::Feedback,
                    InboxKind::ReviewReady,
                    InboxKind::Failure,
                ],
            );
            tracing::info!(%flow, number, "the issue's pull request merged; its run is done");
        }
    }

    /// Keep the pull request as the work item's own, so tracking reads it.
    fn link_pull_request(&self, flow: FlowId, repo: &RepoSlug, number: u64) {
        let Ok(db) = self.db() else { return };
        let Ok(Some(state)) = db.flow(flow) else {
            return;
        };
        let link = WorkLink::PullRequest(PullRequestRef {
            repo: repo.full_name(),
            number,
        });
        if let Err(error) = db.link(state.work_item, &link, self.now()) {
            tracing::warn!(%flow, %error, "could not keep the pull request with its work item");
        }
    }

    fn settle_own_items(&self, send: &OwnSend) {
        let Ok(db) = self.db() else { return };
        let target = InboxTarget::Approval {
            approval: send.approval.id,
        };
        for kind in [InboxKind::Approval, InboxKind::Failure] {
            let _ = db.resolve_inbox(send.approval.flow, kind, Some(&target), self.now());
        }
    }

    fn what(send: &OwnSend) -> (&'static str, String) {
        match &send.request {
            OwnRequest::Draft(plan) => ("draft", format!("{}'s draft pull request", plan.key)),
            OwnRequest::Ready(request) => (
                "publish",
                format!(
                    "Marking {}'s pull request ready and moving it to review",
                    request.key
                ),
            ),
            OwnRequest::Merge(request) => (
                "merge",
                format!(
                    "Merging {}'s pull request #{}",
                    request.key, request.merge.number
                ),
            ),
        }
    }

    fn own_failed(&self, send: &OwnSend, op: &Operation, error: &str) {
        self.change(op.id, &OperationChange::failed(error));
        let (_, what) = Self::what(send);
        let done = send
            .operations
            .iter()
            .filter(|o| o.state == OperationState::Done && o.id != op.id)
            .count();
        self.file(&NewInboxItem {
            flow: send.approval.flow,
            kind: InboxKind::Failure,
            target: InboxTarget::Approval {
                approval: send.approval.id,
            },
            title: format!("{what} did not finish"),
            body: format!(
                "{error} {} Retry reads first and sends only what is missing.",
                if done == 0 {
                    "Nothing of it was sent.".to_owned()
                } else {
                    format!("{done} of its {} writes are done.", send.operations.len())
                }
            ),
        });
    }

    fn own_stopped(&self, send: &OwnSend, op: &Operation, attention: Attention, why: &str) {
        self.change(op.id, &OperationChange::stopped(attention));
        let (_, what) = Self::what(send);
        self.file(&NewInboxItem {
            flow: send.approval.flow,
            kind: InboxKind::Approval,
            target: InboxTarget::Approval {
                approval: send.approval.id,
            },
            title: format!("{what} stopped"),
            body: why.to_owned(),
        });
    }

    /// The pull request read now, for an approval bound to its head.
    fn read_pull(&self, t: &OwnTarget, number: u64) -> Result<PullRequest, String> {
        self.app
            .forge
            .pull_request(
                &t.root,
                &PrTarget {
                    repo: Some(t.repo.clone()),
                    number,
                },
            )
            .map_err(text)
    }

    /// Whether `send`'s approval still covers the pull request at `head`; stops `op` and says so
    /// when it does not.
    fn head_still(&self, send: &OwnSend, op: &Operation, head: &str) -> bool {
        let approved = send.approval.binding.0.get(&Pinned::Head).cloned();
        if send
            .approval
            .check(&Binding::default().with(Pinned::Head, head))
            .is_ok()
        {
            return true;
        }
        let approved = approved.unwrap_or_default();
        self.own_stopped(
            send,
            op,
            Attention::HeadMoved {
                approved: approved.clone(),
                current: head.to_owned(),
            },
            &format!(
                "The pull request is at {} now, not {} as when you approved, so nothing more was \
                 sent. Look at what changed, then open the sheet again.",
                short(head),
                short(&approved)
            ),
        );
        false
    }

    fn own_push(
        &self,
        send: &OwnSend,
        op: &Operation,
        payload: &wtm_core::flow::PushPayload,
        t: &OwnTarget,
    ) -> bool {
        let runner = self.app.runner.as_ref();
        let remote = payload.destination.remote_arg();
        let reference = &payload.destination.reference;
        let now = match push::remote_head(runner, &t.worktree, remote, reference) {
            Ok(now) => now.map(|c| c.as_str().to_owned()),
            Err(error) => {
                self.own_failed(
                    send,
                    op,
                    &format!("The remote could not be read, so nothing was pushed: {error}."),
                );
                return false;
            }
        };
        match before_push(&payload.expect, &payload.commit, now.as_deref()) {
            BeforePush::AlreadyThere => return self.own_pushed(op, payload, true),
            BeforePush::Moved { expected, current } => {
                self.own_stopped(
                    send,
                    op,
                    Attention::HeadMoved {
                        approved: expected.clone().unwrap_or_default(),
                        current: current.clone().unwrap_or_default(),
                    },
                    &format!(
                        "`{}` on {} is at {}, not {} as when the hand-over was drafted, and wtm \
                         pushes only a fast-forward of what it last saw. Nothing was pushed.",
                        t.branch,
                        t.remote,
                        current.as_deref().map_or("nothing", short),
                        expected.as_deref().map_or("nothing", short),
                    ),
                );
                return false;
            }
            BeforePush::Send => {}
        }
        if self.change(op.id, &OperationChange::sending()).is_none() {
            return false;
        }
        match push::push(runner, &t.worktree, remote, &payload.refspec()) {
            Ok(Pushed::Updated | Pushed::UpToDate) => self.own_pushed(op, payload, false),
            Ok(Pushed::Rejected { reason }) => {
                let now = push::remote_head(runner, &t.worktree, remote, reference)
                    .ok()
                    .flatten()
                    .map(|c| c.as_str().to_owned());
                if now.as_deref() == Some(payload.commit.as_str()) {
                    return self.own_pushed(op, payload, true);
                }
                self.own_stopped(
                    send,
                    op,
                    Attention::Refused {
                        message: reason.clone(),
                    },
                    &format!("The remote refused the push: {reason}"),
                );
                false
            }
            Err(error) => match push::remote_head(runner, &t.worktree, remote, reference) {
                Ok(Some(at)) if at.as_str() == payload.commit => self.own_pushed(op, payload, true),
                _ => {
                    self.own_failed(send, op, &format!("The push did not happen: {error}."));
                    false
                }
            },
        }
    }

    fn own_pushed(
        &self,
        op: &Operation,
        payload: &wtm_core::flow::PushPayload,
        adopted: bool,
    ) -> bool {
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

    fn own_open(
        &self,
        send: &OwnSend,
        op: &Operation,
        payload: &wtm_core::flow::DraftPullRequestPayload,
        t: &OwnTarget,
    ) -> bool {
        let marker = Marker::of(&op.key);
        let label = format!("{}:{}", payload.repo.owner, payload.head);
        let find = |found: &[wtm_core::ports::RemotePullRequest]| {
            found
                .iter()
                .find(|p| marker.signs(&p.body))
                .or_else(|| {
                    found
                        .iter()
                        .find(|p| p.base == payload.base && p.state == PrState::Open)
                })
                .cloned()
        };
        match self
            .app
            .pr_writer
            .pull_requests_from(&t.root, &payload.repo, &label)
        {
            Ok(found) => {
                if let Some(existing) = find(&found) {
                    return self.own_opened(op, &existing, true);
                }
            }
            Err(error) => {
                self.own_failed(
                    send,
                    op,
                    &format!("GitHub could not be read, so nothing was opened: {error}."),
                );
                return false;
            }
        }
        if self.change(op.id, &OperationChange::sending()).is_none() {
            return false;
        }
        let body = signed(&payload.body, &marker.tag());
        match self.app.own_pulls.open_draft(
            &t.root,
            &payload.repo,
            &payload.base,
            &payload.head,
            &payload.title,
            &body,
        ) {
            Ok(opened) => self.own_opened(op, &opened, false),
            Err(error) => {
                if let Ok(found) =
                    self.app
                        .pr_writer
                        .pull_requests_from(&t.root, &payload.repo, &label)
                    && let Some(existing) = find(&found)
                {
                    return self.own_opened(op, &existing, true);
                }
                match error {
                    wtm_core::ports::ForgeError::Rejected { reason, .. } => self.own_stopped(
                        send,
                        op,
                        Attention::Refused {
                            message: reason.clone(),
                        },
                        &format!("GitHub refused the draft: {reason}"),
                    ),
                    other => self.own_failed(send, op, &other.to_string()),
                }
                false
            }
        }
    }

    fn own_opened(
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

    fn own_update(
        &self,
        send: &OwnSend,
        op: &Operation,
        payload: &wtm_core::flow::UpdatePullRequestPayload,
        t: &OwnTarget,
    ) -> bool {
        let marker = Marker::of(&op.key);
        let label = format!(
            "{}:{}",
            payload.repo.owner,
            match &send.request {
                OwnRequest::Draft(plan) => plan.branch.as_str(),
                _ => t.branch.as_str(),
            }
        );
        let current = || {
            self.app
                .pr_writer
                .pull_requests_from(&t.root, &payload.repo, &label)
                .map(|found| found.into_iter().find(|p| p.number == payload.number))
        };
        match current() {
            Ok(Some(existing)) if marker.signs(&existing.body) => {
                return self.own_opened(op, &existing, true);
            }
            Ok(_) => {}
            Err(error) => {
                self.own_failed(
                    send,
                    op,
                    &format!("GitHub could not be read, so nothing was updated: {error}."),
                );
                return false;
            }
        }
        if self.change(op.id, &OperationChange::sending()).is_none() {
            return false;
        }
        let body = signed(&payload.body, &marker.tag());
        match self.app.own_pulls.update(
            &t.root,
            &payload.repo,
            payload.number,
            &payload.title,
            &body,
        ) {
            Ok(updated) => self.own_opened(op, &updated, false),
            Err(error) => {
                if let Ok(Some(existing)) = current()
                    && marker.signs(&existing.body)
                {
                    return self.own_opened(op, &existing, true);
                }
                self.own_failed(send, op, &error.to_string());
                false
            }
        }
    }

    fn own_ready(
        &self,
        send: &OwnSend,
        op: &Operation,
        payload: &wtm_core::flow::MarkReadyPayload,
        t: &OwnTarget,
    ) -> bool {
        let pr = match self.read_pull(t, payload.number) {
            Ok(pr) => pr,
            Err(error) => {
                self.own_failed(
                    send,
                    op,
                    &format!("GitHub could not be read, so nothing was sent: {error}."),
                );
                return false;
            }
        };
        if !self.head_still(send, op, pr.head.sha.as_str()) {
            return false;
        }
        if !pr.draft {
            return self.own_readied(op, &pr, true);
        }
        if self.change(op.id, &OperationChange::sending()).is_none() {
            return false;
        }
        let reply = self
            .app
            .own_pulls
            .mark_ready(&t.root, &payload.repo, payload.number);
        match (self.read_pull(t, payload.number), reply) {
            (Ok(now), reply) if !now.draft => self.own_readied(op, &now, reply.is_err()),
            (Ok(_), Err(error)) => {
                self.own_failed(send, op, &format!("GitHub did not mark it ready: {error}."));
                false
            }
            (Ok(_), Ok(())) => {
                self.own_failed(send, op, "GitHub answered, and it still reads as a draft.");
                false
            }
            (Err(read), _) => {
                self.own_failed(
                    send,
                    op,
                    &format!(
                        "it could not be read afterwards to see whether it is ready ({read}); \
                         Retry reads it first"
                    ),
                );
                false
            }
        }
    }

    fn own_readied(&self, op: &Operation, pr: &PullRequest, adopted: bool) -> bool {
        self.change(
            op.id,
            &OperationChange::done(OperationResult::Ready {
                number: pr.number,
                url: pr.url.clone(),
                adopted,
            }),
        )
        .is_some()
    }

    fn own_move(
        &self,
        send: &OwnSend,
        op: &Operation,
        payload: &MoveIssuePayload,
        t: &OwnTarget,
    ) -> bool {
        // Bound to the head like the rest of the approval: a push after you approved stops the
        // move too, rather than telling the team about work you have not seen.
        if let OwnRequest::Ready(request) = &send.request {
            match self.read_pull(t, request.ready.number) {
                Ok(pr) => {
                    if !self.head_still(send, op, pr.head.sha.as_str()) {
                        return false;
                    }
                }
                Err(error) => {
                    self.own_failed(
                        send,
                        op,
                        &format!("GitHub could not be read, so the issue was not moved: {error}."),
                    );
                    return false;
                }
            }
        }
        let target = match self.move_target_for(send.approval.flow, &payload.to) {
            Ok(target) => target,
            Err(error) => {
                self.own_failed(send, op, &format!("{error}. Nothing was sent."));
                return false;
            }
        };
        let current = match self.read_status(&target, &payload.key) {
            Ok(current) => current,
            Err(error) => {
                self.own_failed(
                    send,
                    op,
                    &format!(
                        "{} could not be read from the tracker, so it was not moved: {error}.",
                        payload.key
                    ),
                );
                return false;
            }
        };
        let moved = |now: &str, adopted: bool| {
            self.change(
                op.id,
                &OperationChange::done(OperationResult::Moved {
                    key: payload.key.clone(),
                    status: now.to_owned(),
                    adopted,
                }),
            )
            .is_some()
        };
        match step(payload, &current) {
            MoveStep::Adopt => moved(&current, true),
            MoveStep::Stop(elsewhere) => {
                self.own_stopped(
                    send,
                    op,
                    Attention::StatusMoved(elsewhere),
                    &format!(
                        "{} is in {current}, not {} as when you approved, so it was not moved. The \
                         pull request's part is done; open the sheet again to move it from \
                         {current}.",
                        payload.key, payload.from
                    ),
                );
                false
            }
            MoveStep::Send => {
                if self.change(op.id, &OperationChange::sending()).is_none() {
                    return false;
                }
                let reply =
                    self.app
                        .tracker_writer
                        .move_to(&target.root, &payload.key, &target.spelled);
                match (self.read_status(&target, &payload.key), reply) {
                    (Ok(now), reply) if arrived(payload, &now) => moved(&now, reply.is_err()),
                    (Ok(now), Err(error)) => {
                        self.own_failed(
                            send,
                            op,
                            &format!(
                                "the tracker refused it and {} is still in {now}: {error}.",
                                payload.key
                            ),
                        );
                        false
                    }
                    (Ok(now), Ok(())) => {
                        self.own_failed(
                            send,
                            op,
                            &format!(
                                "the tracker answered, and {} reads {now} rather than {}.",
                                payload.key, payload.to
                            ),
                        );
                        false
                    }
                    (Err(_), Ok(())) => moved(&payload.to, false),
                    (Err(read), Err(error)) => {
                        self.own_failed(
                            send,
                            op,
                            &format!(
                                "{error}; and {} could not be read to see whether it moved \
                                 ({read}).",
                                payload.key
                            ),
                        );
                        false
                    }
                }
            }
        }
    }

    fn own_merge(
        &self,
        send: &OwnSend,
        op: &Operation,
        payload: &wtm_core::flow::MergePayload,
        t: &OwnTarget,
    ) -> bool {
        let pr = match self.read_pull(t, payload.number) {
            Ok(pr) => pr,
            Err(error) => {
                self.own_failed(
                    send,
                    op,
                    &format!("GitHub could not be read, so nothing was merged: {error}."),
                );
                return false;
            }
        };
        if pr.state == PrState::Merged && pr.head.sha.as_str() == payload.head {
            return self.own_merged(op, &pr, payload, true);
        }
        if !self.head_still(send, op, pr.head.sha.as_str()) {
            return false;
        }
        if self.change(op.id, &OperationChange::sending()).is_none() {
            return false;
        }
        let reply = self.app.own_pulls.merge(
            &t.root,
            &payload.repo,
            payload.number,
            &payload.head,
            payload.method,
        );
        match (self.read_pull(t, payload.number), reply) {
            (Ok(now), reply) if now.state == PrState::Merged => {
                self.own_merged(op, &now, payload, reply.is_err())
            }
            (Ok(_), Err(wtm_core::ports::ForgeError::Rejected { reason, .. })) => {
                self.own_stopped(
                    send,
                    op,
                    Attention::Refused {
                        message: reason.clone(),
                    },
                    &format!("GitHub refused the merge: {reason}"),
                );
                false
            }
            (_, Err(error)) => {
                self.own_failed(send, op, &format!("The merge did not happen: {error}."));
                false
            }
            (_, Ok(())) => {
                self.own_failed(
                    send,
                    op,
                    "GitHub answered, and the pull request does not read as merged.",
                );
                false
            }
        }
    }

    fn own_merged(
        &self,
        op: &Operation,
        pr: &PullRequest,
        payload: &wtm_core::flow::MergePayload,
        adopted: bool,
    ) -> bool {
        self.change(
            op.id,
            &OperationChange::done(OperationResult::Merged {
                number: pr.number,
                url: pr.url.clone(),
                head: payload.head.clone(),
                adopted,
            }),
        )
        .is_some()
    }

    /// Settle one of the issue's own writes a crash left executing, by reading. Sends nothing.
    fn settle_own(&self, send: &OwnSend, op: &Operation, t: &OwnTarget) {
        let checked: Result<Option<OperationResult>, String> = match &op.payload {
            OperationPayload::PushBranch(p) => push::remote_head(
                self.app.runner.as_ref(),
                &t.worktree,
                p.destination.remote_arg(),
                &p.destination.reference,
            )
            .map_err(text)
            .map(|now| {
                (now.as_ref().map(wtm_core::model::CommitId::as_str) == Some(p.commit.as_str()))
                    .then(|| OperationResult::Pushed {
                        url: p.destination.url.clone(),
                        reference: p.destination.reference.clone(),
                        commit: p.commit.clone(),
                        adopted: true,
                    })
            }),
            OperationPayload::CreateDraftPullRequest(p) => {
                let marker = Marker::of(&op.key);
                self.app
                    .pr_writer
                    .pull_requests_from(&t.root, &p.repo, &format!("{}:{}", p.repo.owner, p.head))
                    .map_err(text)
                    .map(|found| {
                        found.into_iter().find(|f| marker.signs(&f.body)).map(|f| {
                            OperationResult::PullRequest {
                                number: f.number,
                                url: f.url,
                                adopted: true,
                            }
                        })
                    })
            }
            OperationPayload::UpdatePullRequest(p) => {
                let marker = Marker::of(&op.key);
                self.app
                    .pr_writer
                    .pull_requests_from(&t.root, &p.repo, &format!("{}:{}", p.repo.owner, t.branch))
                    .map_err(text)
                    .map(|found| {
                        found
                            .into_iter()
                            .find(|f| f.number == p.number && marker.signs(&f.body))
                            .map(|f| OperationResult::PullRequest {
                                number: f.number,
                                url: f.url,
                                adopted: true,
                            })
                    })
            }
            OperationPayload::MarkReady(p) => self.read_pull(t, p.number).map(|pr| {
                (!pr.draft).then_some(OperationResult::Ready {
                    number: pr.number,
                    url: pr.url,
                    adopted: true,
                })
            }),
            OperationPayload::Merge(p) => self.read_pull(t, p.number).map(|pr| {
                (pr.state == PrState::Merged).then(|| OperationResult::Merged {
                    number: pr.number,
                    url: pr.url,
                    head: p.head.clone(),
                    adopted: true,
                })
            }),
            OperationPayload::MoveIssue(p) => self
                .move_target_for(send.approval.flow, &p.to)
                .and_then(|target| self.read_status(&target, &p.key))
                .map(|now| {
                    arrived(p, &now).then(|| OperationResult::Moved {
                        key: p.key.clone(),
                        status: now,
                        adopted: true,
                    })
                }),
            _ => Ok(None),
        };
        match checked {
            Ok(Some(result)) => {
                self.change(op.id, &OperationChange::done(result));
                self.own_finished(send.approval.id);
            }
            Ok(None) => self.own_failed(
                send,
                op,
                "wtm stopped while sending this, and it is not there: it did not happen.",
            ),
            Err(error) => self.own_failed(
                send,
                op,
                &format!(
                    "wtm stopped while sending this, and the remote could not be read to see \
                     whether it arrived: {error}."
                ),
            ),
        }
    }

    /// At launch, an executing operation that is one of the issue's own writes: settle it by
    /// reading. `false` when it is not one.
    pub(super) fn recover_own(&self, approval: ApprovalId, op: &Operation) -> bool {
        let Some(send) = self
            .db()
            .ok()
            .and_then(|db| db.own_send(approval).ok().flatten())
        else {
            return false;
        };
        match self.own_target(send.approval.flow) {
            Ok(t) => self.settle_own(&send, op, &t),
            Err(error) => self.own_failed(
                &send,
                op,
                &format!("wtm stopped while sending this, and {error}."),
            ),
        }
        true
    }

    /// Retry one of the issue's own writes that failed or stopped. A head that moved needs the
    /// sheet again, not a retry.
    ///
    /// # Errors
    ///
    /// When it is being sent now, is done, or stopped for a moved head.
    pub fn retry_own(self: &Arc<Self>, approval: ApprovalId) -> Result<(), String> {
        let db = self.db()?;
        let send = db
            .own_send(approval)
            .map_err(text)?
            .ok_or_else(|| format!("approval {approval} is not a write to a pull request"))?;
        if self.activity(approval).is_some() {
            return Err("it is being sent right now".to_owned());
        }
        let op = send.next().ok_or("it is done")?.clone();
        if let Some(Attention::HeadMoved { .. }) = op.attention {
            return Err(
                "the pull request moved past the head you approved; open the sheet again"
                    .to_owned(),
            );
        }
        if matches!(
            op.state,
            OperationState::NeedsAttention | OperationState::Cancelled
        ) {
            self.change(op.id, &OperationChange::to(OperationState::Pending));
        }
        self.settle_own_items(&send);
        self.send_own(approval);
        self.notice.changed(send.approval.flow);
        Ok(())
    }

    // ═══════════════════════════════ ready + needs review ═══════════════════════════════

    /// What "Mark ready + Needs Review" would do now, read fresh.
    ///
    /// # Errors
    ///
    /// When the run has no pull request yet, or GitHub or the tracker cannot be read.
    pub fn ready_sheet(&self, flow: FlowId) -> Result<ReadySheet, String> {
        let t = self.own_target(flow)?;
        let (number, _) = self
            .own_number(flow)
            .ok_or("the task branch has no pull request yet: it is drafted at the hand-over")?;
        let pr = self.read_pull(&t, number)?;
        let handed_over = self
            .app
            .git
            .rev_parse(&t.worktree, "HEAD")
            .map_err(text)?
            .map(|c| c.as_str().to_owned())
            .unwrap_or_default();
        let mut problems = Vec::new();
        let state = self.state(flow)?;
        if !matches!(state.phase, Phase::ReadyToTest | Phase::Publishing) || state.end.is_some() {
            problems.push(format!(
                "Publishing follows the hand-over; this run is at {:?}.",
                state.phase
            ));
        }
        if self.is_running(flow) || self.publishing_now(flow) {
            problems.push("Something of this run is being sent or worked on now.".to_owned());
        }
        if pr.state != PrState::Open {
            problems.push(format!("Pull request #{number} is not open."));
        }
        if pr.head.sha.as_str() != handed_over {
            problems.push(format!(
                "The pull request is at {}, and the worktree at {}: the hand-over you tested is \
                 not what would be marked ready. Its draft is pushed at the next hand-over.",
                short(pr.head.sha.as_str()),
                short(&handed_over)
            ));
        }
        let (from, to) = self.needs_review(&t)?;
        if to.is_none() {
            problems.push(
                "The project maps no \"needs review\" status (`[workflow.statuses] needs_review`), \
                 so there is nothing to move the issue to."
                    .to_owned(),
            );
        }
        Ok(ReadySheet {
            flow,
            key: t.key.clone(),
            repo: t.repo.full_name(),
            number,
            url: pr.url.clone(),
            draft: pr.draft,
            head: pr.head.sha.as_str().to_owned(),
            handed_over,
            tracker_from: from,
            tracker_to: to,
            problems,
        })
    }

    /// The issue's status now, read from the tracker, and the mapped "needs review" status.
    fn needs_review(&self, t: &OwnTarget) -> Result<(Option<String>, Option<String>), String> {
        let project = self
            .app
            .project(t.implementation.project.as_str())
            .map_err(text)?;
        let to = project
            .workflow
            .as_ref()
            .and_then(|w| w.statuses.target(StatusRole::NeedsReview))
            .map(str::to_owned);
        let target = self.move_target_for(t.flow, to.as_deref().unwrap_or_default())?;
        let from = self.read_status(&target, &t.key).ok();
        Ok((from, to))
    }

    /// Approve "Mark ready + Needs Review" at `head`, the pull request's head as the sheet showed
    /// it, and send it.
    ///
    /// # Errors
    ///
    /// What the sheet would have listed, read again now, or a head that moved since it was read.
    pub fn approve_ready(self: &Arc<Self>, flow: FlowId, head: &str) -> Result<(), Refusal> {
        let sheet = self.ready_sheet(flow)?;
        if sheet.head != head {
            return Err(format!(
                "the pull request moved to {} after the sheet showed {}; look again",
                short(&sheet.head),
                short(head)
            )
            .into());
        }
        if let Some(problem) = sheet.problems.first() {
            return Err(problem.clone().into());
        }
        let t = self.own_target(flow)?;
        let (Some(from), Some(to)) = (sheet.tracker_from, sheet.tracker_to) else {
            return Err("the issue's status could not be read".into());
        };
        let request = ReadyRequest {
            key: t.key.clone(),
            ready: wtm_core::flow::MarkReadyPayload {
                repo: t.repo.clone(),
                number: sheet.number,
                head: head.to_owned(),
            },
            to_review: MoveIssuePayload {
                tracker: t.tracker,
                key: t.key.clone(),
                from,
                to,
            },
        };
        let db = self.db()?;
        let approved = db
            .approve(
                &NewApproval::ready(flow, &request),
                &request.operations(flow),
                self.now(),
            )
            .map_err(text)?;
        if self.state(flow)?.phase == Phase::ReadyToTest {
            self.record(
                flow,
                &FlowEvent::Advanced {
                    to: Phase::Publishing,
                },
            )?;
        }
        self.settle(flow, &[InboxKind::ReviewReady]);
        tracing::info!(%flow, number = sheet.number, "approved ready + needs review");
        self.notice.changed(flow);
        self.send_own(approved.approval.id);
        Ok(())
    }

    // ═══════════════════════════════ following ═══════════════════════════════

    /// Read the pull request again: its state, checks, reviews and merge state, kept as the work
    /// item's reading; new feedback filed once. Asked for on demand and on the window's focus.
    ///
    /// # Errors
    ///
    /// When the run has no pull request, or GitHub cannot be read.
    pub fn sync_pull_request(&self, flow: FlowId) -> Result<OwnPrView, String> {
        let t = self.own_target(flow)?;
        let (number, _) = self
            .own_number(flow)
            .ok_or("the task branch has no pull request yet")?;
        let pr = self.read_pull(&t, number)?;
        let db = self.db()?;
        let permission = self
            .app
            .forge
            .permission(&t.root, &pr.base.repo)
            .unwrap_or(wtm_core::flow::Permission::Unknown);
        db.mirror_pr(&MirroredPr {
            item: t.item,
            pull_request: pr.clone(),
            permission,
            read_at: self.now(),
        })
        .map_err(text)?;
        let me = self
            .app
            .forge
            .check(&t.root, &t.repo.host)
            .ok()
            .and_then(|account| account.login);
        let reviews = self
            .app
            .review_writer
            .reviews(&t.root, &t.repo, number)
            .map_err(text)?;
        let known: BTreeSet<u64> = db
            .feedback(flow)
            .map_err(text)?
            .iter()
            .map(|f| f.review)
            .collect();
        let mut fresh = Vec::new();
        for review in reviews.iter().filter(|r| !known.contains(&r.id)) {
            let comments = self
                .app
                .review_writer
                .review_comments(&t.root, &t.repo, number, review.id)
                .unwrap_or_default();
            if !is_feedback(review, me.as_deref(), comments.len()) {
                continue;
            }
            fresh.push(PrFeedback {
                flow,
                review: review.id,
                author: review.author.clone(),
                state: review.state,
                body: review.body.clone(),
                url: review.url.clone(),
                commit_id: review.commit_id.clone(),
                comments: comments.iter().map(FeedbackComment::of).collect(),
                seen_at: self.now(),
                revised_in: None,
            });
        }
        let kept = db.keep_feedback(&fresh).map_err(text)?;
        if !kept.is_empty() && self.state(flow).is_ok_and(|s| s.end.is_none()) {
            let who: Vec<String> = kept
                .iter()
                .map(|f| f.author.clone().unwrap_or_else(|| "someone".to_owned()))
                .collect();
            self.file(&NewInboxItem {
                flow,
                kind: InboxKind::Feedback,
                target: InboxTarget::Flow,
                title: format!("{}: new feedback on #{number}", t.key),
                body: format!(
                    "{} reviewed it. Start a revision from the issue's Implement tab: the \
                     implementer gets the feedback in its same conversation.",
                    who.join(", ")
                ),
            });
        }
        if pr.state == PrState::Merged {
            // Merged on GitHub rather than from the sheet: the run is done all the same.
            self.merged(flow, number);
        }
        if let Some(pushed) = self.last_pushed(flow)
            && pr.head.sha.as_str() != pushed
            && !self.ours(&t, pr.head.sha.as_str())
        {
            self.file(&NewInboxItem {
                flow,
                kind: InboxKind::Failure,
                target: InboxTarget::Flow,
                title: format!("{}: #{number} has commits wtm did not push", t.key),
                body: format!(
                    "Its head is {}, which this worktree does not have; wtm last pushed {}. The \
                     next draft stops rather than overwrite it.",
                    short(pr.head.sha.as_str()),
                    short(&pushed)
                ),
            });
        }
        self.notice.changed(flow);
        self.own_pr_view(flow)
    }

    /// Start a revision from the feedback no revision has taken in: one click.
    ///
    /// # Errors
    ///
    /// When there is no such feedback, or a revision could not start now (`revise`).
    pub fn revise_from_feedback(self: &Arc<Self>, flow: FlowId) -> Result<(), Refusal> {
        let db = self.db()?;
        let open: Vec<PrFeedback> = db
            .feedback(flow)
            .map_err(text)?
            .into_iter()
            .filter(|f| f.revised_in.is_none())
            .collect();
        if open.is_empty() {
            return Err("there is no feedback a revision has not taken in already".into());
        }
        self.revise(flow, &feedback_note(&open))?;
        let round = db
            .rounds(flow)
            .map_err(text)?
            .into_iter()
            .rev()
            .find(|r| r.kind == wtm_core::flow::RoundKind::Revise)
            .map(|r| r.round)
            .ok_or("the revision's round was not recorded")?;
        let reviews: Vec<u64> = open.iter().map(|f| f.review).collect();
        db.feedback_revised(flow, &reviews, round).map_err(text)?;
        self.settle(flow, &[InboxKind::Feedback]);
        self.notice.changed(flow);
        Ok(())
    }

    // ═══════════════════════════════ merge ═══════════════════════════════

    /// What the Merge sheet shows, read fresh.
    ///
    /// # Errors
    ///
    /// When the run has no pull request, or GitHub cannot be read.
    pub fn merge_sheet(&self, flow: FlowId) -> Result<MergeSheet, String> {
        let t = self.own_target(flow)?;
        let (number, _) = self
            .own_number(flow)
            .ok_or("the task branch has no pull request to merge")?;
        let pr = self.read_pull(&t, number)?;
        let mut problems = Vec::new();
        let state = self.state(flow)?;
        if state.phase != Phase::InReview || state.end.is_some() {
            problems.push(format!(
                "Merging follows Mark ready + Needs Review; this run is at {:?}.",
                state.phase
            ));
        }
        if self.is_running(flow) || self.publishing_now(flow) {
            problems.push("Something of this run is being sent or worked on now.".to_owned());
        }
        if pr.state != PrState::Open {
            problems.push(format!("Pull request #{number} is not open."));
        }
        if pr.draft {
            problems.push("It is still a draft.".to_owned());
        }
        if let Some(merge) = pr.merge_state
            && !merge.mergeable()
        {
            problems.push(format!(
                "GitHub says it cannot merge as it stands ({}).",
                serde_json::to_value(merge)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_owned))
                    .unwrap_or_default()
                    .replace('_', " ")
            ));
        }
        Ok(MergeSheet {
            flow,
            key: t.key.clone(),
            repo: t.repo.full_name(),
            number,
            url: pr.url.clone(),
            branch: pr.head.branch.clone(),
            base: pr.base.branch.clone(),
            head: pr.head.sha.as_str().to_owned(),
            methods: vec![MergeMethod::Squash, MergeMethod::Merge, MergeMethod::Rebase],
            pull_request: pr,
            problems,
        })
    }

    /// Merge at `head` by `method`, from the Merge sheet, and nowhere else.
    ///
    /// # Errors
    ///
    /// What the sheet would have listed, read again now, or a head that moved since it was read.
    pub fn approve_merge(
        self: &Arc<Self>,
        flow: FlowId,
        head: &str,
        method: MergeMethod,
    ) -> Result<(), Refusal> {
        let sheet = self.merge_sheet(flow)?;
        if sheet.head != head {
            return Err(format!(
                "the pull request moved to {} after the sheet showed {}; look again",
                short(&sheet.head),
                short(head)
            )
            .into());
        }
        if let Some(problem) = sheet.problems.first() {
            return Err(problem.clone().into());
        }
        let t = self.own_target(flow)?;
        let request = MergeRequest {
            key: t.key.clone(),
            merge: wtm_core::flow::MergePayload {
                repo: t.repo.clone(),
                number: sheet.number,
                head: head.to_owned(),
                method,
            },
            branch: sheet.branch.clone(),
            base: sheet.base.clone(),
            checks: sheet.pull_request.checks,
        };
        let db = self.db()?;
        let approved = db
            .approve(
                &NewApproval::merge(flow, &request),
                &request.operations(flow),
                self.now(),
            )
            .map_err(text)?;
        tracing::info!(%flow, number = sheet.number, ?method, "approved a merge from the sheet");
        self.notice.changed(flow);
        self.send_own(approved.approval.id);
        Ok(())
    }

    // ═══════════════════════════════ the view ═══════════════════════════════

    /// The issue's own pull request, as the Implement tab shows it. Reads nothing remote.
    ///
    /// # Errors
    ///
    /// When the database cannot be read.
    pub fn own_pr_view(&self, flow: FlowId) -> Result<OwnPrView, String> {
        let db = self.db()?;
        let state = self.state(flow)?;
        let target = self.own_target(flow);
        let sends: Vec<OwnSendView> = db
            .own_sends(flow)
            .map_err(text)?
            .into_iter()
            .map(|send| OwnSendView {
                state: send.state(self.activity(send.approval.id)),
                send,
            })
            .collect();
        let reading = db
            .flow(flow)
            .map_err(text)?
            .and_then(|f| db.mirrored_pr(f.work_item).ok().flatten());
        let feedback = db.feedback(flow).map_err(text)?;
        let number = self.own_number(flow);
        let busy = self.is_running(flow) || self.publishing_now(flow);
        let open = state.end.is_none() && state.hold.is_none();
        Ok(OwnPrView {
            flow,
            unavailable: target.as_ref().err().cloned(),
            branch: target.as_ref().ok().map(|t| t.branch.clone()),
            base: target.as_ref().ok().map(|t| t.base.clone()),
            repo: target.as_ref().ok().map(|t| t.repo.full_name()),
            pull_request: number
                .as_ref()
                .zip(target.as_ref().ok())
                .map(|((n, _), t)| PullRequestRef {
                    repo: t.repo.full_name(),
                    number: *n,
                }),
            url: number.map(|(_, url)| url),
            reading,
            can_publish: open
                && !busy
                && self.own_number(flow).is_some()
                && matches!(state.phase, Phase::ReadyToTest | Phase::Publishing),
            can_merge: open && !busy && state.phase == Phase::InReview,
            can_sync: target.is_ok() && self.own_number(flow).is_some(),
            can_revise_feedback: feedback.iter().any(|f| f.revised_in.is_none())
                && self.revisable(flow).is_ok(),
            sends,
            feedback,
        })
    }

    // ═══════════════════════════════ ls-remote ═══════════════════════════════

    /// After a writer's round: read the task branch and the integration branches, and say in the
    /// Inbox when either holds something wtm did not put there. Reads only; a project with no forge
    /// or no remote has nothing to read.
    pub(super) fn watch_remote(&self, flow: FlowId) {
        let Ok(t) = self.own_target(flow) else { return };
        let runner = self.app.runner.as_ref();
        let read = |branch: &str| {
            push::remote_head(
                runner,
                &t.worktree,
                &t.remote,
                &format!("refs/heads/{branch}"),
            )
            .ok()
            .flatten()
            .map(|c| c.as_str().to_owned())
        };
        let now = read(&t.branch);
        let pushed = self.last_pushed(flow);
        let mut branches: Vec<String> = t
            .integration
            .names()
            .into_iter()
            .filter(|b| !b.contains('*'))
            .collect();
        branches.dedup();
        let integration: Vec<(String, Option<String>)> = branches
            .into_iter()
            .map(|b| (b.clone(), read(&b)))
            .collect();
        // This run's commits: in the worktree's history since implementing began.
        let ours: BTreeSet<String> = integration
            .iter()
            .filter_map(|(_, at)| at.clone())
            .filter(|at| {
                self.ours(&t, at)
                    && self
                        .app
                        .git
                        .merge_base(&t.worktree, at, &t.implementation.base_sha)
                        .ok()
                        .flatten()
                        .is_none_or(|base| base.as_str() != at)
            })
            .collect();
        let found = unexplained(
            &TaskRemote {
                branch: &t.branch,
                pushed: pushed.as_deref(),
                now: now.as_deref(),
                now_is_ours: now.as_deref().is_some_and(|now| self.ours(&t, now)),
            },
            &ours,
            &integration,
        );
        if found.is_empty() {
            return;
        }
        tracing::warn!(%flow, ?found, "the remote holds something wtm did not put there");
        self.file(&NewInboxItem {
            flow,
            kind: InboxKind::Failure,
            target: InboxTarget::Flow,
            title: format!("{}: {} changed outside wtm", t.key, t.remote),
            body: unexplained_words(&t, &found),
        });
    }

    /// The approval of a send, for the window.
    pub fn own_send_view(&self, approval: ApprovalId) -> Result<Option<Approval>, String> {
        Ok(self
            .db()?
            .own_send(approval)
            .map_err(text)?
            .map(|send| send.approval))
    }
}

/// What [`unexplained`] found, in sentences.
fn unexplained_words(t: &OwnTarget, found: &[Unexplained]) -> String {
    found
        .iter()
        .map(|u| match u {
            Unexplained::TaskBranch {
                branch,
                pushed,
                now,
            } => match (pushed, now) {
                (Some(pushed), None) => format!(
                    "`{branch}` is gone from {}, after wtm pushed {} there.",
                    t.remote,
                    short(pushed)
                ),
                (_, Some(now)) => format!(
                    "`{branch}` on {} is at {}, a commit this worktree does not have{}. wtm \
                     pushes only fast-forwards of its own work, so nothing was pushed: fetch it \
                     and look before the next hand-over.",
                    t.remote,
                    short(now),
                    pushed
                        .as_deref()
                        .map(|p| format!("; wtm last pushed {}", short(p)))
                        .unwrap_or_default()
                ),
                (None, None) => String::new(),
            },
            Unexplained::Integration { branch, commit } => format!(
                "`{branch}` on {} is at {}, one of this run's own commits. Only a merge from the \
                 Merge sheet should put it there.",
                t.remote,
                short(commit)
            ),
        })
        .collect::<Vec<_>>()
        .join(" ")
}
