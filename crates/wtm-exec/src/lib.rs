//! Process execution: the only crate in the workspace that spawns anything.
//!
//! `clippy.toml` bans `std::process::Command::new` and `SystemTime::now` repo-wide,
//! so this crate holds the few sanctioned exceptions. That is what guarantees every
//! subprocess in the app gets a deadline, a resolved `PATH`, a sanitized
//! environment, a `tracing` span, and a kill that reaches the whole process group.
//!
//! Contents:
//!
//! - [`path`] — probe a login shell for a usable `PATH`, and find programs on it.
//!   This is the mitigation for the app's most likely production failure: a
//!   GUI-launched app inherits a minimal `PATH` and cannot see the tools a project
//!   config calls.
//! - [`runner`] — [`Runner`], the captured-output [`CommandRunner`].
//! - [`pty`] — [`PtyHostImpl`], interactive sessions for anything that prompts or
//!   whose progress the user should watch.
//! - [`signal`] — process-*group* termination, `SIGTERM` then `SIGKILL`.
//! - [`clock`] — [`SystemClock`], the real [`Clock`].
//!
//! [`CommandRunner`]: wtm_core::ports::CommandRunner
//! [`Clock`]: wtm_core::ports::Clock

// Scoped to test code only:
//   * `unwrap_used` — in an assertion it adds noise without adding information,
//     since a panic is the failure report either way.
//   * `disallowed_methods` — the bans on `Command::new` and `Instant::now` exist to
//     keep *production* spawns funnelled through `Runner` and production time
//     behind the `Clock` port. These tests are the ones verifying that machinery, so
//     they have to reach the real syscalls and measure real elapsed time.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::disallowed_methods))]

pub mod clock;
pub mod path;
pub mod pty;
pub mod runner;
pub mod signal;

pub use clock::SystemClock;
pub use path::{PathSource, ResolvedPath, os_tokens};
pub use pty::PtyHostImpl;
pub use runner::Runner;

/// Build the full set of OS-backed adapters with one probed `PATH`.
///
/// Exists so the composition root cannot accidentally construct a [`Runner`] and a
/// [`PtyHostImpl`] with two *different* resolved paths — a discrepancy that would be
/// invisible until a command mysteriously worked in a terminal pane but not in a
/// captured preflight check.
#[must_use]
pub fn adapters(path_override: Option<&str>) -> (ResolvedPath, Runner, PtyHostImpl, SystemClock) {
    let path = ResolvedPath::resolve(path_override);
    let runner = Runner::new(path.clone());
    let pty = PtyHostImpl::new(path.clone());
    (path, runner, pty, SystemClock::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wtm_core::ports::CommandRunner;

    #[test]
    fn adapters_share_one_resolved_path() {
        let (path, runner, _pty, _clock) = adapters(Some("/usr/bin:/bin"));
        assert_eq!(path.value, "/usr/bin:/bin");
        assert_eq!(runner.resolved_path(), path.value);
    }
}
