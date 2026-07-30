//! The captured command runner.
//!
//! # This is the only place a process is spawned
//!
//! `clippy.toml` bans `std::process::Command::new` repo-wide via
//! `disallowed-methods`, so every subprocess in the app funnels through here (and
//! through [`crate::pty`]). That is not bureaucracy — it is what guarantees, for
//! every *captured* spawn, that:
//!
//! - there is a deadline,
//! - expiry kills the process **group**, not just the direct child,
//! - the environment is the resolved one (see [`crate::path`]),
//! - stdin is `/dev/null`, so a prompt fails fast instead of blocking,
//! - and there is a `tracing` span with the argv and the duration.
//!
//! # The one spawn with no deadline
//!
//! [`Runner::launch_detached`] deliberately breaks the first two guarantees, and it
//! is the only thing here that does. Launching a desktop application is not a
//! command whose output we want — it is a hand-off. Every property above that makes
//! a captured run safe makes a launch *wrong*: a deadline that terminates the
//! process group would kill the editor a few seconds after opening it. Read its doc
//! comment before using it; it is not a general-purpose spawn.
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

/// What a spawned child does with stdout and stderr.
///
/// Named rather than passed as a bare `bool`, because the two modes have opposite
/// obligations: [`Streams::Captured`] pipes *must* be drained by someone or the child
/// blocks once the 64 KB buffer fills, while [`Streams::Discarded`] deliberately has
/// nobody listening at all.
#[derive(Debug, Clone, Copy)]
enum Streams {
    /// Both pipes captured for the caller. Only valid where a reader follows.
    Captured,
    /// Both to `/dev/null`. For a launch nobody will ever read the output of, where
    /// a pipe would be a slow leak waiting for a chatty GUI app to fill it.
    Discarded,
}

impl Streams {
    fn stdio(self) -> Stdio {
        match self {
            Self::Captured => Stdio::piped(),
            Self::Discarded => Stdio::null(),
        }
    }
}

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

    fn spawn(&self, inv: &Invocation, streams: Streams) -> Result<std::process::Child, ExecError> {
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
            .stdout(streams.stdio())
            .stderr(streams.stdio());

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

    /// Start a program and walk away — no deadline, no output, no kill.
    ///
    /// For handing a path to a desktop application. Every guarantee in this module's
    /// header that makes a captured run safe makes a launch wrong:
    ///
    /// - **No deadline.** [`Self::run`] terminates the process *group* on expiry
    ///   (see [`crate::signal::terminate_group`]). A GUI shim like `code`, `subl` or
    ///   the JetBrains launcher stays in the foreground for the editor's whole
    ///   lifetime, so a deadline here would not time out a hung command — it would
    ///   kill the application the user just opened, seconds after it appeared.
    /// - **No output.** Nothing is captured because nothing will read it.
    /// - **No exit code.** `Ok(())` means *the program was found and `exec`'d*. It
    ///   does not mean a window appeared, and it cannot: the process outlives this
    ///   call by design.
    ///
    /// `inv.timeout_ms` is therefore **ignored**. It stays on [`Invocation`] because
    /// making it optional would weaken the type for every other caller, where a
    /// missing deadline is the bug this crate exists to prevent.
    ///
    /// The child still gets the resolved `PATH`, the requested `cwd`, null stdin and
    /// its own process group — the last so it is not swept up by a signal aimed at
    /// wtm.
    ///
    /// # Not a replacement for [`Self::run`] on `open`/`xdg-open`
    ///
    /// [`crate::path`]'s platform opener returns within milliseconds and its exit
    /// code is real signal — `xdg-open` exits 3 when no handler is registered, which
    /// is worth surfacing. `open_url` deliberately keeps using [`Self::run`]. The
    /// difference between the two call sites is the argv, not the function.
    ///
    /// # Errors
    ///
    /// [`ExecError::ProgramNotFound`] if the program is not on the resolved `PATH` —
    /// which, naming what it searched, is the entire diagnostic value of this call —
    /// or [`ExecError::Spawn`] if `exec` itself fails.
    pub fn launch_detached(&self, inv: &Invocation) -> Result<(), ExecError> {
        let span = tracing::debug_span!("launch", argv = %inv.display(), cwd = %inv.cwd.display());
        let _guard = span.enter();

        let mut child = self.spawn(inv, Streams::Discarded)?;
        let pid = child.id();

        // Reap on a thread rather than leaking a zombie for the app's lifetime. The
        // thread parks in `wait()` until the editor exits, which may be hours — that
        // is the cost, and it is one parked thread (~8 KiB of virtual stack) per
        // launch, against a process table entry that never goes away otherwise.
        // Detaching without reaping at all is the usual shortcut and it is wrong
        // here: wtm is long-lived, and a user who opens ten worktrees a day would
        // accumulate ten defunct entries a day.
        std::thread::spawn(move || {
            let _ = child.wait();
        });

        tracing::debug!(pid, "launched detached");
        Ok(())
    }

    /// Spawn, stream both pipes on threads, and wait with a deadline.
    fn run_inner(&self, inv: &Invocation, cancel: &CancelToken) -> Result<Output, ExecError> {
        let span = tracing::debug_span!("run", argv = %inv.display(), cwd = %inv.cwd.display());
        let _guard = span.enter();

        let started = instant_now();
        let mut child = self.spawn(inv, Streams::Captured)?;
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
    use std::path::Path;
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
        // Construct the situation rather than lean on what this machine has installed.
        // The previous version asserted that `just` was *not* findable under
        // `/usr/bin:/bin`, which was true only because `just` happens to live in
        // /opt/homebrew/bin here — one `apt install just` away from being wrong, and a
        // test that passes for a reason unrelated to what it claims.
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("wtm-path-marker");
        std::fs::write(&marker, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&marker, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();

        let restricted = Runner::new(ResolvedPath::resolve(Some(&dir.path().to_string_lossy())));

        // The resolved PATH is consulted…
        assert_eq!(restricted.which("wtm-path-marker"), Some(marker));
        // …and nothing else is. `sh` is certainly on the inherited PATH and certainly
        // not in this tempdir, so finding it would prove the inherited one leaked in.
        assert!(
            restricted.which("sh").is_none(),
            "the inherited PATH must not leak into a resolved one"
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

    /// Poll for `path` to appear, up to `limit`. Returns whether it did.
    ///
    /// Polling rather than one long sleep: the assertion is "this eventually
    /// happened", and a fixed sleep either makes the suite slow or makes it flaky on
    /// a loaded CI runner.
    fn appeared_within(path: &Path, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            if path.exists() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    /// The reason [`Runner::launch_detached`] exists.
    ///
    /// Built on `run`, an "Open in PyCharm" button would terminate the process group
    /// at the deadline — killing the editor it had just opened, seconds later, with
    /// no error anywhere. The timeout below is deliberately far shorter than the
    /// child's own sleep, so a deadline of *any* kind would stop the marker being
    /// written.
    #[test]
    fn a_detached_launch_outlives_the_deadline_it_would_have_had() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("survived");

        let started = Instant::now();
        runner()
            .launch_detached(&inv(
                &[
                    "sh",
                    "-c",
                    &format!("sleep 1; : > {}", marker.to_string_lossy()),
                ],
                50,
            ))
            .unwrap();

        assert!(
            started.elapsed() < Duration::from_millis(500),
            "the call must hand off, not wait for the child"
        );
        assert!(
            appeared_within(&marker, Duration::from_secs(10)),
            "the child was killed; a launched application would have died with it"
        );
    }

    #[test]
    fn a_detached_launch_of_a_missing_program_still_reports_where_it_looked() {
        let err = runner()
            .launch_detached(&inv(&["definitely-not-a-real-program-xyz"], 1_000))
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
    fn a_detached_launch_inherits_the_resolved_path_and_the_requested_cwd() {
        // The Linux terminal strategy rests entirely on cwd inheritance: rather than
        // learn each emulator's --working-directory spelling, wtm sets `cwd` and lets
        // the terminal inherit it. If that broke, every Linux terminal would open in
        // the wrong place with nothing to indicate why.
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("cwd-was-honoured");

        let command = Invocation::new(
            vec![
                "sh".to_owned(),
                "-c".to_owned(),
                // Written relative, so it lands in the marker's directory only if the
                // child actually started there.
                "printf %s \"$PATH\" > cwd-was-honoured".to_owned(),
            ],
            dir.path(),
            5_000,
        );
        runner().launch_detached(&command).unwrap();

        assert!(
            appeared_within(&marker, Duration::from_secs(10)),
            "the child did not run in the requested cwd"
        );
        let seen = std::fs::read_to_string(&marker).unwrap();
        assert_eq!(
            seen,
            runner().path().value,
            "the child must get the resolved PATH, not the inherited one"
        );
    }

    /// A zombie answers "alive" to `kill(pid, 0)`, so counting defunct entries is the
    /// only check that actually distinguishes reaped from leaked.
    #[test]
    fn finished_detached_launches_are_reaped_rather_than_left_as_zombies() {
        let runner = runner();
        for _ in 0..8 {
            runner.launch_detached(&inv(&["true"], 1_000)).unwrap();
        }

        // Give the reaper threads a moment; then read the process table. `-o ppid=,stat=`
        // is spelled the same on macOS and Linux.
        let ours = std::process::id().to_string();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let out = runner
                .run(
                    &inv(&["ps", "-A", "-o", "ppid=,stat="], 5_000),
                    &CancelToken::new(),
                )
                .unwrap();
            let zombies = out
                .stdout
                .lines()
                .filter_map(|line| {
                    let (ppid, stat) = line.trim().split_once(char::is_whitespace)?;
                    (ppid == ours && stat.starts_with('Z')).then_some(())
                })
                .count();

            if zombies == 0 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "{zombies} detached children were left defunct"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn empty_argv_is_rejected_rather_than_panicking() {
        let err = runner()
            .run(&inv(&[], 1_000), &CancelToken::new())
            .unwrap_err();
        assert!(matches!(err, ExecError::Spawn { .. }), "got {err:?}");
    }
}
