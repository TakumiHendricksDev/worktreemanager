//! Send to Jira's executor: read the issue's comments, then mark, then send, then read again.
//!
//! # Two clicks, and only the second sends
//!
//! **Send to Jira…** asks for [`Engine::comment_draft`], which reads and writes nothing: it builds
//! the text from the question and your answer (`wtm_core::flow::comment::draft`) for you to edit.
//! Only **Post comment** reaches [`Engine::post_comment`], with the text as it stood in front of
//! you; that call is the approval, and it pins the hash of exactly that text.
//!
//! # In order, for the one operation a comment is
//!
//! 1. **List the issue's comments** and look for this one by its normalized text
//!    ([`find_posted`]): there already, it is adopted and nothing is sent.
//! 2. **Mark it executing**, durably, and only then **send** it.
//! 3. **List them again**, whatever the reply said. Found, it is done, a lost reply included; not
//!    found, it failed, and Retry starts again from step 1.
//!
//! # A crash is checked, never re-sent
//!
//! An operation found executing at launch is settled by step 3 alone and nothing is sent, as for a
//! move. Comments are the one write here a person may need to clean up by hand when it goes twice,
//! and the read before every send is what keeps that from happening.

use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use wtm_core::flow::comment::{check_body, find_posted};
use wtm_core::flow::{
    Activity, ApprovalId, CommentPayload, CommentRequest, CommentSend, FlowId, InboxKind,
    InboxTarget, NewApproval, NewInboxItem, NewOperation, Operation, OperationChange,
    OperationPayload, OperationResult, OperationState, PublicationState, QuestionId,
    TrackerComment, TrackerKind,
};
use wtm_core::ports::{FlowStore, InboxStore, OutboxStore};

use super::{Engine, text};

/// One Send to Jira, as the question card shows it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommentView {
    pub approval: ApprovalId,
    pub request: CommentRequest,
    pub operation: Operation,
    pub state: PublicationState,
}

/// What Send to Jira… opens with: the issue it goes to and the text to edit. Nothing is recorded.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommentDraft {
    pub question: QuestionId,
    pub tracker: TrackerKind,
    pub key: String,
    pub body: String,
}

/// Where a comment writes.
struct CommentTarget {
    root: PathBuf,
}

impl Engine {
    /// The issue a run is for: its tracker and key, from its work item.
    pub(super) fn issue_of(&self, flow: FlowId) -> Result<(TrackerKind, String), String> {
        let db = self.db()?;
        let state = db.flow(flow).map_err(text)?.ok_or("the run has gone")?;
        let item = db
            .work_item(state.work_item)
            .map_err(text)?
            .ok_or("the work item has gone")?;
        let issue = item
            .issue
            .ok_or("this run is not about an issue, so there is nowhere to send it")?;
        let tracker = TrackerKind::parse(&issue.tracker)
            .ok_or_else(|| format!("wtm does not know the tracker `{}`", issue.tracker))?;
        Ok((tracker, issue.key))
    }

    /// The draft **Send to Jira…** opens: the question and your answer, nothing else. Reads and
    /// writes nothing outside wtm, and records nothing in it.
    ///
    /// # Errors
    ///
    /// When the question is not answered, or its run is not about an issue.
    pub fn comment_draft(&self, question: QuestionId) -> Result<CommentDraft, String> {
        let db = self.db()?;
        let asked = db
            .question(question)
            .map_err(text)?
            .ok_or_else(|| format!("no question {question}"))?;
        let body = wtm_core::flow::comment::draft(&asked)
            .ok_or("answer the question first: Send to Jira sends a question with its answer")?;
        let (tracker, key) = self.issue_of(asked.flow)?;
        Ok(CommentDraft {
            question,
            tracker,
            key,
            body,
        })
    }

    /// Post `body` to the question's issue: the second click. `body` is the draft as it stood in
    /// front of you, edits included, and it is exactly what the approval pins and what posts.
    ///
    /// # Errors
    ///
    /// When the question is not answered, the text cannot be sent as written, or it is not the
    /// text the window showed.
    pub fn post_comment(
        self: &Arc<Self>,
        question: QuestionId,
        body: &str,
    ) -> Result<CommentView, String> {
        let db = self.db()?;
        let asked = db
            .question(question)
            .map_err(text)?
            .ok_or_else(|| format!("no question {question}"))?;
        if asked.answer.is_none() {
            return Err(
                "answer the question first: Send to Jira sends a question with its answer"
                    .to_owned(),
            );
        }
        let body = body.trim();
        check_body(body).map_err(|e| e.to_string())?;
        let (tracker, key) = self.issue_of(asked.flow)?;
        let request = CommentRequest {
            payload: CommentPayload {
                tracker,
                key,
                body: body.to_owned(),
            },
            question,
        };
        let approval = self.queue_comment(asked.flow, &request)?;
        self.comment_view(approval)?
            .ok_or_else(|| "the comment was approved and then could not be found".to_owned())
    }

    /// Approve `request` and send it.
    pub(super) fn queue_comment(
        self: &Arc<Self>,
        flow: FlowId,
        request: &CommentRequest,
    ) -> Result<ApprovalId, String> {
        let db = self.db()?;
        let op = NewOperation::new(
            flow,
            0,
            OperationPayload::PostComment(request.payload.clone()),
        );
        let approved = db
            .approve(&NewApproval::comment(flow, request), &[op], self.now())
            .map_err(text)?;
        self.notice.changed(flow);
        self.send_comment(approved.approval.id);
        Ok(approved.approval.id)
    }

    /// Send a comment on a thread of its own. `false` when this process is sending it already.
    pub(super) fn send_comment(self: &Arc<Self>, approval: ApprovalId) -> bool {
        if !self.claim(approval, Activity::Posting) {
            return false;
        }
        let engine = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name(format!("comment-{approval}"))
            .spawn(move || {
                let flow = engine.comment_once(approval);
                engine.release(approval);
                if let Some(flow) = flow {
                    engine.notice.changed(flow);
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "could not start sending a comment");
            self.release(approval);
            return false;
        }
        true
    }

    fn comment_target(&self, send: &CommentSend) -> Result<CommentTarget, String> {
        let db = self.db()?;
        let flow = db
            .flow(send.approval.flow)
            .map_err(text)?
            .ok_or("the run has gone")?;
        let item = db
            .work_item(flow.work_item)
            .map_err(text)?
            .ok_or("the work item has gone")?;
        let (project, _) = crate::tracker::tracker_project(&self.app, item.project.as_str())
            .map_err(|e| e.message)?;
        Ok(CommentTarget {
            root: project.root.clone(),
        })
    }

    fn read_comments(
        &self,
        target: &CommentTarget,
        key: &str,
    ) -> Result<Vec<TrackerComment>, String> {
        self.app.tracker.comments(&target.root, key).map_err(text)
    }

    /// One attempt at a comment. Returns its run, for the window to be told.
    fn comment_once(&self, approval: ApprovalId) -> Option<FlowId> {
        let send = self.db().ok()?.issue_comment(approval).ok().flatten()?;
        let flow = send.approval.flow;
        let op = send.operation()?.clone();
        match op.state {
            OperationState::Done | OperationState::Cancelled | OperationState::NeedsAttention => {
                return Some(flow);
            }
            OperationState::Executing => {
                self.settle_unsent_comment(&send, &op);
                return Some(flow);
            }
            OperationState::Pending | OperationState::Failed => {}
        }
        let payload = &send.request.payload;
        let target = match self.comment_target(&send) {
            Ok(target) => target,
            Err(error) => {
                self.comment_failed(&send, &op, &format!("{error}. Nothing was sent."));
                return Some(flow);
            }
        };
        let before = match self.read_comments(&target, &payload.key) {
            Ok(comments) => comments,
            Err(error) => {
                self.comment_failed(
                    &send,
                    &op,
                    &format!(
                        "{}'s comments could not be read, so nothing was sent: {error}",
                        payload.key
                    ),
                );
                return Some(flow);
            }
        };
        if let Some(found) = find_posted(payload, &before) {
            self.comment_done(&send, &op, found, true);
            return Some(flow);
        }
        if self.change(op.id, &OperationChange::sending()).is_none() {
            return Some(flow);
        }
        let reply = self
            .app
            .tracker_writer
            .comment(&target.root, &payload.key, &payload.body);
        match (self.read_comments(&target, &payload.key), reply) {
            (Ok(after), reply) => match find_posted(payload, &after) {
                // A lost reply to a comment that arrived is the comment posted.
                Some(found) => self.comment_done(&send, &op, found, false),
                None => self.comment_failed(
                    &send,
                    &op,
                    &match reply {
                        Err(error) => format!("the tracker refused it: {error}"),
                        Ok(()) => format!(
                            "the tracker answered, and the comment is not among {}'s comments",
                            payload.key
                        ),
                    },
                ),
            },
            (Err(read), Ok(())) => {
                // Sent and acknowledged; only the confirming read failed. Done, with no id: the
                // next read of the issue shows it.
                tracing::warn!(
                    key = payload.key,
                    read,
                    "a comment's confirming read failed"
                );
                self.comment_done(
                    &send,
                    &op,
                    &TrackerComment {
                        id: String::new(),
                        author: None,
                        body: payload.body.clone(),
                    },
                    false,
                );
            }
            (Err(read), Err(error)) => self.comment_failed(
                &send,
                &op,
                &format!(
                    "{error}; and {}'s comments could not be read to see whether it arrived \
                     ({read}). Retry reads them first.",
                    payload.key
                ),
            ),
        }
        Some(flow)
    }

    fn comment_done(
        &self,
        send: &CommentSend,
        op: &Operation,
        found: &TrackerComment,
        adopted: bool,
    ) {
        self.change(
            op.id,
            &OperationChange::done(OperationResult::Commented {
                key: send.request.payload.key.clone(),
                comment: found.id.clone(),
                author: found.author.clone(),
                adopted,
            }),
        );
        if let Ok(db) = self.db() {
            let target = InboxTarget::Approval {
                approval: send.approval.id,
            };
            let _ = db.resolve_inbox(
                send.approval.flow,
                InboxKind::Failure,
                Some(&target),
                self.now(),
            );
        }
        tracing::info!(
            key = send.request.payload.key,
            adopted,
            "commented on an issue"
        );
    }

    fn comment_failed(&self, send: &CommentSend, op: &Operation, message: &str) {
        self.change(op.id, &OperationChange::failed(message));
        self.file(&NewInboxItem {
            flow: send.approval.flow,
            kind: InboxKind::Failure,
            target: InboxTarget::Approval {
                approval: send.approval.id,
            },
            title: format!(
                "Your comment on {} was not posted",
                send.request.payload.key
            ),
            body: format!(
                "{message}. Retry reads the issue's comments first and sends only if it is not \
                 there."
            ),
        });
    }

    /// Settle a comment a crash left executing, by reading the issue's comments. Sends nothing.
    fn settle_unsent_comment(&self, send: &CommentSend, op: &Operation) {
        let payload = &send.request.payload;
        let checked = self
            .comment_target(send)
            .and_then(|target| self.read_comments(&target, &payload.key));
        match checked {
            Ok(comments) => match find_posted(payload, &comments) {
                Some(found) => self.comment_done(send, op, found, true),
                None => self.comment_failed(
                    send,
                    op,
                    &format!(
                        "wtm stopped while sending this, and it is not among {}'s comments: it \
                         did not arrive",
                        payload.key
                    ),
                ),
            },
            Err(error) => self.comment_failed(
                send,
                op,
                &format!(
                    "wtm stopped while sending this, and the issue's comments could not be read \
                     to see whether it arrived: {error}"
                ),
            ),
        }
    }

    /// At launch, an executing operation that is a comment: settle it by reading. `false` when the
    /// operation is not a comment's.
    pub(super) fn recover_comment(&self, approval: ApprovalId, op: &Operation) -> bool {
        let Some(send) = self
            .db()
            .ok()
            .and_then(|db| db.issue_comment(approval).ok().flatten())
        else {
            return false;
        };
        self.settle_unsent_comment(&send, op);
        true
    }

    /// Try a failed comment again, reading the issue first.
    ///
    /// # Errors
    ///
    /// When it is being sent now, or is done.
    pub fn retry_comment(self: &Arc<Self>, approval: ApprovalId) -> Result<(), String> {
        let send = self
            .db()?
            .issue_comment(approval)
            .map_err(text)?
            .ok_or_else(|| format!("approval {approval} is not a comment"))?;
        let op = send
            .operation()
            .ok_or("a later approval of the same comment has it")?;
        if self.activity(approval).is_some() {
            return Err("it is being sent right now".to_owned());
        }
        if op.state == OperationState::Done {
            return Err("it is posted".to_owned());
        }
        self.send_comment(approval);
        self.notice.changed(send.approval.flow);
        Ok(())
    }

    fn comment_view(&self, approval: ApprovalId) -> Result<Option<CommentView>, String> {
        let Some(send) = self.db()?.issue_comment(approval).map_err(text)? else {
            return Ok(None);
        };
        Ok(self.as_view(send))
    }

    fn as_view(&self, send: CommentSend) -> Option<CommentView> {
        let state = send.state(self.activity(send.approval.id));
        let operation = send.operation()?.clone();
        Some(CommentView {
            approval: send.approval.id,
            request: send.request,
            operation,
            state,
        })
    }

    /// The comments sent for a run's questions, oldest first. An approval whose one comment a later
    /// approval of the same text took over is left out.
    pub(super) fn comment_views(&self, flow: FlowId) -> Result<Vec<CommentView>, String> {
        Ok(self
            .db()?
            .issue_comments(flow)
            .map_err(text)?
            .into_iter()
            .filter_map(|send| self.as_view(send))
            .collect())
    }
}
