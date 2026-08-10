//! Letting one agent session start another, and watch it happen.
//!
//! "Have Codex review this plan" typed at a Claude session, answered by a **live pane** rather than
//! by a blob of text a minute later. That distinction is the whole feature, and it is why this file
//! exists at all.
//!
//! # Why wtm has to be in the middle
//!
//! The cheap version needs no code here: `[agent.claude.mcp.codex]` in `wtm.toml` points Claude at
//! `codex mcp-server`, and Claude will happily open a Codex thread through it. What that buys is a
//! tool call that spins for two minutes and returns a summary, because the thread lives inside a
//! process *Claude* spawned. wtm never sees it. There is no pane, no streaming, no approval card,
//! and no way to watch Codex think — and watching is the thing that was asked for.
//!
//! So the session has to be **wtm's**. Once it is, everything the UI already does comes for free:
//! the pane, the transcript, the thinking blocks, the approval cards, the model pill. Nothing in
//! this file renders anything, because an ordinary pane already knows how.
//!
//! # The shape: a socket, and the app's own binary as the server
//!
//! An MCP server is a child process the *CLI* spawns, so it starts life outside the running app and
//! has to get back in. The route is a Unix socket ([`Hub`]), and the server on the other end of the
//! CLI's stdio is `wtm` itself, re-executed with one flag — see [`crate::bridge`].
//!
//! A separate sidecar binary was the obvious alternative and is worse in a way that only shows up
//! at packaging time: a second executable has to be declared as a Tauri `externalBin`, bundled, and
//! then *found* again at runtime from inside a `.app`, where the path differs between a dev build
//! and a release one. `current_exe()` is already correct in both, and the flag costs three lines in
//! `main.rs`.
//!
//! # Why a token rather than trusting the socket
//!
//! Filesystem permissions on the socket establish that the caller is this user. They do not
//! establish *which session* is calling, and that is the fact this file needs: a handoff has to land
//! in the same worktree as the session that asked for it, and a project that refuses an agent has to
//! keep refusing it here. So each session is issued a token when its MCP config is built, and the
//! token is what resolves to a worktree.
//!
//! The token is not a secret in the sense a password is — it is handed to a child process in its
//! environment, which is the same trust boundary the CLI itself sits on. What it prevents is a
//! *confusion*: two sessions in two worktrees whose bridges are otherwise identical.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::mpsc;

use serde::{Deserialize, Serialize};
use wtm_core::model::{AgentEvent, ExitOutcome, SessionId};

use crate::app::App;

/// The environment variable carrying a session's token.
pub const TOKEN_ENV: &str = "WTM_HANDOFF_TOKEN";

/// The environment variable carrying the socket to call back on.
pub const SOCKET_ENV: &str = "WTM_HANDOFF_SOCKET";

/// The environment variable carrying the agents this repository offers.
///
/// `id:Label` pairs, comma separated. Passed in the environment rather than fetched over the socket
/// so that `tools/list` — which a CLI issues during its own startup, before wtm has finished
/// registering the session — needs no round trip and cannot race.
///
/// This is what makes the tool *self-describing*: the `agent` parameter is an enum of exactly these
/// ids, so a model asked to "let Codex review this" picks from a closed set rather than guessing a
/// name and getting an error it has to interpret.
pub const AGENTS_ENV: &str = "WTM_HANDOFF_AGENTS";

/// The name the bridge is registered under, and therefore the prefix the model sees.
///
/// A tool call shows up as `mcp__wtm__ask_agent`. Short, because it is read in a transcript.
pub const SERVER_NAME: &str = "wtm";

/// How long a handoff waits for the other agent to finish, in milliseconds.
///
/// Ten minutes. A plan review is not a fast operation — the far side reads files, and may stop to
/// ask an approval that a human has to notice and click. The timeout exists only so a caller cannot
/// be wedged forever by a session that died without saying so; it is not a latency budget.
///
/// Measured against `Clock::monotonic_ms`, because `Instant::now` is banned outside the clock
/// adapter and the wall clock can step backwards.
const HANDOFF_TIMEOUT_MS: u64 = 600_000;

/// How long to wait between checks while a handoff runs, in milliseconds.
const HANDOFF_POLL_MS: u64 = 100;

/// What the bridge asks the app to do.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Request {
    /// Which session is asking. Resolves to a project and a worktree.
    pub token: String,
    /// The agent to hand the prompt to, by catalogue id.
    pub agent: String,
    pub prompt: String,
}

/// What the app answers.
///
/// A tagged result rather than a bare string, because "Codex looked and found nothing" and "Codex
/// never started" must not be the same value to the model that reads it. An error here is returned
/// as an MCP tool error, which the calling CLI shows as a failed tool call rather than as findings.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Response {
    pub ok: bool,
    /// The other agent's final message, when `ok`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Response {
    pub fn ok(text: String) -> Self {
        Self {
            ok: true,
            text: Some(text),
            error: None,
        }
    }

    pub fn failed(error: impl Into<String>) -> Self {
        Self {
            ok: false,
            text: None,
            error: Some(error.into()),
        }
    }
}

/// Which session a token belongs to.
///
/// The worktree is the point. A handoff opens its pane beside the session that asked, which means
/// the target worktree is not a parameter the model gets to choose — it is a property of who is
/// calling. Letting the model name a worktree would be a way to run an agent somewhere the user was
/// not looking.
#[derive(Debug, Clone)]
pub struct Caller {
    pub project: String,
    pub worktree: String,
    /// The provider that was issued this token, for the log line and to label the pane.
    pub provider: String,
}

/// The token registry and the socket that reaches it.
///
/// Owned by [`App`] and consulted from the listener thread, so it is a plain mutex-guarded map
/// rather than anything cleverer — a handoff happens at human speed, and the lock is held only long
/// enough to clone one small record.
#[derive(Debug, Default)]
pub struct Hub {
    tokens: parking_lot::Mutex<BTreeMap<String, Caller>>,
}

impl Hub {
    /// Issue a token for a session about to be opened.
    ///
    /// Called while the MCP config is being built, which is *before* the session exists — so this
    /// deliberately keys on the worktree rather than on a session id. Trying to key on the session
    /// would mean either registering after the spawn, leaving a window in which an eager CLI's first
    /// tool call fails, or threading a not-yet-known id through the config builder.
    pub fn issue(&self, caller: Caller) -> String {
        let token = uuid::Uuid::new_v4().to_string();
        self.tokens.lock().insert(token.clone(), caller);
        token
    }

    /// Resolve a token, or `None` if it was never issued.
    pub fn resolve(&self, token: &str) -> Option<Caller> {
        self.tokens.lock().get(token).cloned()
    }

    /// Forget every token issued for a worktree.
    ///
    /// Called when a worktree is removed, for the same reason the resume list is pruned then: every
    /// token names a path that no longer exists, so a handoff through one could only fail — and it
    /// would fail *after* opening a pane, which is a worse way to find out.
    pub fn forget_worktree(&self, worktree: &str) {
        self.tokens.lock().retain(|_, c| c.worktree != worktree);
    }

    /// How many tokens are outstanding. For tests.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tokens.lock().len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Collects one turn's assistant text and reports when the turn is over.
///
/// Wraps the ordinary sink rather than replacing it, and that ordering is the feature: every event
/// still reaches the webview, so the pane streams exactly as a hand-opened one does, and this only
/// *also* remembers the final message. A sink that swallowed events to collect them would produce a
/// pane that sat there looking hung.
struct Capture {
    inner: Arc<dyn wtm_agent::session::AgentSink>,
    /// Assistant messages seen so far, joined on completion.
    ///
    /// A list rather than one slot because a turn can produce several messages — Codex emits one per
    /// agent message item — and keeping only the last would silently drop the body of a review whose
    /// final paragraph happened to arrive as its own item.
    text: parking_lot::Mutex<Vec<String>>,
    /// Fires once, when the turn finishes or the session dies.
    done: parking_lot::Mutex<Option<mpsc::Sender<Outcome>>>,
}

/// How a handoff ended.
enum Outcome {
    /// The turn completed. Carries whatever the far side said.
    Finished,
    /// The session's process is gone.
    Gone(String),
    /// The far side reported a failure.
    Failed(String),
}

impl Capture {
    /// Signal completion, exactly once.
    ///
    /// Taking the sender out of the slot is what enforces the "once": a session emits `TurnFinished`
    /// and *then* exits when it is closed, and a second send on a dropped receiver is an error that
    /// would be logged as if something had gone wrong.
    fn finish(&self, outcome: Outcome) {
        if let Some(tx) = self.done.lock().take() {
            // A failed send means the waiter gave up — a timeout, or the app is quitting. Nothing to
            // do about it, and it must not disturb the reader thread this runs on.
            let _ = tx.send(outcome);
        }
    }

    /// Whether the handoff's own turn is still running.
    ///
    /// The empty sender slot doubles as "we are done", which is what stops this sink collecting for
    /// the rest of the pane's life. That is not a micro-optimisation: the pane is deliberately **left
    /// open** after the handoff answers, so without this guard every message of every later turn the
    /// user typed would be appended to a `Vec` nothing will ever read again.
    fn awaiting(&self) -> bool {
        self.done.lock().is_some()
    }

    fn collected(&self) -> String {
        self.text.lock().join("\n\n")
    }
}

impl wtm_agent::session::AgentSink for Capture {
    fn on_event(&self, session: &SessionId, event: &AgentEvent) {
        // Only while this sink's own turn is in flight. Afterwards it is a pass-through, because the
        // session outlives the handoff by design — see `awaiting`.
        if self.awaiting() {
            match event {
                AgentEvent::Message { text, .. } => self.text.lock().push(text.clone()),
                AgentEvent::TurnFinished { .. } => self.finish(Outcome::Finished),
                AgentEvent::Failed { message } => self.finish(Outcome::Failed(message.clone())),
                _ => {}
            }
        }
        self.inner.on_event(session, event);
    }

    fn on_exit(&self, session: &SessionId, outcome: &ExitOutcome) {
        // Before the inner call, so a caller waiting on this is released even if the emit below
        // fails because the window has gone.
        self.finish(Outcome::Gone(outcome.describe()));
        self.inner.on_exit(session, outcome);
    }

    fn on_ready(&self, session: &SessionId) {
        self.inner.on_ready(session);
    }
}

/// Run one handoff: open a pane, send the prompt, wait, and report what came back.
///
/// Blocking, and called from the listener thread rather than from a Tauri command — which is the
/// unusual thing about it and worth saying plainly. Every other session in the app is opened because
/// the *frontend* asked; this one is opened because a child process did, so the frontend has to be
/// *told*, which is what [`crate::agent_bridge::AGENT_SPAWNED_EVENT`] is for.
///
/// The pane is left open when the turn ends. That is the point of the feature rather than an
/// oversight: the user asked to see what the other agent did, and closing the pane the moment the
/// caller got its answer would destroy the transcript they wanted to read.
pub fn run(handle: &tauri::AppHandle, app: &Arc<App>, request: &Request) -> Response {
    let Some(caller) = app.handoff.resolve(&request.token) else {
        // Deliberately vague to the caller and specific in the log. A token that does not resolve is
        // either a session whose worktree was removed underneath it or something that should not be
        // calling at all, and the model does not need to be able to tell those apart.
        tracing::warn!(agent = %request.agent, "a handoff arrived with an unknown token");
        return Response::failed("this session is not registered for handoffs any more");
    };

    let target = request.agent.trim();
    if target.is_empty() {
        return Response::failed("no agent was named");
    }

    tracing::info!(
        from = %caller.provider,
        to = %target,
        worktree = %caller.worktree,
        "starting a handoff"
    );

    let opened = open_pane(handle, app, &caller, target);
    let (session, capture, rx) = match opened {
        Ok(parts) => parts,
        Err(error) => return Response::failed(error),
    };

    if let Err(error) = app.with_agent(session.as_str(), |agent| agent.send_turn(&request.prompt)) {
        // The pane is left on screen rather than torn down. It carries the stderr notice explaining
        // why the CLI would not take a turn, which is the only useful artefact of a failure here.
        return Response::failed(format!(
            "the {target} session would not take the prompt: {error}"
        ));
    }

    match wait(app, &rx) {
        Ok(()) => {
            let text = capture.collected();
            if text.trim().is_empty() {
                // A completed turn with no assistant text is a real outcome, not an error — an agent
                // can finish by editing files and saying nothing. Saying so beats returning an empty
                // string the caller has to guess the meaning of.
                Response::ok(format!(
                    "The {target} session finished without a written reply. Its pane is open in this \
                     worktree if you want to read what it did."
                ))
            } else {
                Response::ok(text)
            }
        }
        Err(error) => Response::failed(error),
    }
}

/// Open the pane a handoff runs in, and tell the frontend to adopt it.
///
/// Returns the capture sink alongside the session id because the caller needs both: one to send the
/// turn through, the other to read the answer off.
fn open_pane(
    handle: &tauri::AppHandle,
    app: &Arc<App>,
    caller: &Caller,
    target: &str,
) -> Result<(SessionId, Arc<Capture>, mpsc::Receiver<Outcome>), String> {
    let project = app
        .project(&caller.project)
        .map_err(|e| format!("that worktree's project is no longer registered: {e}"))?;
    let worktree = app
        .worktree(&project, &caller.worktree)
        .map_err(|e| format!("that worktree is no longer available: {e}"))?;

    let entry = wtm_agent::entry(target).ok_or_else(|| {
        format!("`{target}` is not an agent this build of wtm knows how to drive")
    })?;

    // The same refusal `open_agent_session` makes, and the reason it lives in more than one place:
    // a repository that does not offer an agent must not be made to run it by a session that asked
    // nicely. A refusal only the launcher enforces is not a refusal.
    if !project.offers_agent(target) {
        return Err(format!(
            "this repository's `wtm.toml` does not offer `{target}`"
        ));
    }

    let spec = project.agent_spec(target);
    let req = crate::commands::session_request_for(app, &project, &spec, entry, &worktree, None)
        .map_err(|e| e.message)?;

    let (tx, rx) = mpsc::channel();
    let capture = Arc::new(Capture {
        inner: crate::agent_bridge::AgentEventSink::new(handle.clone()),
        text: parking_lot::Mutex::new(Vec::new()),
        done: parking_lot::Mutex::new(Some(tx)),
    });

    let sink: Arc<dyn wtm_agent::session::AgentSink> = Arc::clone(&capture) as _;
    let session = app
        .open_agent(entry, &req, &worktree, &caller.project, &sink)
        .map_err(|e| format!("could not start a {target} session: {e}"))?;

    crate::agent_bridge::announce_spawn(
        handle,
        &crate::agent_bridge::SpawnedSession {
            session: session.as_str().to_owned(),
            project: caller.project.clone(),
            worktree: caller.worktree.clone(),
            provider: target.to_owned(),
            model: req.model.clone(),
            effort: req.effort.clone(),
            mode: req.mode.clone(),
        },
    );

    Ok((session, capture, rx))
}

/// Block until the turn is done, the session dies, or the deadline passes.
///
/// Polls a channel with a timeout rather than blocking on `recv` outright, because the deadline has
/// to be measured against the [`Clock`](wtm_core::ports::clock::Clock) port — `Instant::now` is
/// banned outside the clock adapter, and `recv_timeout` would smuggle the system clock back in.
fn wait(app: &Arc<App>, rx: &mpsc::Receiver<Outcome>) -> Result<(), String> {
    let deadline = app.clock.monotonic_ms() + HANDOFF_TIMEOUT_MS;
    loop {
        match rx.recv_timeout(std::time::Duration::from_millis(HANDOFF_POLL_MS)) {
            Ok(Outcome::Finished) => return Ok(()),
            Ok(Outcome::Failed(message)) => return Err(message),
            Ok(Outcome::Gone(summary)) => {
                return Err(format!("that session ended before it answered — {summary}"));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if app.clock.monotonic_ms() >= deadline {
                    return Err(
                        "that session did not finish in ten minutes; its pane is still open"
                            .to_owned(),
                    );
                }
            }
            // The sender was dropped without firing, which means the session object went away
            // without an exit event. Nothing more is coming.
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("that session went away without answering".to_owned());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caller(worktree: &str) -> Caller {
        Caller {
            project: "/repo".to_owned(),
            worktree: worktree.to_owned(),
            provider: "claude".to_owned(),
        }
    }

    #[test]
    fn a_token_resolves_to_the_worktree_it_was_issued_for() {
        // The property the whole design rests on: a handoff lands where the caller is, because the
        // worktree comes from the token rather than from anything the model said.
        let hub = Hub::default();
        let token = hub.issue(caller("wt-a"));

        let resolved = hub.resolve(&token).expect("a fresh token should resolve");
        assert_eq!(resolved.worktree, "wt-a");
        assert_eq!(resolved.provider, "claude");
    }

    #[test]
    fn two_sessions_get_two_different_tokens() {
        // Two panes in one worktree are the ordinary case, and a shared token would make their
        // handoffs indistinguishable in the log — and would mean forgetting one forgot both.
        let hub = Hub::default();
        assert_ne!(hub.issue(caller("wt-a")), hub.issue(caller("wt-a")));
    }

    #[test]
    fn a_token_that_was_never_issued_does_not_resolve() {
        // The refusal `run` turns into "this session is not registered any more". Worth pinning
        // because the alternative — resolving to some default worktree — would run an agent
        // somewhere nobody asked for.
        let hub = Hub::default();
        hub.issue(caller("wt-a"));
        assert!(hub.resolve("not-a-token").is_none());
    }

    #[test]
    fn removing_a_worktree_forgets_its_tokens_and_leaves_the_others() {
        // Each token names a path, so one for a removed worktree could only fail — and it would fail
        // *after* opening a pane, which is a worse way to find out. The second half of the assertion
        // is the one that matters: a tidy-up that took every token would break handoff everywhere
        // the moment any worktree was removed.
        let hub = Hub::default();
        let doomed = hub.issue(caller("wt-a"));
        let survivor = hub.issue(caller("wt-b"));

        hub.forget_worktree("wt-a");

        assert!(hub.resolve(&doomed).is_none(), "its token should be gone");
        assert!(
            hub.resolve(&survivor).is_some(),
            "another worktree's token must survive"
        );
    }

    /// A sink that does nothing, counting what it was handed.
    #[derive(Default)]
    struct Silent {
        events: parking_lot::Mutex<usize>,
    }

    impl wtm_agent::session::AgentSink for Silent {
        fn on_event(&self, _session: &SessionId, _event: &AgentEvent) {
            *self.events.lock() += 1;
        }
        fn on_exit(&self, _session: &SessionId, _outcome: &ExitOutcome) {}
        fn on_ready(&self, _session: &SessionId) {}
    }

    fn message(text: &str) -> AgentEvent {
        AgentEvent::Message {
            text: text.to_owned(),
        }
    }

    #[test]
    fn a_capture_collects_its_own_turn_and_then_stops_growing() {
        // The bug this pins: the pane is deliberately left open after a handoff answers, so this sink
        // stays attached for the rest of the session's life. Without the `awaiting` guard, every
        // message of every later turn the user typed was appended to a `Vec` nothing would ever read
        // — an unbounded leak that would only show up in a long-lived pane.
        use wtm_agent::session::AgentSink;

        let inner = Arc::new(Silent::default());
        let (tx, rx) = mpsc::channel();
        let capture = Capture {
            inner: Arc::clone(&inner) as Arc<dyn AgentSink>,
            text: parking_lot::Mutex::new(Vec::new()),
            done: parking_lot::Mutex::new(Some(tx)),
        };
        let session = SessionId::new("s-1");

        capture.on_event(&session, &message("the review"));
        capture.on_event(
            &session,
            &AgentEvent::TurnFinished {
                turn: "t-1".to_owned(),
                usage: wtm_core::model::Usage::default(),
                cost_usd: None,
            },
        );
        // A later turn, in the pane the user is now chatting in.
        capture.on_event(&session, &message("something said much later"));

        assert!(matches!(rx.try_recv(), Ok(Outcome::Finished)));
        assert_eq!(
            capture.collected(),
            "the review",
            "only the handoff's own turn should be collected"
        );
        // Still a pass-through, though: the pane must keep streaming or it would appear to freeze the
        // moment its handoff finished.
        assert_eq!(
            *inner.events.lock(),
            3,
            "every event must still reach the UI"
        );
    }

    #[test]
    fn a_response_carries_either_text_or_an_error_but_never_both() {
        // The far side branches on `ok`, and a response with both fields set would let a failed
        // handoff be read as findings. `skip_serializing_if` is what keeps the wire clean, so this
        // asserts on the serialized form rather than on the struct.
        let ok = serde_json::to_value(Response::ok("findings".to_owned())).unwrap();
        assert_eq!(ok["ok"], true);
        assert_eq!(ok["text"], "findings");
        assert!(ok.get("error").is_none(), "{ok}");

        let failed = serde_json::to_value(Response::failed("nope")).unwrap();
        assert_eq!(failed["ok"], false);
        assert_eq!(failed["error"], "nope");
        assert!(failed.get("text").is_none(), "{failed}");
    }
}
