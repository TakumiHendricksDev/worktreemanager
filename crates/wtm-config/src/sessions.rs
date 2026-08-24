//! Which agent sessions can be resumed.
//!
//! # What persists, and what cannot
//!
//! A session is a child process, and a child process does not survive a quit. What survives is the
//! **handle**: the provider, the worktree, the id that CLI knows the conversation by, and the model
//! and effort it was on. With those, a session is re-openable; without them it is gone, because both
//! CLIs store the transcript themselves and neither will hand it back without an id.
//!
//! So this is a resume list, not a session list. The distinction matters at launch: wtm shows what
//! *can* be resumed and re-establishes on demand rather than respawning a fleet of CLIs nobody asked
//! for.
//!
//! What that argument never covered is the *arrangement*. The frontend does remember which panes a
//! worktree had and how they were split, because a layout is not a process: it costs nothing to put
//! back, and losing it on every quit was a real complaint. A restored agent pane comes back holding
//! a place and offering to resume — it does not resume itself, which is this file's rule intact.
//! See `sessions.svelte.ts`'s `restore`.
//!
//! # Why a separate file
//!
//! `paths.rs` already argues this for the trust store, and the argument is the same one:
//! `config.toml` is meant to be hand-edited and shared between machines, while this is machine-local
//! state only ever written by the app. Mixing them would invite copying a stale session index
//! between machines along with preferences — and every entry here names an absolute path that is
//! meaningless on another one.
//!
//! # Why not a transcript
//!
//! Deliberately no messages. Both CLIs already keep one, under their own directory and their own
//! permissions, and a second copy would be a second secret-bearing file the user does not know
//! exists — agent output quotes whatever it read, which in a worktree includes `.env`.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use wtm_core::error::ConfigError;

/// One resumable session.
///
/// `PartialEq` but not `Eq`, because `toml::Value` is not — the same reason `UserConfig` derives
/// neither. Nothing here needs a total equality.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionRecord {
    /// Which provider, by catalogue id.
    pub provider: String,
    /// The worktree's absolute path, which is also its id.
    pub worktree: String,
    /// The id that provider knows the conversation by.
    ///
    /// Not wtm's own session id, which is a per-process handle and meaningless after a quit. Claude
    /// lets wtm choose this and Codex assigns it, which is exactly why it is stored rather than
    /// derived.
    pub provider_session: String,
    /// A short label for the resume list. The first thing the user said, truncated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// When it was last written, ISO 8601, for ordering the list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>,
    /// Anything a future version adds, preserved across writes.
    #[serde(flatten)]
    pub extra: BTreeMap<String, toml::Value>,
}

/// The whole resume list.
///
/// Field order matters, as it does in `UserConfig`: TOML emits every plain value before any table, so
/// a scalar added below `session` would make `save` fail at runtime rather than at compile time.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionStore {
    #[serde(default, rename = "session")]
    pub sessions: Vec<SessionRecord>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, toml::Value>,
}

/// How many records are kept, newest first.
///
/// A bound so the file cannot grow without limit across months of use, and generous enough that it is
/// never the reason a session you remember is missing. Trimmed on write rather than on read, so a
/// file someone hand-edited larger is not silently truncated the moment it is looked at.
const KEEP: usize = 200;

impl SessionStore {
    /// Load, treating a missing *or malformed* file as empty.
    ///
    /// Unlike `config.toml`, which errors on a syntax mistake because it is hand-edited and silently
    /// resetting would destroy the user's work. This file is only ever written by the app, so a
    /// corrupt one is a bug in wtm rather than a typo — and the worst outcome of forgetting it is a
    /// resume list that has to be rebuilt, where refusing to start would be a broken app.
    #[must_use]
    pub fn load(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        toml::from_str(&text).unwrap_or_else(|error| {
            tracing::warn!(path = %path.display(), %error, "ignoring an unreadable session store");
            Self::default()
        })
    }

    /// Write atomically: temp file, then rename.
    ///
    /// The same discipline `UserConfig::save` and the trust store use. A half-written file here would
    /// mean a resume list that parses to nothing on the next launch, which is the one failure mode
    /// this file has.
    ///
    /// # Errors
    ///
    /// If the directory cannot be created, or the file cannot be written or renamed.
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| ConfigError::Io {
                path: parent.to_path_buf(),
                message: e.to_string(),
            })?;
        }

        // Mapped to `Io` rather than `Invalid`, following `UserConfig::save`: `Invalid` carries a
        // layer, a line and a config key, and a *serialization* failure has none of those — it is
        // this app failing to write its own state, not a file being wrong.
        let text = toml::to_string_pretty(self).map_err(|e| ConfigError::Io {
            path: path.to_path_buf(),
            message: format!("serialize the session store: {e}"),
        })?;

        let temp = path.with_extension("toml.tmp");
        std::fs::write(&temp, text).map_err(|e| ConfigError::Io {
            path: temp.clone(),
            message: e.to_string(),
        })?;
        std::fs::rename(&temp, path).map_err(|e| ConfigError::Io {
            path: path.to_path_buf(),
            message: e.to_string(),
        })
    }

    /// Record a session, replacing any earlier entry for the same provider conversation.
    ///
    /// Keyed on `(provider, provider_session)` rather than appended, because a session is written to
    /// on every turn — appending would put the same conversation in the list a hundred times and push
    /// everything else past [`KEEP`].
    pub fn remember(&mut self, mut record: SessionRecord) {
        // A resume handshake knows the model and timestamp but not the human label. Keep the title
        // already learned from the first prompt instead of replacing it with `None` every time the
        // same durable conversation is opened again.
        if let Some(existing) = self.sessions.iter().find(|existing| {
            existing.provider == record.provider
                && existing.provider_session == record.provider_session
        }) && record.title.is_none()
        {
            record.title.clone_from(&existing.title);
        }
        self.sessions.retain(|existing| {
            existing.provider != record.provider
                || existing.provider_session != record.provider_session
        });
        // Newest first, which is the order a resume list wants and means the trim below drops the
        // oldest rather than the most recent.
        self.sessions.insert(0, record);
        self.sessions.truncate(KEEP);
    }

    /// Forget one conversation. The user closing a session for good.
    pub fn forget(&mut self, provider: &str, provider_session: &str) {
        self.sessions
            .retain(|s| s.provider != provider || s.provider_session != provider_session);
    }

    /// Forget everything belonging to a worktree, for when that worktree is removed.
    ///
    /// Without this a removed worktree leaves resume entries pointing at a path that no longer
    /// exists, and every one of them would fail on click with an error about a missing directory.
    pub fn forget_worktree(&mut self, worktree: &str) {
        self.sessions.retain(|s| s.worktree != worktree);
    }

    /// What can be resumed in a worktree, newest first.
    #[must_use]
    pub fn in_worktree(&self, worktree: &str) -> Vec<&SessionRecord> {
        self.sessions
            .iter()
            .filter(|s| s.worktree == worktree)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(provider: &str, session: &str, worktree: &str) -> SessionRecord {
        SessionRecord {
            provider: provider.to_owned(),
            worktree: worktree.to_owned(),
            provider_session: session.to_owned(),
            title: Some("do the thing".to_owned()),
            model: Some("opus".to_owned()),
            effort: Some("xhigh".to_owned()),
            updated: Some("2026-08-05T12:00:00Z".to_owned()),
            extra: BTreeMap::new(),
        }
    }

    #[test]
    fn remembering_the_same_conversation_twice_replaces_rather_than_appends() {
        // A session is written to on every turn. Appending would put one conversation in the list a
        // hundred times and push everything else past the cap.
        let mut store = SessionStore::default();
        store.remember(record("claude", "abc", "/w/a"));
        let mut later = record("claude", "abc", "/w/a");
        later.title = Some("do the other thing".to_owned());
        store.remember(later);

        assert_eq!(store.sessions.len(), 1);
        assert_eq!(
            store.sessions[0].title.as_deref(),
            Some("do the other thing")
        );
    }

    #[test]
    fn a_resume_handshake_does_not_erase_the_existing_title() {
        let mut store = SessionStore::default();
        store.remember(record("claude", "abc", "/w/a"));
        let mut resumed = record("claude", "abc", "/w/a");
        resumed.title = None;
        store.remember(resumed);

        assert_eq!(store.sessions[0].title.as_deref(), Some("do the thing"));
    }

    #[test]
    fn the_same_id_from_two_providers_is_two_conversations() {
        // The key is the pair. Claude lets wtm choose its session id, so a collision with a Codex
        // thread id is not impossible, and treating them as one would resume the wrong session.
        let mut store = SessionStore::default();
        store.remember(record("claude", "same", "/w/a"));
        store.remember(record("codex", "same", "/w/a"));
        assert_eq!(store.sessions.len(), 2);
    }

    #[test]
    fn the_newest_is_first_and_the_oldest_is_what_the_cap_drops() {
        let mut store = SessionStore::default();
        for i in 0..(KEEP + 10) {
            store.remember(record("claude", &format!("s{i}"), "/w/a"));
        }
        assert_eq!(store.sessions.len(), KEEP);
        // The most recently remembered is at the front, and the first ten are gone.
        assert_eq!(store.sessions[0].provider_session, format!("s{}", KEEP + 9));
        assert!(!store.sessions.iter().any(|s| s.provider_session == "s0"));
    }

    #[test]
    fn removing_a_worktree_takes_its_resume_entries_with_it() {
        // Otherwise every one of them fails on click with an error about a missing directory.
        let mut store = SessionStore::default();
        store.remember(record("claude", "a", "/w/gone"));
        store.remember(record("codex", "b", "/w/stays"));
        store.forget_worktree("/w/gone");

        assert_eq!(store.sessions.len(), 1);
        assert_eq!(store.sessions[0].worktree, "/w/stays");
    }

    #[test]
    fn a_store_survives_a_round_trip_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.toml");

        let mut store = SessionStore::default();
        store.remember(record("claude", "abc", "/w/a"));
        store.save(&path).unwrap();

        let read = SessionStore::load(&path);
        assert_eq!(read.sessions, store.sessions);
    }

    #[test]
    fn an_unreadable_store_is_forgotten_rather_than_fatal() {
        // Only wtm writes this file, so a corrupt one is a bug in wtm rather than a user's typo —
        // unlike `config.toml`, which errors because silently resetting would destroy hand-edits.
        // The worst outcome of forgetting is a resume list to rebuild; refusing to start is worse.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.toml");
        std::fs::write(&path, "this is not toml { [ ").unwrap();

        assert!(SessionStore::load(&path).sessions.is_empty());
    }

    #[test]
    fn unknown_keys_survive_a_write() {
        // A newer wtm's fields must not be destroyed by an older one round-tripping the file — the
        // same guarantee `UserConfig` makes, and for the same reason.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sessions.toml");
        std::fs::write(
            &path,
            "[[session]]\nprovider = \"claude\"\nworktree = \"/w/a\"\n\
             provider_session = \"abc\"\nfuture_field = \"keep me\"\n",
        )
        .unwrap();

        let store = SessionStore::load(&path);
        store.save(&path).unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().contains("keep me"));
    }
}
