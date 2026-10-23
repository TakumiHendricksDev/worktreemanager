//! Moving an issue to another team status: the one write to a tracker this build makes.
//!
//! # A move is an operation like any other write
//!
//! Start Work's move to the mapped "started" status, and a card dropped on another board column,
//! each change something the whole team sees, so each is an outbox
//! [`Operation`](super::Operation) recorded under an approval before it is sent (§4.4 of the plan,
//! and [`super::outbox`] for why). The approval is the click that asked for it: Start, or the drop
//! and, when the issue leaves review, its confirmation. It pins exactly what is moved, from where
//! and to where ([`MoveIssuePayload`]), so asking for the same move twice is one operation.
//!
//! # Read first, because a tracker has no compare-and-swap
//!
//! A forge can refuse a push from a head that moved; a tracker's transition just happens. So the
//! executor reads the issue's status before it sends anything, and [`step`] decides from that
//! reading alone:
//!
//! - **already at the destination**: someone, or an earlier attempt whose reply was lost, moved
//!   it. Adopted, and nothing is sent.
//! - **where the move expected it**: sent.
//! - **anywhere else**: it moved while nobody here was looking. Sending now would move it out of a
//!   status someone chose on purpose, so the move stops and asks ([`MovedElsewhere`]).
//!
//! The read and the send are two requests, and nothing in between holds the issue still. A change
//! made in that gap would be overwritten by the move, and no reading here can rule it out; the
//! window says so beside every move rather than calling the check a guarantee.

use serde::{Deserialize, Serialize};

use super::tracker::{TrackerKind, same_status};

/// Exactly what one move sends: the issue, the status it was in when you asked, and the status it
/// goes to. Its hash is the operation's key, so the same move asked for twice is one write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveIssuePayload {
    pub tracker: TrackerKind,
    pub key: String,
    /// The team status the issue was in when the move was asked for, as the window showed it.
    pub from: String,
    pub to: String,
}

impl MoveIssuePayload {
    /// What the move writes to, as an operation's target: the tracker and the issue.
    #[must_use]
    pub fn target(&self) -> String {
        format!("{}:{}", self.tracker.as_str(), self.key)
    }
}

/// Why a move was asked for, kept with its approval so the window can say it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MoveCause {
    /// Start Work's move to the mapped "started" status.
    Start,
    /// A card dropped on another column of the board.
    Drop,
    /// "Mark ready + Needs Review": the move to the mapped "needs review" status, beside the pull
    /// request marked ready, on one approval.
    Publish,
}

/// A move as its approval keeps it: what is sent, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveRequest {
    pub payload: MoveIssuePayload,
    pub cause: MoveCause,
}

/// The issue was found in a status the move did not expect, so nothing was sent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MovedElsewhere {
    pub expected: String,
    pub current: String,
    pub to: String,
}

/// What reading the issue's status says to do about a move.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoveStep {
    /// It is already where the move takes it. Done, without sending anything.
    Adopt,
    /// It is where the move expected. Send it.
    Send,
    /// It moved somewhere else meanwhile. Stop and ask.
    Stop(MovedElsewhere),
}

/// Decide a move from the issue's status as just read.
///
/// Names are compared the way the board places an issue in a column
/// ([`StatusMap::column_of`](super::StatusMap::column_of)): ignoring case and the spaces around
/// them, because `to` is the project's config spelling and `current` the tracker's, and an issue
/// the board draws in the destination column must not read as somewhere else.
#[must_use]
pub fn step(payload: &MoveIssuePayload, current: &str) -> MoveStep {
    if same_status(current, &payload.to) {
        MoveStep::Adopt
    } else if same_status(current, &payload.from) {
        MoveStep::Send
    } else {
        MoveStep::Stop(MovedElsewhere {
            expected: payload.from.clone(),
            current: current.to_owned(),
            to: payload.to.clone(),
        })
    }
}

/// After a send whose outcome is unknown — a timeout, a reply that never came — reading the status
/// again settles it: at the destination, it happened; anywhere else, it did not, or something else
/// moved it, and either way it is not done.
#[must_use]
pub fn arrived(payload: &MoveIssuePayload, current: &str) -> bool {
    same_status(current, &payload.to)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload() -> MoveIssuePayload {
        MoveIssuePayload {
            tracker: TrackerKind::Jira,
            key: "ACME-12".to_owned(),
            from: "To Do".to_owned(),
            to: "In Progress".to_owned(),
        }
    }

    #[test]
    fn an_issue_already_at_the_destination_is_adopted_without_sending_anything() {
        assert_eq!(step(&payload(), "In Progress"), MoveStep::Adopt);
    }

    #[test]
    fn an_issue_where_the_move_expected_it_is_sent() {
        assert_eq!(step(&payload(), "To Do"), MoveStep::Send);
    }

    #[test]
    fn an_issue_that_moved_somewhere_else_stops_the_move_and_says_where_it_is() {
        assert_eq!(
            step(&payload(), "Blocked"),
            MoveStep::Stop(MovedElsewhere {
                expected: "To Do".to_owned(),
                current: "Blocked".to_owned(),
                to: "In Progress".to_owned(),
            })
        );
    }

    #[test]
    fn a_status_spelled_in_another_case_is_the_same_status_as_the_board_reads_it() {
        assert_eq!(step(&payload(), "in progress"), MoveStep::Adopt);
        assert_eq!(step(&payload(), " TO DO "), MoveStep::Send);
        assert!(arrived(&payload(), "IN PROGRESS"));
    }

    #[test]
    fn the_target_names_the_tracker_and_the_issue() {
        assert_eq!(payload().target(), "jira:ACME-12");
    }
}
