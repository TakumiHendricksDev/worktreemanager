//! What happens to runs while wtm, or the Mac, is not watching: the processes a crash leaves
//! behind, the Mac asleep, and keeping it awake while runs work, when you asked for that.
//!
//! # Orphans are found by what was recorded, and stopped only when they still match
//!
//! A run session's CLI is its own process group, so when wtm crashes the CLI keeps going, possibly
//! still editing a worktree. Each session's pid and start time are kept as it opens
//! ([`Engine::keep_process`]). At launch, before anything else runs, every one an earlier wtm kept
//! is looked up: still that process — same pid, same start time — it is stopped, group and all, and
//! the run gets a note saying so; anything else is left alone, because a pid alone may belong to
//! something new by now ([`Engine::reap_orphans`]). Either way the record is cleared, so a launch
//! deals with a process once.
//!
//! # Sleep is a note, never progress
//!
//! A thread looks at the two clocks every few seconds ([`Engine::watch`]). When the wall clock has
//! run half a minute ahead of the monotonic one, the Mac slept, and each run that was working gets a
//! note: "the Mac was asleep" and when. It is not an event, so it moves no run's "last worked on";
//! a run the sleep cut off says so by its own hold.
//!
//! # Keeping the Mac awake is yours to turn on
//!
//! Off by default. Turned on (Settings → Work, `ui.keep_awake`), wtm holds an idle-sleep assertion
//! while at least one run is working and lets it go when none is. Closing the lid still sleeps the
//! Mac. The assertion is `caffeinate -i -w <wtm's pid>`, so it ends with wtm even if wtm crashes.

use std::sync::Arc;

use parking_lot::Mutex;
use wtm_core::flow::{
    FlowId, InvocationId, NewNote, NoteKind, ProcessIdentity, UnixMs, WATCH_MS, asleep, keep_awake,
};
use wtm_core::model::SessionId;
use wtm_core::ports::RunStore;
use wtm_core::ports::config::ConfigStore as _;

use super::Engine;

/// The preference that turns keeping the Mac awake on: `"on"`, or anything else for off.
pub const KEEP_AWAKE_PREF: &str = "ui.keep_awake";

/// Something held that keeps the Mac awake until it is dropped.
pub trait Held: Send {}

/// Holds the Mac awake. The real one runs `caffeinate`; a test's counts.
pub trait KeepAwake: Send + Sync {
    /// Start holding it awake.
    ///
    /// # Errors
    ///
    /// Why it could not, in a sentence.
    fn hold(&self) -> Result<Box<dyn Held>, String>;
}

/// `caffeinate -i -w <pid>`: an idle-sleep assertion that ends when it is dropped, or when wtm does.
#[derive(Debug, Default)]
pub struct Caffeinate;

struct Caffeinated(std::process::Child);

impl Held for Caffeinated {}

impl Drop for Caffeinated {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl KeepAwake for Caffeinate {
    fn hold(&self) -> Result<Box<dyn Held>, String> {
        // A long-lived child the app ends itself, which the runner's captured commands are not:
        // `-w` ties it to this process too, so a crash cannot leave the Mac held awake.
        #[allow(clippy::disallowed_methods)]
        let child = std::process::Command::new("/usr/bin/caffeinate")
            .args(["-i", "-w", &std::process::id().to_string()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("caffeinate did not start: {e}"))?;
        Ok(Box::new(Caffeinated(child)))
    }
}

/// What keeps the Mac awake, and whether it is held now.
#[derive(Default)]
pub(super) struct Awake {
    power: Option<Arc<dyn KeepAwake>>,
    held: Option<Box<dyn Held>>,
}

impl Engine {
    /// Give the engine what keeps the Mac awake. Until it has one, nothing is held.
    pub fn set_keep_awake(&self, power: Arc<dyn KeepAwake>) {
        self.awake.lock().power = Some(power);
    }

    /// Whether wtm is holding the Mac awake now.
    #[must_use]
    pub fn holding_awake(&self) -> bool {
        self.awake.lock().held.is_some()
    }

    /// Keep the process behind `session`, which `invocation` opened, so a launch after a crash can
    /// find it.
    pub(super) fn keep_process(&self, invocation: InvocationId, session: &SessionId) {
        let Some(process) = self.host.process(session) else {
            return;
        };
        if let Ok(db) = self.db()
            && let Err(error) = db.keep_process(invocation, &process)
        {
            tracing::warn!(%invocation, %error, "could not keep a run session's process");
        }
    }

    /// At launch: stop each run session an earlier wtm left running, if it is still that process,
    /// and forget them all. Returns how many were stopped.
    pub(super) fn reap_orphans(&self, before: UnixMs) -> usize {
        let Ok(db) = self.db() else { return 0 };
        let kept = match db.kept_processes(before) {
            Ok(kept) => kept,
            Err(error) => {
                tracing::warn!(%error, "could not read the run sessions' processes");
                return 0;
            }
        };
        let mut stopped = 0;
        for (invocation, process) in kept {
            if self.host.reap(&process) {
                stopped += 1;
                tracing::info!(flow = %invocation.flow, role = invocation.role, pid = process.pid,
                    "stopped a run session the last wtm left running");
                let _ = db.note(
                    &NewNote {
                        flow: invocation.flow,
                        kind: NoteKind::Reaped,
                        from: None,
                        to: None,
                        text: format!(
                            "`{}`'s session was still running when wtm started again (pid {}); \
                             wtm stopped it. Its work so far is in the worktree, and Resume \
                             carries it on.",
                            invocation.role, process.pid
                        ),
                    },
                    self.now(),
                );
                self.notice.changed(invocation.flow);
            }
            let _ = db.forget_process(invocation.id);
        }
        stopped
    }

    /// Watch the clocks on a thread of its own, until wtm quits: sleep notes, and keeping the Mac
    /// awake when you turned that on.
    pub fn watch(self: &Arc<Self>) {
        let engine = Arc::downgrade(self);
        let spawned = std::thread::Builder::new()
            .name("wtm-watch".to_owned())
            .spawn(move || {
                let mut last = match engine.upgrade() {
                    Some(engine) => engine.clocks(),
                    None => return,
                };
                loop {
                    std::thread::sleep(std::time::Duration::from_millis(WATCH_MS));
                    let Some(engine) = engine.upgrade() else {
                        return;
                    };
                    if engine.quitting() {
                        engine.awake.lock().held = None;
                        return;
                    }
                    last = engine.watch_tick(last);
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "could not start watching the clocks");
        }
    }

    fn clocks(&self) -> (UnixMs, u64) {
        (self.clock.now_unix_ms(), self.clock.monotonic_ms())
    }

    /// One look at the clocks after `last`: a note on each working run when the Mac slept since,
    /// and the keep-awake assertion held or let go to match. Returns this look, for the next.
    pub fn watch_tick(&self, last: (UnixMs, u64)) -> (UnixMs, u64) {
        let now = self.clocks();
        let working: Vec<FlowId> = self.runs.lock().keys().copied().collect();
        if let Some((from, to)) = asleep(last, now) {
            tracing::info!(from, to, runs = working.len(), "the Mac was asleep");
            if let Ok(db) = self.db() {
                for flow in &working {
                    let _ = db.note(
                        &NewNote {
                            flow: *flow,
                            kind: NoteKind::Asleep,
                            from: Some(from),
                            to: Some(to),
                            text: "The Mac was asleep; nothing in this run worked meanwhile."
                                .to_owned(),
                        },
                        now.0,
                    );
                    self.notice.changed(*flow);
                }
            }
        }
        let turned_on = self
            .app
            .config
            .user_pref(KEEP_AWAKE_PREF)
            .ok()
            .flatten()
            .is_some_and(|value| value == "on");
        let wanted = keep_awake(turned_on, working.len());
        let mut awake = self.awake.lock();
        match (wanted, awake.held.is_some()) {
            (true, false) => {
                if let Some(power) = awake.power.clone() {
                    match power.hold() {
                        Ok(held) => {
                            tracing::info!(runs = working.len(), "holding the Mac awake");
                            awake.held = Some(held);
                        }
                        Err(error) => tracing::warn!(%error, "could not hold the Mac awake"),
                    }
                }
            }
            (false, true) => {
                tracing::info!("letting the Mac sleep again");
                awake.held = None;
            }
            _ => {}
        }
        now
    }

    /// A run's notes, oldest first: the Mac asleep, a session stopped at launch.
    ///
    /// # Errors
    ///
    /// When the database cannot be read.
    pub fn notes(&self, flow: FlowId) -> Result<Vec<wtm_core::flow::FlowNote>, String> {
        self.db()?.notes(flow).map_err(super::text)
    }

    /// The process behind `process`, as the launch would see it: for a test that plays the crash.
    #[must_use]
    pub fn process_of(&self, session: &SessionId) -> Option<ProcessIdentity> {
        self.host.process(session)
    }
}

pub(super) type AwakeState = Mutex<Awake>;
