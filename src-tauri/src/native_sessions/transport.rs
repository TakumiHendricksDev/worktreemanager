//! One private local connection from the unmodified Codex TUI to its app-server pipe.
//! WTM forwards provider requests untouched; only the TUI can answer its permissions.

use std::io::ErrorKind;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use serde_json::Value;
use tungstenite::{Message, protocol::WebSocketConfig};
use wtm_core::model::{ExitOutcome, SessionId};
use wtm_core::ports::pipe::PipeSink;

use super::Session;

#[derive(Debug)]
pub(super) struct Output {
    pub session: Arc<Session>,
    pub frames: mpsc::SyncSender<String>,
}

impl PipeSink for Output {
    fn on_line(&self, _: &SessionId, line: &str) {
        if self.session.closed.load(Ordering::Acquire) {
            return;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            return;
        };
        if self.session.server_message(&value) {
            return;
        }
        if self.frames.send(line.to_owned()).is_err() {
            self.session
                .disconnect("The native terminal connection closed");
        }
    }
    fn on_stderr(&self, _: &SessionId, _: &str) {
        // Provider diagnostics stay out of activity: stderr can include config values or tokens.
    }
    fn on_exit(&self, _: &SessionId, _: &ExitOutcome) {
        self.session
            .disconnect("The Codex session backend exited. Restart the session to reconnect.");
    }
}

pub(super) fn listen(
    session: &Arc<Session>,
    path: PathBuf,
    frames: mpsc::Receiver<String>,
) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let listener = UnixListener::bind(&path).map_err(|e| e.to_string())?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| e.to_string())?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let session = Arc::clone(session);
    std::thread::Builder::new()
        .name("wtm-native-codex".into())
        .spawn(move || {
            let result = serve(&session, &listener, &frames);
            let _ = std::fs::remove_file(&path);
            if let Err(error) = result {
                session.disconnect(&error);
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn serve(
    session: &Arc<Session>,
    listener: &UnixListener,
    frames: &mpsc::Receiver<String>,
) -> Result<(), String> {
    let stream = loop {
        if session.closed.load(Ordering::Acquire) {
            return Ok(());
        }
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(e) if e.kind() == ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(e.to_string()),
        }
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;
    let config = WebSocketConfig::default()
        .max_message_size(Some(16 * 1024 * 1024))
        .max_frame_size(Some(16 * 1024 * 1024));
    let mut socket =
        tungstenite::accept_with_config(stream, Some(config)).map_err(|e| e.to_string())?;
    socket
        .get_ref()
        .set_read_timeout(Some(Duration::from_millis(50)))
        .map_err(|e| e.to_string())?;
    while !session.closed.load(Ordering::Acquire) {
        while let Ok(frame) = frames.try_recv() {
            socket
                .send(Message::Text(frame.into()))
                .map_err(|e| e.to_string())?;
        }
        match socket.read() {
            Ok(Message::Text(text)) => {
                let value = serde_json::from_str(&text)
                    .map_err(|e| format!("Invalid native protocol frame: {e}"))?;
                session.client_message(value)?;
            }
            Ok(Message::Close(_)) => return Err(
                "The native terminal disconnected. Restart it to resume the saved conversation."
                    .into(),
            ),
            Ok(_) => {}
            Err(tungstenite::Error::Io(e))
                if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {}
            Err(e) => return Err(format!("Native terminal connection lost: {e}")),
        }
    }
    let _ = socket.close(None);
    Ok(())
}

pub(super) struct TerminalOutput {
    pub session: Arc<Session>,
    pub downstream: Arc<dyn wtm_core::ports::pty::PtySink>,
}
impl wtm_core::ports::pty::PtySink for TerminalOutput {
    fn on_output(&self, id: &SessionId, bytes: &[u8]) {
        self.downstream.on_output(id, bytes);
    }
    fn on_exit(&self, id: &SessionId, outcome: &ExitOutcome) {
        // The PTY may exit before open() can bind its newly allocated id into the registry.
        self.session.shutdown();
        self.downstream.on_exit(id, outcome);
    }
}
