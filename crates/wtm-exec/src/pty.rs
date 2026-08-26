//! Pseudo-terminal sessions.
//!
//! # Shape of the implementation
//!
//! One OS thread per session. It reads the pty master until EOF, forwarding chunks
//! to the [`PtySink`], then reaps the child and records the outcome. [`PtyHost::wait`]
//! polls that recorded outcome rather than calling `child.wait()` itself, which is
//! what keeps blocking calls out from under the registry lock — the alternative
//! deadlocks the moment a second session starts.
//!
//! Bytes are forwarded as bytes, never decoded. Terminal output does not split on
//! UTF-8 boundaries, and reassembly is the terminal emulator's job.
//!
//! # Kills go to the process group
//!
//! `portable-pty` gives us a `ChildKiller`, but it signals only the direct child.
//! Interactive sessions are precisely where the tree is deepest — a setup script
//! running a shell running `docker` — so termination goes through
//! [`crate::signal::terminate_group`] instead. `portable-pty` calls `setsid()` in the
//! child, making it a session and group leader, so its pid *is* its process-group
//! id. That assumption is verified empirically by
//! `signal::tests::terminating_a_group_reaps_grandchildren`.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use wtm_core::error::ExecError;
use wtm_core::model::{ExitOutcome, SessionId};
use wtm_core::ports::clock::Clock;
use wtm_core::ports::exec::{CancelToken, Invocation};
use wtm_core::ports::pty::{PtyHost, PtySession, PtySink, Spawned};

use crate::clock::SystemClock;
use crate::path::ResolvedPath;
use crate::signal;

/// How often [`PtyHost::wait`] checks for completion, cancellation and the deadline.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// How long to wait for the reader to publish an outcome after the one allowed
/// TERM/grace/KILL escalation has completed.
const FINAL_OUTCOME_WAIT_MS: u64 = 1_000;

/// Read buffer size. Large enough that a chatty build does not cause an event
/// storm, small enough that an interactive prompt appears immediately (the read
/// returns as soon as any data is available, so this is a ceiling, not a batch
/// size).
const READ_CHUNK: usize = 8192;

/// Why a session ended, when we ended it deliberately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Intervention {
    TimedOut,
    Cancelled,
    Killed,
}

struct Session {
    argv: Vec<String>,
    worktree: Option<String>,
    /// `None` only if the child exited before we could ask, which would be
    /// unusual; without it we cannot signal the group.
    pid: Option<u32>,
    timeout_ms: u64,
    started_ms: u64,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    /// Set once by the reader thread when the child is reaped.
    outcome: Arc<Mutex<Option<ExitOutcome>>>,
    /// Set by whoever intervened, so the reported outcome reflects the cause
    /// rather than the resulting signal.
    intervention: Arc<Mutex<Option<Intervention>>>,
}

/// `portable-pty`-backed [`PtyHost`].
pub struct PtyHostImpl {
    path: ResolvedPath,
    clock: Arc<dyn Clock>,
    sessions: Mutex<HashMap<SessionId, Session>>,
}

impl std::fmt::Debug for PtyHostImpl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtyHostImpl")
            .field("sessions", &self.sessions.lock().len())
            .finish_non_exhaustive()
    }
}

impl PtyHostImpl {
    #[must_use]
    pub fn new(path: ResolvedPath) -> Self {
        Self::with_clock(path, Arc::new(SystemClock::new()))
    }

    pub(crate) fn with_clock(path: ResolvedPath, clock: Arc<dyn Clock>) -> Self {
        Self {
            path,
            clock,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// Look up a session's shared handles without holding the registry lock for
    /// longer than the lookup itself.
    fn with_session<T>(
        &self,
        id: &SessionId,
        f: impl FnOnce(&Session) -> T,
    ) -> Result<T, ExecError> {
        let sessions = self.sessions.lock();
        sessions
            .get(id)
            .map(f)
            .ok_or_else(|| ExecError::NoSuchSession(id.as_str().to_owned()))
    }

    /// Record an intervention and signal the group.
    fn intervene(&self, id: &SessionId, why: Intervention) -> Result<(), ExecError> {
        let (pid, flag) = self.with_session(id, |s| (s.pid, Arc::clone(&s.intervention)))?;
        let mut recorded = flag.lock();
        let first = recorded.is_none();
        if first {
            recorded.replace(why);
        }
        drop(recorded);
        if first && let Some(pid) = pid {
            signal::terminate_group(pid);
        }
        Ok(())
    }

    /// Forget finished sessions, keeping the most recent ones so a transcript stays
    /// readable after a failure.
    pub fn reap_finished(&self, keep: usize) {
        let mut sessions = self.sessions.lock();
        let mut finished: Vec<(SessionId, u64)> = sessions
            .iter()
            .filter(|(_, s)| s.outcome.lock().is_some())
            .map(|(id, s)| (id.clone(), s.started_ms))
            .collect();
        if finished.len() <= keep {
            return;
        }
        // Oldest first, drop all but the newest `keep`.
        finished.sort_by_key(|(_, started)| *started);
        let drop_count = finished.len() - keep;
        for (id, _) in finished.into_iter().take(drop_count) {
            sessions.remove(&id);
        }
    }

    /// Terminate every running session's group, with one grace period for all.
    ///
    /// For app shutdown, and it is not optional housekeeping. `portable-pty` calls
    /// `setsid()` in the child, so every session is its own session *and* group
    /// leader and does not die with its parent. Letting the process exit closes the
    /// master, which hangs the tty up and sends `SIGHUP` to the session's
    /// *foreground* group only — a job that job control has already moved into a
    /// group of its own never sees it. Without this, quitting leaves a login shell
    /// per worktree running with nothing attached to it.
    ///
    /// # The ceiling, stated rather than implied
    ///
    /// `killpg` reaches the group led by the session leader. An interactive shell
    /// with a controlling tty enables job control, so a backgrounded `npm run dev`
    /// gets its *own* group and survives — zsh HUPs its jobs on exit by default and
    /// bash does not — and a profile that `exec`s tmux escapes entirely, because the
    /// server daemonises. POSIX has no kill-a-session call, so this is the strongest
    /// primitive available, and saying so is better than implying it is total.
    ///
    /// Returns how many groups were signalled, for the shutdown log.
    pub fn kill_all(&self) -> usize {
        let pids = self.take_running_pids();
        signal::terminate_groups(&pids);
        pids.len()
    }

    /// Mark running sessions killed and return their pids, without signalling.
    ///
    /// The caller batches those pids into one [`signal::terminate_groups`] so teardown
    /// pays a single grace period instead of one per session.
    pub fn take_running_pids(&self) -> Vec<u32> {
        self.take_pids_matching(|_| true)
    }

    /// Same as [`Self::take_running_pids`], but only for the named sessions.
    pub fn take_pids(&self, ids: &[SessionId]) -> Vec<u32> {
        self.take_pids_matching(|id| ids.iter().any(|wanted| wanted == id))
    }

    fn take_pids_matching(&self, wanted: impl Fn(&SessionId) -> bool) -> Vec<u32> {
        let mut pids = Vec::new();
        let sessions = self.sessions.lock();
        for (id, session) in sessions.iter() {
            if !wanted(id) {
                continue;
            }
            if session.outcome.lock().is_some() {
                continue;
            }
            // So the recorded outcome names the cause rather than the resulting
            // signal, exactly as `intervene` does.
            let mut intervention = session.intervention.lock();
            if intervention.is_none() {
                intervention.replace(Intervention::Killed);
            }
            drop(intervention);
            if let Some(pid) = session.pid {
                pids.push(pid);
            }
        }
        pids
    }
}

/// Wait for the reader thread without trusting it to publish an outcome forever.
///
/// A descendant can keep the slave side of a pty open even after the child group
/// has been killed. Once escalation has run, the registry must therefore have its
/// own terminal deadline instead of using EOF as an unbounded completion signal.
fn wait_for_outcome(
    outcome: &Mutex<Option<ExitOutcome>>,
    cancel: &CancelToken,
    started_ms: u64,
    timeout_ms: u64,
    clock: &dyn Clock,
    mut intervene: impl FnMut(Intervention) -> Result<(), ExecError>,
    mut pause: impl FnMut(Duration),
) -> Result<ExitOutcome, ExecError> {
    let timeout_deadline_ms = started_ms.saturating_add(timeout_ms);
    let mut final_wait: Option<(Intervention, u64)> = None;

    loop {
        let now_ms = clock.monotonic_ms();
        if let Some(done) = outcome.lock().clone() {
            return Ok(match done {
                ExitOutcome::TimedOut { .. } => ExitOutcome::TimedOut {
                    after_ms: now_ms.saturating_sub(started_ms),
                },
                other => other,
            });
        }

        if let Some((cause, deadline_ms)) = final_wait {
            if now_ms >= deadline_ms {
                let final_outcome = match cause {
                    Intervention::Cancelled => ExitOutcome::Cancelled,
                    Intervention::TimedOut => ExitOutcome::TimedOut {
                        after_ms: now_ms.saturating_sub(started_ms),
                    },
                    Intervention::Killed => ExitOutcome::Signalled { signal: 9 },
                };
                // The reader may never see EOF when a descendant holds the slave open. Marking
                // the registry terminal here makes liveness and reaping agree with the bounded
                // result this call returns; the reader may still publish the same intervention
                // outcome later if that descriptor eventually closes.
                outcome.lock().get_or_insert_with(|| final_outcome.clone());
                return Ok(final_outcome);
            }
        } else {
            let cause = if cancel.is_cancelled() {
                Some(Intervention::Cancelled)
            } else if now_ms >= timeout_deadline_ms {
                Some(Intervention::TimedOut)
            } else {
                None
            };

            if let Some(cause) = cause {
                intervene(cause)?;
                // Start this deadline after escalation returns because the signal
                // adapter deliberately spends one grace period between TERM and KILL.
                let deadline_ms = clock.monotonic_ms().saturating_add(FINAL_OUTCOME_WAIT_MS);
                final_wait = Some((cause, deadline_ms));
            }
        }

        pause(POLL_INTERVAL);
    }
}

impl PtyHost for PtyHostImpl {
    fn spawn(
        &self,
        inv: &Invocation,
        rows: u16,
        cols: u16,
        worktree: Option<&str>,
        sink: Arc<dyn PtySink>,
    ) -> Result<Spawned, ExecError> {
        let program = inv.argv.first().ok_or_else(|| ExecError::Spawn {
            argv: String::new(),
            message: "empty argv".to_owned(),
        })?;

        let resolved =
            self.path
                .which(program, &inv.cwd)
                .ok_or_else(|| ExecError::ProgramNotFound {
                    program: program.clone(),
                    searched: self.path.value.clone(),
                })?;

        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| ExecError::Spawn {
                argv: inv.display(),
                message: format!("openpty: {e}"),
            })?;

        let mut cmd = CommandBuilder::new(&resolved);
        for arg in &inv.argv[1..] {
            cmd.arg(arg);
        }
        cmd.cwd(&inv.cwd);
        cmd.env_clear();
        let mut env = self.path.child_env();
        env.extend(inv.env.clone());
        // Without a TERM the child may refuse colour or, worse, fall back to a
        // dumb mode that redraws badly in xterm.js.
        env.entry("TERM".to_owned())
            .or_insert_with(|| "xterm-256color".to_owned());
        for (key, value) in &env {
            cmd.env(key, value);
        }

        let mut child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| ExecError::Spawn {
                argv: inv.display(),
                message: e.to_string(),
            })?;
        let pid = child.process_id();

        // Drop the slave now. Holding it keeps the pty open after the child exits,
        // so the reader would never see EOF and the session would never finish.
        drop(pair.slave);

        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| ExecError::Spawn {
                argv: inv.display(),
                message: format!("clone reader: {e}"),
            })?;
        let writer = pair.master.take_writer().map_err(|e| ExecError::Spawn {
            argv: inv.display(),
            message: format!("take writer: {e}"),
        })?;

        let id = SessionId::new(uuid::Uuid::new_v4().to_string());
        let outcome = Arc::new(Mutex::new(None));
        let intervention = Arc::new(Mutex::new(None));

        {
            let id = id.clone();
            let outcome = Arc::clone(&outcome);
            let intervention = Arc::clone(&intervention);
            let argv = inv.display();
            std::thread::Builder::new()
                .name(format!("pty-{}", id.as_str()))
                .spawn(move || {
                    let mut buf = [0_u8; READ_CHUNK];
                    loop {
                        match reader.read(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => sink.on_output(&id, &buf[..n]),
                        }
                    }

                    // EOF means the pty closed, so the child is gone or going.
                    let status = child.wait();
                    let final_outcome = match *intervention.lock() {
                        Some(Intervention::TimedOut) => ExitOutcome::TimedOut { after_ms: 0 },
                        Some(Intervention::Cancelled) => ExitOutcome::Cancelled,
                        Some(Intervention::Killed) => ExitOutcome::Signalled { signal: 9 },
                        None => match status {
                            Ok(status) if status.success() => ExitOutcome::Success,
                            Ok(status) => ExitOutcome::Failed {
                                code: i32::try_from(status.exit_code()).unwrap_or(-1),
                            },
                            Err(e) => {
                                tracing::warn!(argv = %argv, error = %e, "pty child wait failed");
                                ExitOutcome::Failed { code: -1 }
                            }
                        },
                    };

                    outcome.lock().replace(final_outcome.clone());
                    sink.on_exit(&id, &final_outcome);
                })
                .map_err(|e| {
                    // The child exists; failing to start the reader would otherwise leak it.
                    if let Some(pid) = pid {
                        signal::terminate_group(pid);
                    }
                    ExecError::Spawn {
                        argv: inv.display(),
                        message: format!("reader thread: {e}"),
                    }
                })?;
        }

        tracing::debug!(session = %id, argv = %inv.display(), cwd = %inv.cwd.display(), "pty spawned");

        self.sessions.lock().insert(
            id.clone(),
            Session {
                argv: inv.argv.clone(),
                worktree: worktree.map(str::to_owned),
                pid,
                timeout_ms: inv.timeout_ms,
                started_ms: self.clock.monotonic_ms(),
                writer: Arc::new(Mutex::new(writer)),
                master: Arc::new(Mutex::new(pair.master)),
                outcome,
                intervention,
            },
        );

        Ok(Spawned {
            session: id,
            argv: inv.argv.clone(),
        })
    }

    fn wait(&self, session: &SessionId, cancel: &CancelToken) -> Result<ExitOutcome, ExecError> {
        let (outcome, started_ms, timeout_ms) = self.with_session(session, |s| {
            (Arc::clone(&s.outcome), s.started_ms, s.timeout_ms)
        })?;

        wait_for_outcome(
            &outcome,
            cancel,
            started_ms,
            timeout_ms,
            self.clock.as_ref(),
            |why| {
                match why {
                    Intervention::Cancelled => {
                        tracing::info!(session = %session, "cancelled; terminating group");
                    }
                    Intervention::TimedOut => {
                        tracing::warn!(session = %session, timeout_ms, "timed out; terminating group");
                    }
                    Intervention::Killed => {}
                }
                self.intervene(session, why)
            },
            std::thread::sleep,
        )
    }

    fn write(&self, session: &SessionId, data: &[u8]) -> Result<(), ExecError> {
        let writer = self.with_session(session, |s| Arc::clone(&s.writer))?;
        let mut writer = writer.lock();
        writer
            .write_all(data)
            .and_then(|()| writer.flush())
            .map_err(|e| ExecError::Spawn {
                argv: format!("session {session}"),
                message: format!("write: {e}"),
            })
    }

    fn resize(&self, session: &SessionId, rows: u16, cols: u16) -> Result<(), ExecError> {
        let master = self.with_session(session, |s| Arc::clone(&s.master))?;
        master
            .lock()
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| ExecError::Spawn {
                argv: format!("session {session}"),
                message: format!("resize: {e}"),
            })
    }

    fn kill(&self, session: &SessionId) -> Result<(), ExecError> {
        self.intervene(session, Intervention::Killed)
    }

    fn sessions(&self) -> Vec<PtySession> {
        self.sessions
            .lock()
            .iter()
            .filter(|(_, s)| s.outcome.lock().is_none())
            .map(|(id, s)| PtySession {
                session: id.clone(),
                argv: s.argv.clone(),
                worktree: s.worktree.clone(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
    use std::time::Instant;

    use super::*;

    /// Collects everything a session emits.
    #[derive(Default)]
    struct Recorder {
        output: Mutex<Vec<u8>>,
        exit: Mutex<Option<ExitOutcome>>,
        exit_calls: std::sync::atomic::AtomicUsize,
    }

    impl Recorder {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.output.lock()).into_owned()
        }
    }

    impl PtySink for Recorder {
        fn on_output(&self, _session: &SessionId, chunk: &[u8]) {
            self.output.lock().extend_from_slice(chunk);
        }
        fn on_exit(&self, _session: &SessionId, outcome: &ExitOutcome) {
            self.exit.lock().replace(outcome.clone());
            self.exit_calls.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[derive(Default)]
    struct AdvancingClock {
        monotonic_ms: AtomicU64,
    }

    impl AdvancingClock {
        fn advance(&self, duration: Duration) {
            self.monotonic_ms.fetch_add(
                u64::try_from(duration.as_millis()).unwrap(),
                Ordering::SeqCst,
            );
        }
    }

    impl Clock for AdvancingClock {
        fn now_unix_ms(&self) -> u64 {
            0
        }

        fn today(&self) -> String {
            "2026-08-26".to_owned()
        }

        fn now_iso(&self) -> String {
            "2026-08-26T00:00:00Z".to_owned()
        }

        fn monotonic_ms(&self) -> u64 {
            self.monotonic_ms.load(Ordering::SeqCst)
        }
    }

    fn host() -> PtyHostImpl {
        PtyHostImpl::new(ResolvedPath::resolve(None))
    }

    fn inv(argv: &[&str], timeout_ms: u64) -> Invocation {
        Invocation::new(
            argv.iter().map(|s| (*s).to_owned()).collect(),
            std::env::temp_dir(),
            timeout_ms,
        )
    }

    #[test]
    fn runs_a_command_and_streams_its_output() {
        let h = host();
        let rec = Arc::new(Recorder::default());
        let spawned = h
            .spawn(
                &inv(&["echo", "hello-pty"], 10_000),
                24,
                80,
                None,
                Arc::clone(&rec) as Arc<dyn PtySink>,
            )
            .unwrap();

        let outcome = h.wait(&spawned.session, &CancelToken::new()).unwrap();
        assert_eq!(outcome, ExitOutcome::Success);
        assert!(rec.text().contains("hello-pty"), "got {:?}", rec.text());
        assert_eq!(
            rec.exit_calls.load(Ordering::SeqCst),
            1,
            "on_exit must fire exactly once"
        );
    }

    #[test]
    fn a_failing_command_reports_its_code() {
        let h = host();
        let rec = Arc::new(Recorder::default());
        let spawned = h
            .spawn(
                &inv(&["sh", "-c", "exit 7"], 10_000),
                24,
                80,
                None,
                rec as Arc<dyn PtySink>,
            )
            .unwrap();
        assert_eq!(
            h.wait(&spawned.session, &CancelToken::new()).unwrap(),
            ExitOutcome::Failed { code: 7 }
        );
    }

    #[test]
    fn the_child_sees_a_tty_which_is_the_whole_point() {
        // A captured pipe would make this print "no". Progress bars, colour and
        // interactive prompts all hinge on it.
        let h = host();
        let rec = Arc::new(Recorder::default());
        let spawned = h
            .spawn(
                &inv(
                    &[
                        "sh",
                        "-c",
                        "if [ -t 0 ]; then echo yes-tty; else echo no-tty; fi",
                    ],
                    10_000,
                ),
                24,
                80,
                None,
                Arc::clone(&rec) as Arc<dyn PtySink>,
            )
            .unwrap();
        h.wait(&spawned.session, &CancelToken::new()).unwrap();
        assert!(rec.text().contains("yes-tty"), "got {:?}", rec.text());
    }

    #[test]
    fn input_is_forwarded_so_a_prompt_can_be_answered() {
        // The reason PTY sessions exist: a script that prompts is answerable
        // instead of fatal.
        let h = host();
        let rec = Arc::new(Recorder::default());
        let spawned = h
            .spawn(
                &inv(
                    &[
                        "sh",
                        "-c",
                        "printf 'Delete branch? [y/n] '; read -r a; echo \"answer=$a\"",
                    ],
                    10_000,
                ),
                24,
                80,
                None,
                Arc::clone(&rec) as Arc<dyn PtySink>,
            )
            .unwrap();

        // Wait for the prompt before answering.
        let deadline = Instant::now() + Duration::from_secs(5);
        while !rec.text().contains("Delete branch?") && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        h.write(&spawned.session, b"y\n").unwrap();

        let outcome = h.wait(&spawned.session, &CancelToken::new()).unwrap();
        assert_eq!(outcome, ExitOutcome::Success);
        assert!(rec.text().contains("answer=y"), "got {:?}", rec.text());
    }

    #[test]
    fn a_hanging_session_times_out_and_reports_elapsed_time() {
        let h = host();
        let rec = Arc::new(Recorder::default());
        let spawned = h
            .spawn(
                &inv(&["sleep", "30"], 400),
                24,
                80,
                None,
                rec as Arc<dyn PtySink>,
            )
            .unwrap();
        let started = Instant::now();
        let outcome = h.wait(&spawned.session, &CancelToken::new()).unwrap();
        match outcome {
            ExitOutcome::TimedOut { after_ms } => assert!(after_ms >= 400, "after_ms={after_ms}"),
            other => panic!("expected TimedOut, got {other:?}"),
        }
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn wait_returns_after_one_intervention_when_the_reader_never_records_an_outcome() {
        let clock = AdvancingClock::default();
        let outcome = Mutex::new(None);
        let interventions = AtomicUsize::new(0);

        let result = wait_for_outcome(
            &outcome,
            &CancelToken::new(),
            0,
            100,
            &clock,
            |cause| {
                assert_eq!(cause, Intervention::TimedOut);
                interventions.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
            |duration| clock.advance(duration),
        )
        .unwrap();

        assert!(matches!(result, ExitOutcome::TimedOut { .. }));
        assert_eq!(interventions.load(Ordering::SeqCst), 1);
        assert!(
            outcome.lock().is_some(),
            "the registry must become terminal too"
        );
        assert!(
            clock.monotonic_ms() <= 100 + FINAL_OUTCOME_WAIT_MS + 25,
            "wait exceeded its final deadline"
        );
    }

    /// An interactive prompt loop that ignores EOF is the exact hazard a GUI hits
    /// when it drives someone else's script. Under a pty there is no EOF at all, so
    /// only the deadline saves us.
    #[test]
    fn an_endless_prompt_loop_is_terminated_by_the_deadline() {
        let h = host();
        let rec = Arc::new(Recorder::default());
        let spawned = h
            .spawn(
                &inv(
                    &[
                        "sh",
                        "-c",
                        "while true; do read -r x; echo 'Please enter y or n.'; done",
                    ],
                    500,
                ),
                24,
                80,
                None,
                rec as Arc<dyn PtySink>,
            )
            .unwrap();
        assert!(matches!(
            h.wait(&spawned.session, &CancelToken::new()).unwrap(),
            ExitOutcome::TimedOut { .. }
        ));
    }

    #[test]
    fn cancellation_reports_cancelled_not_timed_out() {
        let h = host();
        let rec = Arc::new(Recorder::default());
        let spawned = h
            .spawn(
                &inv(&["sleep", "30"], 60_000),
                24,
                80,
                None,
                rec as Arc<dyn PtySink>,
            )
            .unwrap();

        let cancel = CancelToken::new();
        let c = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            c.cancel();
        });

        assert_eq!(
            h.wait(&spawned.session, &cancel).unwrap(),
            ExitOutcome::Cancelled
        );
    }

    #[test]
    fn sessions_are_tracked_per_worktree_while_running_and_gone_after() {
        let h = host();
        let rec = Arc::new(Recorder::default());
        let spawned = h
            .spawn(
                &inv(&["sleep", "10"], 60_000),
                24,
                80,
                Some("/x/wt-a"),
                rec as Arc<dyn PtySink>,
            )
            .unwrap();

        // Used to enforce one-setup-per-worktree.
        assert!(h.has_session_for("/x/wt-a"));
        assert!(!h.has_session_for("/x/wt-b"));
        assert_eq!(h.sessions().len(), 1);

        h.kill(&spawned.session).unwrap();
        h.wait(&spawned.session, &CancelToken::new()).unwrap();
        assert!(
            !h.has_session_for("/x/wt-a"),
            "a finished session must not block a retry"
        );
    }

    #[test]
    fn resize_is_accepted_while_running() {
        let h = host();
        let rec = Arc::new(Recorder::default());
        let spawned = h
            .spawn(
                &inv(&["sleep", "5"], 30_000),
                24,
                80,
                None,
                rec as Arc<dyn PtySink>,
            )
            .unwrap();
        h.resize(&spawned.session, 40, 120).unwrap();
        h.kill(&spawned.session).unwrap();
        h.wait(&spawned.session, &CancelToken::new()).unwrap();
    }

    #[test]
    fn operations_on_an_unknown_session_are_errors_not_panics() {
        let h = host();
        let ghost = SessionId::new("does-not-exist");
        assert!(matches!(
            h.write(&ghost, b"x"),
            Err(ExecError::NoSuchSession(_))
        ));
        assert!(matches!(
            h.resize(&ghost, 1, 1),
            Err(ExecError::NoSuchSession(_))
        ));
        assert!(matches!(h.kill(&ghost), Err(ExecError::NoSuchSession(_))));
        assert!(matches!(
            h.wait(&ghost, &CancelToken::new()),
            Err(ExecError::NoSuchSession(_))
        ));
    }

    #[test]
    fn a_missing_program_is_reported_before_a_session_is_created() {
        let h = host();
        let rec = Arc::new(Recorder::default());
        let err = h
            .spawn(
                &inv(&["definitely-not-real-xyz"], 1_000),
                24,
                80,
                None,
                rec as Arc<dyn PtySink>,
            )
            .unwrap_err();
        assert!(
            matches!(err, ExecError::ProgramNotFound { .. }),
            "got {err:?}"
        );
        assert!(
            h.sessions().is_empty(),
            "a failed spawn must not leak a session"
        );
    }

    #[test]
    fn concurrent_sessions_do_not_block_each_other() {
        // Regression guard for the deadlock this design avoids: if `wait` held the
        // registry lock, the second session could never be observed.
        let h = Arc::new(host());
        let ready = Arc::new(AtomicBool::new(false));

        let rec_a = Arc::new(Recorder::default());
        let a = h
            .spawn(
                &inv(&["sleep", "3"], 30_000),
                24,
                80,
                Some("/x/a"),
                rec_a as Arc<dyn PtySink>,
            )
            .unwrap();

        let h2 = Arc::clone(&h);
        let ready2 = Arc::clone(&ready);
        let waiter = std::thread::spawn(move || {
            ready2.store(true, Ordering::SeqCst);
            h2.wait(&a.session, &CancelToken::new()).unwrap()
        });

        while !ready.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(100));

        let rec_b = Arc::new(Recorder::default());
        let b = h
            .spawn(
                &inv(&["echo", "second"], 10_000),
                24,
                80,
                Some("/x/b"),
                Arc::clone(&rec_b) as Arc<dyn PtySink>,
            )
            .unwrap();
        assert_eq!(
            h.wait(&b.session, &CancelToken::new()).unwrap(),
            ExitOutcome::Success
        );
        assert!(rec_b.text().contains("second"));

        waiter.join().unwrap();
    }

    #[test]
    fn reaping_keeps_the_most_recent_transcripts() {
        let h = host();
        for _ in 0..4 {
            let rec = Arc::new(Recorder::default());
            let s = h
                .spawn(
                    &inv(&["true"], 10_000),
                    24,
                    80,
                    None,
                    rec as Arc<dyn PtySink>,
                )
                .unwrap();
            h.wait(&s.session, &CancelToken::new()).unwrap();
            // Distinct `started` instants so the ordering is well defined.
            std::thread::sleep(Duration::from_millis(5));
        }
        h.reap_finished(2);
        assert_eq!(h.sessions.lock().len(), 2);
    }

    /// Reaping must not be able to touch a session that is still running.
    ///
    /// [`PtyHostImpl::reap_finished`] filters on `outcome.is_some()`, and the
    /// terminal dock now calls it on every open — so a filter inverted by a later
    /// edit would silently forget live shells, and the symptom would be a terminal
    /// whose keystrokes stop working rather than anything that looks like reaping.
    #[test]
    fn a_reaped_session_is_forgotten_but_the_running_ones_are_not() {
        let h = host();
        let mut running = Vec::new();
        for _ in 0..2 {
            let rec = Arc::new(Recorder::default());
            running.push(
                h.spawn(
                    &inv(&["sleep", "30"], 60_000),
                    24,
                    80,
                    None,
                    rec as Arc<dyn PtySink>,
                )
                .unwrap()
                .session,
            );
        }
        for _ in 0..4 {
            let rec = Arc::new(Recorder::default());
            let s = h
                .spawn(
                    &inv(&["true"], 10_000),
                    24,
                    80,
                    None,
                    rec as Arc<dyn PtySink>,
                )
                .unwrap();
            h.wait(&s.session, &CancelToken::new()).unwrap();
            std::thread::sleep(Duration::from_millis(5));
        }

        h.reap_finished(2);

        assert_eq!(
            h.sessions.lock().len(),
            4,
            "two running plus the two newest finished"
        );
        let live = h.sessions();
        assert_eq!(live.len(), 2, "both running sessions must survive a reap");
        for session in &running {
            assert!(live.iter().any(|s| &s.session == session));
        }

        h.kill_all();
    }

    /// **The test that protects the shutdown hook.**
    ///
    /// `portable-pty` calls `setsid()`, so a session outlives its parent and quitting
    /// leaks a login shell per worktree. The leak is silent and cumulative — you find
    /// it weeks later in `ps` — so it needs a test rather than a code review.
    #[test]
    fn killing_every_session_at_once_leaves_none_running() {
        let h = host();
        let mut sessions = Vec::new();
        for _ in 0..3 {
            let rec = Arc::new(Recorder::default());
            sessions.push(
                h.spawn(
                    &inv(&["sleep", "30"], 60_000),
                    24,
                    80,
                    None,
                    rec as Arc<dyn PtySink>,
                )
                .unwrap()
                .session,
            );
        }
        assert_eq!(h.sessions().len(), 3);

        assert_eq!(h.kill_all(), 3, "every running session should be signalled");

        for session in &sessions {
            let outcome = h.wait(session, &CancelToken::new()).unwrap();
            assert!(
                matches!(outcome, ExitOutcome::Signalled { .. }),
                "expected a signalled outcome, got {outcome:?}"
            );
        }
        assert!(
            h.sessions().is_empty(),
            "nothing may still be reported as running after kill_all"
        );

        // Idempotent, which is what lets the quit hook run on more than one route
        // without anyone having to reason about which fired first.
        assert_eq!(h.kill_all(), 0);
    }

    /// The leak this feature had to fix before it could open shells all day.
    ///
    /// A finished `Session` holds a `Box<dyn MasterPty>` and its writer — roughly two
    /// descriptors — for the lifetime of the process, and until the terminal dock
    /// existed nothing ever called [`PtyHostImpl::reap_finished`]. This counts real
    /// descriptors rather than registry entries, because the registry shrinking is
    /// the change and the descriptors being released is the *property*: a future
    /// `Session` that stashed a clone somewhere else would keep the entry count
    /// honest and the leak intact.
    ///
    /// `/dev/fd` exists on macOS and Linux. The slack covers the reaped-but-kept
    /// sessions plus whatever the test harness opens while this runs; the leak it
    /// catches is twenty sessions' worth, which is an order of magnitude larger.
    #[test]
    fn opening_and_closing_many_sessions_does_not_accumulate_file_descriptors() {
        fn open_descriptors() -> usize {
            std::fs::read_dir("/dev/fd").map_or(0, Iterator::count)
        }

        let h = host();
        let before = open_descriptors();
        assert!(before > 0, "/dev/fd is unreadable, so this proves nothing");

        for _ in 0..20 {
            let rec = Arc::new(Recorder::default());
            let s = h
                .spawn(
                    &inv(&["true"], 10_000),
                    24,
                    80,
                    None,
                    rec as Arc<dyn PtySink>,
                )
                .unwrap();
            h.wait(&s.session, &CancelToken::new()).unwrap();
            h.reap_finished(4);
        }

        let after = open_descriptors();
        assert!(
            after <= before + 16,
            "twenty sessions took the descriptor count from {before} to {after}; \
             finished sessions are holding their pty master open"
        );
    }
}
