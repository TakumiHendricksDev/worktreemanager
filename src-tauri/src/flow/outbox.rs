//! The outbox's executor: it sends an approved publication's operations one at a time, reading
//! GitHub before every send, and settles what a crash left mid-write before anything is sent again.
//!
//! # Read, then mark, then send
//!
//! For each operation, in order:
//!
//! 1. **Read the pull request** and check the approval against its head now. A head that moved
//!    stops the publish and files an approval item: nothing more is sent without a new approval.
//! 2. **Read its reviews** and look for this operation's marker. Found, the write already happened
//!    (a reply lost on the way back, a retry after a crash) and is adopted; a pending review of
//!    yours that is not this operation's stops it for you to decide about, since GitHub allows one.
//! 3. **Mark it executing**, durably (`synchronous = FULL`), and only then **send** it.
//! 4. A reply settles it. A failure whose outcome is unknown — a timeout, a dropped connection — is
//!    settled by reading again: found, done; not found, failed and waiting for Retry.
//!
//! # A crash is checked, never re-sent
//!
//! An operation found executing at launch was cut off mid-write by a process that is gone. Recovery
//! reads GitHub ([`Engine::recover_publications`]) and settles it as done or failed; the window
//! shows "Checking GitHub…" meanwhile. Nothing is sent by recovery, not even the next operation:
//! whether to carry on is yours, and Retry is how you say so.
//!
//! # A fault hook, for click-throughs only
//!
//! `WTM_DEV_PUBLISH_FAULT` makes a debug build fail where a real publish can, so a partial publish
//! and a crash can be shown on a real pull request without waiting for GitHub to misbehave. A
//! release build never reads it. See [`Fault`].

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use wtm_core::flow::{
    Activity, ApprovalId, Attention, InboxKind, Marker, Operation, OperationChange,
    OperationPayload, OperationResult, OperationState, PendingFound, PendingReviewPayload,
    Publication, RemoteReview, RemoteReviewState, Resolution, SubmitFound, SubmitReviewPayload,
    carries_marker, find_pending, find_submitted, missing_comments, signed,
};
use wtm_core::ports::{FlowStore, ForgeError, OutboxStore, RunStore, SentComment};

use super::Engine;
use crate::reviews;

/// What a debug build's `WTM_DEV_PUBLISH_FAULT` asks it to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// `fail-submit-once`: the first submission fails without being sent, as if GitHub had been
    /// unreachable, so the comments are posted and the submission is left for Retry.
    FailSubmitOnce,
    /// `lose-comments-reply`: the first pending review is created on GitHub and its reply is
    /// thrown away, as a dropped connection would, so the executor has to find it by its marker.
    LoseCommentsReply,
    /// `stall-after-comments`: once the pending review is created, wait ten minutes before
    /// recording it, which is long enough to kill the app mid-publish.
    StallAfterComments,
    /// `stall-before-push`: once a route's push has read the pull request, wait a minute before
    /// it reads the remote branch, which is long enough for "the author" to push in between and
    /// show the read before the push stopping it.
    StallBeforePush,
}

impl Fault {
    /// The fault this process was started with. Always `None` in a release build.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        if !cfg!(debug_assertions) {
            return None;
        }
        match std::env::var("WTM_DEV_PUBLISH_FAULT").ok()?.trim() {
            "fail-submit-once" => Some(Self::FailSubmitOnce),
            "lose-comments-reply" => Some(Self::LoseCommentsReply),
            "stall-after-comments" => Some(Self::StallAfterComments),
            "stall-before-push" => Some(Self::StallBeforePush),
            other => {
                tracing::warn!(
                    value = other,
                    "WTM_DEV_PUBLISH_FAULT names no fault; ignored"
                );
                None
            }
        }
    }
}

/// How long `stall-after-comments` holds a publish, in half-second steps so a quit is not held.
const STALL_STEPS: u32 = 1_200;

/// How long `stall-before-push` holds a push, in half-second steps.
pub(super) const PUSH_STALL_STEPS: u32 = 120;

/// The comments of a pending review as they are sent: each body ends with its marker.
pub(super) fn sent_comments(payload: &PendingReviewPayload, marker: &Marker) -> Vec<SentComment> {
    payload
        .comments
        .iter()
        .enumerate()
        .map(|(index, comment)| SentComment {
            path: comment.path.clone(),
            start_line: comment.start_line,
            line: comment.line,
            body: signed(&comment.body, &marker.comment_tag(index)),
        })
        .collect()
}

/// A review's body as it is submitted: its text, then its marker.
pub(super) fn submit_body(payload: &SubmitReviewPayload, marker: &Marker) -> String {
    signed(&payload.body, &marker.tag())
}

/// What the executor does after one operation.
enum Then {
    Next,
    Stop,
}

/// Where an operation writes to, and as whom.
struct Target {
    root: std::path::PathBuf,
    head: String,
    login: Option<String>,
}

fn reviewed(review: &RemoteReview, adopted: bool) -> OperationResult {
    OperationResult::Review {
        review: review.id,
        node_id: review.node_id.clone(),
        url: review.url.clone(),
        adopted,
    }
}

fn short(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

impl Engine {
    /// What this process is doing with a publication now.
    pub(super) fn activity(&self, approval: ApprovalId) -> Option<Activity> {
        self.publishing.lock().get(&approval).copied()
    }

    /// Whether any publication of `flow` is being sent by this process.
    pub(super) fn publishing_in(&self, flow: wtm_core::flow::FlowId) -> bool {
        let busy: Vec<ApprovalId> = self.publishing.lock().keys().copied().collect();
        let Ok(db) = self.db() else { return false };
        busy.into_iter().any(|approval| {
            db.publication(approval)
                .ok()
                .flatten()
                .is_some_and(|p| p.approval.flow == flow)
        })
    }

    pub(super) fn claim(&self, approval: ApprovalId, activity: Activity) -> bool {
        let mut busy = self.publishing.lock();
        if busy.contains_key(&approval) {
            return false;
        }
        busy.insert(approval, activity);
        true
    }

    pub(super) fn release(&self, approval: ApprovalId) {
        self.publishing.lock().remove(&approval);
    }

    /// Change an operation and tell the window. A refused change is logged: the executor has read
    /// something the rules do not expect, and stopping there is the safe answer.
    pub(super) fn change(
        &self,
        op: wtm_core::flow::OperationId,
        change: &OperationChange,
    ) -> Option<Operation> {
        let db = self.db().ok()?;
        match db.change_operation(op, change, self.now()) {
            Ok(changed) => {
                self.notice.changed(changed.key.flow);
                Some(changed)
            }
            Err(error) => {
                tracing::warn!(%op, %error, "could not record an operation's change");
                None
            }
        }
    }

    /// Send what is left of a publication, on a thread of its own. `false` when this process is
    /// sending or checking it already.
    pub(super) fn send(self: &Arc<Self>, approval: ApprovalId) -> bool {
        if !self.claim(approval, Activity::Posting) {
            return false;
        }
        let engine = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name(format!("publish-{approval}"))
            .spawn(move || {
                engine.send_all(approval);
                engine.release(approval);
                if let Ok(Some(p)) = engine
                    .db()
                    .map(|db| db.publication(approval).ok().flatten())
                {
                    engine.notice.changed(p.approval.flow);
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "could not start sending a publication");
            self.release(approval);
            return false;
        }
        true
    }

    /// Run the publication's operations in order until one stops.
    fn send_all(&self, approval: ApprovalId) {
        loop {
            let Some(publication) = self
                .db()
                .ok()
                .and_then(|db| db.publication(approval).ok().flatten())
            else {
                return;
            };
            let Some(op) = publication.next().cloned() else {
                self.settle_publication(publication.approval.flow, approval);
                tracing::info!(%approval, "published a review");
                return;
            };
            let then = match op.state {
                OperationState::Cancelled
                | OperationState::NeedsAttention
                | OperationState::Done => Then::Stop,
                // Left mid-write by an earlier process and not checked yet: check it first. A
                // retry asked for it, so a send that turns out not to have happened is sent once
                // the check has said so, after the reads the next pass makes.
                OperationState::Executing => match self.settle_unsent(&publication, &op) {
                    Some(OperationState::Done | OperationState::Failed) => Then::Next,
                    _ => Then::Stop,
                },
                OperationState::Pending | OperationState::Failed => {
                    self.send_one(&publication, &op)
                }
            };
            if matches!(then, Then::Stop) {
                return;
            }
        }
    }

    /// Read the pull request a publication writes to, as the signed-in account.
    fn target(&self, publication: &Publication) -> Result<Target, String> {
        let db = self.db()?;
        let state = db
            .flow(publication.approval.flow)
            .map_err(|e| e.to_string())?
            .ok_or("the review is gone")?;
        let run = db
            .review(publication.approval.flow)
            .map_err(|e| e.to_string())?
            .ok_or("the review is gone")?;
        let read = reviews::read_as(&self.app, run.project.as_str(), state.work_item)
            .map_err(|e| e.message)?;
        Ok(Target {
            root: read.project.root,
            head: read.pull_request.head.sha.as_str().to_owned(),
            login: read.account.login,
        })
    }

    fn send_one(&self, publication: &Publication, op: &Operation) -> Then {
        let target = match self.target(publication) {
            Ok(target) => target,
            Err(error) => {
                self.failed(
                    publication,
                    op,
                    &format!("GitHub could not be read: {error}"),
                );
                return Then::Stop;
            }
        };
        let diff_now = self.fix_diff_now(publication.approval.flow);
        if let Err(stale) = publication
            .approval
            .check(&publication.binding_now(&target.head, diff_now.as_deref()))
        {
            tracing::info!(approval = %publication.approval.id, changes = stale.changes.len(),
                "a publish stopped: the pull request moved after it was approved");
            let approved = Self::approved_head(publication);
            self.change(
                op.id,
                &OperationChange::stopped(Attention::HeadMoved {
                    approved: approved.clone(),
                    current: target.head.clone(),
                }),
            );
            let number = publication.review.number;
            self.file_publication(
                publication,
                InboxKind::Approval,
                format!(
                    "Publishing your review of #{number} stopped: it changed after you approved"
                ),
                format!(
                    "You approved posting at {}, and #{number} is at {} now. {} Re-review the \
                     changes, or publish against {} anyway.",
                    short(&approved),
                    short(&target.head),
                    Self::posted_so_far(publication),
                    short(&publication.review.commit_id)
                ),
            );
            return Then::Stop;
        }
        match &op.payload {
            OperationPayload::PendingReview(payload) => match &op.resolution {
                None => self.start_pending(&target, publication, op, payload),
                Some(Resolution::AddToPendingReview { review, node_id }) => {
                    self.add_to(&target, publication, op, payload, *review, node_id)
                }
            },
            OperationPayload::SubmitReview(payload) => {
                self.submit(&target, publication, op, payload)
            }
            OperationPayload::PushBranch(_)
            | OperationPayload::OpenPullRequest(_)
            | OperationPayload::MoveIssue(_) => {
                self.failed(publication, op, "this is not a review's write");
                Then::Stop
            }
        }
    }

    /// What has already reached GitHub, in a sentence, for an item about a publish that stopped.
    fn posted_so_far(publication: &Publication) -> &'static str {
        if publication.done() == 0 {
            "Nothing was posted."
        } else {
            "Your inline comments are on a pending review only you can see; nothing has been \
             submitted."
        }
    }

    fn start_pending(
        &self,
        target: &Target,
        publication: &Publication,
        op: &Operation,
        payload: &PendingReviewPayload,
    ) -> Then {
        let marker = Marker::of(&op.key);
        let reviews =
            match self
                .app
                .review_writer
                .reviews(&target.root, &payload.repo, payload.number)
            {
                Ok(reviews) => reviews,
                Err(error) => return self.unread(publication, op, &error),
            };
        match find_pending(&marker, target.login.as_deref(), &reviews) {
            PendingFound::Posted(review) => return self.adopted(op, &review),
            PendingFound::InTheWay(review) => {
                return self.in_the_way(target, publication, op, payload, &review);
            }
            PendingFound::Absent => {}
        }
        if self.change(op.id, &OperationChange::sending()).is_none() {
            return Then::Stop;
        }
        let comments = sent_comments(payload, &marker);
        let sent = self.app.review_writer.create_pending_review(
            &target.root,
            &payload.repo,
            payload.number,
            &payload.commit_id,
            &marker.tag(),
            &comments,
        );
        let sent = match (sent, self.fault) {
            (Ok(_), Some(Fault::LoseCommentsReply)) if !self.fired.swap(true, Ordering::SeqCst) => {
                tracing::warn!("WTM_DEV_PUBLISH_FAULT: dropping the pending review's reply");
                Err(ForgeError::Failed {
                    argv: "gh api (fault hook)".to_owned(),
                    message: "WTM_DEV_PUBLISH_FAULT=lose-comments-reply threw the reply away"
                        .to_owned(),
                })
            }
            (Ok(review), Some(Fault::StallAfterComments)) => {
                tracing::warn!(
                    review = review.id,
                    "WTM_DEV_PUBLISH_FAULT: holding a created pending review unrecorded"
                );
                for _ in 0..STALL_STEPS {
                    if self.quitting() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
                Ok(review)
            }
            (sent, _) => sent,
        };
        match sent {
            Ok(review) => {
                self.change(op.id, &OperationChange::done(reviewed(&review, false)));
                Then::Next
            }
            Err(ForgeError::PendingReviewExists { .. }) => {
                // Started between the read and the send, by you elsewhere or by an earlier attempt
                // of this one: read again to tell which.
                match self
                    .app
                    .review_writer
                    .reviews(&target.root, &payload.repo, payload.number)
                {
                    Ok(reviews) => {
                        match find_pending(&marker, target.login.as_deref(), &reviews) {
                            PendingFound::Posted(review) => self.adopted(op, &review),
                            PendingFound::InTheWay(review) => {
                                self.in_the_way(target, publication, op, payload, &review)
                            }
                            PendingFound::Absent => {
                                self.failed(publication, op, "GitHub said a pending review was in the way and then showed none");
                                Then::Stop
                            }
                        }
                    }
                    Err(error) => self.unread(publication, op, &error),
                }
            }
            Err(ForgeError::Rejected { reason, .. }) => self.refused(publication, op, &reason),
            Err(error) if error.outcome_unknown() => {
                match self
                    .app
                    .review_writer
                    .reviews(&target.root, &payload.repo, payload.number)
                {
                    Ok(reviews) => {
                        if let PendingFound::Posted(review) =
                            find_pending(&marker, target.login.as_deref(), &reviews)
                        {
                            self.adopted(op, &review)
                        } else {
                            self.failed(publication, op, &error.to_string());
                            Then::Stop
                        }
                    }
                    Err(read) => {
                        self.failed(
                            publication,
                            op,
                            &format!(
                                "{error}. GitHub could not be read afterwards to see whether it \
                                 arrived ({read}); Retry checks before sending anything."
                            ),
                        );
                        Then::Stop
                    }
                }
            }
            Err(error) => {
                self.failed(publication, op, &error.to_string());
                Then::Stop
            }
        }
    }

    fn add_to(
        &self,
        target: &Target,
        publication: &Publication,
        op: &Operation,
        payload: &PendingReviewPayload,
        review: u64,
        node_id: &str,
    ) -> Then {
        let marker = Marker::of(&op.key);
        let writer = &self.app.review_writer;
        let reviews = match writer.reviews(&target.root, &payload.repo, payload.number) {
            Ok(reviews) => reviews,
            Err(error) => return self.unread(publication, op, &error),
        };
        let Some(pending) = reviews
            .iter()
            .find(|r| r.id == review && r.state == RemoteReviewState::Pending)
            .cloned()
        else {
            self.change(
                op.id,
                &OperationChange {
                    resolution: wtm_core::flow::Field::Clear,
                    ..OperationChange::stopped(Attention::Refused {
                        message: "The pending review you chose to add to is not pending on \
                                  GitHub any more: it was submitted or deleted there. Retry \
                                  starts a review of wtm's own."
                            .to_owned(),
                    })
                },
            );
            return Then::Stop;
        };
        let existing =
            match writer.review_comments(&target.root, &payload.repo, payload.number, review) {
                Ok(comments) => comments,
                Err(error) => return self.unread(publication, op, &error),
            };
        let missing = missing_comments(&marker, payload.comments.len(), &existing);
        if missing.is_empty() {
            return self.adopted(op, &pending);
        }
        if self.change(op.id, &OperationChange::sending()).is_none() {
            return Then::Stop;
        }
        let comments = sent_comments(payload, &marker);
        for index in missing {
            match writer.add_review_comment(&target.root, &payload.repo, node_id, &comments[index])
            {
                Ok(()) => {}
                Err(ForgeError::Rejected { reason, .. }) => {
                    return self.refused(publication, op, &reason);
                }
                Err(error) => {
                    // Some may have arrived; the next attempt adds only what its markers show is
                    // missing.
                    self.failed(publication, op, &error.to_string());
                    return Then::Stop;
                }
            }
        }
        self.change(op.id, &OperationChange::done(reviewed(&pending, false)));
        Then::Next
    }

    fn submit(
        &self,
        target: &Target,
        publication: &Publication,
        op: &Operation,
        payload: &SubmitReviewPayload,
    ) -> Then {
        let pending = publication
            .operations
            .iter()
            .filter(|earlier| earlier.position < op.position)
            .find_map(|earlier| earlier.result.as_ref().and_then(OperationResult::review));
        let Some((review, _)) = pending else {
            self.failed(
                publication,
                op,
                "the pending review it submits is not known",
            );
            return Then::Stop;
        };
        let marker = Marker::of(&op.key);
        let writer = &self.app.review_writer;
        let reviews = match writer.reviews(&target.root, &payload.repo, payload.number) {
            Ok(reviews) => reviews,
            Err(error) => return self.unread(publication, op, &error),
        };
        match find_submitted(&marker, review, &reviews) {
            SubmitFound::Submitted(found) => return self.adopted(op, &found),
            SubmitFound::Pending(_) => {}
            SubmitFound::SubmittedElsewhere(_) => {
                return self.refused(
                    publication,
                    op,
                    &format!(
                        "Your pending review on #{} was submitted on GitHub, without this \
                         summary and state. Its comments are posted; nothing more is sent.",
                        payload.number
                    ),
                );
            }
            SubmitFound::Gone => {
                return self.refused(
                    publication,
                    op,
                    "The pending review holding your comments is not on GitHub any more: it was \
                     deleted there. Nothing more is sent.",
                );
            }
        }
        if self.change(op.id, &OperationChange::sending()).is_none() {
            return Then::Stop;
        }
        if self.fault == Some(Fault::FailSubmitOnce) && !self.fired.swap(true, Ordering::SeqCst) {
            tracing::warn!("WTM_DEV_PUBLISH_FAULT: failing a submission without sending it");
            self.failed(
                publication,
                op,
                "The submission was not sent: WTM_DEV_PUBLISH_FAULT=fail-submit-once stood in for \
                 GitHub being unreachable.",
            );
            return Then::Stop;
        }
        match writer.submit_review(
            &target.root,
            &payload.repo,
            payload.number,
            review,
            payload.event,
            &submit_body(payload, &marker),
        ) {
            Ok(found) => {
                self.change(op.id, &OperationChange::done(reviewed(&found, false)));
                Then::Next
            }
            Err(ForgeError::Rejected { reason, .. }) => self.refused(publication, op, &reason),
            Err(error) if error.outcome_unknown() => {
                let found = writer
                    .reviews(&target.root, &payload.repo, payload.number)
                    .ok()
                    .map(|reviews| find_submitted(&marker, review, &reviews));
                if let Some(SubmitFound::Submitted(found)) = found {
                    self.adopted(op, &found)
                } else {
                    self.failed(publication, op, &error.to_string());
                    Then::Stop
                }
            }
            Err(error) => {
                self.failed(publication, op, &error.to_string());
                Then::Stop
            }
        }
    }

    /// The write was already on GitHub: done, without sending it.
    fn adopted(&self, op: &Operation, review: &RemoteReview) -> Then {
        tracing::info!(op = %op.id, review = review.id,
            "found an operation's write on GitHub by its marker; not sending it again");
        match self.change(op.id, &OperationChange::done(reviewed(review, true))) {
            Some(_) => Then::Next,
            None => Then::Stop,
        }
    }

    /// A pending review of yours is on the pull request and it is not this operation's.
    fn in_the_way(
        &self,
        target: &Target,
        publication: &Publication,
        op: &Operation,
        payload: &PendingReviewPayload,
        review: &RemoteReview,
    ) -> Then {
        let comments = self
            .app
            .review_writer
            .review_comments(&target.root, &payload.repo, payload.number, review.id)
            .unwrap_or_default();
        let started_by_wtm =
            carries_marker(&review.body) || comments.iter().any(|c| carries_marker(&c.body));
        self.change(
            op.id,
            &OperationChange::stopped(Attention::PendingReview {
                review: review.id,
                node_id: review.node_id.clone(),
                url: review.url.clone(),
                comments: u32::try_from(comments.len()).unwrap_or(u32::MAX),
                started_by_wtm,
            }),
        );
        let number = payload.number;
        self.file_publication(
            publication,
            InboxKind::Failure,
            format!("Your review of #{number} is waiting: you already have a pending review there"),
            format!(
                "GitHub allows one pending review per person, and yours on #{number} {} Nothing of \
                 this review was posted. Add your comments to it, open it on GitHub, or leave it \
                 as it is.",
                if started_by_wtm {
                    "was started by an earlier publish from wtm."
                } else {
                    "was started outside wtm, probably in the browser."
                }
            ),
        );
        Then::Stop
    }

    fn refused(&self, publication: &Publication, op: &Operation, message: &str) -> Then {
        self.change(
            op.id,
            &OperationChange::stopped(Attention::Refused {
                message: message.to_owned(),
            }),
        );
        let number = publication.review.number;
        self.file_publication(
            publication,
            InboxKind::Failure,
            format!("GitHub refused part of your review of #{number}"),
            format!("{message} {}", Self::posted_so_far(publication)),
        );
        Then::Stop
    }

    fn failed(&self, publication: &Publication, op: &Operation, error: &str) {
        self.change(op.id, &OperationChange::failed(error));
        let number = publication.review.number;
        let title = if publication.done() == 0 {
            format!("Your review of #{number} was not published")
        } else {
            format!("Your review of #{number} was only partly published")
        };
        self.file_publication(
            publication,
            InboxKind::Failure,
            title,
            format!(
                "{error} {} Retry checks GitHub first, so nothing is posted twice.",
                Self::posted_so_far(publication)
            ),
        );
    }

    /// GitHub could not be read before sending, so nothing was sent.
    fn unread(&self, publication: &Publication, op: &Operation, error: &ForgeError) -> Then {
        self.failed(
            publication,
            op,
            &format!("GitHub could not be read, so nothing was sent: {error}."),
        );
        Then::Stop
    }

    // ═══════════════════════════════ recovery ═══════════════════════════════

    /// At launch: every operation a crash left executing is checked against GitHub and settled as
    /// done or failed. Nothing is sent. Returns the publications it checked.
    ///
    /// Reads nothing, and opens no database, for someone who has never published.
    ///
    /// # Errors
    ///
    /// When the database cannot be read.
    pub fn recover_publications(&self) -> Result<Vec<ApprovalId>, String> {
        if !self.app.files.exists(&self.app.config.paths().flow_db) {
            return Ok(Vec::new());
        }
        let db = self.db()?;
        let mut checked: Vec<ApprovalId> = Vec::new();
        for op in db.executing_operations().map_err(|e| e.to_string())? {
            if checked.contains(&op.approval) || !self.claim(op.approval, Activity::Checking) {
                continue;
            }
            self.notice.changed(op.key.flow);
            if self.recover_route(op.approval, &op) {
                // A fix's route, checked by its own executor's rules (`routes.rs`).
            } else if self.recover_move(op.approval, &op) {
                // An issue's move, settled by reading the issue (`moves.rs`).
            } else if let Ok(Some(publication)) = db.publication(op.approval) {
                self.settle_unsent(&publication, &op);
                // Whatever the check found, a publish that is not finished waits for you, so it is
                // news: the comments may be on GitHub and the submission never sent.
                if let Ok(Some(now)) = db.publication(op.approval)
                    && now.state(None) != wtm_core::flow::PublicationState::Published
                {
                    let number = now.review.number;
                    self.file_publication(
                        &now,
                        InboxKind::Failure,
                        format!("wtm stopped while publishing your review of #{number}"),
                        format!(
                            "It was checked against GitHub at launch. {} Retry checks again and \
                             sends only what is missing.",
                            Self::posted_so_far(&now)
                        ),
                    );
                }
            }
            self.release(op.approval);
            self.notice.changed(op.key.flow);
            checked.push(op.approval);
        }
        Ok(checked)
    }

    /// Settle an operation a crash left executing by reading GitHub: done when its write is there,
    /// failed when it is not. Sends nothing. Returns the state it settled in.
    fn settle_unsent(&self, publication: &Publication, op: &Operation) -> Option<OperationState> {
        let target = match self.target(publication) {
            Ok(target) => target,
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
        let marker = Marker::of(&op.key);
        let writer = &self.app.review_writer;
        let gone = "wtm stopped while sending this, and GitHub shows it did not arrive. Retry \
                    sends it.";
        let change = match &op.payload {
            OperationPayload::PendingReview(payload) => match &op.resolution {
                None => match writer.reviews(&target.root, &payload.repo, payload.number) {
                    Ok(reviews) => match find_pending(&marker, target.login.as_deref(), &reviews) {
                        PendingFound::Posted(review) => {
                            OperationChange::done(reviewed(&review, true))
                        }
                        _ => OperationChange::failed(gone),
                    },
                    Err(error) => OperationChange::failed(format!(
                        "wtm stopped while sending this, and GitHub could not be read: {error}."
                    )),
                },
                Some(Resolution::AddToPendingReview { review, node_id }) => {
                    match writer.review_comments(
                        &target.root,
                        &payload.repo,
                        payload.number,
                        *review,
                    ) {
                        Ok(existing) => {
                            let missing =
                                missing_comments(&marker, payload.comments.len(), &existing);
                            if missing.is_empty() {
                                OperationChange::done(OperationResult::Review {
                                    review: *review,
                                    node_id: node_id.clone(),
                                    url: String::new(),
                                    adopted: true,
                                })
                            } else {
                                OperationChange::failed(format!(
                                    "wtm stopped while adding these comments: {} of {} are on \
                                     your pending review. Retry adds the rest.",
                                    payload.comments.len() - missing.len(),
                                    payload.comments.len()
                                ))
                            }
                        }
                        Err(error) => OperationChange::failed(format!(
                            "wtm stopped while adding these comments, and GitHub could not be \
                             read: {error}."
                        )),
                    }
                }
            },
            OperationPayload::SubmitReview(payload) => {
                let review = publication
                    .operations
                    .iter()
                    .filter(|earlier| earlier.position < op.position)
                    .find_map(|earlier| earlier.result.as_ref().and_then(OperationResult::review))
                    .map(|(review, _)| review);
                match (
                    review,
                    writer.reviews(&target.root, &payload.repo, payload.number),
                ) {
                    (Some(review), Ok(reviews)) => {
                        match find_submitted(&marker, review, &reviews) {
                            SubmitFound::Submitted(found) => {
                                OperationChange::done(reviewed(&found, true))
                            }
                            SubmitFound::Pending(_) => OperationChange::failed(gone),
                            SubmitFound::SubmittedElsewhere(_) | SubmitFound::Gone => {
                                OperationChange::stopped(Attention::Refused {
                                    message:
                                        "The pending review holding your comments was submitted \
                                          or deleted on GitHub while wtm was stopped. Nothing \
                                          more is sent."
                                            .to_owned(),
                                })
                            }
                        }
                    }
                    (_, Err(error)) => OperationChange::failed(format!(
                        "wtm stopped while sending this, and GitHub could not be read: {error}."
                    )),
                    (None, Ok(_)) => OperationChange::failed(gone),
                }
            }
            OperationPayload::PushBranch(_)
            | OperationPayload::OpenPullRequest(_)
            | OperationPayload::MoveIssue(_) => {
                OperationChange::failed("this is not a review's write")
            }
        };
        tracing::info!(op = %op.id, to = ?change.state, "settled an operation a crash left mid-write");
        self.change(op.id, &change).map(|op| op.state)
    }
}
