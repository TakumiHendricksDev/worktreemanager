//! What happened to a run while nothing in it did: the Mac asleep, a session the last wtm left
//! running and this one stopped.
//!
//! # A note is not progress
//!
//! A run's events are its transitions (`super::event`), and every one of them moves `updated_at`,
//! which is what "last worked on" means everywhere. Sleep is not a transition, and nothing worked
//! while the lid was shut, so it is a [`FlowNote`] beside the events: kept, shown in its run, and
//! never read as the run doing anything. A run interrupted by the sleep is still said to be
//! interrupted, by the hold that says so; the note says only when the Mac was asleep.
//!
//! # Sleep is read from two clocks
//!
//! macOS's monotonic clock does not advance while the Mac sleeps, and the wall clock does
//! ([`super::capacity::slept`]). Between two looks a few seconds apart, the wall clock having moved
//! half a minute further is sleep, and its span is known to within one look.

use serde::{Deserialize, Serialize};

use super::UnixMs;
use super::capacity::SLEEP_GAP_MS;
use super::state::FlowId;

/// How often wtm looks at the two clocks while it runs.
pub const WATCH_MS: u64 = 5_000;

/// What a note is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteKind {
    /// The Mac slept from `from` to `to` while the run was working.
    Asleep,
    /// A session the last wtm left running was found at launch and stopped.
    Reaped,
}

/// Something that happened to a run while it was not moving.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowNote {
    pub id: i64,
    pub flow: FlowId,
    pub kind: NoteKind,
    pub from: Option<UnixMs>,
    pub to: Option<UnixMs>,
    pub text: String,
    pub at: UnixMs,
}

/// A note to keep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewNote {
    pub flow: FlowId,
    pub kind: NoteKind,
    pub from: Option<UnixMs>,
    pub to: Option<UnixMs>,
    pub text: String,
}

/// When the Mac was asleep between two looks at `(wall, monotonic)` milliseconds, if it was: from
/// the moment the monotonic clock says the first look's time ran out, to the second look. `None`
/// when the gap is within [`SLEEP_GAP_MS`].
#[must_use]
pub fn asleep(before: (UnixMs, u64), now: (UnixMs, u64)) -> Option<(UnixMs, UnixMs)> {
    let wall = now.0.saturating_sub(before.0);
    let awake = now.1.saturating_sub(before.1);
    (wall > awake.saturating_add(SLEEP_GAP_MS)).then(|| (before.0.saturating_add(awake), now.0))
}

/// Whether wtm should hold the Mac awake: only when you turned it on, and only while a run works.
/// Closing the lid sleeps the Mac whatever this says; the assertion stops only idle sleep.
#[must_use]
pub fn keep_awake(turned_on: bool, working: usize) -> bool {
    turned_on && working > 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wall_clock_far_ahead_of_the_monotonic_one_is_sleep_and_says_when() {
        let before = (1_000_000, 50_000);
        // Five seconds awake, then forty minutes asleep.
        let now = (1_000_000 + 5_000 + 2_400_000, 55_000);
        assert_eq!(asleep(before, now), Some((1_005_000, 3_405_000)));
    }

    #[test]
    fn a_busy_scheduler_or_a_nudged_clock_is_not_sleep() {
        assert_eq!(asleep((1_000, 0), (6_000, 5_000)), None);
        assert_eq!(asleep((1_000, 0), (36_000, 5_500)), None);
        assert_eq!(
            asleep((50_000, 10_000), (40_000, 15_000)),
            None,
            "a clock set back"
        );
    }

    #[test]
    fn the_mac_is_held_awake_only_when_turned_on_and_only_while_a_run_works() {
        assert!(!keep_awake(false, 3), "off by default");
        assert!(!keep_awake(true, 0));
        assert!(keep_awake(true, 1));
    }
}
