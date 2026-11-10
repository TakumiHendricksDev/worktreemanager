//! One private local connection from the unmodified Codex TUI to its app-server pipe.
//! WTM forwards provider requests untouched; only the TUI can answer its permissions.
//!
//! Three threads, and none of them waits on another's peer. The pipe reader parses each provider
//! frame and queues it without blocking; a writer drains the queue to the socket; the relay thread
//! reads the TUI's frames into the pipe. A TUI that stops reading fills the queue, and the
//! connection ends visibly, instead of stalling the reader that also carries WTM's own replies.
//! Nothing polls: `shutdown` wakes a waiting `accept` and hangs up a live connection.

use std::io::{ErrorKind, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, TrySendError};

use parking_lot::Mutex;
use serde_json::Value;
use tungstenite::Message;
use tungstenite::protocol::{Role, WebSocket, WebSocketConfig};
use wtm_core::model::{ExitOutcome, SessionId};
use wtm_core::ports::pipe::PipeSink;

use super::Session;

/// Provider frames waiting for the TUI. A TUI this far behind has stopped reading.
pub(super) const FRAMES: usize = 512;

const BEHIND: &str = "The native terminal stopped reading wtm's connection, so wtm closed it rather than stall the provider. Reconnect to resume the saved conversation.";
const CLOSED: &str = "The native terminal connection closed";
const TUI_LEFT: &str =
    "The native terminal disconnected. Restart it to resume the saved conversation.";

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
        // Never wait here. This thread also carries WTM's own replies, and a TUI that missed
        // frames is out of step anyway, so it is disconnected rather than fed a gap.
        match self.frames.try_send(line.to_owned()) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => self.session.disconnect(BEHIND),
            Err(TrySendError::Disconnected(_)) => self.session.disconnect(CLOSED),
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

/// What it takes to stop the relay without polling for the session's end.
#[derive(Debug, Default)]
pub(super) struct Relay {
    /// The listening socket, until the TUI connects.
    socket: Option<PathBuf>,
    /// The TUI's connection, once it has.
    stream: Option<UnixStream>,
}

impl Relay {
    /// End a live connection, so the TUI reports its backend gone instead of waiting on it.
    pub(super) fn hang_up(&mut self) {
        if let Some(stream) = self.stream.take() {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }

    /// Also wake an `accept` that is still waiting for a TUI that will now never come.
    pub(super) fn close(&mut self) {
        self.hang_up();
        if let Some(socket) = self.socket.take() {
            let _ = UnixStream::connect(socket);
        }
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
    session.relay.lock().socket = Some(path.clone());
    let session = Arc::clone(session);
    std::thread::Builder::new()
        .name("wtm-native-codex".into())
        .spawn(move || {
            let result = serve(&session, &listener, frames);
            let _ = std::fs::remove_file(&path);
            if let Err(error) = result {
                session.disconnect(&error);
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(16 * 1024 * 1024))
        .max_frame_size(Some(16 * 1024 * 1024))
}

fn serve(
    session: &Arc<Session>,
    listener: &UnixListener,
    frames: mpsc::Receiver<String>,
) -> Result<(), String> {
    let (stream, _) = listener.accept().map_err(|e| e.to_string())?;
    {
        let mut relay = session.relay.lock();
        relay.socket = None;
        relay.stream = Some(stream.try_clone().map_err(|e| e.to_string())?);
    }
    // After attaching, never before: a shutdown that ran first found nothing to hang up, and this
    // is the only place left that can notice it.
    if session.closed.load(Ordering::Acquire) {
        return Ok(());
    }
    let serialized = Arc::new(Mutex::new(stream.try_clone().map_err(|e| e.to_string())?));
    let outgoing = Duplex {
        read: stream.try_clone().map_err(|e| e.to_string())?,
        write: Arc::clone(&serialized),
    };
    let mut socket = tungstenite::accept_with_config(
        Duplex {
            read: stream,
            write: serialized,
        },
        Some(config()),
    )
    .map_err(|e| e.to_string())?;
    let writer = WebSocket::from_raw_socket(outgoing, Role::Server, Some(config()));
    let forwarding = Arc::clone(session);
    std::thread::Builder::new()
        .name("wtm-native-codex-out".into())
        .spawn(move || forward(&forwarding, writer, &frames))
        .map_err(|e| e.to_string())?;
    loop {
        match socket.read() {
            Ok(Message::Text(text)) => {
                let value = serde_json::from_str(&text)
                    .map_err(|e| format!("Invalid native protocol frame: {e}"))?;
                session.client_message(value)?;
            }
            Ok(Message::Close(_)) => return Err(TUI_LEFT.into()),
            Ok(_) => {}
            Err(_) if session.closed.load(Ordering::Acquire) => return Ok(()),
            Err(tungstenite::Error::Io(e)) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(format!("Native terminal connection lost: {e}")),
        }
    }
}

fn forward(session: &Session, mut socket: WebSocket<Duplex>, frames: &mpsc::Receiver<String>) {
    for frame in frames {
        if let Err(error) = socket.send(Message::Text(frame.into())) {
            session.disconnect(&format!("Native terminal connection lost: {error}"));
            return;
        }
    }
}

/// One socket as two `WebSocket`s, the reader's and the writer's.
///
/// The reader answers pings and closes by itself, so both halves write. Each `write` here is
/// every complete frame tungstenite has buffered, written whole under one lock, so a pong can
/// never land in the middle of a large provider frame.
struct Duplex {
    read: UnixStream,
    write: Arc<Mutex<UnixStream>>,
}

impl Read for Duplex {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.read.read(buf)
    }
}

impl Write for Duplex {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.write.lock().write_all(buf)?;
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.write.lock().flush()
    }
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
