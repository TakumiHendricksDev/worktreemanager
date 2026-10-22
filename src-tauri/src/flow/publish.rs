//! Publishing a teammate's pull request review: the preview, the approval, and what you can do
//! with a publish that stopped.
//!
//! # What you approve is what posts
//!
//! The outcome sheet sends a draft; [`Engine::preview_publish`] reads the pull request and the
//! forge's own diff of the reviewed commit, builds exactly what would post
//! ([`ReviewPayload::build`]), and shows it with every marker in place. Approving sends the same
//! draft back with the hash the preview showed, and [`Engine::approve_publish`] builds it again from
//! a fresh read: a different hash means a finding, the diff or the text changed in between, and
//! nothing is approved. An agent has no tool that reaches either: they are IPC commands, which only
//! the window calls (ARCHITECTURE §6i).
//!
//! # A moved head asks again
//!
//! The approval pins the payload's hash, the reviewed commit and the head the pull request was at.
//! When the head is not the reviewed commit at approval time, approving is refused with the two
//! heads ([`HEAD_MOVED`]) unless you chose to publish against the reviewed commit anyway, which
//! approves with the newer head pinned. The executor checks the pin again before each operation it
//! sends (`outbox.rs`), so a push after the approval stops the publish and asks again too.
//!
//! # One publish of a review at a time
//!
//! A new approval replaces an earlier one of the same review that never finished: its operations
//! are cancelled and its inbox items settled, unless one is mid-send, which refuses the new
//! approval until it is known what happened. Approving exactly the same text again is the same
//! operations, so a done one stays done and nothing is posted twice.

use std::sync::Arc;

use serde::Serialize;
use wtm_core::flow::{
    Activity, ApprovalId, Attention, ChangedLines, FindingId, FindingRecord, FlowId, InboxKind,
    InboxTarget, Marker, NewApproval, OperationChange, OperationPayload, OperationResult,
    OperationState, Pinned, Publication, PublicationState, PublishContext, Resolution, ReviewDraft,
    ReviewPayload, ReviewRun, Snapshot,
};
use wtm_core::ports::{FlowStore, FlowStoreError, InboxStore, OutboxStore, RunStore};

use super::Engine;
use super::outbox::{sent_comments, submit_body};
use crate::reviews::{self, ReadNow};
use crate::view::ErrorView;

/// The error kind a refused approval carries when the pull request moved past the reviewed commit.
/// Its detail names both heads, so the sheet can offer to re-review or to publish anyway.
pub const HEAD_MOVED: &str = "headMoved";

/// One inline comment exactly as it posts: its place, and its body with its marker.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PostedComment {
    pub finding: FindingId,
    pub path: String,
    pub start_line: Option<u32>,
    pub line: u32,
    pub body: String,
}

/// What the preview shows: exactly what posts, and the hash an approval of it pins.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishPreview {
    pub review: ReviewPayload,
    pub hash: String,
    /// The inline comments as they post, markers included, in the order they are sent.
    pub comments: Vec<PostedComment>,
    /// The pending review's own body, which is its marker and nothing else.
    pub pending_body: String,
    /// The review's body as it is submitted: the summary, the folded comments and its marker.
    pub body: String,
    /// Who it posts as, by the forge's sign-in.
    pub account: Option<String>,
    /// The pull request is yours, so only a comment is possible.
    pub own_pull_request: bool,
    /// The pull request's head by this read. Not the reviewed commit when it moved.
    pub head: String,
    pub url: String,
}

/// A publication as the window shows it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicationView {
    #[serde(flatten)]
    pub publication: Publication,
    pub state: PublicationState,
    /// The posted review on the forge, once an operation has left one.
    pub url: Option<String>,
}

/// `review` exactly as it would post from `flow`: every body with its marker, in the order sent.
/// What the preview of a review and of a fix's suggestions shows.
pub(super) fn preview_of(
    review: ReviewPayload,
    flow: FlowId,
    account: Option<String>,
    own_pull_request: bool,
    head: &str,
    url: &str,
) -> PublishPreview {
    let operations = review.operations(flow);
    let mut comments = Vec::new();
    let mut pending_body = String::new();
    let mut body = String::new();
    for op in &operations {
        let marker = Marker::of(&op.key);
        match &op.payload {
            OperationPayload::PendingReview(p) => {
                pending_body = marker.tag();
                comments = p
                    .comments
                    .iter()
                    .zip(sent_comments(p, &marker))
                    .map(|(comment, sent)| PostedComment {
                        finding: comment.finding,
                        path: sent.path,
                        start_line: sent.start_line,
                        line: sent.line,
                        body: sent.body,
                    })
                    .collect();
            }
            OperationPayload::SubmitReview(p) => body = submit_body(p, &marker),
            OperationPayload::PushBranch(_) | OperationPayload::OpenPullRequest(_) => {}
        }
    }
    PublishPreview {
        hash: review.hash(),
        own_pull_request,
        account,
        head: head.to_owned(),
        url: url.to_owned(),
        review,
        comments,
        pending_body,
        body,
    }
}

/// Everything a review is built against, read fresh.
struct Facts {
    read: ReadNow,
    snapshot: Snapshot,
    findings: Vec<FindingRecord>,
    lines: ChangedLines,
}

impl Facts {
    fn own_pull_request(&self) -> bool {
        match (&self.read.pull_request.author, &self.read.account.login) {
            (Some(author), Some(login)) => author.eq_ignore_ascii_case(login),
            _ => false,
        }
    }

    fn build(&self, draft: &ReviewDraft) -> Result<ReviewPayload, ErrorView> {
        let pr = &self.read.pull_request;
        ReviewPayload::build(
            &PublishContext {
                repo: &pr.base.repo,
                number: pr.number,
                commit_id: &self.snapshot.head_sha,
                findings: &self.findings,
                lines: &self.lines,
                own_pull_request: self.own_pull_request(),
            },
            draft,
        )
        .map_err(|e| refused(e.0))
    }
}

fn refused(message: impl Into<String>) -> ErrorView {
    ErrorView::new("flow", message)
}

fn store_error(error: &FlowStoreError) -> ErrorView {
    reviews::store_error(error)
}

fn short(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

/// The refusal for a head that moved past the reviewed commit, with both heads.
fn head_moved(number: u64, reviewed: &str, current: &str) -> ErrorView {
    ErrorView::new(
        HEAD_MOVED,
        format!(
            "#{number} changed after you reviewed it: it is at {} now, and your review covers {}. \
             Nothing was posted.",
            short(current),
            short(reviewed)
        ),
    )
    .with_detail(serde_json::json!({ "reviewed": reviewed, "current": current }))
}

impl Engine {
    fn facts(&self, flow: FlowId) -> Result<Facts, ErrorView> {
        if self.is_running(flow) {
            return Err(refused(
                "This review is still running; publish it once it has finished.",
            ));
        }
        let db = self.db().map_err(refused)?;
        let state = db
            .flow(flow)
            .map_err(|e| store_error(&e))?
            .ok_or_else(|| refused(format!("no run {flow}")))?;
        let item = self.pull_request_item(&state).ok_or_else(|| {
            refused("Only a review of a teammate's pull request can be published.")
        })?;
        let run: ReviewRun = db
            .review(flow)
            .map_err(|e| store_error(&e))?
            .ok_or_else(|| refused(format!("run {flow} is not a review")))?;
        let snapshot = run
            .snapshot
            .and_then(|id| db.snapshot(id).ok().flatten())
            .ok_or_else(|| refused("This review has not read a head yet."))?;
        let read = reviews::read_as(&self.app, run.project.as_str(), item.id)?;
        let findings = db.findings(flow).map_err(|e| store_error(&e))?;
        let pr = &read.pull_request;
        let base = pr
            .base
            .sha
            .as_ref()
            .map_or_else(|| pr.base.branch.clone(), |sha| sha.as_str().to_owned());
        let lines = self.app.forge.changed_lines(
            &read.project.root,
            &pr.base.repo,
            &base,
            &snapshot.head_sha,
        )?;
        Ok(Facts {
            read,
            snapshot,
            findings,
            lines,
        })
    }

    /// Exactly what `draft` would post on the review's pull request, markers and all.
    ///
    /// # Errors
    ///
    /// When the review cannot be published (still running, not a teammate's pull request, nothing
    /// read yet), the forge cannot be read, or the draft would post nothing or something GitHub
    /// refuses outright.
    pub fn preview_publish(
        &self,
        flow: FlowId,
        draft: &ReviewDraft,
    ) -> Result<PublishPreview, ErrorView> {
        let facts = self.facts(flow)?;
        let review = facts.build(draft)?;
        Ok(preview_of(
            review,
            flow,
            facts.read.account.login.clone(),
            facts.own_pull_request(),
            facts.read.pull_request.head.sha.as_str(),
            &facts.read.pull_request.url,
        ))
    }

    /// Approve exactly what the preview showed and start sending it.
    ///
    /// `previewed` is the hash the preview showed. `against` is the pull request's head when you
    /// chose to publish against the reviewed commit after it moved: the approval then pins that
    /// head, and another push stops it again.
    ///
    /// # Errors
    ///
    /// When what would post is not what was previewed, the head moved and `against` does not name
    /// it ([`HEAD_MOVED`]), an earlier publish of the review is mid-send, or anything
    /// [`Self::preview_publish`] refuses.
    pub fn approve_publish(
        self: &Arc<Self>,
        flow: FlowId,
        draft: &ReviewDraft,
        previewed: &str,
        against: Option<&str>,
    ) -> Result<PublicationView, ErrorView> {
        let facts = self.facts(flow)?;
        let review = facts.build(draft)?;
        if review.hash() != previewed {
            return Err(refused(
                "What would post has changed since you previewed it: a finding, the pull \
                 request's diff or your text. Preview it again.",
            ));
        }
        let head = facts.read.pull_request.head.sha.as_str();
        if head != review.commit_id && against != Some(head) {
            return Err(head_moved(review.number, &review.commit_id, head));
        }
        if self.publishing_in(flow) {
            return Err(refused(
                "An earlier publish of this review is still being sent. Wait for it to finish.",
            ));
        }
        let approval =
            NewApproval::review(flow, &review, head).map_err(|e| refused(e.to_string()))?;
        let db = self.db().map_err(refused)?;
        for earlier in db.publications(flow).map_err(|e| store_error(&e))? {
            self.replace(&earlier);
        }
        let published = db
            .approve(&approval, &review.operations(flow), self.now())
            .map_err(|e| match e {
                FlowStoreError::StillSending { .. } => refused(
                    "An earlier publish of exactly this text was being sent when wtm stopped, \
                     and GitHub has not been checked yet. Retry that one instead.",
                ),
                other => store_error(&other),
            })?;
        tracing::info!(%flow, approval = %published.approval.id, hash = %review.hash(),
            "approved publishing a review");
        self.notice.changed(flow);
        self.send(published.approval.id);
        self.publication_view(published.approval.id)
    }

    /// Send what is left of a publish that failed or stopped: after a crash, after GitHub was
    /// unreachable, or after you dealt with a pending review on GitHub yourself.
    ///
    /// # Errors
    ///
    /// When it waits for a new approval because the head moved, or it does not exist.
    pub fn retry_publication(
        self: &Arc<Self>,
        approval: ApprovalId,
    ) -> Result<PublicationView, ErrorView> {
        let publication = self.publication_of(approval)?;
        for op in &publication.operations {
            match (&op.state, &op.attention) {
                (OperationState::NeedsAttention, Some(Attention::HeadMoved { .. })) => {
                    return Err(refused(
                        "The pull request moved after you approved this, so it needs your \
                         approval again rather than a retry.",
                    ));
                }
                (OperationState::NeedsAttention, _) => {
                    self.change(op.id, &OperationChange::to(OperationState::Pending));
                }
                _ => {}
            }
        }
        self.send(approval);
        self.publication_view(approval)
    }

    /// Put the review's comments on the pending review that was in the way, then submit that review
    /// with yours. Whatever it already held is submitted with it, which the card says before you
    /// choose this.
    ///
    /// # Errors
    ///
    /// When no operation of it is stopped on a pending review.
    pub fn add_to_pending_review(
        self: &Arc<Self>,
        approval: ApprovalId,
    ) -> Result<PublicationView, ErrorView> {
        let publication = self.publication_of(approval)?;
        let (op, review, node_id) = publication
            .operations
            .iter()
            .find_map(|op| match &op.attention {
                Some(Attention::PendingReview {
                    review, node_id, ..
                }) => Some((op.id, *review, node_id.clone())),
                _ => None,
            })
            .ok_or_else(|| refused("Nothing of this publish is waiting on a pending review."))?;
        self.change(
            op,
            &OperationChange {
                resolution: wtm_core::flow::Field::Set(Resolution::AddToPendingReview {
                    review,
                    node_id,
                }),
                ..OperationChange::to(OperationState::Pending)
            },
        );
        self.send(approval);
        self.publication_view(approval)
    }

    /// Stop a publish where it is: nothing more is sent, and what was posted stays posted.
    ///
    /// # Errors
    ///
    /// When it does not exist, or an operation is mid-send in this process.
    pub fn cancel_publication(&self, approval: ApprovalId) -> Result<PublicationView, ErrorView> {
        if self.activity(approval) == Some(Activity::Posting) {
            return Err(refused(
                "It is being sent right now. Wait a moment and look again.",
            ));
        }
        let publication = self.publication_of(approval)?;
        self.replace(&publication);
        self.notice.changed(publication.approval.flow);
        self.publication_view(approval)
    }

    /// Cancel what is left of `publication` and settle its items. An operation whose send is
    /// unsettled is left alone: whether it happened is not known yet.
    pub(super) fn replace(&self, publication: &Publication) {
        for op in &publication.operations {
            if matches!(
                op.state,
                OperationState::Pending | OperationState::Failed | OperationState::NeedsAttention
            ) {
                self.change(op.id, &OperationChange::to(OperationState::Cancelled));
            }
        }
        self.settle_publication(publication.approval.flow, publication.approval.id);
    }

    fn publication_of(&self, approval: ApprovalId) -> Result<Publication, ErrorView> {
        self.db()
            .map_err(refused)?
            .publication(approval)
            .map_err(|e| store_error(&e))?
            .ok_or_else(|| refused(format!("no approval {approval}")))
    }

    /// One publication as the window shows it.
    ///
    /// # Errors
    ///
    /// When it does not exist or the database cannot be read.
    pub fn publication_view(&self, approval: ApprovalId) -> Result<PublicationView, ErrorView> {
        Ok(self.view_of(self.publication_of(approval)?))
    }

    fn view_of(&self, publication: Publication) -> PublicationView {
        let state = publication.state(self.activity(publication.approval.id));
        let url = publication
            .operations
            .iter()
            .rev()
            .find_map(|op| match &op.result {
                Some(OperationResult::Review { url, .. }) if !url.is_empty() => Some(url.clone()),
                _ => None,
            });
        PublicationView {
            publication,
            state,
            url,
        }
    }

    /// A review's publications, oldest first, as the window shows them.
    pub(super) fn publication_views(&self, flow: FlowId) -> Result<Vec<PublicationView>, String> {
        let db = self.db()?;
        Ok(db
            .publications(flow)
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|publication| !publication.operations.is_empty())
            .map(|publication| self.view_of(publication))
            .collect())
    }

    /// File an inbox item about a publication.
    pub(super) fn file_publication(
        &self,
        publication: &Publication,
        kind: InboxKind,
        title: String,
        body: String,
    ) {
        self.file(&wtm_core::flow::NewInboxItem {
            flow: publication.approval.flow,
            kind,
            target: InboxTarget::Approval {
                approval: publication.approval.id,
            },
            title,
            body,
        });
    }

    /// A publication's items are settled: it finished, or you stopped or replaced it.
    pub(super) fn settle_publication(&self, flow: FlowId, approval: ApprovalId) {
        let Ok(db) = self.db() else { return };
        let target = InboxTarget::Approval { approval };
        for kind in [InboxKind::Approval, InboxKind::Failure] {
            let _ = db.resolve_inbox(flow, kind, Some(&target), self.now());
        }
    }

    /// The approved head of a publication, as its approval pinned it.
    pub(super) fn approved_head(publication: &Publication) -> String {
        publication
            .approval
            .binding
            .0
            .get(&Pinned::Head)
            .cloned()
            .unwrap_or_else(|| publication.review.commit_id.clone())
    }
}
