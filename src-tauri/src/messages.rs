//! A record of what agents have said to each other, for drawing it.
//!
//! When one session hands work to another — `ask_agent`, `spawn_agents`, and Home's own tools — the
//! prompt goes out as an ordinary turn and the answer comes back as a tool result. Both ends show
//! up in their own transcripts, but nothing connected them: the parent's row read
//! `mcp__wtm__ask_agent` with no prompt, and the child's first message had no sender. Home draws
//! every such message as a wire between two rows of its session tree while it is in flight, and
//! lists it afterwards; this is what it draws them from.
//!
//! # One record per exchange, replaced whole
//!
//! An exchange is a prompt and, later, its reply or failure. It is announced on [`MESSAGE_EVENT`]
//! when it starts and again when it settles, carrying the whole record both times, and the window
//! replaces by id. That is the convention the browser panes already follow — one shape, never a
//! delta — and it is what makes a reload trivially correct: [`agent_messages`] returns the same
//! records the events carried.
//!
//! # Memory only, and bounded
//!
//! Previews are cut short, the ring keeps the newest few hundred, and nothing is written to disk.
//! That is the replay buffer's reasoning in `app.rs`: agents quote whatever they read, so a file of
//! their messages would be a second secret-bearing record the user does not know exists. A quit
//! ends it, which loses only the drawing; each transcript still holds its half.

use std::collections::VecDeque;

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::app::App;
use crate::turns::{Outcome, Settled};

/// Event name for an exchange that started or settled. Carries the whole [`Exchange`].
pub const MESSAGE_EVENT: &str = "agent:message";

/// How many exchanges are kept. Past this, the oldest go first.
const MAX_EXCHANGES: usize = 500;

/// How much of a prompt or reply an exchange keeps, in characters.
///
/// Enough for a chip and a list row to say what the message was about. The full text is in the
/// two transcripts, which is where anyone wanting it would look.
const PREVIEW_CHARS: usize = 280;

/// What carried a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Via {
    AskAgent,
    SpawnAgents,
    OpenSession,
    MessageSession,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExchangeState {
    InFlight,
    Answered,
    Failed,
}

/// One prompt from one session to another, and what came of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Exchange {
    pub id: u64,
    /// The orchestration run, for `spawn_agents`' siblings and `ask_agent`'s single child.
    pub run: Option<String>,
    /// The sending session. `None` only when it had not finished starting when it sent.
    pub from: Option<String>,
    /// The receiving session.
    pub to: String,
    pub via: Via,
    /// The start of the prompt.
    pub prompt: String,
    /// The start of the reply, once there is one.
    pub reply: Option<String>,
    pub error: Option<String>,
    pub state: ExchangeState,
    /// Unix milliseconds.
    pub sent_at: u64,
    pub settled_at: Option<u64>,
}

/// The ring of recent exchanges.
#[derive(Debug, Default)]
pub struct Log {
    ring: parking_lot::Mutex<VecDeque<Exchange>>,
    next: parking_lot::Mutex<u64>,
}

/// The first [`PREVIEW_CHARS`] characters, with an ellipsis when something was cut.
///
/// By character, not byte, so a cut can never land inside a multi-byte one and panic.
fn preview(text: &str) -> String {
    let trimmed = text.trim();
    let mut out: String = trimmed.chars().take(PREVIEW_CHARS).collect();
    if trimmed.chars().nth(PREVIEW_CHARS).is_some() {
        out.push('…');
    }
    out
}

/// What a new exchange is, before it has an id.
#[derive(Debug)]
pub struct Begin<'a> {
    pub run: Option<&'a str>,
    pub from: Option<&'a str>,
    pub to: &'a str,
    pub via: Via,
    pub prompt: &'a str,
}

impl Log {
    /// Record a prompt that is about to be sent.
    pub fn begin(&self, begin: &Begin<'_>, now: u64) -> Exchange {
        let id = {
            let mut next = self.next.lock();
            *next += 1;
            *next
        };
        let exchange = Exchange {
            id,
            run: begin.run.map(str::to_owned),
            from: begin.from.map(str::to_owned),
            to: begin.to.to_owned(),
            via: begin.via,
            prompt: preview(begin.prompt),
            reply: None,
            error: None,
            state: ExchangeState::InFlight,
            sent_at: now,
            settled_at: None,
        };
        let mut ring = self.ring.lock();
        ring.push_back(exchange.clone());
        while ring.len() > MAX_EXCHANGES {
            ring.pop_front();
        }
        exchange
    }

    /// Record how an exchange ended. `None` if the ring has already let it go — not an error: a
    /// window that missed the start has nothing to draw, and one that saw it will be reloaded
    /// from [`Self::snapshot`] without it.
    pub fn settle(&self, id: u64, outcome: &Outcome, text: &str, now: u64) -> Option<Exchange> {
        let mut ring = self.ring.lock();
        let exchange = ring.iter_mut().find(|exchange| exchange.id == id)?;
        match outcome {
            Outcome::Finished => {
                exchange.state = ExchangeState::Answered;
                exchange.reply = Some(preview(text));
            }
            Outcome::Failed(error) | Outcome::Gone(error) => {
                exchange.state = ExchangeState::Failed;
                exchange.error = Some(preview(error));
            }
        }
        exchange.settled_at = Some(now);
        Some(exchange.clone())
    }

    /// Every exchange still kept, oldest first.
    #[must_use]
    pub fn snapshot(&self) -> Vec<Exchange> {
        self.ring.lock().iter().cloned().collect()
    }
}

/// Tell the window an exchange started or settled.
pub fn announce(handle: &AppHandle, exchange: &Exchange) {
    if let Err(error) = handle.emit(MESSAGE_EVENT, exchange) {
        // The window is gone; a new one reads the ring instead.
        tracing::debug!(%error, "could not announce an agent message");
    }
}

/// Start an exchange and announce it.
pub fn begin(handle: &AppHandle, app: &App, begin: &Begin<'_>) -> Exchange {
    let exchange = app.messages.begin(begin, app.clock.now_unix_ms());
    announce(handle, &exchange);
    exchange
}

/// Settle an exchange whose prompt never ran, and announce it.
pub fn fail(handle: &AppHandle, app: &App, id: u64, error: &str) {
    if let Some(exchange) = app.messages.settle(
        id,
        &Outcome::Failed(error.to_owned()),
        "",
        app.clock.now_unix_ms(),
    ) {
        announce(handle, &exchange);
    }
}

/// Settle the exchanges a set of ended turns answer, and announce each.
pub fn settle_turns(handle: &AppHandle, app: &App, settled: &[Settled]) {
    for turn in settled {
        let Some(id) = turn.exchange else { continue };
        if let Some(exchange) =
            app.messages
                .settle(id, &turn.outcome, &turn.text, app.clock.now_unix_ms())
        {
            announce(handle, &exchange);
        }
    }
}

/// Every exchange still kept, for a window that has just loaded.
#[tauri::command]
pub async fn agent_messages(
    app: crate::commands::AppState<'_>,
) -> crate::commands::Reply<Vec<Exchange>> {
    Ok(app.messages.snapshot())
}

#[cfg(test)]
mod tests {
    //! The log is what Home draws its wires from, so what it must not do is the point: grow without
    //! bound, panic on a cut, or leave a second record when an exchange settles.

    use super::*;

    fn begin(prompt: &str) -> Begin<'_> {
        Begin {
            run: Some("run-1"),
            from: Some("parent"),
            to: "child",
            via: Via::AskAgent,
            prompt,
        }
    }

    #[test]
    fn the_log_keeps_only_the_newest_exchanges_once_full() {
        let log = Log::default();
        for n in 0..(MAX_EXCHANGES + 5) {
            log.begin(&begin(&format!("prompt {n}")), 0);
        }
        let kept = log.snapshot();
        assert_eq!(kept.len(), MAX_EXCHANGES);
        assert_eq!(kept[0].prompt, "prompt 5");
    }

    #[test]
    fn a_preview_is_cut_on_a_character_boundary() {
        // Every character here is three bytes, so a byte-indexed cut at the limit would land
        // inside one and panic.
        let long = "語".repeat(PREVIEW_CHARS + 10);
        let cut = preview(&long);
        assert_eq!(cut.chars().count(), PREVIEW_CHARS + 1);
        assert!(cut.ends_with('…'));
        assert_eq!(preview("  short  "), "short");
    }

    #[test]
    fn settling_replaces_the_exchange_rather_than_adding_a_second_record() {
        let log = Log::default();
        let started = log.begin(&begin("review this"), 10);
        assert_eq!(started.state, ExchangeState::InFlight);

        let settled = log
            .settle(started.id, &Outcome::Finished, "Looks fine.", 20)
            .expect("still in the ring");

        assert_eq!(settled.state, ExchangeState::Answered);
        assert_eq!(settled.reply.as_deref(), Some("Looks fine."));
        assert_eq!(settled.settled_at, Some(20));
        assert_eq!(log.snapshot(), vec![settled]);
    }

    #[test]
    fn a_failed_turn_settles_as_failed_with_its_reason() {
        let log = Log::default();
        let started = log.begin(&begin("review this"), 0);
        let settled = log
            .settle(started.id, &Outcome::Gone("exited".to_owned()), "", 1)
            .expect("still in the ring");
        assert_eq!(settled.state, ExchangeState::Failed);
        assert_eq!(settled.error.as_deref(), Some("exited"));
        assert!(settled.reply.is_none());
    }

    #[test]
    fn settling_an_exchange_the_ring_already_dropped_is_not_an_error() {
        let log = Log::default();
        assert!(log.settle(99, &Outcome::Finished, "late", 0).is_none());
    }

    #[test]
    fn an_exchange_crosses_the_boundary_in_camel_case() {
        let log = Log::default();
        let json = serde_json::to_value(log.begin(&begin("p"), 5)).unwrap();
        let object = json.as_object().unwrap();
        for key in ["sentAt", "settledAt", "via", "state", "from", "to", "run"] {
            assert!(object.contains_key(key), "missing `{key}` in {object:?}");
        }
        assert_eq!(object["via"], "ask_agent");
        assert_eq!(object["state"], "in_flight");
    }
}
