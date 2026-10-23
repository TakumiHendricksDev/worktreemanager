//! The outbox's executor for moving an issue: read, then mark, then send, then read again.
//!
//! # In order, for the one operation a move is
//!
//! 1. **Read the issue's status** through the tracker's reader. [`wtm_core::flow::moves::step`]
//!    decides from it alone: already at the destination is adopted and nothing is sent; somewhere
//!    other than where the move expected stops it and files an approval item, because moving it
//!    now would undo a change someone made on purpose.
//! 2. **Mark it executing**, durably, and only then **send** it through the tracker's writer.
//! 3. **Read again**, whatever the reply said. The status is the tracker's own answer: at the
//!    destination, it is done, even when the reply was lost; anywhere else, it failed, and Retry
//!    starts again from step 1.
//!
//! Every attempt starts with a read, so Retry after any failure is safe: a move that happened
//! after all is adopted, not sent twice.
//!
//! # A crash is checked, never re-sent
//!
//! An operation found executing at launch is settled by step 3 alone ([`Engine::recover_move`])
//! and nothing is sent; Retry is yours, as for a review's publish (`outbox.rs`).
//!
//! # The race a tracker leaves open
//!
//! Between the read in step 1 and the send in step 2, nothing holds the issue still: Jira has no
//! compare-and-swap for a transition. A change made in that second would be overwritten. The window
//! says so beside every move rather than calling the read a guarantee.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde::Deserialize;
use wtm_core::flow::moves::{arrived, step};
use wtm_core::flow::{
    Activity, ApprovalId, Attention, FlowId, InboxKind, InboxTarget, MoveRequest, MoveSend,
    MoveStep, NewApproval, NewInboxItem, NewOperation, Operation, OperationChange,
    OperationPayload, OperationResult, OperationState, TrackerKind,
};
use wtm_core::ports::{FlowStore, InboxStore, OutboxStore, TrackerMirror};

use super::{Engine, text};

/// What you decided about a move that stopped or failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MoveAction {
    /// Send it again, reading the issue first.
    Retry,
    /// Move it from wherever it is now: a new approval of a new move.
    Anyway,
    /// Leave the issue where it is.
    Leave,
}

/// Where a move writes, read from its run's work item and project.
pub(super) struct MoveTarget {
    pub(super) root: PathBuf,
    tracker: TrackerKind,
    fields: BTreeMap<String, String>,
    /// The destination as the tracker spells it, when the queue holds an issue in it.
    pub(super) spelled: String,
}

impl Engine {
    /// Approve `request` and send it. The approval is the click that asked for it — Start, or a
    /// drop and its confirmation — and covers exactly this move. Returns the approval.
    pub(super) fn queue_move(
        self: &Arc<Self>,
        flow: FlowId,
        request: &MoveRequest,
    ) -> Result<ApprovalId, String> {
        let db = self.db()?;
        let op = NewOperation::new(
            flow,
            0,
            OperationPayload::MoveIssue(request.payload.clone()),
        );
        let approved = db
            .approve(&NewApproval::move_issue(flow, request), &[op], self.now())
            .map_err(text)?;
        self.notice.changed(flow);
        self.send_move(approved.approval.id);
        Ok(approved.approval.id)
    }

    /// Send a move on a thread of its own. `false` when this process is sending it already.
    pub(super) fn send_move(self: &Arc<Self>, approval: ApprovalId) -> bool {
        if !self.claim(approval, Activity::Posting) {
            return false;
        }
        let engine = Arc::clone(self);
        let spawned = std::thread::Builder::new()
            .name(format!("move-{approval}"))
            .spawn(move || {
                let flow = engine.move_once(approval);
                engine.release(approval);
                if let Some(flow) = flow {
                    engine.notice.changed(flow);
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "could not start sending a move");
            self.release(approval);
            return false;
        }
        true
    }

    fn move_target(&self, send: &MoveSend) -> Result<MoveTarget, String> {
        self.move_target_for(send.approval.flow, &send.request.payload.to)
    }

    /// Where a move of `flow`'s issue to `to` writes.
    pub(super) fn move_target_for(&self, flow: FlowId, to: &str) -> Result<MoveTarget, String> {
        let db = self.db()?;
        let flow = db.flow(flow).map_err(text)?.ok_or("the run has gone")?;
        let item = db
            .work_item(flow.work_item)
            .map_err(text)?
            .ok_or("the work item has gone")?;
        let (project, tracker) = crate::tracker::tracker_project(&self.app, item.project.as_str())
            .map_err(|e| e.message)?;
        let spelled = db
            .mirrored_queue(&item.project)
            .map_err(text)?
            .and_then(|m| m.reading)
            .and_then(|r| {
                r.issues
                    .into_iter()
                    .map(|q| q.issue.status.name)
                    .find(|name| wtm_core::flow::tracker::same_status(name, to))
            })
            .unwrap_or_else(|| to.to_owned());
        Ok(MoveTarget {
            root: project.root.clone(),
            tracker,
            fields: project.workflow.unwrap_or_default().fields,
            spelled,
        })
    }

    /// The issue's status now, read whole and kept in the mirror, so the board shows it.
    pub(super) fn read_status(&self, target: &MoveTarget, key: &str) -> Result<String, String> {
        let detail = self
            .app
            .tracker
            .issue(&target.root, key, &target.fields)
            .map_err(text)?;
        if let Ok(db) = self.db()
            && let Err(error) = db.mirror_issue(target.tracker, &detail, self.now())
        {
            tracing::warn!(key, %error, "could not keep an issue read for a move");
        }
        Ok(detail.issue.status.name)
    }

    /// One attempt at a move. Returns its run, for the window to be told.
    fn move_once(&self, approval: ApprovalId) -> Option<FlowId> {
        let send = self.db().ok()?.issue_move(approval).ok().flatten()?;
        let flow = send.approval.flow;
        // Taken over by a later approval of the same move: that approval sends it.
        let op = send.operation()?.clone();
        match op.state {
            OperationState::Done | OperationState::Cancelled | OperationState::NeedsAttention => {
                return Some(flow);
            }
            OperationState::Executing => {
                self.settle_unsent_move(&send, &op);
                return Some(flow);
            }
            OperationState::Pending | OperationState::Failed => {}
        }
        let payload = &send.request.payload;
        let target = match self.move_target(&send) {
            Ok(target) => target,
            Err(error) => {
                self.move_failed(&send, &op, &format!("{error}. Nothing was sent."));
                return Some(flow);
            }
        };
        let current = match self.read_status(&target, &payload.key) {
            Ok(current) => current,
            Err(error) => {
                self.move_failed(
                    &send,
                    &op,
                    &format!(
                        "{} could not be read from the tracker, so nothing was sent: {error}",
                        payload.key
                    ),
                );
                return Some(flow);
            }
        };
        match step(payload, &current) {
            MoveStep::Adopt => self.move_done(&send, &op, &current, true),
            MoveStep::Stop(moved) => {
                tracing::info!(
                    key = payload.key,
                    current,
                    "a move stopped: the issue moved"
                );
                self.change(
                    op.id,
                    &OperationChange::stopped(Attention::StatusMoved(moved)),
                );
                self.file(&NewInboxItem {
                    flow,
                    kind: InboxKind::Approval,
                    target: InboxTarget::Approval {
                        approval: send.approval.id,
                    },
                    title: format!(
                        "{} is in {current}, not {}: it was not moved",
                        payload.key, payload.from
                    ),
                    body: format!(
                        "You asked to move it from {} to {}. Someone moved it to {current} \
                         meanwhile, so nothing was sent. Move it to {} anyway, or leave it in \
                         {current}.",
                        payload.from, payload.to, payload.to
                    ),
                });
            }
            MoveStep::Send => {
                if self.change(op.id, &OperationChange::sending()).is_none() {
                    return Some(flow);
                }
                let reply =
                    self.app
                        .tracker_writer
                        .move_to(&target.root, &payload.key, &target.spelled);
                match (self.read_status(&target, &payload.key), reply) {
                    (Ok(now), reply) if arrived(payload, &now) => {
                        // A lost reply to a move that happened is the move done.
                        self.move_done(&send, &op, &now, reply.is_err());
                    }
                    (Ok(now), Err(error)) => self.move_failed(
                        &send,
                        &op,
                        &format!(
                            "the tracker refused it and {} is still in {now}: {error}",
                            payload.key
                        ),
                    ),
                    (Ok(now), Ok(())) => self.move_failed(
                        &send,
                        &op,
                        &format!(
                            "the tracker answered, and {} reads {now} rather than {}",
                            payload.key, payload.to
                        ),
                    ),
                    (Err(read), Ok(())) => {
                        // Sent and acknowledged; only the confirming read failed. The next read of
                        // the issue shows where it is.
                        tracing::warn!(key = payload.key, read, "a move's confirming read failed");
                        self.move_done(&send, &op, &payload.to, false);
                    }
                    (Err(read), Err(error)) => self.move_failed(
                        &send,
                        &op,
                        &format!(
                            "{error}; and {} could not be read to see whether it moved ({read}). \
                             Retry reads it first.",
                            payload.key
                        ),
                    ),
                }
            }
        }
        Some(flow)
    }

    fn move_done(&self, send: &MoveSend, op: &Operation, status: &str, adopted: bool) {
        self.change(
            op.id,
            &OperationChange::done(OperationResult::Moved {
                key: send.request.payload.key.clone(),
                status: status.to_owned(),
                adopted,
            }),
        );
        self.settle_move_items(send);
        tracing::info!(
            key = send.request.payload.key,
            status,
            adopted,
            "moved an issue"
        );
    }

    fn move_failed(&self, send: &MoveSend, op: &Operation, message: &str) {
        self.change(op.id, &OperationChange::failed(message));
        self.file(&NewInboxItem {
            flow: send.approval.flow,
            kind: InboxKind::Failure,
            target: InboxTarget::Approval {
                approval: send.approval.id,
            },
            title: format!(
                "Moving {} to {} did not happen",
                send.request.payload.key, send.request.payload.to
            ),
            body: format!("{message}. Retry reads the issue first and sends only if it has to."),
        });
    }

    fn settle_move_items(&self, send: &MoveSend) {
        let Ok(db) = self.db() else { return };
        let target = InboxTarget::Approval {
            approval: send.approval.id,
        };
        for kind in [InboxKind::Approval, InboxKind::Failure] {
            let _ = db.resolve_inbox(send.approval.flow, kind, Some(&target), self.now());
        }
    }

    /// Settle a move a crash left executing, by reading the issue. Sends nothing.
    fn settle_unsent_move(&self, send: &MoveSend, op: &Operation) {
        let checked = self
            .move_target(send)
            .and_then(|target| self.read_status(&target, &send.request.payload.key));
        match checked {
            Ok(now) if arrived(&send.request.payload, &now) => {
                self.move_done(send, op, &now, true);
            }
            Ok(now) => self.move_failed(
                send,
                op,
                &format!(
                    "wtm stopped while sending this, and {} reads {now}: it did not move",
                    send.request.payload.key
                ),
            ),
            Err(error) => self.move_failed(
                send,
                op,
                &format!(
                    "wtm stopped while sending this, and the tracker could not be read to see \
                     whether it arrived: {error}"
                ),
            ),
        }
    }

    /// At launch, an executing operation that is a move: settle it by reading the issue. `false`
    /// when the operation is not a move's.
    pub(super) fn recover_move(&self, approval: ApprovalId, op: &Operation) -> bool {
        let Some(send) = self
            .db()
            .ok()
            .and_then(|db| db.issue_move(approval).ok().flatten())
        else {
            return false;
        };
        self.settle_unsent_move(&send, op);
        true
    }

    /// Do what you decided about a move that stopped or failed.
    ///
    /// # Errors
    ///
    /// When the move is being sent now, is done, or the issue cannot be read for "anyway".
    pub fn settle_move(
        self: &Arc<Self>,
        approval: ApprovalId,
        action: MoveAction,
    ) -> Result<(), String> {
        let db = self.db()?;
        let send = db
            .issue_move(approval)
            .map_err(text)?
            .ok_or_else(|| format!("approval {approval} is not a move"))?;
        let op = send
            .operation()
            .ok_or("a later approval of the same move has it")?
            .clone();
        if self.activity(approval).is_some() {
            return Err("it is being sent right now".to_owned());
        }
        if op.state == OperationState::Done {
            return Err("it is done".to_owned());
        }
        match action {
            MoveAction::Retry => {
                if op.state == OperationState::NeedsAttention
                    || op.state == OperationState::Cancelled
                {
                    self.change(op.id, &OperationChange::to(OperationState::Pending));
                }
                self.send_move(approval);
            }
            MoveAction::Leave => {
                if op.state != OperationState::Cancelled {
                    self.change(op.id, &OperationChange::to(OperationState::Cancelled));
                }
                self.settle_move_items(&send);
            }
            MoveAction::Anyway => {
                let target = self.move_target(&send)?;
                let current = self.read_status(&target, &send.request.payload.key)?;
                if op.state != OperationState::Cancelled {
                    self.change(op.id, &OperationChange::to(OperationState::Cancelled));
                }
                self.settle_move_items(&send);
                let mut request = send.request.clone();
                request.payload.from = current;
                self.queue_move(send.approval.flow, &request)?;
            }
        }
        self.notice.changed(send.approval.flow);
        Ok(())
    }
}
