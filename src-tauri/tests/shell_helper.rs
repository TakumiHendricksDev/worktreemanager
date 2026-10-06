//! The private helper must execute inside the actual ZLE-owned terminal, without
//! constructing Tauri or reading the running app's data. A temporary socket acts
//! as an already-approved run service; no live session credentials are inherited.

#![allow(clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::{Condvar, Mutex};
use serde_json::{Value, json};
use wtm_app_lib::shell_admission::{Availability, Shell};
use wtm_core::model::{ExitOutcome, SessionId};
use wtm_core::ports::clock::Clock;
use wtm_core::ports::exec::Invocation;
use wtm_core::ports::pty::{PtyHost, PtySink};
use wtm_exec::shell::ScriptFile;
use wtm_exec::shell_frames::{FrameFilter, Framing};
use wtm_exec::shell_integration::ShellIntegration;
use wtm_exec::{PtyHostImpl, ResolvedPath, SystemClock};

struct Output {
    bytes: Mutex<Vec<u8>>,
    changed: Condvar,
}

impl PtySink for Output {
    fn on_output(&self, _: &SessionId, bytes: &[u8]) {
        self.bytes.lock().extend_from_slice(bytes);
        self.changed.notify_all();
    }
    fn on_exit(&self, _: &SessionId, _: &ExitOutcome) {
        self.changed.notify_all();
    }
}

impl Output {
    fn await_text(&self, text: &str) {
        let clock = SystemClock::new();
        let deadline = clock.monotonic_ms() + 10_000;
        let mut bytes = self.bytes.lock();
        while !String::from_utf8_lossy(&bytes).contains(text) {
            assert!(
                clock.monotonic_ms() < deadline,
                "missing {text:?}: {:?}",
                String::from_utf8_lossy(&bytes)
            );
            self.changed.wait_for(&mut bytes, Duration::from_millis(25));
        }
    }
}

struct Terminal {
    host: PtyHostImpl,
    session: SessionId,
    shell: Arc<Shell>,
    output: Arc<Output>,
    listener: UnixListener,
    root: tempfile::TempDir,
}

fn accept(listener: &UnixListener) -> UnixStream {
    let clock = SystemClock::new();
    let deadline = clock.monotonic_ms() + 10_000;
    loop {
        if let Ok((stream, _)) = listener.accept() {
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            return stream;
        }
        assert!(
            clock.monotonic_ms() < deadline,
            "the private helper did not connect"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn line(reader: &mut impl BufRead) -> String {
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert!(
        !line.is_empty(),
        "the helper disconnected without an outcome"
    );
    line
}

impl Terminal {
    fn open() -> Self {
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let socket = root.path().join("s");
        let listener = UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        std::fs::write(root.path().join(".zshrc"), "PROMPT='READY> '\n").unwrap();
        let integration =
            ShellIntegration::create(&socket, Path::new(env!("CARGO_BIN_EXE_wtm")), root.path())
                .unwrap();
        let shell = Arc::new(Shell::new(
            "test-project",
            "test-worktree",
            None,
            integration,
        ));
        let mut invocation =
            Invocation::new(vec!["/bin/zsh".into(), "-il".into()], root.path(), 30_000);
        invocation
            .env
            .insert("HOME".into(), root.path().to_string_lossy().into_owned());
        invocation.env.insert(
            "ZDOTDIR".into(),
            shell.integration.directory().to_string_lossy().into_owned(),
        );
        invocation
            .env
            .insert("SHELL_SESSIONS_DISABLE".into(), "1".into());
        let host = PtyHostImpl::new(ResolvedPath::resolve(Some("/usr/bin:/bin")));
        let output = Arc::new(Output {
            bytes: Mutex::new(Vec::new()),
            changed: Condvar::new(),
        });
        let session = host
            .spawn(&invocation, 24, 100, None, output.clone())
            .unwrap()
            .session;
        let mut connection = BufReader::new(accept(&listener));
        let intro: Value = serde_json::from_str(&line(&mut connection)).unwrap();
        assert_eq!(intro["kind"], "shellIntegration");
        assert_eq!(intro["token"], shell.integration.token());
        connection.get_ref().set_read_timeout(None).unwrap();
        shell
            .connect(connection.get_ref().try_clone().unwrap())
            .unwrap();
        let controller = Arc::clone(&shell);
        std::thread::spawn(move || {
            let mut text = String::new();
            while connection
                .read_line(&mut text)
                .is_ok_and(|length| length > 0)
            {
                let parts: Vec<_> = text.split_whitespace().collect();
                match parts.as_slice() {
                    ["prompt", epoch, jobs] => {
                        controller.prompt(epoch.parse().unwrap(), jobs.parse().unwrap());
                    }
                    ["busy", _] => controller.busy(),
                    _ => {}
                }
                text.clear();
            }
        });
        Self {
            host,
            session,
            shell,
            output,
            listener,
            root,
        }
    }

    fn run(
        &self,
        script: &ScriptFile,
        framing: &Framing,
        timeout_ms: u64,
    ) -> BufReader<UnixStream> {
        let clock = SystemClock::new();
        let deadline = clock.monotonic_ms() + 5_000;
        while self
            .shell
            .availability(self.host.shell_owns_foreground(&self.session))
            != Availability::Ready
        {
            assert!(
                clock.monotonic_ms() < deadline,
                "no empty prompt: {:?}",
                String::from_utf8_lossy(&self.output.bytes.lock())
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let capability = "0123456789abcdef0123456789abcdef";
        self.shell.reserve(capability, true).unwrap();
        let mut reader = BufReader::new(accept(&self.listener));
        let intro: Value = serde_json::from_str(&line(&mut reader)).unwrap();
        assert_eq!(intro, json!({"kind":"shellRun", "token":capability}));
        self.shell.consume(capability).unwrap();
        let launch = json!({"Ok": {
            "interpreter":"sh", "executable":"/bin/sh", "script":script.path(),
            "directory":self.root.path(), "begin":framing.begin(), "end":framing.end(), "timeoutMs":timeout_ms,
        }});
        writeln!(reader.get_mut(), "{launch}").unwrap();
        let started: Value = serde_json::from_str(&line(&mut reader)).unwrap();
        assert_eq!(started["kind"], "started", "{started}");
        assert_ne!(started["child"], started["helper"]);
        reader
    }
}

#[test]
fn a_reviewed_command_can_be_suspended_and_resumed_with_normal_shell_job_control() {
    let terminal = Terminal::open();
    let script =
        ScriptFile::create("printf 'INPUT?'; read value; printf 'continued=%s\\n' \"$value\"")
            .unwrap();
    let framing = Framing::default();
    let mut control = terminal.run(&script, &framing, 10_000);
    terminal.output.await_text("INPUT?");
    terminal.host.write(&terminal.session, b"\x1a").unwrap();
    terminal.output.await_text("suspended");
    terminal.host.write(&terminal.session, b"fg\n").unwrap();
    terminal.host.write(&terminal.session, b"yes\n").unwrap();
    let finished: Value = serde_json::from_str(&line(&mut control)).unwrap();
    assert_eq!(
        finished["outcome"]["Ok"],
        json!({"kind":"success"}),
        "{finished}"
    );
    terminal.output.await_text("continued=yes");
}

#[test]
fn control_cancellation_ends_the_child_without_closing_its_shell() {
    let terminal = Terminal::open();
    let script = ScriptFile::create("sleep 30 & wait").unwrap();
    let framing = Framing::default();
    let mut control = terminal.run(&script, &framing, 10_000);
    control.get_mut().write_all(b"cancel\n").unwrap();
    let finished: Value = serde_json::from_str(&line(&mut control)).unwrap();
    assert_eq!(
        finished["outcome"]["Ok"],
        json!({"kind":"cancelled"}),
        "{finished}"
    );
    terminal.output.await_text(framing.end());
    assert!(
        terminal
            .host
            .sessions()
            .iter()
            .any(|s| s.session == terminal.session)
    );
}

impl Drop for Terminal {
    fn drop(&mut self) {
        self.shell.close();
        let _ = self.host.kill(&self.session);
    }
}

#[test]
fn the_helper_runs_reviewed_bytes_in_zle_and_preserves_input_exit_signal_and_deadline() {
    for (script, input, expected) in [
        (
            "printf 'BODY\\n'; exit 7",
            None,
            json!({"kind":"failed","code":7}),
        ),
        (
            "printf 'INPUT?'; read value; printf 'answer=%s\\n' \"$value\"",
            Some("answer\n"),
            json!({"kind":"success"}),
        ),
        (
            "printf 'INPUT?'; sleep 30",
            Some("\u{3}"),
            json!({"kind":"signalled","signal":2}),
        ),
        ("sleep 30 & wait", None, json!({"kind":"timed_out"})),
    ] {
        let terminal = Terminal::open();
        let script = ScriptFile::create(script).unwrap();
        let framing = Framing::default();
        let timed = expected["kind"] == "timed_out";
        let mut control = terminal.run(&script, &framing, if timed { 100 } else { 5_000 });
        if let Some(input) = input {
            terminal.output.await_text("INPUT?");
            terminal
                .host
                .write(&terminal.session, input.as_bytes())
                .unwrap();
        }
        let finished: Value = serde_json::from_str(&line(&mut control)).unwrap();
        assert_eq!(finished["kind"], "finished");
        if timed {
            assert!(
                finished["outcome"]["Ok"]["kind"] == "timed_out",
                "{finished}"
            );
        } else {
            assert_eq!(finished["outcome"]["Ok"], expected, "{finished}");
        }
        terminal.output.await_text(framing.end());
        let mut filter = FrameFilter::new(framing.clone());
        let captured = filter.push(&terminal.output.bytes.lock());
        assert!(captured.started && captured.ended);
        assert!(!String::from_utf8_lossy(&captured.captured).contains("READY>"));
        if input == Some("answer\n") {
            assert!(String::from_utf8_lossy(&captured.captured).contains("answer=answer"));
        }
        assert!(
            terminal
                .host
                .sessions()
                .iter()
                .any(|s| s.session == terminal.session)
        );
    }
}
