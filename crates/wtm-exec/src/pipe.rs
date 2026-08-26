//! Line-protocol sessions over a child's pipes.
//!
//! # Shape of the implementation
//!
//! **Two OS threads per session**, where [`crate::pty`] needs one. The extra one is stderr:
//! both agent CLIs write diagnostics there, and a single-threaded read of stdout would
//! deadlock the moment the child filled the stderr pipe buffer — the same trap
//! [`crate::runner`] spawns two drain threads to avoid. The stdout thread doubles as the
//! reaper, so this is two rather than three: it reads to EOF, joins stderr, waits for the
//! child, records the outcome and calls [`PipeSink::on_exit`] exactly once.
//!
//! That ordering is the point. `on_exit` after both readers have finished means the UI can
//! never be told a session ended while lines from it are still in flight.
//!
//! # Lines are reassembled here, and nowhere else
//!
//! `read_until(b'\n')` across reads, because a JSON frame is routinely larger than a single
//! `read` returns and a partial line is not a frame. Decoded with `from_utf8_lossy` rather
//! than `read_line`'s strict decode: one malformed byte in a diagnostic should not end a
//! session that is otherwise working.
//!
//! # Kills go to the process group
//!
//! `Command::process_group(0)` at spawn — the safe `CommandExt` call, not `pre_exec`, because
//! `unsafe_code` is forbidden workspace-wide. Termination then goes through
//! [`crate::signal::terminate_group`], because an agent CLI's tree is deep: it spawns the
//! shells and MCP servers it was configured with, and signalling only the direct child leaves
//! every one of them running.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Instant;

use parking_lot::Mutex;
use wtm_core::error::ExecError;
use wtm_core::model::{ExitOutcome, SessionId};
use wtm_core::ports::exec::Invocation;
use wtm_core::ports::pipe::{PipeHost, PipeSession, PipeSink};
use wtm_core::ports::pty::Spawned;

use crate::clock::instant_now;
use crate::path::ResolvedPath;
use crate::signal;

/// Read buffer for the line reader.
///
/// Only the size of one `read` syscall, not a ceiling on a line: [`read_until`] keeps calling
/// until it finds a newline. Larger than [`crate::pty`]'s 8 KiB because there is no
/// interactive prompt to reveal promptly here — a frame is delivered when it is complete, so
/// fewer, bigger reads are strictly better.
///
/// [`read_until`]: std::io::BufRead::read_until
const READ_CAPACITY: usize = 64 * 1024;

/// The largest single line this will accumulate before giving up on the session.
///
/// A cap is unavoidable — a child emitting no newline would otherwise grow this buffer until
/// the app is killed by the OS — but the number is deliberately far above any real frame so it
/// is never reached in normal use. 64 MiB is roughly a thousand times the largest turn either
/// CLI has been observed to emit.
///
/// **Exceeding it ends the session with a stated reason rather than truncating.** Truncation is
/// exactly the failure the [`PipeHost`] port docs exist to avoid: a silently shortened JSON
/// line is invalid JSON, and the symptom is an agent that inexplicably ignores long messages.
/// A dead session with "a single line exceeded 64 MiB" in its transcript is diagnosable; a
/// quietly corrupted one is not.
const MAX_LINE_BYTES: usize = 64 * 1024 * 1024;

/// Why a session ended, when we ended it deliberately.
#[derive(Debug, Clone, Copy)]
enum Intervention {
    Killed,
    /// A line grew past [`MAX_LINE_BYTES`]. Its own variant so the reported outcome does not
    /// look like a user-requested kill.
    LineTooLong,
}

struct Session {
    argv: Vec<String>,
    worktree: Option<String>,
    /// `None` only if the child exited before we could ask; without it we cannot signal the
    /// group.
    pid: Option<u32>,
    started: Instant,
    /// `Option` so [`PipeHost::close_stdin`] can drop the handle — closing the pipe — while
    /// leaving the session in the registry to be reaped normally.
    stdin: Arc<Mutex<Option<std::process::ChildStdin>>>,
    /// Set once by the reader thread when the child is reaped.
    outcome: Arc<Mutex<Option<ExitOutcome>>>,
    intervention: Arc<Mutex<Option<Intervention>>>,
}

/// Pipe-backed [`PipeHost`].
pub struct PipeHostImpl {
    path: ResolvedPath,
    sessions: Mutex<HashMap<SessionId, Session>>,
}

impl std::fmt::Debug for PipeHostImpl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PipeHostImpl")
            .field("sessions", &self.sessions.lock().len())
            .finish_non_exhaustive()
    }
}

impl PipeHostImpl {
    #[must_use]
    pub fn new(path: ResolvedPath) -> Self {
        Self {
            path,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    /// The `PATH` this host resolves programs against.
    ///
    /// Mirrors [`crate::runner::Runner::path`], and exists so `adapters()`'s one-resolved-path
    /// invariant can be *asserted* for this adapter rather than assumed. `PtyHostImpl` has no
    /// such accessor, so that third of the invariant is still only assumed.
    #[must_use]
    pub fn path(&self) -> &ResolvedPath {
        &self.path
    }

    /// Look up a session's shared handles without holding the registry lock for longer than
    /// the lookup itself. Same discipline as [`crate::pty::PtyHostImpl`], and for the same
    /// reason: nothing blocking may run under this lock.
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

    /// Forget finished sessions, keeping the most recent ones.
    ///
    /// The same descriptor-hygiene problem [`crate::pty::PtyHostImpl::reap_finished`] exists
    /// for, at a smaller scale: a finished entry here holds one `ChildStdin` rather than a pty
    /// master and its writer. One descriptor apiece is still a leak once something opens and
    /// closes sessions all day.
    pub fn reap_finished(&self, keep: usize) {
        let mut sessions = self.sessions.lock();
        let mut finished: Vec<(SessionId, Instant)> = sessions
            .iter()
            .filter(|(_, s)| s.outcome.lock().is_some())
            .map(|(id, s)| (id.clone(), s.started))
            .collect();
        if finished.len() <= keep {
            return;
        }
        finished.sort_by_key(|(_, started)| *started);
        let drop_count = finished.len() - keep;
        for (id, _) in finished.into_iter().take(drop_count) {
            sessions.remove(&id);
        }
    }

    /// Terminate every running session's group, with one grace period for all.
    ///
    /// For app shutdown. A piped child does not get a `SIGHUP` from a closing tty the way a pty
    /// session does — there is no tty — so without this an agent CLI and everything it started
    /// survives the app that spawned it, holding a model connection open. Returns how many
    /// groups were signalled, for the shutdown log.
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
}

/// Forward every complete line from `reader` to `deliver`, until EOF.
///
/// Returns `Err` only when a line grew past `max_bytes`, which the caller turns into an
/// intervention. An I/O error is EOF as far as this is concerned: the child is gone and the
/// exit path is what reports why.
///
/// The cap is applied *while* reading, via [`Read::take`]. Checking after `read_until` has
/// already grown the buffer would let a child with no newline allocate until the process is
/// killed.
fn pump(reader: impl std::io::Read, mut deliver: impl FnMut(&str)) -> Result<(), ()> {
    pump_limited(reader, MAX_LINE_BYTES, &mut deliver)
}

fn pump_limited(
    reader: impl std::io::Read,
    max_bytes: usize,
    deliver: &mut impl FnMut(&str),
) -> Result<(), ()> {
    let mut buffered = BufReader::with_capacity(READ_CAPACITY, reader);
    let mut line = Vec::new();

    loop {
        line.clear();
        let mut limited =
            (&mut buffered).take(u64::try_from(max_bytes.saturating_add(1)).unwrap_or(u64::MAX));
        match limited.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => return Ok(()),
            Ok(_) => {}
        }

        if line.len() > max_bytes {
            return Err(());
        }

        // Trailing `\n`, and the `\r` before it if the child wrote CRLF. Stripped here so a
        // sink never has to know how the frame was terminated.
        while matches!(line.last(), Some(b'\n' | b'\r')) {
            line.pop();
        }

        // Lossy rather than strict: one malformed byte in a diagnostic must not end a session
        // that is otherwise working. A malformed byte inside a JSON frame becomes a parse
        // error one layer up, which is where it can be reported against the frame.
        deliver(&String::from_utf8_lossy(&line));
    }
}

impl PipeHost for PipeHostImpl {
    fn spawn(
        &self,
        inv: &Invocation,
        worktree: Option<&str>,
        sink: Arc<dyn PipeSink>,
    ) -> Result<Spawned, ExecError> {
        let program = inv.argv.first().ok_or_else(|| ExecError::Spawn {
            argv: String::new(),
            message: "empty argv".to_owned(),
        })?;

        // Resolved before spawning so a missing CLI is a clear diagnosis rather than a bare
        // ENOENT — the app's most likely production failure is a GUI launch that cannot see
        // the user's PATH.
        let resolved =
            self.path
                .which(program, &inv.cwd)
                .ok_or_else(|| ExecError::ProgramNotFound {
                    program: program.clone(),
                    searched: self.path.value.clone(),
                })?;

        let mut env = self.path.child_env();
        env.extend(inv.env.clone());

        // A sanctioned spawn site. See this crate's module docs.
        #[allow(clippy::disallowed_methods)]
        let mut cmd = Command::new(&resolved);
        cmd.args(&inv.argv[1..])
            .current_dir(&inv.cwd)
            .env_clear()
            .envs(&env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Deliberately no `TERM`, which is the one env difference from `pty::spawn`. These
        // children must not believe they are on a terminal: both CLIs switch to a human-facing
        // renderer when they think they are, and that output is not the protocol.

        // Its own process group, so the whole tree can be signalled together. The safe
        // `CommandExt` call rather than `pre_exec`, because `unsafe` is forbidden here.
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }

        let mut child = cmd.spawn().map_err(|e| ExecError::Spawn {
            argv: inv.display(),
            message: e.to_string(),
        })?;

        let pid = child.id();
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();

        let id = SessionId::new(uuid::Uuid::new_v4().to_string());
        let outcome = Arc::new(Mutex::new(None));
        let intervention = Arc::new(Mutex::new(None));

        // stderr on its own thread. Not merged into stdout: a diagnostic interleaved into the
        // JSON stream turns a useful message into a parse error.
        let stderr_thread = match stderr {
            Some(handle) => {
                let id = id.clone();
                let sink = Arc::clone(&sink);
                match std::thread::Builder::new()
                    .name(format!("pipe-err-{}", id.as_str()))
                    .spawn(move || {
                        // A too-long line on stderr is not worth ending a session over, so the
                        // result is ignored here where stdout's is not.
                        let _ = pump(handle, |line| sink.on_stderr(&id, line));
                    }) {
                    Ok(join) => Some(join),
                    Err(e) => {
                        // The child is running and stdout is still open. A missing stderr
                        // reader would deadlock the moment the child filled that pipe.
                        signal::terminate_group(pid);
                        return Err(ExecError::Spawn {
                            argv: inv.display(),
                            message: format!("could not start the stderr thread: {e}"),
                        });
                    }
                }
            }
            None => None,
        };

        {
            let id = id.clone();
            let outcome = Arc::clone(&outcome);
            let intervention = Arc::clone(&intervention);
            let argv = inv.display();

            std::thread::Builder::new()
                .name(format!("pipe-{}", id.as_str()))
                .spawn(move || {
                    let overflowed = match stdout {
                        Some(handle) => pump(handle, |line| sink.on_line(&id, line)).is_err(),
                        None => false,
                    };

                    if overflowed {
                        intervention.lock().replace(Intervention::LineTooLong);
                        sink.on_stderr(
                            &id,
                            &format!(
                                "wtm ended this session: a single output line exceeded {} MiB",
                                MAX_LINE_BYTES / 1024 / 1024
                            ),
                        );
                        signal::terminate_group(pid);
                    }

                    // Both readers finished before the exit is reported, so the UI can never be
                    // told a session ended while its output is still arriving. Joined rather
                    // than detached for exactly that ordering.
                    if let Some(handle) = stderr_thread {
                        let _ = handle.join();
                    }

                    let status = child.wait();
                    let final_outcome = match *intervention.lock() {
                        Some(Intervention::Killed) => ExitOutcome::Signalled { signal: 9 },
                        Some(Intervention::LineTooLong) => ExitOutcome::Failed { code: -1 },
                        None => match status {
                            Ok(status) if status.success() => ExitOutcome::Success,
                            Ok(status) => ExitOutcome::Failed {
                                code: status.code().unwrap_or(-1),
                            },
                            Err(e) => {
                                tracing::warn!(argv = %argv, error = %e, "could not reap session");
                                ExitOutcome::Failed { code: -1 }
                            }
                        },
                    };

                    outcome.lock().replace(final_outcome.clone());
                    sink.on_exit(&id, &final_outcome);
                })
                .map_err(|e| {
                    signal::terminate_group(pid);
                    ExecError::Spawn {
                        argv: inv.display(),
                        message: format!("could not start the reader thread: {e}"),
                    }
                })?;
        }

        self.sessions.lock().insert(
            id.clone(),
            Session {
                argv: inv.argv.clone(),
                worktree: worktree.map(str::to_owned),
                pid: Some(pid),
                started: instant_now(),
                stdin: Arc::new(Mutex::new(stdin)),
                outcome,
                intervention,
            },
        );

        Ok(Spawned {
            session: id,
            argv: inv.argv.clone(),
        })
    }

    fn write_line(&self, session: &SessionId, line: &str) -> Result<(), ExecError> {
        let handle = self.with_session(session, |s| Arc::clone(&s.stdin))?;
        let mut guard = handle.lock();
        let stdin = guard
            .as_mut()
            .ok_or_else(|| ExecError::NoSuchSession(session.as_str().to_owned()))?;

        // One write for the frame and its terminator. Two calls would let a concurrent writer
        // interleave between them and produce a frame nobody sent.
        let mut frame = String::with_capacity(line.len() + 1);
        frame.push_str(line);
        frame.push('\n');

        stdin
            .write_all(frame.as_bytes())
            .and_then(|()| stdin.flush())
            .map_err(|e| ExecError::Spawn {
                argv: session.as_str().to_owned(),
                message: format!("could not write to the session: {e}"),
            })
    }

    fn close_stdin(&self, session: &SessionId) -> Result<(), ExecError> {
        let handle = self.with_session(session, |s| Arc::clone(&s.stdin))?;
        // Dropping the handle closes the pipe, which is the EOF the child is waiting for.
        handle.lock().take();
        Ok(())
    }

    fn kill(&self, session: &SessionId) -> Result<(), ExecError> {
        self.intervene(session, Intervention::Killed)
    }

    fn sessions(&self) -> Vec<PipeSession> {
        self.sessions
            .lock()
            .iter()
            .filter(|(_, s)| s.outcome.lock().is_none())
            .map(|(id, s)| PipeSession {
                session: id.clone(),
                argv: s.argv.clone(),
                worktree: s.worktree.clone(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::*;

    /// Collects everything a session emits, keeping the three streams apart.
    #[derive(Default)]
    struct Recorder {
        lines: Mutex<Vec<String>>,
        errors: Mutex<Vec<String>>,
        exit: Mutex<Option<ExitOutcome>>,
        exit_calls: AtomicUsize,
        /// How many lines had arrived by the time `on_exit` fired.
        ///
        /// The ordering guarantee is the one property here that a later refactor could break
        /// without breaking anything else, so it is recorded rather than inferred.
        lines_at_exit: AtomicUsize,
    }

    impl PipeSink for Recorder {
        fn on_line(&self, _session: &SessionId, line: &str) {
            self.lines.lock().push(line.to_owned());
        }
        fn on_stderr(&self, _session: &SessionId, line: &str) {
            self.errors.lock().push(line.to_owned());
        }
        fn on_exit(&self, _session: &SessionId, outcome: &ExitOutcome) {
            self.lines_at_exit
                .store(self.lines.lock().len(), Ordering::SeqCst);
            self.exit.lock().replace(outcome.clone());
            self.exit_calls.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn host() -> PipeHostImpl {
        PipeHostImpl::new(ResolvedPath::resolve(None))
    }

    /// A deadline is passed because [`Invocation`] insists on one, and it is never enforced —
    /// see the port docs. Ten seconds rather than the app's one-week sentinel only so a hung
    /// test is a failure rather than a wait.
    fn inv(argv: &[&str]) -> Invocation {
        Invocation::new(
            argv.iter().map(|s| (*s).to_owned()).collect(),
            std::env::temp_dir(),
            10_000,
        )
    }

    /// Block until `done` or the deadline, then return whether it happened.
    ///
    /// Polling rather than a condvar, matching `PtyHost::wait`: there is nothing to signal from,
    /// because the thing being waited for is a sink call on another thread.
    fn settle(done: impl Fn() -> bool) -> bool {
        for _ in 0..600 {
            if done() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    fn run(argv: &[&str]) -> (PipeHostImpl, Arc<Recorder>, SessionId) {
        let h = host();
        let rec = Arc::new(Recorder::default());
        let spawned = h
            .spawn(&inv(argv), None, Arc::clone(&rec) as Arc<dyn PipeSink>)
            .expect("spawn");
        (h, rec, spawned.session)
    }

    #[test]
    fn a_line_split_across_reads_is_delivered_once_and_whole() {
        // 200 KiB on one line, which is three times `READ_CAPACITY` and therefore cannot arrive
        // in a single read. If framing ever keys off read boundaries instead of newlines, this
        // sees several truncated lines instead of one long one.
        let width = 200 * 1024;
        let (_h, rec, _id) = run(&[
            "sh",
            "-c",
            &format!("printf 'a%.0s' $(seq 1 {width}); printf '\\n'"),
        ]);

        assert!(settle(|| rec.exit.lock().is_some()), "session never ended");
        let lines = rec.lines.lock();
        assert_eq!(lines.len(), 1, "expected one line, got {}", lines.len());
        assert_eq!(
            lines[0].len(),
            width,
            "the line came back a different length"
        );
    }

    #[test]
    fn a_frame_written_to_stdin_survives_being_longer_than_a_terminal_line() {
        // The property the whole port exists for. A pty in canonical mode caps one line at
        // `MAX_CANON` — 1024 bytes on macOS, 4096 on Linux — and *discards* the rest, so this
        // frame would come back short if anyone ever "simplified" this onto `PtyHost`. It is
        // 32 KiB so it clears both caps by a wide margin.
        //
        // The child echoes back the byte count rather than the payload, so a failure reports
        // how much arrived instead of dumping 32 KiB into the test output.
        let width = 32 * 1024;
        let (h, rec, id) = run(&["sh", "-c", "read -r line; printf '%s\\n' \"${#line}\""]);

        h.write_line(&id, &"x".repeat(width)).expect("write");

        assert!(settle(|| !rec.lines.lock().is_empty()), "no reply arrived");
        assert_eq!(rec.lines.lock()[0], width.to_string());
    }

    #[test]
    fn stderr_is_reported_apart_from_the_protocol_stream() {
        // Both CLIs write diagnostics to stderr. Merged into stdout they would turn a useful
        // message into a JSON parse error, so a sink must be able to tell them apart.
        let (_h, rec, _id) = run(&["sh", "-c", "printf 'out\\n'; printf 'err\\n' >&2"]);

        assert!(settle(|| rec.exit.lock().is_some()), "session never ended");
        assert_eq!(*rec.lines.lock(), vec!["out".to_owned()]);
        assert_eq!(*rec.errors.lock(), vec!["err".to_owned()]);
    }

    #[test]
    fn the_exit_is_reported_after_every_line_the_child_wrote() {
        // A session marked ended while its output is still arriving would truncate a transcript
        // at whatever had been flushed. The reader thread joins stderr and reaps only after
        // stdout hits EOF specifically to make this true.
        let (_h, rec, _id) = run(&[
            "sh",
            "-c",
            "for i in 1 2 3 4 5; do printf '%s\\n' \"$i\"; done",
        ]);

        assert!(settle(|| rec.exit.lock().is_some()), "session never ended");
        assert_eq!(rec.lines.lock().len(), 5);
        assert_eq!(
            rec.lines_at_exit.load(Ordering::SeqCst),
            5,
            "on_exit fired while lines were still arriving"
        );
        assert_eq!(
            rec.exit_calls.load(Ordering::SeqCst),
            1,
            "on_exit is once per session"
        );
    }

    #[test]
    fn closing_stdin_lets_the_child_finish_with_a_real_exit_status() {
        // The graceful shutdown for a protocol that ends on EOF. Killing instead would report
        // `Signalled` for a session that exited cleanly, which reads as a crash in the UI.
        let (h, rec, id) = run(&["cat"]);

        h.write_line(&id, "hello").expect("write");
        assert!(settle(|| !rec.lines.lock().is_empty()), "no echo");

        h.close_stdin(&id).expect("close");
        assert!(
            settle(|| rec.exit.lock().is_some()),
            "cat outlived its stdin"
        );
        assert_eq!(*rec.exit.lock(), Some(ExitOutcome::Success));
    }

    #[test]
    fn writing_to_a_session_whose_stdin_is_closed_is_an_error_rather_than_a_silent_no_op() {
        let (h, _rec, id) = run(&["cat"]);
        h.close_stdin(&id).expect("close");
        assert!(
            h.write_line(&id, "too late").is_err(),
            "a write after close must not appear to succeed"
        );
    }

    #[test]
    fn killing_a_session_reaps_the_whole_process_group() {
        // The tree an agent CLI builds is deep — it spawns the shells and MCP servers it was
        // configured with — so signalling the direct child would leave grandchildren running.
        // `sh` here stands in for that: the `sleep` is the grandchild.
        let (h, rec, id) = run(&["sh", "-c", "sleep 30 & printf 'ready\\n'; wait"]);

        assert!(
            settle(|| !rec.lines.lock().is_empty()),
            "child never started"
        );
        h.kill(&id).expect("kill");

        assert!(
            settle(|| rec.exit.lock().is_some()),
            "the group survived the kill"
        );
        // Reported as the intervention rather than as whatever signal happened to land, so a
        // deliberate kill never looks like a crash.
        assert_eq!(*rec.exit.lock(), Some(ExitOutcome::Signalled { signal: 9 }));
    }

    #[test]
    fn a_finished_session_is_not_reported_as_running() {
        let (h, rec, _id) = run(&["true"]);
        assert!(settle(|| rec.exit.lock().is_some()), "session never ended");
        assert!(
            h.sessions().is_empty(),
            "sessions() must report only what is still running"
        );
    }

    #[test]
    fn a_missing_program_is_refused_before_anything_is_spawned() {
        let h = host();
        let rec = Arc::new(Recorder::default());
        let err = h
            .spawn(
                &inv(&["wtm-no-such-program-anywhere"]),
                None,
                Arc::clone(&rec) as Arc<dyn PipeSink>,
            )
            .expect_err("a missing program must not spawn");
        assert!(matches!(err, ExecError::ProgramNotFound { .. }));
        // Nothing was registered, so nothing has to be cleaned up.
        assert!(h.sessions().is_empty());
    }

    #[test]
    fn opening_and_closing_many_sessions_does_not_accumulate_file_descriptors() {
        // Counts real descriptors rather than registry entries, because the registry shrinking
        // is the change and the descriptors being released is the property — the same reasoning
        // `pty.rs` records for its twin. A finished session here holds one `ChildStdin`.
        fn open_descriptors() -> usize {
            std::fs::read_dir("/dev/fd").map_or(0, Iterator::count)
        }

        let h = host();
        let before = open_descriptors();
        assert!(before > 0, "/dev/fd is unreadable, so this proves nothing");

        for _ in 0..20 {
            let rec = Arc::new(Recorder::default());
            let spawned = h
                .spawn(&inv(&["true"]), None, Arc::clone(&rec) as Arc<dyn PipeSink>)
                .expect("spawn");
            assert!(settle(|| rec.exit.lock().is_some()), "session never ended");
            let _ = spawned;
            h.reap_finished(4);
        }

        let after = open_descriptors();
        assert!(
            after <= before + 16,
            "twenty sessions took the descriptor count from {before} to {after}; \
             finished sessions are holding their stdin open"
        );
    }

    #[test]
    fn kill_all_reports_how_many_running_sessions_it_signalled() {
        let h = host();
        let mut recorders = Vec::new();
        for _ in 0..3 {
            let rec = Arc::new(Recorder::default());
            h.spawn(
                &inv(&["sh", "-c", "printf 'ready\\n'; sleep 30"]),
                None,
                Arc::clone(&rec) as Arc<dyn PipeSink>,
            )
            .expect("spawn");
            recorders.push(rec);
        }
        assert!(
            settle(|| recorders.iter().all(|r| !r.lines.lock().is_empty())),
            "not every child started"
        );

        assert_eq!(h.kill_all(), 3);
        for rec in &recorders {
            assert!(
                settle(|| rec.exit.lock().is_some()),
                "a session survived quit"
            );
        }
    }

    #[test]
    fn a_line_without_a_newline_is_refused_before_the_buffer_grows_unbounded() {
        // The cap used to be checked after `read_until` had already allocated the whole
        // line. A child that never writes `\n` would grow until the process was killed.
        let input = vec![b'x'; 32];
        let mut delivered = Vec::new();
        let result = super::pump_limited(input.as_slice(), 16, &mut |line| {
            delivered.push(line.to_owned());
        });
        assert!(result.is_err(), "a line past the cap must end the pump");
        assert!(
            delivered.is_empty(),
            "the oversized line must not be delivered"
        );
    }
}
