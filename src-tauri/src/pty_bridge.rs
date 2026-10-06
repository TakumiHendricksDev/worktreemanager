//! Streaming a PTY session to the webview.
//!
//! # Why bytes and not strings
//!
//! A terminal's output is not guaranteed to split on UTF-8 boundaries — a chunk can end
//! mid-character, and an escape sequence can be split across reads. Decoding here would
//! corrupt both. So chunks are forwarded as bytes (base64 for JSON transport) and reassembled
//! by the terminal emulator, which is the component that actually knows how.
//!
//! # Why a dock shell's output is also kept
//!
//! So a pane can move to another window, or survive a reload, with its history. The xterm that
//! received a chunk used to be the only place it existed. See `App::record_shell_output`.
//!
//! # Why events and not a channel per call
//!
//! A session outlives the command that started it: `create_worktree` returns once setup
//! finishes, but the transcript stays on screen. Emitting to a window event with the session
//! id lets the frontend attach a terminal to a session at any point, including after the
//! command that spawned it has returned.

use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::app::App;
use wtm_core::model::{ExitOutcome, SessionId};
use wtm_core::ports::pty::PtySink;

/// Event name for a chunk of output.
pub const OUTPUT_EVENT: &str = "pty:output";

/// Event name for a session finishing.
pub const EXIT_EVENT: &str = "pty:exit";

/// Event name for pipeline progress.
pub const PROGRESS_EVENT: &str = "wtm:progress";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct OutputEvent {
    session: String,
    /// Base64, because JSON cannot carry arbitrary bytes and terminal output is arbitrary.
    chunk_base64: String,
    /// The chunk's number in a dock shell's replay ring; `None` for every other session.
    ///
    /// What lets a window that attaches late drop the chunks its snapshot already contained. See
    /// `App::record_shell_output`.
    seq: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ExitEvent {
    session: String,
    outcome: ExitOutcome,
    /// A short human sentence, so the UI does not have to switch on the enum to say
    /// something useful.
    summary: String,
}

/// Forwards session output to the window as Tauri events.
pub struct EventSink {
    app: AppHandle,
    /// Set for a dock shell only, whose output is kept for a window that attaches later.
    ///
    /// Not for a pipeline's sessions — setup, an action, a worktree removal. Their panes never
    /// leave the view that started them, and keeping their output would hold it for nobody.
    recorder: Option<Arc<App>>,
}

impl std::fmt::Debug for EventSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventSink").finish_non_exhaustive()
    }
}

impl EventSink {
    #[must_use]
    pub fn new(app: AppHandle) -> Arc<Self> {
        Arc::new(Self {
            app,
            recorder: None,
        })
    }

    /// A sink that also keeps what it forwards, for a dock shell.
    #[must_use]
    pub fn recording(app: AppHandle, recorder: Arc<App>) -> Arc<Self> {
        Arc::new(Self {
            app,
            recorder: Some(recorder),
        })
    }
}

impl PtySink for EventSink {
    fn on_output(&self, session: &SessionId, chunk: &[u8]) {
        let filtered;
        let chunk = if let Some(app) = &self.recorder {
            filtered = crate::shell_control::capture(&self.app, app, session, chunk);
            filtered.as_slice()
        } else {
            chunk
        };
        if chunk.is_empty() {
            return;
        }
        // Kept before it is emitted, and numbered by the same call, so a window can never be sent
        // a chunk the ring does not have — the order `agent_bridge` keeps for the same reason.
        let seq = self
            .recorder
            .as_ref()
            .map(|app| app.record_shell_output(session, chunk));
        let event = OutputEvent {
            session: session.as_str().to_owned(),
            chunk_base64: base64_encode(chunk),
            seq,
        };
        // A failed emit means the window is gone. Nothing useful to do, and it must not
        // interrupt the reader thread.
        if let Err(err) = self.app.emit(OUTPUT_EVENT, event) {
            tracing::debug!(error = %err, "could not emit pty output");
        }
    }

    fn on_exit(&self, session: &SessionId, outcome: &ExitOutcome) {
        if let Some(app) = &self.recorder {
            if let Some(run) = app.shell_runs.active(session.as_str()) {
                crate::shell_control::cancel(
                    &self.app,
                    app,
                    &run,
                    "The shell exited before its run settled.",
                );
            }
            app.shell_admission.close(session.as_str());
        }
        let event = ExitEvent {
            session: session.as_str().to_owned(),
            outcome: outcome.clone(),
            summary: outcome.describe(),
        };
        if let Err(err) = self.app.emit(EXIT_EVENT, event) {
            tracing::debug!(error = %err, "could not emit pty exit");
        }
    }
}

/// Forwards pipeline progress to the window.
pub struct ProgressBridge {
    app: AppHandle,
}

impl std::fmt::Debug for ProgressBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProgressBridge").finish_non_exhaustive()
    }
}

impl ProgressBridge {
    #[must_use]
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl wtm_core::ports::progress::ProgressSink for ProgressBridge {
    fn emit(&self, event: wtm_core::ports::progress::ProgressEvent) {
        if let Err(err) = self.app.emit(PROGRESS_EVENT, event) {
            tracing::debug!(error = %err, "could not emit progress");
        }
    }
}

/// Standard base64, hand-rolled.
///
/// Three lines and no dependency. The alternative is pulling in a crate to do the same thing
/// for one call site, and this has no configuration to get wrong.
pub(crate) fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = chunk.get(1).copied().map_or(0, u32::from);
        let b2 = chunk.get(2).copied().map_or(0, u32::from);
        let triple = (b0 << 16) | (b1 << 8) | b2;

        out.push(char::from(ALPHABET[((triple >> 18) & 0x3F) as usize]));
        out.push(char::from(ALPHABET[((triple >> 12) & 0x3F) as usize]));
        out.push(if chunk.len() > 1 {
            char::from(ALPHABET[((triple >> 6) & 0x3F) as usize])
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            char::from(ALPHABET[(triple & 0x3F) as usize])
        } else {
            '='
        });
    }
    out
}

/// Standard base64 decode, for input coming back from the terminal.
pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    fn value(c: u8) -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => u32::from(c - b'A'),
            b'a'..=b'z' => u32::from(c - b'a') + 26,
            b'0'..=b'9' => u32::from(c - b'0') + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    }

    let bytes: Vec<u8> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);

    for chunk in bytes.chunks(4) {
        if chunk.len() < 2 {
            return None;
        }
        // Checked positionally rather than counted: base64 padding only ever occupies the
        // last two bytes of a quartet, and clippy's suggestion to add a byte-counting crate
        // for a four-byte slice is not a trade worth making.
        let padding =
            usize::from(chunk.get(2) == Some(&b'=')) + usize::from(chunk.get(3) == Some(&b'='));
        let mut triple = 0_u32;
        for (index, byte) in chunk.iter().enumerate() {
            let bits = if *byte == b'=' { 0 } else { value(*byte)? };
            triple |= bits << (18 - 6 * index);
        }
        out.push(((triple >> 16) & 0xFF) as u8);
        if padding < 2 {
            out.push(((triple >> 8) & 0xFF) as u8);
        }
        if padding < 1 {
            out.push((triple & 0xFF) as u8);
        }
    }

    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_standard_alphabet_and_padding() {
        // The canonical RFC 4648 vectors, so a hand-rolled encoder cannot drift.
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn round_trips_arbitrary_bytes_including_invalid_utf8() {
        // The case this exists for: terminal output is not valid UTF-8 in general, and a
        // chunk boundary can land mid-character.
        for case in [
            vec![],
            vec![0x00],
            vec![0xFF, 0xFE, 0xFD],
            // A UTF-8 sequence cut in half.
            vec![0xE6, 0x97],
            // An ANSI colour escape.
            b"\x1b[31mred\x1b[0m".to_vec(),
            (0..=255).collect::<Vec<u8>>(),
        ] {
            let encoded = base64_encode(&case);
            assert_eq!(
                base64_decode(&encoded).as_deref(),
                Some(case.as_slice()),
                "round trip failed for {case:?}"
            );
        }
    }

    #[test]
    fn decode_rejects_garbage_rather_than_guessing() {
        assert!(base64_decode("!!!!").is_none());
        assert!(base64_decode("Z").is_none());
    }

    #[test]
    fn decode_tolerates_whitespace() {
        assert_eq!(
            base64_decode("Zm9v YmFy").as_deref(),
            Some(b"foobar".as_slice())
        );
    }
}
