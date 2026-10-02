//! The handles Home conversations have given out, so a handle means the same session for the whole
//! conversation, across quits.
//!
//! # Why this has to outlive the process
//!
//! Home's tools name sessions by short handles (`s1`, `s2`) minted per Home conversation. Those
//! handles are written into the conversation itself — every tool result and every reply that says
//! "s3 finished" — and the conversation outlives wtm: a relaunch resumes it. A handle table that
//! died with the process started again from `s1` while the conversation went on reading its own
//! history, so `s1` came to name a different session and a message meant for one session went to
//! another. A handle has to be as durable as the transcript that quotes it.
//!
//! # What a handle is bound to
//!
//! A conversation, not a process: the provider and the id it knows the conversation by. That is the
//! identity a restored session takes over (`App::open_agent` records the resumed id), so a session
//! that comes back keeps its handle, and one that does not has a handle that names nobody and says
//! so. Numbers are never reused: `next` only grows, and a handle whose entry is gone is still known
//! to have been issued.
//!
//! # Where
//!
//! Beside `sessions.toml`, for that file's reasons (see `paths.rs`): machine-local state only the
//! app writes, holding nothing but ids. No prompt or reply is kept here.

use std::path::Path;

use serde::{Deserialize, Serialize};
use wtm_core::error::ConfigError;

/// A provider conversation, by the id its provider knows it by.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Conversation {
    pub provider: String,
    pub id: String,
}

/// One handle and the conversation it names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandleRecord {
    pub handle: String,
    pub provider: String,
    pub conversation: String,
}

impl HandleRecord {
    #[must_use]
    pub fn named(&self) -> Conversation {
        Conversation {
            provider: self.provider.clone(),
            id: self.conversation.clone(),
        }
    }
}

/// What one Home conversation has handed out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HomeHandles {
    /// The Home conversation, by its provider's id.
    pub provider: String,
    pub conversation: String,
    /// The highest number issued, bound or not. The next handle is one past it.
    pub next: u32,
    #[serde(default, rename = "handle")]
    pub handles: Vec<HandleRecord>,
}

/// How many Home conversations are remembered. Past this the least recently written is dropped,
/// and a conversation resumed after that starts its numbering past what its transcript mentions
/// (see `wtm_app_lib::app`), so no number is reused even then.
const KEEP_HOMES: usize = 50;

/// How many handles one Home conversation keeps. The oldest go first; their numbers stay issued.
const KEEP_HANDLES: usize = 1_000;

/// Every Home conversation's handles.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HandleStore {
    #[serde(default, rename = "home")]
    pub homes: Vec<HomeHandles>,
}

impl HandleStore {
    /// Load, treating a missing or malformed file as empty, as `SessionStore::load` does and for
    /// its reason: only the app writes this, and the worst a reset can do is make old handles name
    /// nobody, which they then say.
    #[must_use]
    pub fn load(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        toml::from_str(&text).unwrap_or_else(|error| {
            tracing::warn!(path = %path.display(), %error, "ignoring an unreadable handle store");
            Self::default()
        })
    }

    /// Write atomically: temp file, then rename.
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
        let text = toml::to_string_pretty(self).map_err(|e| ConfigError::Io {
            path: path.to_path_buf(),
            message: format!("serialize the handle store: {e}"),
        })?;
        let temp = crate::fs::unique_temp_path(path);
        std::fs::write(&temp, text).map_err(|e| ConfigError::Io {
            path: temp.clone(),
            message: e.to_string(),
        })?;
        std::fs::rename(&temp, path).map_err(|e| ConfigError::Io {
            path: path.to_path_buf(),
            message: e.to_string(),
        })
    }

    /// One Home conversation's handles.
    #[must_use]
    pub fn get(&self, home: &Conversation) -> Option<&HomeHandles> {
        self.homes
            .iter()
            .find(|h| h.provider == home.provider && h.conversation == home.id)
    }

    /// Replace one Home conversation's handles, keeping it as the most recently written.
    pub fn put(&mut self, mut handles: HomeHandles) {
        self.homes
            .retain(|h| h.provider != handles.provider || h.conversation != handles.conversation);
        if handles.handles.len() > KEEP_HANDLES {
            let dropped = handles.handles.len() - KEEP_HANDLES;
            handles.handles.drain(..dropped);
        }
        self.homes.push(handles);
        if self.homes.len() > KEEP_HOMES {
            let dropped = self.homes.len() - KEEP_HOMES;
            self.homes.drain(..dropped);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home(id: &str, next: u32) -> HomeHandles {
        HomeHandles {
            provider: "claude".to_owned(),
            conversation: id.to_owned(),
            next,
            handles: vec![HandleRecord {
                handle: "s1".to_owned(),
                provider: "codex".to_owned(),
                conversation: "thread-1".to_owned(),
            }],
        }
    }

    fn key(id: &str) -> Conversation {
        Conversation {
            provider: "claude".to_owned(),
            id: id.to_owned(),
        }
    }

    #[test]
    fn a_home_conversations_handles_survive_a_round_trip_through_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("home_handles.toml");
        let mut store = HandleStore::default();
        store.put(home("h1", 4));
        store.save(&path).unwrap();

        let loaded = HandleStore::load(&path);
        let handles = loaded.get(&key("h1")).expect("kept");
        assert_eq!(handles.next, 4);
        assert_eq!(handles.handles[0].named().id, "thread-1");
        assert!(loaded.get(&key("h2")).is_none());
    }

    #[test]
    fn writing_a_home_again_replaces_it_and_the_oldest_homes_go_first() {
        let mut store = HandleStore::default();
        for index in 0..KEEP_HOMES + 3 {
            store.put(home(&format!("h{index}"), 1));
        }
        store.put(home("h10", 9));

        assert_eq!(store.homes.len(), KEEP_HOMES);
        assert!(store.get(&key("h0")).is_none(), "the oldest went");
        assert_eq!(store.get(&key("h10")).unwrap().next, 9);
        assert_eq!(store.homes.last().unwrap().conversation, "h10");
    }

    #[test]
    fn an_unreadable_file_is_an_empty_store_rather_than_a_failed_launch() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("home_handles.toml");
        std::fs::write(&path, "not [ toml").unwrap();
        assert!(HandleStore::load(&path).homes.is_empty());
    }
}
