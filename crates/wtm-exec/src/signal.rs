//! Terminating a process tree.
//!
//! # Why the group and not the child
//!
//! The processes this app spawns are not leaves. A project setup command is
//! typically a shell script that invokes another shell, which invokes `docker`,
//! which talks to a daemon. Killing only the direct child leaves the rest running
//! and holding the resources — a half-copied volume, a container mid-start — which
//! is worse than not killing anything, because now nothing is watching it.
//!
//! So spawns put the child in a fresh process group (`process_group(0)` for captured
//! commands, `setsid()` inside the pty for interactive ones), which makes the child
//! the group leader with `pgid == pid`, and termination signals the whole group.
//!
//! # Why TERM then KILL
//!
//! `SIGTERM` first, so a `docker` client can tear down cleanly and a shell can run
//! its traps. Then `SIGKILL` unconditionally after a short grace period, because the
//! caller is about to `wait()` and a process that ignores `SIGTERM` would hang that
//! wait forever. Politeness with a deadline.
//!
//! There is no `unsafe` here: the signal syscalls come from `nix`'s safe wrappers.
//! See the workspace manifest for why that dependency was chosen over `libc`.

use std::time::Duration;

use nix::errno::Errno;
use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;

/// How long a group gets to exit after `SIGTERM` before `SIGKILL`.
const GRACE: Duration = Duration::from_millis(400);

/// `SIGTERM` the process group led by `pid`, then `SIGKILL` it.
///
/// Best-effort and never panics: by the time this runs the process may already be
/// gone, which is success, not failure.
pub fn terminate_group(pid: u32) {
    let Some(group) = group_of(pid) else {
        tracing::warn!(pid, "pid does not fit in a pid_t; cannot signal group");
        return;
    };

    match killpg(group, Signal::SIGTERM) {
        Ok(()) => {}
        // Already gone — nothing to do.
        Err(Errno::ESRCH) => return,
        Err(e) => tracing::debug!(pid, error = %e, "SIGTERM to group failed"),
    }

    std::thread::sleep(GRACE);

    match killpg(group, Signal::SIGKILL) {
        Ok(()) | Err(Errno::ESRCH) => {}
        Err(e) => tracing::warn!(pid, error = %e, "SIGKILL to group failed"),
    }
}

/// Whether any process in the group led by `pid` is still alive.
///
/// Signal 0 performs permission and existence checks without delivering anything.
#[must_use]
pub fn group_alive(pid: u32) -> bool {
    group_of(pid).is_some_and(|group| !matches!(killpg(group, None), Err(Errno::ESRCH)))
}

/// Whether a single process is still alive.
#[must_use]
pub fn process_alive(pid: u32) -> bool {
    // Same trap as `group_of`: `kill(0, …)` targets our own process group.
    let Some(pid) = group_of(pid) else {
        return false;
    };
    !matches!(nix::sys::signal::kill(pid, None), Err(Errno::ESRCH))
}

/// Convert a child pid into a process-group id to signal.
///
/// **Rejects 0.** In `killpg`/`kill` semantics a pid of 0 means "every process in
/// the *caller's* process group" — so passing it through would make
/// [`terminate_group`] SIGKILL the app itself. A child pid is never 0, so any 0
/// reaching here is a bug upstream (an unavailable `process_id()`), and the correct
/// response is to do nothing rather than to commit suicide.
///
/// Negative values are equally dangerous (`-1` means every process the user may
/// signal), but `u32` already excludes them.
fn group_of(pid: u32) -> Option<Pid> {
    if pid == 0 {
        tracing::error!("refusing to signal pid 0: that would target our own process group");
        return None;
    }
    i32::try_from(pid).ok().map(Pid::from_raw)
}

/// The signal that killed a process, if one did.
#[must_use]
pub fn signal_of(status: std::process::ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.signal()
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    /// Regression test for a genuine self-destruct.
    ///
    /// The first version of this module passed `pid` straight to `killpg`. Calling
    /// it with 0 then `SIGKILL`ed our own process group — which, running under a test
    /// harness, killed the harness (exit 144) rather than failing a test. A child's
    /// `process_id()` is `Option<u32>`, so a `None` collapsing to 0 anywhere
    /// upstream would have shipped this.
    #[test]
    fn pid_zero_is_refused_rather_than_signalling_our_own_process_group() {
        assert!(
            group_of(0).is_none(),
            "pid 0 must never become a signal target"
        );
        // Must be a no-op. If this regresses, the test process dies here.
        terminate_group(0);
        assert!(!process_alive(0));
        assert!(!group_alive(0));
    }

    #[test]
    fn signalling_a_nonexistent_process_is_harmless() {
        assert!(!process_alive(u32::MAX));
        terminate_group(u32::MAX);
    }

    /// **The load-bearing test for the kill design.**
    ///
    /// The plan for this crate flagged the process-group assumption as something to
    /// verify empirically rather than trust: `ChildKiller::kill` and a plain
    /// `child.kill()` both signal only the direct child, so if `process_group(0)`
    /// did not actually make the child a group leader, grandchildren would survive
    /// every timeout and cancel. This spawns a shell that backgrounds a long
    /// `sleep` — a real grandchild — records its pid, then kills the group and
    /// asserts the grandchild is gone.
    #[test]
    fn terminating_a_group_reaps_grandchildren() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("grandchild.pid");

        // sh (child) → sleep (grandchild, backgrounded). The child then waits, so
        // both are alive when we signal.
        let script = format!("sleep 60 & echo $! > {}; wait", pidfile.display());

        #[allow(clippy::disallowed_methods)]
        let mut child = {
            use std::os::unix::process::CommandExt;
            let mut cmd = std::process::Command::new("/bin/sh");
            cmd.args(["-c", &script])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            cmd.process_group(0);
            cmd.spawn().unwrap()
        };

        // Wait for the grandchild pid to be written.
        let deadline = Instant::now() + Duration::from_secs(5);
        let grandchild = loop {
            assert!(
                Instant::now() < deadline,
                "grandchild never reported its pid"
            );
            if let Ok(text) = std::fs::read_to_string(&pidfile)
                && let Ok(pid) = text.trim().parse::<u32>()
            {
                break pid;
            }
            std::thread::sleep(Duration::from_millis(25));
        };

        assert!(
            process_alive(grandchild),
            "grandchild should be running before the kill"
        );

        terminate_group(child.id());
        let _ = child.wait();

        // Give the kernel a moment to finish reaping.
        let deadline = Instant::now() + Duration::from_secs(5);
        while process_alive(grandchild) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }

        assert!(
            !process_alive(grandchild),
            "grandchild {grandchild} survived the group kill — the process-group \
             assumption is wrong, and every timeout leaks a process tree"
        );
    }

    #[test]
    fn a_child_that_ignores_sigterm_is_still_killed() {
        // `trap '' TERM` makes SIGTERM a no-op, which is why the escalation to
        // SIGKILL is unconditional rather than conditional on the process exiting.
        #[allow(clippy::disallowed_methods)]
        let mut child = {
            use std::os::unix::process::CommandExt;
            let mut cmd = std::process::Command::new("/bin/sh");
            cmd.args(["-c", "trap '' TERM; sleep 60"])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            cmd.process_group(0);
            cmd.spawn().unwrap()
        };
        let pid = child.id();
        std::thread::sleep(Duration::from_millis(150));

        let started = Instant::now();
        terminate_group(pid);
        let status = child.wait().unwrap();

        assert!(
            started.elapsed() < Duration::from_secs(5),
            "escalation must be prompt"
        );
        assert_eq!(
            signal_of(status),
            Some(Signal::SIGKILL as i32),
            "should have been SIGKILLed after ignoring SIGTERM"
        );
    }

    #[test]
    fn a_cooperative_child_exits_on_sigterm_without_needing_sigkill() {
        #[allow(clippy::disallowed_methods)]
        let mut child = {
            use std::os::unix::process::CommandExt;
            let mut cmd = std::process::Command::new("/bin/sh");
            cmd.args(["-c", "sleep 60"])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            cmd.process_group(0);
            cmd.spawn().unwrap()
        };
        std::thread::sleep(Duration::from_millis(150));
        terminate_group(child.id());
        let status = child.wait().unwrap();
        assert_eq!(signal_of(status), Some(Signal::SIGTERM as i32));
    }

    #[test]
    fn group_alive_tracks_a_real_group() {
        #[allow(clippy::disallowed_methods)]
        let mut child = {
            use std::os::unix::process::CommandExt;
            let mut cmd = std::process::Command::new("/bin/sh");
            cmd.args(["-c", "sleep 5"])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
            cmd.process_group(0);
            cmd.spawn().unwrap()
        };
        std::thread::sleep(Duration::from_millis(150));
        assert!(group_alive(child.id()));
        terminate_group(child.id());
        let _ = child.wait();
    }
}
