//! Reviewed scripts run as children of the visible shell's private helper.
//!
//! Unlike a captured command, this child must inherit the terminal and exported
//! environment. It gets its own foreground process group so Ctrl-C reaches the
//! script while the helper survives to report an authoritative outcome. No agent
//! text is typed into the line editor, and no command text is placed in argv.

use std::fs::{DirBuilder, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use nix::sys::signal::{SigSet, SigmaskHow, Signal, kill, killpg, pthread_sigmask};
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::{Pid, getpgrp, tcgetpgrp, tcsetpgrp};
use wtm_core::model::ExitOutcome;
use wtm_core::ports::clock::Clock;
use wtm_core::ports::exec::{CancelToken, CommandRunner, Invocation};

use crate::clock::SystemClock;
use crate::signal;

pub const MAX_SCRIPT_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interpreter {
    Sh,
    Bash,
    Zsh,
}

impl Interpreter {
    pub fn parse(name: &str) -> Result<Self, String> {
        match name {
            "sh" => Ok(Self::Sh),
            "bash" => Ok(Self::Bash),
            "zsh" => Ok(Self::Zsh),
            _ => Err("Choose sh, bash or zsh. Other interpreters are not supported.".into()),
        }
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Sh => "sh",
            Self::Bash => "bash",
            Self::Zsh => "zsh",
        }
    }

    #[must_use]
    pub fn argv(self, executable: &Path, script: &Path, parse_only: bool) -> Vec<String> {
        let mut argv = vec![executable.to_string_lossy().into_owned()];
        match self {
            Self::Bash => argv.extend(["--noprofile".into(), "--norc".into()]),
            Self::Zsh => argv.push("-f".into()),
            Self::Sh => {}
        }
        if parse_only {
            argv.push("-n".into());
        }
        argv.push("--".into());
        argv.push(script.to_string_lossy().into_owned());
        argv
    }
}

/// Canonical bytes are returned to the review sheet before it can authorize them.
pub fn canonical_script(text: &str) -> Result<String, String> {
    if text.len() > MAX_SCRIPT_BYTES {
        return Err("Commands are limited to 64 KiB. Copy this block instead.".into());
    }
    if text.trim().is_empty() {
        return Err("Enter a command to run.".into());
    }
    for (offset, character) in text.char_indices() {
        let crlf = character == '\r' && text[offset..].starts_with("\r\n");
        if character.is_control() && !matches!(character, '\n' | '\t') && !crlf {
            return Err(format!(
                "Remove the terminal control character at byte {}.",
                offset + 1
            ));
        }
    }
    // This is the sole normalization. No trimming, prompt removal or newline is
    // silently added, which would alter heredoc data and continuation semantics.
    Ok(text.replace("\r\n", "\n"))
}

/// The private script never lives in a repository and is removed with its owner.
#[derive(Debug)]
pub struct ScriptFile {
    path: PathBuf,
}

impl ScriptFile {
    pub fn create(text: &str) -> Result<Self, String> {
        let directory = std::env::temp_dir().join(format!("wtm-run-{}", uuid::Uuid::new_v4()));
        DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .map_err(|e| e.to_string())?;
        let script = Self {
            path: directory.join("command"),
        };
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&script.path)
            .map_err(|e| e.to_string())?;
        file.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
        Ok(script)
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn check(
        &self,
        runner: &dyn CommandRunner,
        interpreter: Interpreter,
        executable: &Path,
        cwd: &Path,
    ) -> Result<(), String> {
        let mut invocation =
            Invocation::new(interpreter.argv(executable, &self.path, true), cwd, 5_000);
        // Bash reads BASH_ENV even with --norc. Syntax review must not execute a
        // startup file; the execution helper applies the same environment rule.
        for name in ["BASH_ENV", "ENV"] {
            invocation.env.insert(name.into(), String::new());
        }
        let output = runner
            .run_allow_failure(&invocation, &CancelToken::new())
            .map_err(|e| e.to_string())?;
        if output.is_success() {
            Ok(())
        } else {
            Err(output.stderr)
        }
    }
}

impl Drop for ScriptFile {
    fn drop(&mut self) {
        if let Some(directory) = self.path.parent() {
            let _ = std::fs::remove_dir_all(directory);
        }
    }
}

/// Change foreground ownership without stopping the helper with SIGTTOU when
/// reclaiming the terminal from the child's process group.
fn foreground(group: Pid) -> Result<(), String> {
    let mut blocked = SigSet::empty();
    blocked.add(Signal::SIGTTOU);
    let mut previous = SigSet::empty();
    pthread_sigmask(SigmaskHow::SIG_BLOCK, Some(&blocked), Some(&mut previous))
        .map_err(|e| e.to_string())?;
    let changed = tcsetpgrp(std::io::stdin(), group);
    let restored = pthread_sigmask(SigmaskHow::SIG_SETMASK, Some(&previous), None);
    changed.and(restored).map_err(|e| e.to_string())
}

struct Foreground(Pid);

impl Drop for Foreground {
    fn drop(&mut self) {
        if let Err(error) = foreground(self.0) {
            tracing::debug!(%error, "could not restore the helper's foreground group");
        }
    }
}

struct ChildGroup {
    pid: u32,
    running: bool,
}

impl Drop for ChildGroup {
    fn drop(&mut self) {
        if self.running {
            // A failed terminal handoff or wait cannot strand an unseen child.
            signal::terminate_group(self.pid);
            if let Ok(pid) = i32::try_from(self.pid) {
                let _ = waitpid(Pid::from_raw(pid), None);
            }
        }
    }
}

/// Called only in the private helper process, never in the GUI process.
pub fn run_inherited(
    interpreter: Interpreter,
    executable: &Path,
    script: &Path,
    cwd: &Path,
    timeout_ms: u64,
    cancel: &CancelToken,
    on_started: impl FnOnce(u32) -> Result<(), String>,
) -> Result<ExitOutcome, String> {
    if cancel.is_cancelled() {
        return Ok(ExitOutcome::Cancelled);
    }
    let parent = getpgrp();
    if tcgetpgrp(std::io::stdin()).map_err(|e| e.to_string())? != parent {
        return Err("The shell helper no longer owns the foreground terminal.".into());
    }
    let argv = interpreter.argv(executable, script, false);
    // The sole inherited-stdio spawn adapter. The GUI never calls it: stdin here
    // belongs to the target shell's helper, not to the app or a captured runner.
    #[allow(clippy::disallowed_methods)]
    let mut command = Command::new(executable);
    command
        .args(&argv[1..])
        .current_dir(cwd)
        .process_group(0)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .env_remove("BASH_ENV")
        .env_remove("ENV");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("WTM_") {
            command.env_remove(key);
        }
    }
    let child = command.spawn().map_err(|e| e.to_string())?;
    let pid = child.id();
    let mut owned = ChildGroup { pid, running: true };
    let group = Pid::from_raw(i32::try_from(pid).map_err(|e| e.to_string())?);
    let _restore = Foreground(parent);
    foreground(group)?;
    // A child which reads immediately can receive SIGTTIN in the tiny interval
    // between spawn and tcsetpgrp. Resume it after assigning the foreground.
    let _ = killpg(group, Signal::SIGCONT);
    on_started(pid)?;
    let clock = SystemClock::new();
    let started = clock.monotonic_ms();
    let mut intervention = None;
    loop {
        match waitpid(group, Some(WaitPidFlag::WNOHANG | WaitPidFlag::WUNTRACED)) {
            Ok(WaitStatus::Exited(_, code)) => {
                owned.running = false;
                return Ok(intervention.unwrap_or(if code == 0 {
                    ExitOutcome::Success
                } else {
                    ExitOutcome::Failed { code }
                }));
            }
            Ok(WaitStatus::Signaled(_, signal, _)) => {
                owned.running = false;
                return Ok(intervention.unwrap_or(ExitOutcome::Signalled {
                    signal: signal as i32,
                }));
            }
            Ok(WaitStatus::Stopped(_, _)) => {
                // Let the interactive parent expose a stopped job. On fg, give
                // the terminal back to the script before continuing its group.
                foreground(parent)?;
                kill(Pid::this(), Signal::SIGSTOP).map_err(|e| e.to_string())?;
                foreground(group)?;
                let _ = killpg(group, Signal::SIGCONT);
            }
            Ok(_) => {}
            Err(nix::errno::Errno::EINTR) => continue,
            Err(nix::errno::Errno::ECHILD) => {
                owned.running = false;
                return Err("The command outcome could not be recovered.".into());
            }
            Err(error) => return Err(error.to_string()),
        }
        let elapsed = clock.monotonic_ms().saturating_sub(started);
        if intervention.is_none() && (cancel.is_cancelled() || elapsed >= timeout_ms) {
            intervention = Some(if cancel.is_cancelled() {
                ExitOutcome::Cancelled
            } else {
                ExitOutcome::TimedOut { after_ms: elapsed }
            });
            signal::terminate_group(pid);
        }
        std::thread::sleep(Duration::from_millis(15));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn canonical_scripts_preserve_prompts_heredocs_comments_and_continuations() {
        let text = "# note\ncat <<'END'\n$ literal prompt\n> literal continuation\nEND\nprintf '%s' \\\n'日本語'";
        assert_eq!(canonical_script(text).unwrap(), text);
        assert_eq!(canonical_script("printf x\r\n").unwrap(), "printf x\n");
        for text in ["", " \n", "printf x\0", "printf x\u{1b}", "printf x\u{7f}"] {
            assert!(canonical_script(text).is_err());
        }
        assert!(canonical_script(&"x".repeat(MAX_SCRIPT_BYTES + 1)).is_err());
    }

    #[test]
    fn prepared_scripts_are_private_exact_and_removed_when_released() {
        let text = "cat <<'END'\n$ do not strip\nEND\n";
        let file = ScriptFile::create(text).unwrap();
        let path = file.path().to_owned();
        assert_eq!(std::fs::read(&path).unwrap(), text.as_bytes());
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        drop(file);
        assert!(!path.exists());
    }

    #[test]
    fn cancellation_before_admission_never_touches_the_terminal_or_spawns_a_child() {
        let cancel = CancelToken::new();
        cancel.cancel();
        let result = run_inherited(
            Interpreter::Sh,
            Path::new("/not/an/interpreter"),
            Path::new("/not/a/script"),
            Path::new("/not/a/worktree"),
            1,
            &cancel,
            |_| panic!("a cancelled command must not start"),
        )
        .unwrap();
        assert_eq!(result, ExitOutcome::Cancelled);
    }

    #[test]
    fn syntax_review_does_not_run_the_script_and_reports_incomplete_quotes() {
        let root = tempfile::tempdir().unwrap();
        let runner = crate::Runner::with_probed_path(Some("/usr/bin:/bin"));
        for interpreter in [Interpreter::Sh, Interpreter::Bash, Interpreter::Zsh] {
            let executable = PathBuf::from("/bin").join(interpreter.name());
            let script = ScriptFile::create("touch executed; printf '%s' 'hello'\n").unwrap();
            script
                .check(&runner, interpreter, &executable, root.path())
                .unwrap();
            assert!(!root.path().join("executed").exists());
            let broken = ScriptFile::create("printf '%s' 'unfinished\n").unwrap();
            assert!(
                broken
                    .check(&runner, interpreter, &executable, root.path())
                    .is_err()
            );
        }
    }

    // This is a subprocess entry point for the real-PTY test below, not an app
    // launch. Without its private test environment it deliberately does nothing.
    #[test]
    #[ignore = "entered only by the isolated PTY regression test"]
    fn the_private_test_helper_runs_inside_its_parent_terminal() {
        let Ok(script) = std::env::var("WTM_TEST_SCRIPT") else {
            return;
        };
        let cwd = std::env::var("WTM_TEST_CWD").unwrap();
        let timeout = std::env::var("WTM_TEST_TIMEOUT").unwrap().parse().unwrap();
        let begin = std::env::var("WTM_TEST_BEGIN").unwrap();
        let end = std::env::var("WTM_TEST_END").unwrap();
        std::io::stdout().write_all(begin.as_bytes()).unwrap();
        std::io::stdout().flush().unwrap();
        let result = run_inherited(
            Interpreter::Sh,
            Path::new("/bin/sh"),
            Path::new(&script),
            Path::new(&cwd),
            timeout,
            &CancelToken::new(),
            |_| Ok(()),
        );
        std::io::stdout().write_all(end.as_bytes()).unwrap();
        std::io::stdout().flush().unwrap();
        // The real interactive parent remains alive after a run. Keep this test
        // helper alive long enough to observe a descendant printing after end.
        std::thread::sleep(Duration::from_millis(250));
        std::io::stdout()
            .write_all(format!("\nHELPER_OUTCOME:{result:?}\n").as_bytes())
            .unwrap();
        assert!(result.is_ok());
    }

    #[derive(Default)]
    struct Output(parking_lot::Mutex<Vec<u8>>, parking_lot::Condvar);

    impl wtm_core::ports::pty::PtySink for Output {
        fn on_output(&self, _: &wtm_core::model::SessionId, bytes: &[u8]) {
            self.0.lock().extend_from_slice(bytes);
            self.1.notify_all();
        }
        fn on_exit(&self, _: &wtm_core::model::SessionId, _: &ExitOutcome) {
            self.1.notify_all();
        }
    }

    impl Output {
        fn await_text(&self, text: &str) -> String {
            let clock = SystemClock::new();
            let deadline = clock.monotonic_ms() + 5_000;
            let mut bytes = self.0.lock();
            loop {
                let output = String::from_utf8_lossy(&bytes).into_owned();
                if output.contains(text) {
                    return output;
                }
                assert!(
                    clock.monotonic_ms() < deadline,
                    "missing {text:?} in {output:?}"
                );
                self.1.wait_for(&mut bytes, Duration::from_millis(25));
            }
        }
    }

    #[test]
    fn inherited_children_read_the_terminal_and_report_exit_signal_and_deadline() {
        use std::sync::Arc;
        use wtm_core::ports::pty::PtyHost;

        let root = tempfile::tempdir().unwrap();
        let host = crate::PtyHostImpl::new(crate::ResolvedPath::resolve(Some("/usr/bin:/bin")));
        for (text, expected, timeout, input) in [
            (
                "printf 'VALUE?'; read value; printf 'value=%s\\n' \"$value\"; exit 7",
                "Failed { code: 7 }",
                5_000,
                Some("answer\n"),
            ),
            ("kill -TERM $$", "Signalled { signal: 15 }", 5_000, None),
            ("sleep 30 & wait", "TimedOut", 100, None),
            (
                "printf early; (sleep 0.1; printf late) &",
                "Success",
                5_000,
                None,
            ),
        ] {
            let script = ScriptFile::create(text).unwrap();
            let mut invocation = Invocation::new(
                vec![
                    std::env::current_exe()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    "--ignored".into(),
                    "--exact".into(),
                    "shell::tests::the_private_test_helper_runs_inside_its_parent_terminal".into(),
                    "--nocapture".into(),
                ],
                root.path(),
                10_000,
            );
            invocation.env.insert(
                "WTM_TEST_SCRIPT".into(),
                script.path().to_string_lossy().into_owned(),
            );
            invocation.env.insert(
                "WTM_TEST_CWD".into(),
                root.path().to_string_lossy().into_owned(),
            );
            invocation
                .env
                .insert("WTM_TEST_TIMEOUT".into(), timeout.to_string());
            let framing = crate::shell_frames::Framing::default();
            invocation
                .env
                .insert("WTM_TEST_BEGIN".into(), framing.begin().into());
            invocation
                .env
                .insert("WTM_TEST_END".into(), framing.end().into());
            let output = Arc::new(Output::default());
            let session = host
                .spawn(&invocation, 24, 80, None, output.clone())
                .unwrap()
                .session;
            if let Some(input) = input {
                output.await_text("VALUE?");
                host.write(&session, input.as_bytes()).unwrap();
            }
            let outcome = host.wait(&session, &CancelToken::new()).unwrap();
            assert!(
                outcome.is_success(),
                "helper failed: {:?}",
                String::from_utf8_lossy(&output.0.lock())
            );
            let captured = output.await_text("HELPER_OUTCOME:");
            assert!(captured.contains(expected), "{captured}");
            if input.is_some() {
                assert!(captured.contains("value=answer"), "{captured}");
            }
            let mut filter = crate::shell_frames::FrameFilter::new(framing);
            let filtered = filter.push(&output.0.lock());
            assert!(filtered.started && filtered.ended);
            assert!(!String::from_utf8_lossy(&filtered.captured).contains("HELPER_OUTCOME"));
            if text.contains("printf early") {
                assert_eq!(filtered.captured, b"early");
                assert!(String::from_utf8_lossy(&filtered.display).contains("late"));
            }
        }
    }
}
