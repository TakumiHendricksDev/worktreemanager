//! The captured command runner.
//!
//! # This is the only place a process is spawned
//!
//! `clippy.toml` bans `std::process::Command::new` repo-wide via
//! `disallowed-methods`, so every subprocess in the app funnels through here (and
//! through [`crate::pty`]). That is not bureaucracy — it is what guarantees, for
//! every spawn, that:
//!
//! - there is a deadline,
//! - expiry kills the process **group**, not just the direct child,
//! - the environment is the resolved one (see [`crate::path`]),
//! - stdin is `/dev/null`, so a prompt fails fast instead of blocking,
//! - and there is a `tracing` span with the argv and the duration.
//!
//! # Why stdin is null *and* there is a timeout
//!
//! Both, because either alone is insufficient. Null stdin makes a well-behaved
//! script's `read` fail immediately. But a script whose prompt loop ignores read
//! failures spins forever printing its retry message — the reference project's
//! `confirm()` does exactly this. Only a deadline terminates that, which is why
//! [`Invocation::timeout_ms`] is not an `Option`.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use wtm_core::error::ExecError;
use wtm_core::ports::exec::{CancelToken, CommandRunner, Invocation, Output};

use crate::clock::instant_now;
use crate::path::ResolvedPath;
use crate::signal;

/// How often to check for completion, cancellation and the deadline.
const POLL_INTERVAL: Duration = Duration::from_millis(15);

/// Cap on captured output per stream.
///
/// A runaway script can produce gigabytes (see the `confirm()` loop above, which
/// prints a line per iteration as fast as the CPU allows). Capturing all of it
/// would exhaust memory before the timeout fired.
const MAX_CAPTURE_BYTES: usize = 4 * 1024 * 1024;

/// Captured-output command runner.
#[derive(Debug)]
pub struct Runner {
    path: ResolvedPath,
}

impl Runner {
    #[must_use]
    pub fn new(path: ResolvedPath) -> Self {
        Self { path }
    }

    /// Probe for a usable `PATH` and build a runner.
    #[must_use]
    pub fn with_probed_path(override_value: Option<&str>) -> Self {
        Self::new(ResolvedPath::resolve(override_value))
    }

    #[must_use]
    pub fn path(&self) -> &ResolvedPath {
        &self.path
    }

    fn spawn(&self, inv: &Invocation) -> Result<std::process::Child, ExecError> {
        let program = inv.argv.first().ok_or_else(|| ExecError::Spawn {
            argv: String::new(),
            message: "empty argv".to_owned(),
        })?;

        // Resolve before spawning so a missing tool is a clear diagnosis rather
        // than a bare ENOENT.
        let resolved =
            self.path
                .which(program, &inv.cwd)
                .ok_or_else(|| ExecError::ProgramNotFound {
                    program: program.clone(),
                    searched: self.path.value.clone(),
                })?;

        let mut env = self.path.child_env();
        env.extend(inv.env.clone());

        // The single sanctioned spawn site. See the module docs.
        #[allow(clippy::disallowed_methods)]
        let mut cmd = Command::new(&resolved);
        cmd.args(&inv.argv[1..])
            .current_dir(&inv.cwd)
            .env_clear()
            .envs(&env)
            // Not inherited: a child that reads the app's stdin would consume
            // whatever the parent process had, and under `cargo tauri dev` that is
            // the developer's terminal.
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Put the child in its own process group so the whole tree can be
        // signalled together. Without this, killing `worktree.sh` leaves its
        // `docker` grandchild running.
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }

        cmd.spawn().map_err(|e| ExecError::Spawn {
            argv: inv.display(),
            message: e.to_string(),
        })
    }

    /// Spawn, stream both pipes on threads, and wait with a deadline.
    fn run_inner(&self, inv: &Invocation, cancel: &CancelToken) -> Result<Output, ExecError> {
        let span = tracing::debug_span!("run", argv = %inv.display(), cwd = %inv.cwd.display());
        let _guard = span.enter();

        let started = instant_now();
        let mut child = self.spawn(inv)?;
        let pid = child.id();

        // Drain both pipes concurrently. A single-threaded read of stdout would
        // deadlock as soon as the child filled the stderr pipe buffer, which is
        // exactly what a chatty script does.
        let stdout = Self::drain(child.stdout.take());
        let stderr = Self::drain(child.stderr.take());

        let deadline = started + Duration::from_millis(inv.timeout_ms);
        let mut timed_out = false;
        let mut cancelled = false;

        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {}
                Err(e) => {
                    return Err(ExecError::Spawn {
                        argv: inv.display(),
                        message: e.to_string(),
                    });
                }
            }

            if cancel.is_cancelled() {
                cancelled = true;
            } else if instant_now() >= deadline {
                timed_out = true;
            } else {
                std::thread::sleep(POLL_INTERVAL);
                continue;
            }

            tracing::warn!(
                argv = %inv.display(),
                timed_out,
                cancelled,
                "terminating process group"
            );
            signal::terminate_group(pid);
            // Reap so the child does not linger as a zombie. The group has been
            // SIGKILLed, so this returns promptly.
            break child.wait().map_err(|e| ExecError::Spawn {
                argv: inv.display(),
                message: e.to_string(),
            })?;
        };

        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let stdout = stdout.take();
        let stderr = stderr.take();

        if timed_out {
            return Err(ExecError::Timeout {
                argv: inv.display(),
                timeout_ms: inv.timeout_ms,
            });
        }
        if cancelled {
            // Reported as a timeout of zero would be a lie; cancellation is the
            // caller's own doing, so surface it as such via the cancel token.
            return Err(ExecError::Spawn {
                argv: inv.display(),
                message: "cancelled".to_owned(),
            });
        }

        let code = status.code().unwrap_or_else(|| {
            // Killed by a signal. Mirror the shell's 128+n convention so callers
            // and logs see something recognizable.
            signal::signal_of(status).map_or(-1, |sig| 128 + sig)
        });

        tracing::debug!(code, duration_ms, "finished");
        Ok(Output {
            code,
            stdout,
            stderr,
            duration_ms,
        })
    }

    /// Read a pipe to completion on its own thread, capped at
    /// [`MAX_CAPTURE_BYTES`].
    fn drain<R: Read + Send + 'static>(reader: Option<R>) -> Drained {
        let buffer = Arc::new(Mutex::new(String::new()));
        let truncated = Arc::new(AtomicBool::new(false));

        let Some(mut reader) = reader else {
            return Drained {
                buffer,
                truncated,
                handle: None,
            };
        };

        let sink = Arc::clone(&buffer);
        let flag = Arc::clone(&truncated);
        let handle = std::thread::spawn(move || {
            let mut chunk = [0_u8; 8192];
            let mut total = 0_usize;
            while let Ok(n) = reader.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                if total >= MAX_CAPTURE_BYTES {
                    flag.store(true, Ordering::SeqCst);
                    // Keep draining so the child never blocks on a full pipe —
                    // discarding is what lets the timeout actually fire.
                    total = total.saturating_add(n);
                    continue;
                }
                total += n;
                // Lossy: terminal output is not guaranteed to be valid UTF-8, and
                // a captured command's diagnostics are worth more than byte
                // fidelity.
                sink.lock().push_str(&String::from_utf8_lossy(&chunk[..n]));
            }
        });

        Drained {
            buffer,
            truncated,
            handle: Some(handle),
        }
    }
}

/// A pipe being drained on a background thread.
struct Drained {
    buffer: Arc<Mutex<String>>,
    truncated: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Drained {
    /// Join the reader thread and return what it captured.
    fn take(self) -> String {
        if let Some(handle) = self.handle {
            // The child has exited or been killed, so the pipe is closed and this
            // returns promptly. A panic in the reader thread is not worth
            // propagating — it would mask the command's own failure.
            let _ = handle.join();
        }
        let mut out = std::mem::take(&mut *self.buffer.lock());
        if self.truncated.load(Ordering::SeqCst) {
            out.push_str("\n[output truncated by wtm]\n");
        }
        out
    }
}

impl CommandRunner for Runner {
    fn run(&self, inv: &Invocation, cancel: &CancelToken) -> Result<Output, ExecError> {
        let out = self.run_inner(inv, cancel)?;
        if out.is_success() {
            Ok(out)
        } else {
            Err(ExecError::NonZeroExit {
                argv: inv.display(),
                code: out.code,
                stdout: out.stdout,
                stderr: out.stderr,
            })
        }
    }

    fn run_allow_failure(
        &self,
        inv: &Invocation,
        cancel: &CancelToken,
    ) -> Result<Output, ExecError> {
        self.run_inner(inv, cancel)
    }

    fn which(&self, program: &str) -> Option<PathBuf> {
        self.path.which(program, std::path::Path::new("."))
    }

    fn resolved_path(&self) -> String {
        self.path.value.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    fn runner() -> Runner {
        Runner::with_probed_path(None)
    }

    fn inv(argv: &[&str], timeout_ms: u64) -> Invocation {
        Invocation::new(
            argv.iter().map(|s| (*s).to_owned()).collect(),
            std::env::temp_dir(),
            timeout_ms,
        )
    }

    #[test]
    fn captures_stdout_and_reports_success() {
        let out = runner()
            .run(&inv(&["echo", "hello"], 5_000), &CancelToken::new())
            .unwrap();
        assert_eq!(out.stdout.trim(), "hello");
        assert_eq!(out.code, 0);
    }

    #[test]
    fn a_nonzero_exit_is_an_error_by_default_but_available_on_request() {
        let cancel = CancelToken::new();
        let command = inv(&["sh", "-c", "echo out; echo err >&2; exit 3"], 5_000);

        let err = runner().run(&command, &cancel).unwrap_err();
        match err {
            ExecError::NonZeroExit {
                code,
                ref stdout,
                ref stderr,
                ..
            } => {
                assert_eq!(code, 3);
                assert_eq!(stdout.trim(), "out");
                assert_eq!(stderr.trim(), "err");
            }
            other => panic!("expected NonZeroExit, got {other:?}"),
        }

        let out = runner().run_allow_failure(&command, &cancel).unwrap();
        assert_eq!(out.code, 3);
    }

    #[test]
    fn a_missing_program_names_the_path_it_searched() {
        let err = runner()
            .run(
                &inv(&["definitely-not-a-real-program-xyz"], 1_000),
                &CancelToken::new(),
            )
            .unwrap_err();
        match err {
            ExecError::ProgramNotFound { program, searched } => {
                assert_eq!(program, "definitely-not-a-real-program-xyz");
                assert!(!searched.is_empty(), "must report where it looked");
            }
            other => panic!("expected ProgramNotFound, got {other:?}"),
        }
    }

    #[test]
    fn a_hanging_command_is_killed_at_the_deadline() {
        let started = Instant::now();
        let err = runner()
            .run(&inv(&["sleep", "30"], 300), &CancelToken::new())
            .unwrap_err();
        assert!(matches!(err, ExecError::Timeout { .. }), "got {err:?}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "should not have waited for sleep"
        );
    }

    /// The failure mode this whole module exists to defend against: a script that
    /// prompts in a loop and ignores EOF, printing as fast as it can.
    #[test]
    fn a_prompt_loop_that_ignores_eof_is_still_killed() {
        let started = Instant::now();
        let err = runner()
            .run(
                &inv(
                    &[
                        "sh",
                        "-c",
                        "while true; do read -r x; echo 'Please enter y or n.'; done",
                    ],
                    400,
                ),
                &CancelToken::new(),
            )
            .unwrap_err();
        assert!(matches!(err, ExecError::Timeout { .. }), "got {err:?}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn stdin_is_null_so_a_well_behaved_read_sees_eof_immediately() {
        // Assert on `read`'s own status, not the script's: the script's exit code is
        // whatever its *last* command returns, so `read -r x; echo ...` exits 0 no
        // matter what `read` did.
        let started = Instant::now();
        let out = runner()
            .run(
                &inv(
                    &[
                        "sh",
                        "-c",
                        "if read -r x; then echo read-ok; else echo read-eof; fi",
                    ],
                    5_000,
                ),
                &CancelToken::new(),
            )
            .unwrap();
        assert_eq!(
            out.stdout.trim(),
            "read-eof",
            "stdin should be at EOF, not a live tty"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "must not have blocked on input"
        );
    }

    #[test]
    fn cancellation_stops_a_running_command() {
        let cancel = CancelToken::new();
        let c = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            c.cancel();
        });
        let started = Instant::now();
        let err = runner()
            .run(&inv(&["sleep", "30"], 60_000), &cancel)
            .unwrap_err();
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "cancel must be prompt: {err:?}"
        );
    }

    #[test]
    fn both_pipes_are_drained_so_a_chatty_command_cannot_deadlock() {
        // ~256 KB on each stream — comfortably past the 64 KB pipe buffer, so a
        // single-threaded reader would hang here.
        let out = runner()
            .run(
                &inv(
                    &[
                        "sh",
                        "-c",
                        "i=0; while [ $i -lt 4000 ]; do \
                         echo aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa; \
                         echo bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb >&2; \
                         i=$((i+1)); done",
                    ],
                    30_000,
                ),
                &CancelToken::new(),
            )
            .unwrap();
        assert!(
            out.stdout.len() > 200_000,
            "stdout was {} bytes",
            out.stdout.len()
        );
        assert!(
            out.stderr.len() > 200_000,
            "stderr was {} bytes",
            out.stderr.len()
        );
    }

    #[test]
    fn the_resolved_path_is_used_not_the_inherited_one() {
        let restricted = Runner::new(ResolvedPath::resolve(Some("/usr/bin:/bin")));
        assert!(restricted.which("sh").is_some());
        // `just` lives in /opt/homebrew/bin, which this PATH excludes — the exact
        // situation a bundled .app finds itself in.
        assert!(
            restricted.which("just").is_none(),
            "restricted PATH must not find just"
        );
    }

    #[test]
    fn env_overrides_from_the_invocation_win() {
        let mut command = inv(&["sh", "-c", "printf %s \"$WTM_TEST_VAR\""], 5_000);
        command
            .env
            .insert("WTM_TEST_VAR".to_owned(), "from-invocation".to_owned());
        let out = runner().run(&command, &CancelToken::new()).unwrap();
        assert_eq!(out.stdout, "from-invocation");
    }

    #[test]
    fn cwd_is_honoured() {
        let dir = tempfile::tempdir().unwrap();
        let command = Invocation::new(vec!["pwd".to_owned()], dir.path(), 5_000);
        let out = runner().run(&command, &CancelToken::new()).unwrap();
        // macOS temp dirs are under a symlinked /var -> /private/var.
        let reported = std::fs::canonicalize(out.stdout.trim()).unwrap();
        let expected = std::fs::canonicalize(dir.path()).unwrap();
        assert_eq!(reported, expected);
    }

    #[test]
    fn empty_argv_is_rejected_rather_than_panicking() {
        let err = runner()
            .run(&inv(&[], 1_000), &CancelToken::new())
            .unwrap_err();
        assert!(matches!(err, ExecError::Spawn { .. }), "got {err:?}");
    }
}
