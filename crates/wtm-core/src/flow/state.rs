//! A flow's execution state: a phase, an optional hold, and, once it is over, an end.
//!
//! # Why a hold is not a phase
//!
//! "Awaiting your answer" can happen in almost any phase, and so can a usage limit, a quit or a
//! pause. Folding those into the phase list would multiply it by every reason to wait, and lose the
//! thing the UI needs once the wait is over: which phase to go back to. So the phase says how far a
//! flow has got, the hold says why it is not moving, and the UI shows the hold if there is one,
//! otherwise the phase ([`FlowState::status`]).
//!
//! # Why the paths are data
//!
//! Each kind's legal steps are a table of `(from, to)` pairs rather than a `match`. The table can be
//! read top to bottom against the plan, and the tests walk it as a graph: every phase a kind can
//! enter is reachable from the start and has a way to finish. A `match` would prove neither.
//!
//! # What these rules do not decide
//!
//! Whether a step is *allowed right now* is policy, and policy needs more than the state. Planned to
//! Implementing is a legal step, but taking it also needs an approval of the current plan revision.
//! These rules answer only whether the step exists for this kind of flow at all.

use serde::{Deserialize, Serialize};

use super::UnixMs;
use super::work::WorkItemId;

id! {
    /// One flow, as the store numbers it.
    FlowId
}

/// What a flow is doing to its work item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowKind {
    /// Investigate, plan, implement, review and publish an issue.
    Implement,
    /// Review my own work, which may have no pull request and no issue.
    ReviewMine,
    /// Review someone else's pull request.
    ReviewPr,
    /// Revise published work after new feedback.
    ///
    /// The plan names this kind but leaves its phases to the revision items. Until then it is the
    /// Implement path without the planning front, ending once the revision is published: the
    /// Implement flow that published the pull request is still the one following it.
    Revise,
    /// Operate a worktree's environment: its services, and its commands.
    ///
    /// Not an execution with an end like the others, and that is deliberate. An approval and an
    /// inbox item each belong to a flow, and a destructive command on a shared database needs both,
    /// so a worktree whose environment wtm operates has one open flow of this kind for them to
    /// belong to. Its only phase is [`Phase::Operating`]; the services' own states are the
    /// supervisor's, not the flow's.
    Environment,
}

impl FlowKind {
    pub const ALL: [Self; 5] = [
        Self::Implement,
        Self::ReviewMine,
        Self::ReviewPr,
        Self::Revise,
        Self::Environment,
    ];

    /// Whether a flow of this kind has a step from `from` to `to`.
    #[must_use]
    pub fn can_move(self, from: Phase, to: Phase) -> bool {
        self.lifecycle().steps.contains(&(from, to))
    }

    /// Whether a flow of this kind can be done once it has reached `phase`.
    #[must_use]
    pub fn can_finish_from(self, phase: Phase) -> bool {
        self.lifecycle().finishes_from.contains(&phase)
    }

    fn lifecycle(self) -> &'static Lifecycle {
        match self {
            Self::Implement => &IMPLEMENT,
            Self::ReviewMine | Self::ReviewPr => &REVIEW,
            Self::Revise => &REVISE,
            Self::Environment => &ENVIRONMENT,
        }
    }
}

/// How far a flow has got.
///
/// One list for every kind, because several phases are shared (Resolving, Reviewing, Fixing) and
/// the UI names a phase the same way whichever kind it belongs to. Which phases a kind actually
/// passes through is its lifecycle table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Finding the repository, base, pull request and worktree the work item resolves to.
    Resolving,
    /// Setting up the worktree and gathering context.
    Preparing,
    Investigating,
    /// A plan exists. Implementing it needs that plan approved.
    Planned,
    Designing,
    DesignReady,
    Implementing,
    /// Configured checks are running.
    Verifying,
    Reviewing,
    /// Fixing what a review found, within the plan's scope.
    Fixing,
    /// Handed to you to test.
    ReadyToTest,
    Revising,
    Publishing,
    /// Published and being tracked until it merges.
    InReview,
    /// Every role's findings are in, deduplicated.
    Consolidated,
    FixReady,
    /// Sending a review's outcome somewhere.
    Submitting,
    /// Watching the pull request for new revisions.
    Following,
    /// A worktree's environment, operated for as long as the worktree is in use.
    Operating,
}

struct Lifecycle {
    steps: &'static [(Phase, Phase)],
    finishes_from: &'static [Phase],
}

// Implement: Resolving → Preparing → Investigating → Planned → (Designing → DesignReady) →
// Implementing → Verifying → Reviewing ⇄ Fixing → ReadyToTest ⇄ Revising → Publishing → InReview.
const IMPLEMENT: Lifecycle = Lifecycle {
    steps: &[
        (Phase::Resolving, Phase::Preparing),
        (Phase::Preparing, Phase::Investigating),
        (Phase::Investigating, Phase::Planned),
        // Asking for changes to a plan goes back to the planner, in the same conversation.
        (Phase::Planned, Phase::Investigating),
        (Phase::Planned, Phase::Designing),
        (Phase::Planned, Phase::Implementing),
        (Phase::Designing, Phase::DesignReady),
        // Asking for changes to a design goes back to designing it.
        (Phase::DesignReady, Phase::Designing),
        (Phase::DesignReady, Phase::Implementing),
        (Phase::Implementing, Phase::Verifying),
        (Phase::Verifying, Phase::Reviewing),
        (Phase::Reviewing, Phase::Fixing),
        (Phase::Reviewing, Phase::ReadyToTest),
        (Phase::Fixing, Phase::Reviewing),
        // A fix round's changes run the checks they affect before they are reviewed again.
        (Phase::Fixing, Phase::Verifying),
        // The fix loop is bounded, and its last round hands over without another review.
        (Phase::Fixing, Phase::ReadyToTest),
        (Phase::ReadyToTest, Phase::Revising),
        // "Rerun reviews" from the handoff.
        (Phase::ReadyToTest, Phase::Reviewing),
        (Phase::ReadyToTest, Phase::Publishing),
        (Phase::Revising, Phase::ReadyToTest),
        (Phase::Publishing, Phase::InReview),
        // A revision you ask for, after testing it or from the pull request's feedback, goes back
        // to the writer, in its same conversation, and through the checks and the review again.
        (Phase::ReadyToTest, Phase::Implementing),
        (Phase::Publishing, Phase::Implementing),
        (Phase::InReview, Phase::Implementing),
        // Work the plan does not cover, which you agreed should be in it: the plan revision that
        // covers it goes back to you, and implementing waits for that approval as it did for the
        // first. From wherever the implementation's thread stops to let it.
        (Phase::Implementing, Phase::Planned),
        (Phase::Reviewing, Phase::Planned),
        (Phase::Fixing, Phase::Planned),
        (Phase::ReadyToTest, Phase::Planned),
    ],
    finishes_from: &[Phase::InReview],
};

// Review mine and Review PR: Resolving → Preparing → Reviewing → Consolidated → (Fixing →
// FixReady) → Submitting → Following.
const REVIEW: Lifecycle = Lifecycle {
    steps: &[
        (Phase::Resolving, Phase::Preparing),
        (Phase::Preparing, Phase::Reviewing),
        (Phase::Reviewing, Phase::Consolidated),
        // "Rerun affected" after the evidence went stale.
        (Phase::Consolidated, Phase::Reviewing),
        (Phase::Consolidated, Phase::Fixing),
        (Phase::Consolidated, Phase::Submitting),
        (Phase::Fixing, Phase::FixReady),
        (Phase::FixReady, Phase::Fixing),
        (Phase::FixReady, Phase::Submitting),
        (Phase::Submitting, Phase::Following),
        // "Re-review changes" when the pull request gets a new head.
        (Phase::Following, Phase::Reviewing),
    ],
    // Keeping a review private ends it at Consolidated, and keeping local fixes ends it at
    // FixReady. Neither is a failure, so neither may be forced through Submitting.
    finishes_from: &[Phase::Consolidated, Phase::FixReady, Phase::Following],
};

// Revise: Resolving → Preparing → Implementing → Verifying → Reviewing ⇄ Fixing → ReadyToTest ⇄
// Revising → Publishing.
const REVISE: Lifecycle = Lifecycle {
    steps: &[
        (Phase::Resolving, Phase::Preparing),
        (Phase::Preparing, Phase::Implementing),
        (Phase::Implementing, Phase::Verifying),
        (Phase::Verifying, Phase::Reviewing),
        (Phase::Reviewing, Phase::Fixing),
        (Phase::Reviewing, Phase::ReadyToTest),
        (Phase::Fixing, Phase::Reviewing),
        (Phase::Fixing, Phase::ReadyToTest),
        (Phase::ReadyToTest, Phase::Revising),
        (Phase::ReadyToTest, Phase::Reviewing),
        (Phase::ReadyToTest, Phase::Publishing),
        (Phase::Revising, Phase::ReadyToTest),
    ],
    finishes_from: &[Phase::Publishing],
};

// Environment: Resolving → Operating, which it stays in.
const ENVIRONMENT: Lifecycle = Lifecycle {
    steps: &[(Phase::Resolving, Phase::Operating)],
    finishes_from: &[Phase::Operating],
};

/// Why a flow is not moving.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Hold {
    /// A question needs your answer. The answer resumes the same flow.
    AwaitingAnswer,
    /// An approval is needed before the next step.
    AwaitingApproval,
    /// A provider's usage limit was reached.
    #[serde(rename_all = "camelCase")]
    WaitingCapacity {
        provider: String,
        /// Unix seconds, when the provider said. Never a guess: an unknown reset stays unknown.
        resets_at: Option<u64>,
    },
    /// You paused it.
    Paused,
    /// Its processes went away underneath it.
    Interrupted { cause: InterruptCause },
    /// Something outside the flow has to change first.
    Blocked { cause: BlockCause },
}

impl Hold {
    #[must_use]
    pub fn kind(&self) -> HoldKind {
        match self {
            Self::AwaitingAnswer => HoldKind::AwaitingAnswer,
            Self::AwaitingApproval => HoldKind::AwaitingApproval,
            Self::WaitingCapacity { .. } => HoldKind::WaitingCapacity,
            Self::Paused => HoldKind::Paused,
            Self::Interrupted { .. } => HoldKind::Interrupted,
            Self::Blocked { .. } => HoldKind::Blocked,
        }
    }
}

/// A [`Hold`] without its details: what a release names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HoldKind {
    AwaitingAnswer,
    AwaitingApproval,
    WaitingCapacity,
    Paused,
    Interrupted,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterruptCause {
    Quit,
    Crash,
    Sleep,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockCause {
    /// The environment it needs is not running.
    Env,
    /// It needs a permission it does not have.
    Permission,
    Conflict,
    /// The remote changed since it was read, so writing now would overwrite someone.
    StaleRemote,
}

/// How a flow ended. Every end is terminal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum End {
    Done,
    Failed { reason: String },
    Cancelled,
}

/// What the UI shows for a flow: how it ended, otherwise why it is waiting, otherwise its phase.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Status {
    Ended { end: End },
    Held { hold: Hold },
    InPhase { phase: Phase },
}

/// A transition the rules refuse. The state it was asked of is unchanged.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TransitionError {
    #[error("the flow has already ended ({end:?})")]
    Ended { end: End },
    #[error("the flow is held ({hold:?}) and cannot move until that is released")]
    Held { hold: HoldKind },
    #[error("a {flow:?} flow has no step from {from:?} to {to:?}")]
    NoSuchStep {
        flow: FlowKind,
        from: Phase,
        to: Phase,
    },
    #[error("a {flow:?} flow cannot be done while {phase:?}")]
    CannotFinish { flow: FlowKind, phase: Phase },
    #[error("the flow is already held ({current:?})")]
    AlreadyHeld { current: HoldKind },
    #[error("the flow is not held")]
    NotHeld,
    #[error("the flow is held for {held:?}, not {released:?}")]
    WrongHold { held: HoldKind, released: HoldKind },
    #[error("a flow's history must begin with the event that started it")]
    NotStarted,
    #[error("a flow can only be started once")]
    AlreadyStarted,
}

/// A flow's execution state.
///
/// The fields are public so a store can rebuild a state from its row, but only
/// [`FlowState::apply`] and the transitions below should ever produce a new one. Each returns the
/// next state rather than mutating, so a refused transition cannot leave a half-changed state
/// behind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowState {
    pub kind: FlowKind,
    pub phase: Phase,
    pub hold: Option<Hold>,
    pub end: Option<End>,
}

impl FlowState {
    /// A flow that has just started: resolving, with nothing holding it.
    #[must_use]
    pub fn new(kind: FlowKind) -> Self {
        Self {
            kind,
            phase: Phase::Resolving,
            hold: None,
            end: None,
        }
    }

    /// Whether the flow has not ended. At most one open flow of each kind may exist per work item.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.end.is_none()
    }

    #[must_use]
    pub fn status(&self) -> Status {
        if let Some(end) = &self.end {
            Status::Ended { end: end.clone() }
        } else if let Some(hold) = &self.hold {
            Status::Held { hold: hold.clone() }
        } else {
            Status::InPhase { phase: self.phase }
        }
    }

    /// Move to the next phase.
    ///
    /// A held flow cannot move: a hold means something is waiting on you or on the world, and
    /// moving past it would act on an answer or an approval nobody gave.
    pub fn advance(&self, to: Phase) -> Result<Self, TransitionError> {
        self.ensure_moving()?;
        if !self.kind.can_move(self.phase, to) {
            return Err(TransitionError::NoSuchStep {
                flow: self.kind,
                from: self.phase,
                to,
            });
        }
        Ok(Self {
            phase: to,
            ..self.clone()
        })
    }

    /// Stop the flow until `hold` is released.
    ///
    /// # One hold at a time
    ///
    /// A second hold is refused rather than stacked or swapped in. Swapping would lose what the
    /// flow was waiting for, and the hold a release names would no longer be the one in place. The
    /// cases that look like they need two holds do not: a flow awaiting your answer has no process
    /// for a quit to interrupt, so recovery leaves it as it is.
    pub fn hold(&self, hold: Hold) -> Result<Self, TransitionError> {
        self.ensure_open()?;
        if let Some(current) = &self.hold {
            return Err(TransitionError::AlreadyHeld {
                current: current.kind(),
            });
        }
        Ok(Self {
            hold: Some(hold),
            ..self.clone()
        })
    }

    /// Release the hold, which the caller must name.
    ///
    /// Naming it is what stops a late answer from releasing a flow that has since been blocked
    /// for another reason: the answer names `AwaitingAnswer`, the flow is held for something else,
    /// and the release is refused instead of quietly unblocking it.
    pub fn release(&self, which: HoldKind) -> Result<Self, TransitionError> {
        self.ensure_open()?;
        let Some(current) = &self.hold else {
            return Err(TransitionError::NotHeld);
        };
        if current.kind() != which {
            return Err(TransitionError::WrongHold {
                held: current.kind(),
                released: which,
            });
        }
        Ok(Self {
            hold: None,
            ..self.clone()
        })
    }

    /// End the flow as done. Only from the phases its kind can finish from, and never while held.
    pub fn finish(&self) -> Result<Self, TransitionError> {
        self.ensure_moving()?;
        if !self.kind.can_finish_from(self.phase) {
            return Err(TransitionError::CannotFinish {
                flow: self.kind,
                phase: self.phase,
            });
        }
        Ok(self.ended(End::Done))
    }

    /// End the flow as failed, from anywhere it is still open, held or not.
    pub fn fail(&self, reason: &str) -> Result<Self, TransitionError> {
        self.ensure_open()?;
        Ok(self.ended(End::Failed {
            reason: reason.to_owned(),
        }))
    }

    /// End the flow as cancelled, from anywhere it is still open, held or not.
    pub fn cancel(&self) -> Result<Self, TransitionError> {
        self.ensure_open()?;
        Ok(self.ended(End::Cancelled))
    }

    /// An ended flow keeps the phase it ended in, so "failed while reviewing" stays answerable,
    /// and drops its hold, because nothing waits on a flow that is over.
    fn ended(&self, end: End) -> Self {
        Self {
            hold: None,
            end: Some(end),
            ..self.clone()
        }
    }

    fn ensure_open(&self) -> Result<(), TransitionError> {
        match &self.end {
            Some(end) => Err(TransitionError::Ended { end: end.clone() }),
            None => Ok(()),
        }
    }

    fn ensure_moving(&self) -> Result<(), TransitionError> {
        self.ensure_open()?;
        match &self.hold {
            Some(hold) => Err(TransitionError::Held { hold: hold.kind() }),
            None => Ok(()),
        }
    }
}

/// A flow as the store holds it: its state plus where that state came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Flow {
    pub id: FlowId,
    pub work_item: WorkItemId,
    pub state: FlowState,
    /// The sequence number of the last event applied, so the state is the replay of events
    /// `1..=seq`.
    pub seq: u64,
    pub started_at: UnixMs,
    pub updated_at: UnixMs,
}

#[cfg(test)]
mod tests {
    use std::collections::{HashSet, VecDeque};

    use super::*;

    fn walk(kind: FlowKind, phases: &[Phase]) -> FlowState {
        let mut state = FlowState::new(kind);
        for phase in phases {
            state = state.advance(*phase).unwrap();
        }
        state
    }

    fn every_phase_in(lifecycle: &Lifecycle) -> HashSet<Phase> {
        lifecycle
            .steps
            .iter()
            .flat_map(|(from, to)| [*from, *to])
            .chain(lifecycle.finishes_from.iter().copied())
            .collect()
    }

    fn reachable_from(lifecycle: &Lifecycle, start: Phase) -> HashSet<Phase> {
        let mut seen = HashSet::from([start]);
        let mut queue = VecDeque::from([start]);
        while let Some(at) = queue.pop_front() {
            for (from, to) in lifecycle.steps {
                if *from == at && seen.insert(*to) {
                    queue.push_back(*to);
                }
            }
        }
        seen
    }

    #[test]
    fn a_new_flow_starts_resolving_with_nothing_holding_it() {
        let state = FlowState::new(FlowKind::ReviewPr);
        assert_eq!(state.phase, Phase::Resolving);
        assert_eq!(state.hold, None);
        assert!(state.is_open());
    }

    #[test]
    fn an_implement_flow_walks_its_whole_path_to_done() {
        let state = walk(
            FlowKind::Implement,
            &[
                Phase::Preparing,
                Phase::Investigating,
                Phase::Planned,
                Phase::Designing,
                Phase::DesignReady,
                Phase::Implementing,
                Phase::Verifying,
                Phase::Reviewing,
                Phase::Fixing,
                Phase::Reviewing,
                Phase::ReadyToTest,
                Phase::Revising,
                Phase::ReadyToTest,
                Phase::Publishing,
                Phase::InReview,
            ],
        );
        assert_eq!(state.finish().unwrap().end, Some(End::Done));
    }

    #[test]
    fn an_implementation_whose_plan_grows_goes_back_for_approval_and_on_again() {
        let state = walk(
            FlowKind::Implement,
            &[
                Phase::Preparing,
                Phase::Investigating,
                Phase::Planned,
                Phase::Implementing,
                Phase::Verifying,
                Phase::Reviewing,
                Phase::Fixing,
                Phase::Verifying,
                Phase::Reviewing,
                Phase::Fixing,
                Phase::Planned,
                Phase::Implementing,
                Phase::Verifying,
                Phase::Reviewing,
                Phase::ReadyToTest,
                Phase::Planned,
            ],
        );
        assert_eq!(state.phase, Phase::Planned);
    }

    #[test]
    fn a_revision_goes_back_to_implementing_from_the_hand_over_and_from_review() {
        let state = walk(
            FlowKind::Implement,
            &[
                Phase::Preparing,
                Phase::Investigating,
                Phase::Planned,
                Phase::Implementing,
                Phase::Verifying,
                Phase::Reviewing,
                Phase::ReadyToTest,
                Phase::Implementing,
                Phase::Verifying,
                Phase::Reviewing,
                Phase::ReadyToTest,
                Phase::Publishing,
                Phase::InReview,
                Phase::Implementing,
            ],
        );
        assert_eq!(state.phase, Phase::Implementing);
    }

    #[test]
    fn a_review_flow_walks_its_whole_path_to_done() {
        for kind in [FlowKind::ReviewMine, FlowKind::ReviewPr] {
            let state = walk(
                kind,
                &[
                    Phase::Preparing,
                    Phase::Reviewing,
                    Phase::Consolidated,
                    Phase::Fixing,
                    Phase::FixReady,
                    Phase::Submitting,
                    Phase::Following,
                    Phase::Reviewing,
                    Phase::Consolidated,
                ],
            );
            assert_eq!(state.finish().unwrap().end, Some(End::Done), "{kind:?}");
        }
    }

    #[test]
    fn design_is_a_detour_an_implement_flow_may_skip() {
        let planned = walk(
            FlowKind::Implement,
            &[Phase::Preparing, Phase::Investigating, Phase::Planned],
        );
        assert!(planned.advance(Phase::Implementing).is_ok());
        assert!(planned.advance(Phase::Designing).is_ok());
    }

    #[test]
    fn a_flow_cannot_skip_ahead() {
        let state = FlowState::new(FlowKind::Implement);
        assert_eq!(
            state.advance(Phase::Implementing),
            Err(TransitionError::NoSuchStep {
                flow: FlowKind::Implement,
                from: Phase::Resolving,
                to: Phase::Implementing,
            })
        );
    }

    #[test]
    fn a_review_flow_cannot_enter_a_phase_only_implementing_has() {
        let reviewing = walk(FlowKind::ReviewPr, &[Phase::Preparing, Phase::Reviewing]);
        assert!(matches!(
            reviewing.advance(Phase::ReadyToTest),
            Err(TransitionError::NoSuchStep { .. })
        ));
    }

    #[test]
    fn reviewing_and_fixing_loop_in_both_directions() {
        let reviewing = walk(
            FlowKind::Revise,
            &[
                Phase::Preparing,
                Phase::Implementing,
                Phase::Verifying,
                Phase::Reviewing,
            ],
        );
        let fixing = reviewing.advance(Phase::Fixing).unwrap();
        assert_eq!(
            fixing.advance(Phase::Reviewing).unwrap().phase,
            Phase::Reviewing
        );
    }

    #[test]
    fn a_flow_can_be_done_only_from_the_end_of_its_path() {
        let reviewing = walk(
            FlowKind::Implement,
            &[
                Phase::Preparing,
                Phase::Investigating,
                Phase::Planned,
                Phase::Implementing,
                Phase::Verifying,
                Phase::Reviewing,
            ],
        );
        assert_eq!(
            reviewing.finish(),
            Err(TransitionError::CannotFinish {
                flow: FlowKind::Implement,
                phase: Phase::Reviewing,
            })
        );
    }

    #[test]
    fn a_review_kept_private_is_done_straight_after_consolidation() {
        let consolidated = walk(
            FlowKind::ReviewMine,
            &[Phase::Preparing, Phase::Reviewing, Phase::Consolidated],
        );
        assert_eq!(consolidated.finish().unwrap().end, Some(End::Done));
    }

    #[test]
    fn a_held_flow_cannot_move_or_finish_until_released() {
        let consolidated = walk(
            FlowKind::ReviewPr,
            &[Phase::Preparing, Phase::Reviewing, Phase::Consolidated],
        );
        let held = consolidated.hold(Hold::AwaitingApproval).unwrap();

        let refused = Err(TransitionError::Held {
            hold: HoldKind::AwaitingApproval,
        });
        assert_eq!(held.advance(Phase::Submitting), refused);
        assert_eq!(held.finish(), refused);

        let released = held.release(HoldKind::AwaitingApproval).unwrap();
        assert_eq!(
            released.advance(Phase::Submitting).unwrap().phase,
            Phase::Submitting
        );
    }

    #[test]
    fn a_release_that_names_the_wrong_hold_is_refused() {
        let blocked = FlowState::new(FlowKind::ReviewPr)
            .hold(Hold::Blocked {
                cause: BlockCause::StaleRemote,
            })
            .unwrap();
        assert_eq!(
            blocked.release(HoldKind::AwaitingAnswer),
            Err(TransitionError::WrongHold {
                held: HoldKind::Blocked,
                released: HoldKind::AwaitingAnswer,
            })
        );
    }

    #[test]
    fn a_held_flow_refuses_a_second_hold() {
        let waiting = FlowState::new(FlowKind::Implement)
            .hold(Hold::WaitingCapacity {
                provider: "claude".to_owned(),
                resets_at: Some(1_800_000_000),
            })
            .unwrap();
        assert_eq!(
            waiting.hold(Hold::Paused),
            Err(TransitionError::AlreadyHeld {
                current: HoldKind::WaitingCapacity,
            })
        );
    }

    #[test]
    fn releasing_a_flow_nothing_holds_is_refused() {
        let state = FlowState::new(FlowKind::Implement);
        assert_eq!(
            state.release(HoldKind::Paused),
            Err(TransitionError::NotHeld)
        );
    }

    #[test]
    fn a_held_flow_can_still_be_cancelled_and_the_hold_goes_with_it() {
        let held = FlowState::new(FlowKind::Implement)
            .hold(Hold::AwaitingAnswer)
            .unwrap();
        let cancelled = held.cancel().unwrap();
        assert_eq!(cancelled.end, Some(End::Cancelled));
        assert_eq!(cancelled.hold, None);
        assert_eq!(cancelled.phase, Phase::Resolving);
    }

    #[test]
    fn an_ended_flow_accepts_nothing_more() {
        let open = walk(FlowKind::ReviewMine, &[Phase::Preparing, Phase::Reviewing]);
        let ends = [
            open.fail("the provider exited").unwrap(),
            open.cancel().unwrap(),
            open.advance(Phase::Consolidated).unwrap().finish().unwrap(),
        ];
        for ended in ends {
            let refused = Err(TransitionError::Ended {
                end: ended.end.clone().unwrap(),
            });
            assert_eq!(ended.advance(Phase::Consolidated), refused);
            assert_eq!(ended.hold(Hold::Paused), refused);
            assert_eq!(ended.release(HoldKind::Paused), refused);
            assert_eq!(ended.finish(), refused);
            assert_eq!(ended.fail("again"), refused);
            assert_eq!(ended.cancel(), refused);
        }
    }

    #[test]
    fn the_ui_shows_the_end_then_the_hold_then_the_phase() {
        let reviewing = walk(FlowKind::ReviewPr, &[Phase::Preparing, Phase::Reviewing]);
        assert_eq!(
            reviewing.status(),
            Status::InPhase {
                phase: Phase::Reviewing
            }
        );

        let paused = reviewing.hold(Hold::Paused).unwrap();
        assert_eq!(paused.status(), Status::Held { hold: Hold::Paused });

        assert_eq!(
            paused.cancel().unwrap().status(),
            Status::Ended {
                end: End::Cancelled
            }
        );
    }

    #[test]
    fn every_phase_a_kind_uses_is_reachable_and_can_lead_to_done() {
        for kind in FlowKind::ALL {
            let lifecycle = kind.lifecycle();
            let used = every_phase_in(lifecycle);

            let from_start = reachable_from(lifecycle, Phase::Resolving);
            assert_eq!(from_start, used, "{kind:?} has a phase nothing leads to");

            for phase in used {
                let onward = reachable_from(lifecycle, phase);
                assert!(
                    lifecycle
                        .finishes_from
                        .iter()
                        .any(|end| onward.contains(end)),
                    "{kind:?} can reach {phase:?} and then never finish"
                );
            }
        }
    }

    #[test]
    fn no_step_leads_back_to_resolving() {
        // Resolving decides which worktree and pull request a flow is about. Going back to it
        // mid-flow would let a running flow change what it is working on.
        for kind in FlowKind::ALL {
            assert!(
                kind.lifecycle()
                    .steps
                    .iter()
                    .all(|(_, to)| *to != Phase::Resolving),
                "{kind:?}"
            );
        }
    }
}
