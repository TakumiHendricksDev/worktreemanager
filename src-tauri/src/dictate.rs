//! Driving dictation: record, transcribe, hand back text.
//!
//! `wtm-dictate` builds every argv and parses every reply and cannot spawn anything. This is where
//! the processes actually run, for the same reason `open_agent` lives in the composition root: a
//! crate that can spawn is a crate whose purity nothing enforces.
//!
//! # Recording is a cancelled command, not a stream
//!
//! There is no audio in this process at any point. `SoX` writes raw PCM to a temp file and
//! "stop" means cancelling the invocation, which [`wtm_exec::Runner`] implements by signalling the
//! process group — the same mechanism a cancelled setup script uses. That is why the recording is
//! headerless: a killed WAV writer leaves a header describing a length the file does not have,
//! where raw PCM has nothing to be wrong about.
//!
//! The consequence worth knowing is that a recording always ends in a "cancelled" or "timed out"
//! error from the runner's point of view, and both are successes here. What matters is whether the
//! file has bytes in it.
//!
//! # Why the backend re-checks the preference
//!
//! [`start`] refuses unless `ui.dictate` is `on`, which the frontend has already checked. That is
//! deliberate duplication: the webview is the part of this application most exposed to content it
//! did not write, and "turning the microphone on" is not a capability that should rest on a check
//! living there. The CSP means an injected script cannot reach the network itself — this is what
//! stops it reaching the microphone through a command that would.

use std::path::PathBuf;
use std::sync::Arc;

use wtm_core::ports::config::ConfigStore;
use wtm_core::ports::dictate::{DictateError, Utterance};
use wtm_core::ports::{CancelToken, Invocation};

use crate::app::App;

/// How long a single recording may run before the runner kills it.
///
/// A cap rather than an option, and mandatory for the same reason every captured command's timeout
/// is: the failure being prevented is a microphone left open by a user who walked away, which costs
/// them money and privacy rather than merely a stuck process. Overridable downward by
/// `ui.dictate_max_seconds`; never upward past this.
const MAX_RECORD_SECONDS: u64 = 300;

/// The default cap when the preference is unset.
const DEFAULT_RECORD_SECONDS: u64 = 120;

/// How long the transcription request may take.
const TRANSCRIBE_TIMEOUT_MS: u64 = 60_000;

/// A recording in progress.
struct Recording {
    cancel: CancelToken,
    path: PathBuf,
}

/// One recording at a time, globally.
///
/// Not per pane, because there is one microphone and one person: two panes recording at once would
/// be two processes fighting over the same input device, and the second `rec` fails in a way that
/// reads as a wtm bug. Starting a second recording cancels the first.
#[derive(Default)]
pub struct Dictation(parking_lot::Mutex<Option<Recording>>);

impl std::fmt::Debug for Dictation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dictation")
            .field("recording", &self.0.lock().is_some())
            .finish()
    }
}

/// What the UI needs to know before offering a microphone button.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DictationStatus {
    /// Everything needed is present and a key is stored.
    pub ready: bool,
    /// Whether a credential is stored. **Never the credential.**
    ///
    /// The whole reason this is a `bool`: `§6a`'s rule about environment values is that no value
    /// crosses the IPC boundary, not that no secret does, and a key is a value. The frontend can
    /// know that setup is done and cannot learn what with.
    pub key_set: bool,
    /// Programs that are needed and missing, so the message can name them.
    pub missing: Vec<String>,
}

/// Which secret store this machine has, by asking PATH rather than the compiler.
fn keystore(app: &Arc<App>) -> Option<wtm_dictate::Keystore> {
    [
        wtm_dictate::Keystore::Keychain,
        wtm_dictate::Keystore::SecretTool,
    ]
    .into_iter()
    .find(|store| app.runner.which(store.program()).is_some())
}

/// A short invocation that is allowed to fail, for the keychain calls.
fn quiet(app: &Arc<App>, argv: Vec<String>, stdin: Option<String>) -> Option<String> {
    let mut inv = Invocation::new(argv, std::env::temp_dir(), 10_000);
    if let Some(stdin) = stdin {
        inv = inv.with_stdin(stdin);
    }
    let out = app
        .runner
        .run_allow_failure(&inv, &CancelToken::new())
        .ok()?;
    out.is_success().then(|| out.stdout.trim().to_owned())
}

/// The stored key, or `None`.
fn stored_key(app: &Arc<App>) -> Option<String> {
    let store = keystore(app)?;
    quiet(app, store.read_argv(), None).filter(|k| !k.is_empty())
}

/// Report whether dictation can be offered at all.
pub fn status(app: &Arc<App>) -> DictationStatus {
    let mut missing = Vec::new();
    for program in ["rec", "curl"] {
        if app.runner.which(program).is_none() {
            missing.push(program.to_owned());
        }
    }
    let store = keystore(app);
    if store.is_none() {
        missing.push(wtm_dictate::Keystore::Keychain.program().to_owned());
    }
    let key_set = stored_key(app).is_some();
    DictationStatus {
        ready: missing.is_empty() && key_set,
        key_set,
        missing,
    }
}

/// Store a transcription key.
///
/// One-way across IPC: this comes *in* and nothing ever reads it back out. See
/// [`DictationStatus::key_set`].
pub fn set_key(app: &Arc<App>, key: &str) -> Result<(), String> {
    let key = key.trim();
    if key.is_empty() {
        clear_key(app);
        return Ok(());
    }
    let store = keystore(app).ok_or_else(|| {
        "no secret store on PATH — dictation needs `security` on macOS or `secret-tool` on Linux"
            .to_owned()
    })?;
    let (argv, stdin) = store.write(key);
    quiet(app, argv, Some(stdin))
        .map(|_| ())
        .ok_or_else(|| "the secret store refused to save the key".to_owned())
}

/// Forget the stored key.
///
/// Infallible on purpose, which is why it returns nothing. "There was no key" and "the key is gone
/// now" are the same state from a Remove button's point of view, and reporting the first as an
/// error would make clearing an already-clear setting look like a failure.
pub fn clear_key(app: &Arc<App>) {
    if let Some(store) = keystore(app) {
        let _ = quiet(app, store.delete_argv(), None);
    }
}

/// Read the utterance settings out of the user's preferences.
fn utterance(app: &Arc<App>) -> Utterance {
    let pref = |key: &str| app.config.user_pref(key).ok().flatten();
    Utterance {
        language: pref("ui.dictate_language")
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "en".to_owned()),
        // Comma-separated, because `set_pref` is stringly typed by design — see `user.rs`, which
        // documents unknown keys landing in `ui.extra` so a preference needs no Rust change. The
        // cost is this parse, and the empty entries a trailing comma leaves are dropped downstream.
        keyterms: pref("ui.dictate_keyterms")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_owned)
            .collect(),
        ..Utterance::default()
    }
}

/// Begin recording. Cancels any recording already running.
pub fn start(app: &Arc<App>) -> Result<(), String> {
    // Defence in depth, not a duplicate check. See the module docs.
    if app.config.user_pref("ui.dictate").ok().flatten().as_deref() != Some("on") {
        return Err("dictation is turned off".to_owned());
    }
    let status = status(app);
    if !status.missing.is_empty() {
        return Err(format!("dictation needs {}", status.missing.join(" and ")));
    }

    let seconds = app
        .config
        .user_pref("ui.dictate_max_seconds")
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_RECORD_SECONDS)
        .clamp(1, MAX_RECORD_SECONDS);

    // A fresh name per recording, so a previous file cannot be transcribed twice if a stop races a
    // start. Not in the worktree: audio is not part of anybody's repository.
    let path = std::env::temp_dir().join(format!("wtm-dictate-{}.raw", uuid::Uuid::new_v4()));
    let cancel = CancelToken::new();

    // Replaces whatever was recording, and cancelling the old one is what frees the input device.
    if let Some(previous) = self_replace(
        app,
        Recording {
            cancel: cancel.clone(),
            path: path.clone(),
        },
    ) {
        previous.cancel.cancel();
        let _ = std::fs::remove_file(&previous.path);
    }

    let argv = wtm_dictate::recorder_argv(&path.to_string_lossy(), &utterance(app));
    let inv = Invocation::new(argv, std::env::temp_dir(), seconds * 1_000);
    let runner = Arc::clone(&app.runner);
    // A thread rather than a task: the runner blocks, which is the whole reason ARCHITECTURE §3
    // keeps these ports synchronous. It ends when the recording is cancelled or hits its cap.
    std::thread::spawn(move || {
        let _ = runner.run_allow_failure(&inv, &cancel);
    });
    Ok(())
}

/// Install `next` as the current recording, returning whatever it displaced.
fn self_replace(app: &Arc<App>, next: Recording) -> Option<Recording> {
    app.dictation.0.lock().replace(next)
}

/// Stop recording and transcribe what was captured.
pub fn stop(app: &Arc<App>) -> Result<String, String> {
    let Some(recording) = app.dictation.0.lock().take() else {
        return Err("nothing was being recorded".to_owned());
    };
    // Signals the process group, which is what makes SoX close the file.
    recording.cancel.cancel();

    // The recorder is a separate process being killed, so the bytes are not all there the moment
    // `cancel` returns. Bounded polling rather than a sleep: a short recording is ready almost at
    // once, and waiting a flat interval would add that delay to every single dictation.
    let audio = wait_for_audio(&recording.path);
    let result = transcribe(app, &audio);
    // Deleted whatever happened. Audio outlives its usefulness the moment it is text, and a temp
    // directory full of recordings of somebody talking is not a thing to leave behind.
    let _ = std::fs::remove_file(&recording.path);
    result
}

/// Wait briefly for the killed recorder to have flushed, then read the file.
fn wait_for_audio(path: &std::path::Path) -> Vec<u8> {
    let mut last = 0usize;
    for _ in 0..40 {
        let bytes = std::fs::read(path).unwrap_or_default();
        // Two consecutive equal non-zero reads means the writer is done with it.
        if !bytes.is_empty() && bytes.len() == last {
            return bytes;
        }
        last = bytes.len();
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    std::fs::read(path).unwrap_or_default()
}

/// Send the audio and return the transcript.
fn transcribe(app: &Arc<App>, audio: &[u8]) -> Result<String, String> {
    // 16-bit mono at 16 kHz is 32 000 bytes a second, so this is about a tenth of a second. Below
    // that there is nothing to transcribe and the request would be spent for a certain `Silent`.
    if audio.len() < 3_200 {
        return Err(human(&DictateError::Silent));
    }
    let key = stored_key(app).ok_or_else(|| human(&DictateError::NoKey))?;
    let utterance = utterance(app);

    // Written next to the recording and removed straight after: `curl --data-binary @-` would read
    // the body from stdin, but stdin is carrying the credential, and only one of the two can go
    // that way. The audio is the half that is not a secret.
    let body_path = std::env::temp_dir().join(format!("wtm-dictate-{}.body", uuid::Uuid::new_v4()));
    std::fs::write(&body_path, audio).map_err(|e| format!("could not stage the recording: {e}"))?;

    let inv = Invocation::new(
        wtm_dictate::request_argv(&body_path.to_string_lossy()),
        std::env::temp_dir(),
        TRANSCRIBE_TIMEOUT_MS,
    )
    .with_stdin(wtm_dictate::request_config(&utterance, &key));

    let out = app.runner.run_allow_failure(&inv, &CancelToken::new());
    let _ = std::fs::remove_file(&body_path);

    let out = out.map_err(|e| format!("could not run curl: {e}"))?;
    // `--fail-with-body` makes curl exit non-zero on an HTTP error while still writing the body,
    // so the exit code is not the interesting part — the appended status is. stderr only matters
    // when there is no reply at all to parse.
    wtm_dictate::parse_response(&if out.stdout.trim().is_empty() {
        out.stderr.clone()
    } else {
        out.stdout.clone()
    })
    .map_err(|e| human(&e))
}

/// A dictation failure in words a person can act on.
///
/// Each one names the remedy rather than the cause, which is the difference between a message that
/// helps and a message that is merely accurate.
fn human(error: &DictateError) -> String {
    match error {
        DictateError::NoKey => "No transcription key. Add one in Settings → Advanced.".to_owned(),
        DictateError::Unauthorized => {
            "The transcription key was rejected. Check it in Settings → Advanced.".to_owned()
        }
        DictateError::Silent => "Nothing was recorded. Is the microphone muted?".to_owned(),
        DictateError::Unreachable { message } => {
            format!("Could not reach the transcription service. {message}")
        }
        DictateError::Refused { status, message } => {
            format!("The transcription service refused the request ({status}). {message}")
        }
    }
}
