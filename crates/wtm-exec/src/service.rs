//! [`ServiceHost`] on [`Runner`]: a worktree's services, started through the same spawn site as
//! every captured command, so they get the same resolved `PATH`, sanitized environment and process
//! group.
//!
//! # What differs from a captured run, and why
//!
//! No deadline, because a server runs until it is stopped; output appended to a file rather than
//! piped, because a pipe closes with wtm and a server writing to it then dies of `SIGPIPE`; and the
//! child is reaped on a thread of its own, which also reports its exit. A service is the second
//! kind of process that outlives the call that started it, beside
//! [`Runner::launch_detached`], and the difference is that this one is still wtm's: it can be
//! recognised again, and stopped.
//!
//! # Recognising a process again
//!
//! `ps -o lstart= -p <pid>` gives the time a process started, to the second, as text. That and the
//! pid together are its [`ProcessIdentity`]. It is compared, never parsed, and read with `LC_ALL=C`
//! so the words do not change with your language. Reading it costs one short `ps`, which is the
//! price of never signalling a pid that has come to belong to something else.

use std::fs::OpenOptions;
use std::path::Path;

use wtm_core::error::ExecError;
use wtm_core::flow::ProcessIdentity;
use wtm_core::model::ExitOutcome;
use wtm_core::ports::exec::{CancelToken, CommandRunner, Invocation};
use wtm_core::ports::service::{OnExit, ServiceHost, Termination};

use crate::runner::{Runner, Streams};
use crate::signal;

/// How long `ps` may take to say when a process started. It answers in milliseconds; a `ps` that
/// takes seconds is a machine in trouble, and wtm would rather not recognise a process than wait.
const IDENTIFY_TIMEOUT_MS: u64 = 5_000;

fn outcome_of(status: std::process::ExitStatus) -> ExitOutcome {
    match (status.code(), signal::signal_of(status)) {
        (Some(0), _) => ExitOutcome::Success,
        (Some(code), _) => ExitOutcome::Failed { code },
        (None, Some(signal)) => ExitOutcome::Signalled { signal },
        (None, None) => ExitOutcome::Failed { code: -1 },
    }
}

impl ServiceHost for Runner {
    fn launch(
        &self,
        inv: &Invocation,
        log: &Path,
        on_exit: OnExit,
    ) -> Result<ProcessIdentity, ExecError> {
        let refused = |message: String| ExecError::Spawn {
            argv: inv.display(),
            message,
        };
        if let Some(parent) = log.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| refused(format!("cannot make {}: {e}", parent.display())))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)
            .map_err(|e| refused(format!("cannot open {}: {e}", log.display())))?;

        let span = tracing::debug_span!("service", argv = %inv.display(), cwd = %inv.cwd.display());
        let _guard = span.enter();
        let mut child = self.spawn(inv, &Streams::Logged(file))?;
        let pid = child.id();

        let Some(identity) = self.identify(pid) else {
            signal::terminate_group(pid);
            let _ = child.wait();
            return Err(refused(
                "it started, but its start time could not be read, so wtm could not have \
                 recognised it again; it was stopped"
                    .to_owned(),
            ));
        };

        // One parked thread per running service, as `launch_detached` parks one per editor: the
        // wait is what reaps it, and the only way to hear about its exit without polling.
        let spawned = std::thread::Builder::new()
            .name(format!("service-{pid}"))
            .spawn(move || {
                let outcome = child
                    .wait()
                    .map_or(ExitOutcome::Failed { code: -1 }, outcome_of);
                on_exit(outcome);
            });
        if let Err(e) = spawned {
            signal::terminate_group(pid);
            return Err(refused(format!("cannot watch it: {e}")));
        }
        tracing::debug!(pid, "service started");
        Ok(identity)
    }

    fn identify(&self, pid: u32) -> Option<ProcessIdentity> {
        if pid == 0 {
            return None;
        }
        let inv = Invocation::new(
            vec![
                "ps".to_owned(),
                "-o".to_owned(),
                "lstart=".to_owned(),
                "-p".to_owned(),
                pid.to_string(),
            ],
            "/",
            IDENTIFY_TIMEOUT_MS,
        )
        .with_env([("LC_ALL".to_owned(), "C".to_owned())].into());
        let out = self.run_allow_failure(&inv, &CancelToken::new()).ok()?;
        let started = out.stdout.trim();
        (out.is_success() && !started.is_empty()).then(|| ProcessIdentity {
            pid,
            started: started.to_owned(),
        })
    }

    fn terminate(&self, process: &ProcessIdentity) -> Termination {
        match self.identify(process.pid) {
            None => Termination::Gone,
            Some(now) if now != *process => Termination::NotOurs,
            Some(_) => {
                signal::terminate_group(process.pid);
                Termination::Ended
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Duration;

    use super::*;

    fn runner() -> Runner {
        Runner::with_probed_path(None)
    }

    fn sh(script: &str, cwd: &Path) -> Invocation {
        Invocation::new(
            vec!["sh".to_owned(), "-c".to_owned(), script.to_owned()],
            cwd,
            1,
        )
    }

    #[test]
    fn a_service_writes_its_output_to_its_log_and_reports_its_exit() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("logs/web.log");
        let (tx, rx) = mpsc::channel();
        let identity = runner()
            .launch(
                &sh("echo listening; echo oops >&2; exit 3", dir.path()),
                &log,
                Box::new(move |outcome| tx.send(outcome).unwrap()),
            )
            .unwrap();
        assert!(!identity.started.is_empty());
        let outcome = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(outcome, ExitOutcome::Failed { code: 3 });
        let text = std::fs::read_to_string(&log).unwrap();
        assert!(
            text.contains("listening") && text.contains("oops"),
            "{text}"
        );
    }

    #[test]
    fn a_service_outlives_the_call_until_it_is_terminated_and_then_its_whole_group_ends() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = mpsc::channel();
        let host = runner();
        // A shell with a child of its own, as `npm run dev` starts a node under a shell.
        let identity = host
            .launch(
                &sh("sleep 300 & wait", dir.path()),
                &dir.path().join("web.log"),
                Box::new(move |outcome| tx.send(outcome).unwrap()),
            )
            .unwrap();
        assert!(
            rx.recv_timeout(Duration::from_millis(500)).is_err(),
            "no deadline: it keeps running"
        );
        assert_eq!(host.identify(identity.pid), Some(identity.clone()));

        assert_eq!(host.terminate(&identity), Termination::Ended);
        let outcome = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(
            matches!(outcome, ExitOutcome::Signalled { .. }),
            "{outcome:?}"
        );
        assert!(!signal::group_alive(identity.pid), "the sleep went with it");
        assert_eq!(host.terminate(&identity), Termination::Gone);
    }

    #[test]
    fn a_pid_whose_start_time_differs_is_not_signalled() {
        let dir = tempfile::tempdir().unwrap();
        let host = runner();
        let (tx, rx) = mpsc::channel();
        let real = host
            .launch(
                &sh("sleep 300", dir.path()),
                &dir.path().join("x.log"),
                Box::new(move |outcome| tx.send(outcome).unwrap()),
            )
            .unwrap();
        let impostor = ProcessIdentity {
            pid: real.pid,
            started: "Thu Jan  1 00:00:00 1970".to_owned(),
        };

        assert_eq!(host.terminate(&impostor), Termination::NotOurs);
        assert!(
            signal::process_alive(real.pid),
            "a stranger's process is left alone"
        );

        assert_eq!(host.terminate(&real), Termination::Ended);
        rx.recv_timeout(Duration::from_secs(10)).unwrap();
    }

    #[test]
    fn a_program_that_is_not_there_is_a_clear_error_and_starts_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let error = runner()
            .launch(
                &Invocation::new(vec!["wtm-no-such-server".to_owned()], dir.path(), 1),
                &dir.path().join("x.log"),
                Box::new(|_| {}),
            )
            .unwrap_err();
        assert!(
            matches!(error, ExecError::ProgramNotFound { .. }),
            "{error:?}"
        );
    }
}
