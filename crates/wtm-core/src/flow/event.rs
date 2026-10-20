//! The append-only history a flow's state is the projection of.
//!
//! Every change to a flow is one of these, and nothing else changes a flow. That is what lets the
//! Activity tab tell the whole story, lets recovery after a crash start from the last durable
//! event, and makes the stored state checkable: replaying the events must give back exactly the
//! state that was stored.

use serde::{Deserialize, Serialize};

use super::UnixMs;
use super::state::{FlowKind, FlowState, Hold, HoldKind, Phase, TransitionError};

/// One change to a flow.
///
/// Tagged `event` rather than this crate's usual `kind`, because the first variant has a field of
/// that name and a flow's kind is the more natural owner of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum FlowEvent {
    /// Always the first event, and only the first.
    Started {
        kind: FlowKind,
    },
    Advanced {
        to: Phase,
    },
    Held {
        hold: Hold,
    },
    Released {
        hold: HoldKind,
    },
    Finished,
    Failed {
        reason: String,
    },
    Cancelled,
}

/// An event as the store numbered and timed it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordedEvent {
    /// 1 for `Started`, then one more for each event after it.
    pub seq: u64,
    pub at: UnixMs,
    pub event: FlowEvent,
}

impl FlowState {
    /// The state after `event`, or why the event is refused.
    pub fn apply(&self, event: &FlowEvent) -> Result<Self, TransitionError> {
        match event {
            FlowEvent::Started { .. } => Err(TransitionError::AlreadyStarted),
            FlowEvent::Advanced { to } => self.advance(*to),
            FlowEvent::Held { hold } => self.hold(hold.clone()),
            FlowEvent::Released { hold } => self.release(*hold),
            FlowEvent::Finished => self.finish(),
            FlowEvent::Failed { reason } => self.fail(reason),
            FlowEvent::Cancelled => self.cancel(),
        }
    }

    /// The state a history of events produces.
    ///
    /// Refuses a history that does not begin with `Started`, or that contains any event the rules
    /// would have refused when it was recorded. A store that only ever wrote events through
    /// [`Self::apply`] cannot produce such a history, so either one means the history was written
    /// some other way.
    pub fn replay<'a>(
        events: impl IntoIterator<Item = &'a FlowEvent>,
    ) -> Result<Self, TransitionError> {
        let mut events = events.into_iter();
        let Some(FlowEvent::Started { kind }) = events.next() else {
            return Err(TransitionError::NotStarted);
        };
        events.try_fold(Self::new(*kind), |state, event| state.apply(event))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::state::{End, InterruptCause};

    #[test]
    fn replaying_a_flows_events_gives_back_the_state_they_produced() {
        let events = [
            FlowEvent::Started {
                kind: FlowKind::ReviewPr,
            },
            FlowEvent::Advanced {
                to: Phase::Preparing,
            },
            FlowEvent::Advanced {
                to: Phase::Reviewing,
            },
            FlowEvent::Held {
                hold: Hold::Interrupted {
                    cause: InterruptCause::Quit,
                },
            },
        ];

        let mut live = FlowState::new(FlowKind::ReviewPr);
        for event in &events[1..] {
            live = live.apply(event).unwrap();
        }

        assert_eq!(FlowState::replay(&events).unwrap(), live);
        assert_eq!(live.phase, Phase::Reviewing);
        assert_eq!(
            live.hold.map(|hold| hold.kind()),
            Some(HoldKind::Interrupted)
        );
    }

    #[test]
    fn a_history_that_does_not_begin_with_started_is_refused() {
        assert_eq!(FlowState::replay(&[]), Err(TransitionError::NotStarted));
        assert_eq!(
            FlowState::replay(&[FlowEvent::Advanced {
                to: Phase::Preparing
            }]),
            Err(TransitionError::NotStarted)
        );
    }

    #[test]
    fn a_flow_cannot_be_started_twice() {
        let state = FlowState::new(FlowKind::Implement);
        assert_eq!(
            state.apply(&FlowEvent::Started {
                kind: FlowKind::Implement
            }),
            Err(TransitionError::AlreadyStarted)
        );
    }

    #[test]
    fn a_history_containing_a_step_the_rules_refuse_does_not_replay() {
        let events = [
            FlowEvent::Started {
                kind: FlowKind::Implement,
            },
            FlowEvent::Finished,
        ];
        assert!(matches!(
            FlowState::replay(&events),
            Err(TransitionError::CannotFinish { .. })
        ));
    }

    #[test]
    fn a_failure_keeps_its_reason_through_a_replay() {
        let events = [
            FlowEvent::Started {
                kind: FlowKind::ReviewMine,
            },
            FlowEvent::Failed {
                reason: "the worktree was removed".to_owned(),
            },
        ];
        assert_eq!(
            FlowState::replay(&events).unwrap().end,
            Some(End::Failed {
                reason: "the worktree was removed".to_owned()
            })
        );
    }

    #[test]
    fn events_serialize_to_the_tagged_json_a_store_writes_and_reads_back() {
        // The store keeps each event as this JSON, so its shape is a file format: changing it
        // without a migration would make every stored history unreadable.
        let event = FlowEvent::Held {
            hold: Hold::WaitingCapacity {
                provider: "codex".to_owned(),
                resets_at: Some(1_800_000_000),
            },
        };
        let json = serde_json::to_string(&event).unwrap();
        assert_eq!(
            json,
            r#"{"event":"held","hold":{"kind":"waiting_capacity","provider":"codex","resetsAt":1800000000}}"#
        );
        assert_eq!(serde_json::from_str::<FlowEvent>(&json).unwrap(), event);
    }
}
