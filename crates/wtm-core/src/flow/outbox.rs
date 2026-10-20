//! The outbox: every write a flow makes outside this machine.
//!
//! # Why writes are queued rather than made
//!
//! Pushing a branch, opening a pull request, publishing a review and moving an issue each change
//! something other people see, and the app can die between deciding to do one and learning whether
//! it happened. So each is an [`Operation`] recorded first and executed second, keyed by
//! [`OperationKey`] so the same write is never queued twice.
//!
//! # Why a crash is not a retry
//!
//! An operation found `Executing` at launch may or may not have reached the remote. Running it again
//! could post a second review or open a second pull request. So `Executing` cannot move to
//! `Executing`: recovery must first read the remote and settle the operation as `Done` (the result
//! exists, adopt it) or `Failed` (it does not), and only a failed operation is retried.

use serde::{Deserialize, Serialize};

use super::state::FlowId;

id! {
    /// One operation, as the store numbers it.
    OperationId
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    PushBranch,
    CreateDraftPullRequest,
    UpdatePullRequest,
    MarkReady,
    PublishReview,
    PostComment,
    MoveIssue,
    Merge,
}

/// What makes two operations the same write.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationKey {
    pub flow: FlowId,
    pub kind: OperationKind,
    /// What it writes to, such as `owner/name#40` or a branch.
    pub target: String,
    /// The hash of exactly what it sends.
    pub payload_hash: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    Pending,
    Executing,
    Done,
    /// Did not happen, and may be retried.
    Failed,
    /// Stopped because the remote was not as expected. You decide what happens next.
    NeedsAttention,
}

impl OperationState {
    /// Whether an operation in this state may move to `next`.
    #[must_use]
    pub fn can_move_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Pending | Self::Failed, Self::Executing)
                | (
                    Self::Executing,
                    Self::Done | Self::Failed | Self::NeedsAttention
                )
                | (Self::NeedsAttention, Self::Pending)
        )
    }

    pub fn move_to(self, next: Self) -> Result<Self, OutboxError> {
        if self.can_move_to(next) {
            Ok(next)
        } else {
            Err(OutboxError::NoSuchStep {
                from: self,
                to: next,
            })
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OutboxError {
    #[error("an operation cannot move from {from:?} to {to:?}")]
    NoSuchStep {
        from: OperationState,
        to: OperationState,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Operation {
    pub id: OperationId,
    pub key: OperationKey,
    pub state: OperationState,
    pub attempts: u32,
    pub last_error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn through(states: &[OperationState]) -> Result<OperationState, OutboxError> {
        states[1..]
            .iter()
            .try_fold(states[0], |state, next| state.move_to(*next))
    }

    #[test]
    fn an_operation_runs_from_pending_through_executing_to_done() {
        assert_eq!(
            through(&[
                OperationState::Pending,
                OperationState::Executing,
                OperationState::Done
            ]),
            Ok(OperationState::Done)
        );
    }

    #[test]
    fn a_failed_operation_can_be_retried() {
        assert_eq!(
            OperationState::Failed.move_to(OperationState::Executing),
            Ok(OperationState::Executing)
        );
    }

    #[test]
    fn an_operation_left_executing_by_a_crash_is_settled_before_it_can_run_again() {
        assert_eq!(
            OperationState::Executing.move_to(OperationState::Executing),
            Err(OutboxError::NoSuchStep {
                from: OperationState::Executing,
                to: OperationState::Executing,
            })
        );
        assert!(OperationState::Executing.can_move_to(OperationState::Done));
        assert!(OperationState::Executing.can_move_to(OperationState::Failed));
    }

    #[test]
    fn a_done_operation_never_runs_again() {
        for next in [
            OperationState::Pending,
            OperationState::Executing,
            OperationState::Failed,
            OperationState::NeedsAttention,
        ] {
            assert!(!OperationState::Done.can_move_to(next), "{next:?}");
        }
    }

    #[test]
    fn an_operation_that_needs_attention_waits_for_you_before_it_runs_again() {
        assert!(!OperationState::NeedsAttention.can_move_to(OperationState::Executing));
        assert_eq!(
            through(&[
                OperationState::NeedsAttention,
                OperationState::Pending,
                OperationState::Executing
            ]),
            Ok(OperationState::Executing)
        );
    }
}
