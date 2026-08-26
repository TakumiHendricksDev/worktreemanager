//! Reporting progress out of a long-running use-case.
//!
//! The create pipeline runs on a blocking thread and can take minutes, most of it
//! inside someone else's script. Without this, the UI would have nothing to show
//! between "Create" and "done", which is exactly the window in which a user decides
//! the app is broken.
//!
//! Events are data rather than formatted strings so the frontend controls wording
//! and can localize or restyle without a Rust change.

use crate::model::PlanWarning;

/// Something worth telling the user about, mid-operation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum ProgressEvent {
    /// A pipeline stage began. `index`/`total` drive a determinate progress bar.
    Stage {
        id: String,
        label: String,
        index: u16,
        total: u16,
    },
    /// A lookup command started — shown as a per-field spinner.
    LookupStarted {
        id: String,
    },
    /// A lookup resolved, with the tokens it produced. Surfaced so the user can
    /// see what the tracker actually returned before committing to a branch name.
    LookupFinished {
        id: String,
        tokens: std::collections::BTreeMap<String, String>,
    },
    /// About to run a command. The argv is included because the review screen
    /// promised it, and the promise should hold at execution time too.
    CommandStarted {
        argv: Vec<String>,
        cwd: String,
    },
    CommandFinished {
        argv: Vec<String>,
        code: i32,
        duration_ms: u64,
    },
    /// A PTY session now exists and is producing output.
    ///
    /// Without this the frontend cannot show a live transcript at all: the session id is
    /// otherwise known only from the *return value*, which arrives after the command has
    /// already finished — so a terminal attached then shows an empty pane for a run that took
    /// minutes.
    ///
    /// The id cannot be announced before the spawn, because the spawn is what mints it. The
    /// frontend closes that gap from its side: it attaches its terminal before issuing the
    /// call and buffers output for every session until it learns which one is its own.
    SessionStarted {
        session: String,
    },
    Warning(PlanWarning),
    /// A free-form note. Deliberately last and deliberately rare: prefer a typed
    /// variant so the frontend can style it.
    Note {
        message: String,
    },
}

pub trait ProgressSink: Send + Sync {
    fn emit(&self, event: ProgressEvent);

    fn stage(&self, id: &str, label: &str, index: u16, total: u16) {
        self.emit(ProgressEvent::Stage {
            id: id.to_owned(),
            label: label.to_owned(),
            index,
            total,
        });
    }

    fn warn(&self, id: &str, message: &str) {
        self.emit(ProgressEvent::Warning(PlanWarning::new(id, message)));
    }
}

/// Discards everything.
///
/// Not a testing convenience so much as an honest default: `preview` is called on
/// every keystroke-debounce and has nobody to report to.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullProgress;

impl ProgressSink for NullProgress {
    fn emit(&self, _event: ProgressEvent) {}
}
