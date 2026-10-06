//! Private shell connections are not MCP requests and cannot approve a command.
//!
//! The helper exits before the Tauri runtime is constructed. Its control socket
//! carries the authoritative child outcome; terminal text carries only output.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::Emitter;
use wtm_core::model::{ExitOutcome, SessionId};
use wtm_core::ports::exec::CancelToken;
use wtm_exec::shell::{Interpreter, close_inherited_control, run_inherited};

use crate::app::App;
use crate::shell_runs::{Phase, Run};

pub const ARGV_FLAG: &str = "--shell-run";
pub const CHANGED_EVENT: &str = "shell-run:changed";
pub const PANE_EVENT: &str = "shell-run:needs-pane";

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
enum Intro {
    ShellIntegration { token: String },
    ShellRun { token: String },
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Launch {
    interpreter: String,
    executable: PathBuf,
    script: PathBuf,
    directory: PathBuf,
    begin: String,
    end: String,
    timeout_ms: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum Report {
    Started {
        child: u32,
        helper: u32,
    },
    Finished {
        outcome: Result<ExitOutcome, String>,
    },
}

fn send(stream: &mut UnixStream, value: &impl Serialize) -> Result<(), String> {
    serde_json::to_writer(&mut *stream, value).map_err(|e| e.to_string())?;
    stream.write_all(b"\n").map_err(|e| e.to_string())
}

fn line(reader: &mut impl BufRead, limit: u64) -> Result<String, String> {
    let mut text = String::new();
    reader
        .take(limit)
        .read_line(&mut text)
        .map_err(|e| e.to_string())?;
    if !text.ends_with('\n') {
        return Err("The private shell connection closed or exceeded its message limit.".into());
    }
    Ok(text)
}

pub fn is_private_request(text: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|value| {
            value
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .is_some_and(|kind| matches!(kind.as_str(), "shellIntegration" | "shellRun"))
}

pub fn serve(
    handle: &tauri::AppHandle,
    app: &Arc<App>,
    text: &str,
    mut reader: BufReader<UnixStream>,
    mut stream: UnixStream,
) {
    let Ok(intro) = serde_json::from_str::<Intro>(text) else {
        return;
    };
    match intro {
        Intro::ShellIntegration { token } => {
            let Some(shell) = app.shell_admission.for_token(&token) else {
                return;
            };
            let Ok(writer) = stream.try_clone() else {
                return;
            };
            if shell.connect(writer).is_err() {
                return;
            }
            while let Ok(message) = line(&mut reader, 256) {
                let parts: Vec<_> = message.split_whitespace().collect();
                match parts.as_slice() {
                    ["prompt", epoch, jobs] => {
                        if let (Ok(epoch), Ok(jobs)) = (epoch.parse(), jobs.parse()) {
                            shell.prompt(epoch, jobs);
                        }
                    }
                    ["busy", _] => shell.busy(),
                    ["refused", capability] => {
                        if let Ok(run) = app.shell_runs.by_capability(capability) {
                            run.interrupt(
                                "The shell is no longer at an empty primary prompt. Nothing ran.",
                            );
                            settle(handle, app, &run);
                        }
                    }
                    _ => break,
                }
            }
            shell.close();
            for run in app.shell_runs.all() {
                let session = run.state.lock().session.clone();
                if session
                    .as_deref()
                    .and_then(|session| app.shell_admission.get(session))
                    .is_some_and(|candidate| Arc::ptr_eq(&candidate, &shell))
                {
                    cancel(handle, app, &run, "The shell control connection closed.");
                }
            }
        }
        Intro::ShellRun { token } => {
            let launch = consume(app, &token, &stream);
            let run = launch.as_ref().ok().map(|(run, _)| Arc::clone(run));
            let reply = launch.map(|(_, launch)| launch);
            if send(&mut stream, &reply).is_err() || reply.is_err() {
                if let Some(run) = run {
                    cancel(
                        handle,
                        app,
                        &run,
                        "The helper could not receive its command.",
                    );
                } else if let Ok(run) = app.shell_runs.by_capability(&token) {
                    run.interrupt(reply.err().as_deref().unwrap_or("The helper disconnected."));
                    settle(handle, app, &run);
                }
                return;
            }
            let Some(run) = run else {
                return;
            };
            announce(handle, &run);
            let result = receive_reports(app, &run, &mut reader);
            if let Err(problem) = result {
                cancel(handle, app, &run, &problem);
            } else {
                // The PTY reader and this socket have independent scheduling.
                // Wait for the end record, never for a quiet interval in output.
                let deadline = app.clock.monotonic_ms() + 5_000;
                let mut state = run.state.lock();
                while !state.output.complete() && app.clock.monotonic_ms() < deadline {
                    run.changed.wait_for(&mut state, Duration::from_millis(50));
                }
                if !state.phase.terminal() {
                    if state.output.complete() {
                        state.phase = Phase::Completed;
                    } else {
                        state.phase = Phase::Interrupted;
                        state.problem =
                            Some("The command ended but its output boundary was lost.".into());
                        state
                            .output
                            .interrupt("The command ended but its output boundary was lost.");
                    }
                }
                drop(state);
                settle(handle, app, &run);
            }
        }
    }
}

fn consume(app: &App, token: &str, stream: &UnixStream) -> Result<(Arc<Run>, Launch), String> {
    let run = app.shell_runs.by_capability(token)?;
    validate(app, &run)?;
    let control = stream.try_clone().map_err(|e| e.to_string())?;
    let session = run
        .state
        .lock()
        .session
        .clone()
        .ok_or("This run has no shell.")?;
    let shell = app
        .shell_admission
        .get(&session)
        .ok_or("The shell closed.")?;
    shell.consume_if(token, || {
        let mut state = run.state.lock();
        if state.phase != Phase::Reserved || state.capability.as_deref() != Some(token) {
            return Err("This run was already consumed or cancelled.".into());
        }
        if state.grant
            && run
                .prepared
                .source
                .home()
                .is_none_or(|home| !app.shell_runs.has_grant(home, &run.prepared.target))
        {
            return Err("Home's worktree grant was revoked. Nothing ran.".into());
        }
        state.phase = Phase::Running;
        state.control = Some(control);
        Ok(())
    })?;
    run.changed.notify_all();
    app.shell_runs.consume_capability(token);
    let launch = Launch {
        interpreter: run.prepared.interpreter.name().into(),
        executable: run.prepared.executable.clone(),
        script: run.prepared.script.path().into(),
        directory: run.prepared.target.directory.clone(),
        begin: run.framing.begin().into(),
        end: run.framing.end().into(),
        timeout_ms: crate::commands::SHELL_TIMEOUT_MS,
    };
    Ok((run, launch))
}

pub fn validate(app: &App, run: &Run) -> Result<(), String> {
    run.prepared.target.revalidate(app)?;
    if let Some(home) = run.prepared.source.home() {
        if !app
            .overview_of(home)
            .is_some_and(|home| home.scope.is_home())
        {
            return Err("The requesting Home session is no longer running.".into());
        }
        let granted = run.state.lock().grant;
        if granted && !app.shell_runs.has_grant(home, &run.prepared.target) {
            return Err("Home's worktree grant was revoked. Nothing ran.".into());
        }
    }
    Ok(())
}

fn receive_reports(app: &App, run: &Run, reader: &mut impl BufRead) -> Result<(), String> {
    loop {
        let report: Report =
            serde_json::from_str(&line(reader, 8_192)?).map_err(|e| e.to_string())?;
        let mut state = run.state.lock();
        match report {
            Report::Started { child, helper } => {
                if state.phase != Phase::Running || state.child.is_some() || child < 2 || helper < 2
                {
                    return Err("The helper reported an unexpected start.".into());
                }
                state.child = Some(child);
                state.helper = Some(helper);
            }
            Report::Finished { outcome } => {
                state.child = None;
                state.helper = None;
                state.control = None;
                let outcome = outcome?;
                state.output.finish(outcome);
                run.changed.notify_all();
                return Ok(());
            }
        }
        // Liveness is checked on control transitions too, independently of UI.
        drop(state);
        if run
            .prepared
            .source
            .home()
            .is_some_and(|home| app.overview_of(home).is_none())
        {
            return Err("Home closed while the command was starting.".into());
        }
    }
}

pub fn announce(handle: &tauri::AppHandle, run: &Run) {
    let _ = handle.emit(CHANGED_EVENT, run.view());
}

pub fn settle(handle: &tauri::AppHandle, app: &App, run: &Run) {
    release(app, run);
    announce(handle, run);
}

pub fn release(app: &App, run: &Run) {
    let (session, capability) = {
        let state = run.state.lock();
        (state.session.clone(), state.capability.clone())
    };
    if let (Some(session), Some(capability)) = (session, capability)
        && let Some(shell) = app.shell_admission.get(&session)
    {
        shell.release(&capability);
    }
    app.shell_runs.release(run);
    run.changed.notify_all();
}

pub fn capture(handle: &tauri::AppHandle, app: &App, session: &SessionId, bytes: &[u8]) -> Vec<u8> {
    let Some(run) = app.shell_runs.active(session.as_str()) else {
        return bytes.to_vec();
    };
    let (display, complete) = {
        let mut state = run.state.lock();
        let display = state.output.push(bytes);
        let complete = state.phase == Phase::Running && state.output.complete();
        if complete {
            state.phase = Phase::Completed;
        }
        (display, complete)
    };
    run.changed.notify_all();
    if complete {
        settle(handle, app, &run);
    }
    display
}

pub fn cancel(handle: &tauri::AppHandle, app: &App, run: &Run, reason: &str) {
    interrupt(run, reason);
    settle(handle, app, run);
}

pub fn interrupt(run: &Run, reason: &str) {
    let (child, helper, control) = {
        let mut state = run.state.lock();
        if state.phase.terminal() {
            return;
        }
        state.phase = Phase::Interrupted;
        state.problem = Some(reason.into());
        state.output.interrupt(reason);
        (
            state.child.take(),
            state.helper.take(),
            state.control.take(),
        )
    };
    if let Some(control) = control {
        let _ = control.shutdown(Shutdown::Both);
    }
    if let Some(child) = child {
        wtm_exec::signal::terminate_group(child);
    }
    if let Some(helper) = helper {
        wtm_exec::signal::terminate_helper(helper);
    }
    run.changed.notify_all();
}

/// Only the private binary mode calls this; no GUI, app state or live data path
/// is constructed. The caller supplies the already-known socket explicitly.
pub fn run_helper(socket: &Path, capability: &str, inherited_fd: i32) -> Result<(), String> {
    close_inherited_control(inherited_fd)?;
    let mut stream = UnixStream::connect(socket).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    send(
        &mut stream,
        &Intro::ShellRun {
            token: capability.into(),
        },
    )?;
    let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
    let launch: Result<Launch, String> =
        serde_json::from_str(&line(&mut reader, 16_384)?).map_err(|e| e.to_string())?;
    let launch = launch?;
    reader
        .get_ref()
        .set_read_timeout(None)
        .map_err(|e| e.to_string())?;
    let cancel = CancelToken::new();
    let watcher = cancel.clone();
    std::thread::Builder::new()
        .name("wtm-shell-cancel".into())
        .spawn(move || {
            let mut byte = [0];
            let _ = reader.read(&mut byte);
            watcher.cancel();
        })
        .map_err(|e| e.to_string())?;
    let marker = |text: &str| -> Result<(), String> {
        let mut stdout = std::io::stdout().lock();
        stdout
            .write_all(text.as_bytes())
            .and_then(|()| stdout.flush())
            .map_err(|e| e.to_string())
    };
    marker(&launch.begin)?;
    let outcome = run_inherited(
        Interpreter::parse(&launch.interpreter)?,
        &launch.executable,
        &launch.script,
        &launch.directory,
        launch.timeout_ms,
        &cancel,
        |child| {
            send(
                &mut stream,
                &Report::Started {
                    child,
                    helper: std::process::id(),
                },
            )
        },
    );
    marker(&launch.end)?;
    send(&mut stream, &Report::Finished { outcome })
}
