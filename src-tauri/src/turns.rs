//! Waiting for an agent's turn to end, and keeping what it said.
//!
//! A delegation sends a prompt to a session and blocks until that session's turn is over, then
//! hands back the assistant's text. That used to be a sink wrapped around the child's own sink
//! (`handoff::Capture`), which worked only because a delegation also *opened* the child: the sink
//! is fixed when a session opens, so there was no way to wait on a session somebody else had
//! started. Home messages sessions it did not open, so the watching moved here — one registry on
//! [`App`](crate::app::App), fed by [`crate::agent_bridge::AgentEventSink`] for every session, and
//! armed per turn by whoever is waiting.
//!
//! # A watch outlives its waiter
//!
//! A caller that gives up — the ten-minute deadline, or the app quitting — drops its receiver, but
//! the watch stays until the turn really ends. That is what lets an exchange in the message log
//! settle as *answered* when a slow child finally replies, rather than being left drawn as a wire
//! in flight for the rest of the session. Watches are bounded per session so a session that never
//! finishes a turn cannot accumulate them.

use std::collections::BTreeMap;
use std::sync::mpsc;

use wtm_core::model::AgentEvent;
use wtm_core::ports::clock::Clock;

/// How long a delegated or Home-sent turn may take, in milliseconds.
///
/// Ten minutes. A plan review is not a fast operation — the far side reads files, and may stop to
/// ask an approval that a human has to notice and click. The timeout exists only so a caller cannot
/// be wedged forever by a session that died without saying so; it is not a latency budget.
pub const TURN_TIMEOUT_MS: u64 = 600_000;

/// How long to wait between deadline checks, in milliseconds.
const POLL_MS: u64 = 100;

/// How many unfinished watches one session may carry.
///
/// One per caller waiting on it, and a session takes one turn at a time, so in practice this is one.
/// Eight leaves room for waiters that gave up on turns that never ended, without letting them grow
/// without bound.
const MAX_WATCHES_PER_SESSION: usize = 8;

/// How a watched turn ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The turn completed.
    Finished,
    /// The far side reported a failure, a usage limit included.
    Failed(String),
    /// The session's process is gone.
    Gone(String),
}

/// One turn's assistant text, merged across the ways providers report it.
#[derive(Debug, Default, Clone)]
pub struct Reply {
    text: String,
}

impl Reply {
    /// Take in one event; anything that is not assistant text is ignored.
    pub fn absorb(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::MessageDelta { text } => self.text.push_str(text),
            AgentEvent::Message { text } => {
                // Codex can report the complete item after streaming the same text as deltas.
                // A whole-message-only provider still lands here with an empty accumulator.
                if self.text.is_empty() {
                    self.text.push_str(text);
                } else if self.text != *text && !self.text.ends_with(text.as_str()) {
                    self.text.push_str("\n\n");
                    self.text.push_str(text);
                }
            }
            _ => {}
        }
    }

    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// Whether an event ends the watched turn, and how.
///
/// `LimitReached` ends it as a failure. Claude follows it with an ordinary `TurnFinished`, which
/// used to make an exhausted child answer "finished without a written reply"; Codex follows it with
/// nothing at all, which made the caller wait out the whole ten minutes.
fn ending(event: &AgentEvent) -> Option<Outcome> {
    match event {
        AgentEvent::TurnFinished { .. } => Some(Outcome::Finished),
        AgentEvent::Failed { message } | AgentEvent::LimitReached { message, .. } => {
            Some(Outcome::Failed(message.clone()))
        }
        _ => None,
    }
}

/// A watched turn that has ended. Handed back so the caller can settle its exchange.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settled {
    /// The message-log exchange this turn answers, if one was recorded.
    pub exchange: Option<u64>,
    pub outcome: Outcome,
    pub text: String,
}

struct Watch {
    id: u64,
    exchange: Option<u64>,
    reply: Reply,
    /// Dropped by a waiter that gave up; a failed send is expected then and means nothing.
    tx: mpsc::Sender<(Outcome, String)>,
}

impl Watch {
    fn settle(self, outcome: Outcome) -> Settled {
        let text = self.reply.text;
        let _ = self.tx.send((outcome.clone(), text.clone()));
        Settled {
            exchange: self.exchange,
            outcome,
            text,
        }
    }
}

/// The receiving end of one watch.
#[derive(Debug)]
pub struct Waiter {
    session: String,
    id: u64,
    exchange: Option<u64>,
    rx: mpsc::Receiver<(Outcome, String)>,
}

impl Waiter {
    /// The message-log exchange this watch will settle, if one was recorded.
    #[must_use]
    pub const fn exchange(&self) -> Option<u64> {
        self.exchange
    }

    /// How the turn ended, if it already has and nothing has read it yet.
    ///
    /// For a waiter that stopped waiting at the same moment the turn ended: the ending was sent
    /// before anyone outside this module could learn the watch was gone, so it is already here.
    #[must_use]
    pub fn take(&self) -> Option<(Outcome, String)> {
        self.rx.try_recv().ok()
    }
}

/// Every armed watch, by session id.
#[derive(Default)]
pub struct Registry {
    watches: parking_lot::Mutex<BTreeMap<String, Vec<Watch>>>,
    next: parking_lot::Mutex<u64>,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Registry")
            .field("sessions", &self.watches.lock().len())
            .finish_non_exhaustive()
    }
}

impl Registry {
    /// Arm a watch on `session`'s next turn. Call it *before* sending the turn, or its first
    /// events can arrive before anything is listening.
    ///
    /// # Errors
    ///
    /// If the session already carries [`MAX_WATCHES_PER_SESSION`] unfinished watches.
    pub fn watch(&self, session: &str, exchange: Option<u64>) -> Result<Waiter, String> {
        let mut watches = self.watches.lock();
        let list = watches.entry(session.to_owned()).or_default();
        if list.len() >= MAX_WATCHES_PER_SESSION {
            return Err(
                "that session has too many unanswered requests waiting on it already".to_owned(),
            );
        }
        let id = {
            let mut next = self.next.lock();
            *next += 1;
            *next
        };
        let (tx, rx) = mpsc::channel();
        list.push(Watch {
            id,
            exchange,
            reply: Reply::default(),
            tx,
        });
        Ok(Waiter {
            session: session.to_owned(),
            id,
            exchange,
            rx,
        })
    }

    /// Disarm a watch whose turn was never sent — the session refused the prompt.
    ///
    /// Returns its exchange, so the caller can settle that as failed itself.
    pub fn cancel(&self, waiter: &Waiter) -> Option<u64> {
        let mut watches = self.watches.lock();
        let list = watches.get_mut(&waiter.session)?;
        let index = list.iter().position(|watch| watch.id == waiter.id)?;
        let exchange = list.remove(index).exchange;
        if list.is_empty() {
            watches.remove(&waiter.session);
        }
        exchange
    }

    /// Feed one event of one session. Returns the watches it ended.
    pub fn observe(&self, session: &str, event: &AgentEvent) -> Vec<Settled> {
        let mut watches = self.watches.lock();
        let Some(list) = watches.get_mut(session) else {
            return Vec::new();
        };
        let Some(outcome) = ending(event) else {
            for watch in list.iter_mut() {
                watch.reply.absorb(event);
            }
            return Vec::new();
        };
        let ended = std::mem::take(list);
        watches.remove(session);
        // Sent with the lock released: a waiter woken here goes straight back into the app, and
        // nothing it does should be able to queue behind this map.
        drop(watches);
        ended
            .into_iter()
            .map(|watch| watch.settle(outcome.clone()))
            .collect()
    }

    /// The session's process is gone: end every watch on it.
    pub fn gone(&self, session: &str, summary: &str) -> Vec<Settled> {
        let ended = self.watches.lock().remove(session).unwrap_or_default();
        ended
            .into_iter()
            .map(|watch| watch.settle(Outcome::Gone(summary.to_owned())))
            .collect()
    }

    /// Whether anything is waiting on this session's turn.
    #[must_use]
    pub fn in_flight(&self, session: &str) -> bool {
        self.watches
            .lock()
            .get(session)
            .is_some_and(|list| !list.is_empty())
    }
}

/// How a [`wait_until`] ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Wait {
    /// The turn ended, with its text.
    Ended(Outcome, String),
    /// The deadline passed first. The watch is still armed, and settles when the turn ends.
    TimedOut,
    /// The caller's own condition said to stop waiting. The watch is still armed, as above.
    Stopped,
    /// The watch went without settling, which only `cancel` does. Nothing more is coming.
    Lost,
}

/// Block until the watched turn ends, the deadline passes, or `stop` says to give up.
///
/// Polls the channel with a timeout rather than blocking on `recv` outright, because the deadline
/// has to be measured against the [`Clock`] port — `Instant::now` is banned outside the clock
/// adapter, and `recv_timeout`'s own deadline would smuggle the system clock back in. `stop` is
/// asked on the same beat, which is what lets Home's `wait: true` come back the moment the session
/// it asked starts waiting on the user, rather than holding Home's turn shut while it does.
pub fn wait_until(
    clock: &dyn Clock,
    waiter: &Waiter,
    timeout_ms: u64,
    mut stop: impl FnMut() -> bool,
) -> Wait {
    let deadline = clock.monotonic_ms() + timeout_ms;
    loop {
        match waiter
            .rx
            .recv_timeout(std::time::Duration::from_millis(POLL_MS))
        {
            Ok((outcome, text)) => return Wait::Ended(outcome, text),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if clock.monotonic_ms() >= deadline {
                    return Wait::TimedOut;
                }
                if stop() {
                    return Wait::Stopped;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return Wait::Lost,
        }
    }
}

/// Block until the watched turn ends or the deadline passes. The reply text, or why there is none.
///
/// # Errors
///
/// The far side's failure message, the session's death, or the deadline — each phrased for the
/// model that will read it.
pub fn wait(clock: &dyn Clock, waiter: &Waiter, timeout_ms: u64) -> Result<String, String> {
    match wait_until(clock, waiter, timeout_ms, || false) {
        Wait::Ended(Outcome::Finished, text) => Ok(text),
        Wait::Ended(Outcome::Failed(message), _) => Err(message),
        Wait::Ended(Outcome::Gone(summary), _) => Err(ended_before_answering(&summary)),
        Wait::TimedOut | Wait::Stopped => {
            Err("that session did not finish in ten minutes; its pane is still open".to_owned())
        }
        Wait::Lost => Err("that session went away without answering".to_owned()),
    }
}

/// What a caller is told about a session that died mid-turn.
#[must_use]
pub fn ended_before_answering(summary: &str) -> String {
    format!("that session ended before it answered — {summary}")
}

/// Why a tracked turn produced no reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendFailure {
    /// The session would not take the prompt, so nothing ran.
    Refused(String),
    /// The turn ran and ended without a reply: a failure, a death, or the deadline.
    Ended(String),
}

/// Send `text` to a session as a recorded exchange, armed so the exchange settles when the turn
/// ends whether or not anyone waits for it.
///
/// `begin.prompt` is what the log shows; `text` is what the session receives. They differ when a
/// sender labels its message — Home prefixes its own — and the label is not what the wire is about.
///
/// # Errors
///
/// [`SendFailure::Refused`] when the session will not take a turn, with the exchange settled as
/// failed.
pub fn send(
    handle: &tauri::AppHandle,
    app: &crate::app::App,
    begin: &crate::messages::Begin<'_>,
    text: &str,
) -> Result<Waiter, SendFailure> {
    send_tracked(handle, app, begin, text, |_| {})
}

/// [`send`], handing the new exchange to `armed` after the watch is set and before the turn is
/// sent.
///
/// That moment is the only safe one for a caller that keeps its own record of the exchange — Home's
/// delegations. Any later and a turn that fails at once can settle before the record exists, and
/// the record would then wait for an ending that has already happened. A refused send settles the
/// exchange as failed, so whatever `armed` recorded must be dropped by the caller on `Err`.
///
/// # Errors
///
/// As [`send`].
pub fn send_tracked(
    handle: &tauri::AppHandle,
    app: &crate::app::App,
    begin: &crate::messages::Begin<'_>,
    text: &str,
    armed: impl FnOnce(&crate::messages::Exchange),
) -> Result<Waiter, SendFailure> {
    let exchange = crate::messages::begin(handle, app, begin);
    let waiter = match app.turns.watch(begin.to, Some(exchange.id)) {
        Ok(waiter) => waiter,
        Err(error) => {
            crate::messages::fail(handle, app, exchange.id, &error);
            return Err(SendFailure::Refused(error));
        }
    };
    armed(&exchange);
    if let Err(error) = app.send_agent_turn(begin.to, text, &[]) {
        app.turns.cancel(&waiter);
        let error = error.to_string();
        crate::messages::fail(handle, app, exchange.id, &error);
        return Err(SendFailure::Refused(error));
    }
    Ok(waiter)
}

/// [`send`], then block until the turn ends. The reply text.
///
/// # Errors
///
/// As [`send`], and [`SendFailure::Ended`] for a turn that ended without one.
pub fn send_and_wait(
    handle: &tauri::AppHandle,
    app: &crate::app::App,
    begin: &crate::messages::Begin<'_>,
    text: &str,
) -> Result<String, SendFailure> {
    let waiter = send(handle, app, begin, text)?;
    wait(app.clock.as_ref(), &waiter, TURN_TIMEOUT_MS).map_err(SendFailure::Ended)
}

#[cfg(test)]
mod tests {
    //! The watch is the only route by which a delegation learns what its child said, so these pin
    //! the two things it has to get right: whose events it collects, and when it stops.

    use super::*;
    use wtm_core::model::Usage;

    fn delta(text: &str) -> AgentEvent {
        AgentEvent::MessageDelta {
            text: text.to_owned(),
        }
    }

    fn finished() -> AgentEvent {
        AgentEvent::TurnFinished {
            turn: "t".to_owned(),
            usage: Usage::default(),
            cost_usd: None,
        }
    }

    fn received(waiter: &Waiter) -> Option<(Outcome, String)> {
        waiter.rx.try_recv().ok()
    }

    #[test]
    fn a_watch_collects_its_own_turn_and_then_stops_growing() {
        // The pane is left open after a delegation answers, so a watch that kept collecting would
        // append every later turn the user typed to a buffer nothing reads.
        let turns = Registry::default();
        let waiter = turns.watch("s", None).expect("armed");

        turns.observe("s", &delta("one "));
        turns.observe("s", &delta("two"));
        let settled = turns.observe("s", &finished());
        turns.observe("s", &delta("a later turn"));

        assert_eq!(
            received(&waiter),
            Some((Outcome::Finished, "one two".to_owned()))
        );
        assert_eq!(settled.len(), 1);
        assert!(!turns.in_flight("s"));
    }

    #[test]
    fn a_watch_collects_streaming_deltas_without_duplicating_the_completed_message() {
        let mut reply = Reply::default();
        reply.absorb(&delta("Looks "));
        reply.absorb(&delta("fine."));
        reply.absorb(&AgentEvent::Message {
            text: "Looks fine.".to_owned(),
        });
        assert_eq!(reply.text(), "Looks fine.");

        let mut whole = Reply::default();
        whole.absorb(&AgentEvent::Message {
            text: "First.".to_owned(),
        });
        whole.absorb(&AgentEvent::Message {
            text: "Second.".to_owned(),
        });
        assert_eq!(whole.text(), "First.\n\nSecond.");
    }

    #[test]
    fn a_watch_on_one_session_never_sees_another_sessions_events() {
        let turns = Registry::default();
        let mine = turns.watch("mine", None).expect("armed");

        turns.observe("other", &delta("not for you"));
        turns.observe("other", &finished());

        assert!(received(&mine).is_none());
        assert!(turns.in_flight("mine"));
    }

    #[test]
    fn a_session_that_exits_settles_every_watch_on_it_as_gone() {
        let turns = Registry::default();
        let first = turns.watch("s", Some(1)).expect("armed");
        let second = turns.watch("s", Some(2)).expect("armed");

        let settled = turns.gone("s", "exited with code 1");

        assert_eq!(
            settled
                .iter()
                .map(|settled| settled.exchange)
                .collect::<Vec<_>>(),
            vec![Some(1), Some(2)]
        );
        for waiter in [&first, &second] {
            assert!(matches!(received(waiter), Some((Outcome::Gone(_), _))));
        }
    }

    #[test]
    fn a_usage_limit_ends_a_wait_rather_than_running_out_the_clock() {
        let turns = Registry::default();
        let waiter = turns.watch("s", None).expect("armed");

        turns.observe(
            "s",
            &AgentEvent::LimitReached {
                message: "You've hit your limit.".to_owned(),
                resets_at: None,
            },
        );

        assert_eq!(
            received(&waiter),
            Some((
                Outcome::Failed("You've hit your limit.".to_owned()),
                String::new()
            ))
        );
    }

    #[test]
    fn a_waiter_that_gave_up_still_settles_its_exchange_when_the_turn_ends() {
        // The deadline drops the receiver; the exchange it was waiting on must still be answered
        // when the child finally finishes, or its wire would stay drawn in flight.
        let turns = Registry::default();
        let waiter = turns.watch("s", Some(7)).expect("armed");
        drop(waiter);

        turns.observe("s", &delta("late"));
        let settled = turns.observe("s", &finished());

        assert_eq!(
            settled,
            vec![Settled {
                exchange: Some(7),
                outcome: Outcome::Finished,
                text: "late".to_owned(),
            }]
        );
    }

    #[test]
    fn a_session_cannot_accumulate_unbounded_watches() {
        let turns = Registry::default();
        let waiters = (0..MAX_WATCHES_PER_SESSION)
            .map(|_| turns.watch("s", None).expect("within the bound"))
            .collect::<Vec<_>>();

        assert!(turns.watch("s", None).is_err());
        assert!(turns.watch("other", None).is_ok());
        drop(waiters);
    }

    #[test]
    fn a_wait_told_to_stop_leaves_the_watch_armed_so_the_turn_still_settles_its_exchange() {
        // Home's `wait: true` stops waiting when its session starts waiting on the user. The
        // delegation is still running, and its end has to reach the exchange — and Home — later.
        let clock = wtm_testkit::FakeClock::new();
        let turns = Registry::default();
        let waiter = turns.watch("s", Some(5)).expect("armed");
        assert_eq!(waiter.exchange(), Some(5));

        assert_eq!(
            wait_until(&clock, &waiter, TURN_TIMEOUT_MS, || true),
            Wait::Stopped
        );
        assert!(turns.in_flight("s"));

        turns.observe("s", &delta("done"));
        let settled = turns.observe("s", &finished());
        assert_eq!(settled.len(), 1);
        assert_eq!(settled[0].exchange, Some(5));
    }

    #[test]
    fn a_turn_that_ends_as_its_waiter_gives_up_can_still_be_taken() {
        // The ending is sent before the watch can be seen to be gone, so a waiter that stopped at
        // that same moment finds it waiting rather than losing the reply between the two.
        let clock = wtm_testkit::FakeClock::new();
        let turns = Registry::default();
        let waiter = turns.watch("s", None).expect("armed");
        assert_eq!(wait_until(&clock, &waiter, 0, || false), Wait::TimedOut);

        turns.observe("s", &delta("late"));
        turns.observe("s", &finished());

        assert_eq!(waiter.take(), Some((Outcome::Finished, "late".to_owned())));
        assert_eq!(waiter.take(), None);
    }

    #[test]
    fn cancelling_a_watch_hands_back_its_exchange_and_leaves_the_others() {
        let turns = Registry::default();
        let refused = turns.watch("s", Some(3)).expect("armed");
        let _kept = turns.watch("s", Some(4)).expect("armed");

        assert_eq!(turns.cancel(&refused), Some(3));
        let settled = turns.observe("s", &finished());
        assert_eq!(settled.len(), 1);
        assert_eq!(settled[0].exchange, Some(4));
    }
}
