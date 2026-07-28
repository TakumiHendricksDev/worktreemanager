//! Loading configuration, and the trust gate.
//!
//! # Trust is a port concern, not a UI nicety
//!
//! A `wtm.toml` can declare shell commands, and it lives inside a repository. That
//! makes opening an unfamiliar repo equivalent to running its code. The same
//! bargain `direnv` and VS Code workspace trust make, so the same answer: show the
//! user exactly what would run, get an explicit approval, bind that approval to a
//! content hash, and re-ask when the content changes.
//!
//! Putting it in the port rather than in the UI means the domain cannot be wired up
//! in a way that skips it — [`ConfigStore::load`] returns
//! [`crate::error::ConfigError::Untrusted`] instead of a `Project`, so there is no
//! path to a command without a decision.

use std::path::{Path, PathBuf};

use crate::error::{ConfigError, ConfigLayer};
use crate::model::Project;

/// The user's answer to a trust prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustDecision {
    /// Approve this exact content. Superseded automatically when the file changes.
    Approve,
    /// Reject, and remember the rejection so the prompt is not re-shown for the
    /// same content.
    Reject,
}

/// Which layers contributed to a resolved project, in precedence order.
///
/// Reported because "why is this value not what my `wtm.toml` says" is the single
/// most common config confusion, and the answer is almost always an untracked local
/// override.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerProvenance {
    pub layer: ConfigLayer,
    pub path: Option<PathBuf>,
}

pub trait ConfigStore: Send + Sync {
    /// Resolve the config for a repository.
    ///
    /// Precedence, most specific first: `<git-common-dir>/wtm.local.toml`,
    /// `<repo>/wtm.toml`, `~/.config/wtm/config.toml`, compiled-in defaults.
    ///
    /// Returns [`ConfigError::Untrusted`] when the config declares commands that
    /// have not been approved. That is a gate, not a failure — the UI turns it into
    /// the trust prompt.
    fn load(&self, repo_root: &Path) -> Result<Project, ConfigError>;

    /// Which layers were used, for display.
    fn provenance(&self, repo_root: &Path) -> Result<Vec<LayerProvenance>, ConfigError>;

    /// Record a trust decision for a config file's current content.
    fn set_trust(&self, path: &Path, decision: TrustDecision) -> Result<(), ConfigError>;

    /// Whether the file at its *current* content is approved. A file edited after
    /// approval is not trusted.
    fn is_trusted(&self, path: &Path) -> Result<bool, ConfigError>;

    /// Registered project roots.
    fn projects(&self) -> Result<Vec<PathBuf>, ConfigError>;

    fn register_project(&self, repo_root: &Path) -> Result<(), ConfigError>;

    fn unregister_project(&self, repo_root: &Path) -> Result<(), ConfigError>;

    /// Read a user preference (theme, panel sizes) as raw TOML text.
    ///
    /// Deliberately untyped here: UI preferences are the frontend's business, and
    /// threading a `UiPrefs` struct through the domain would couple this crate to
    /// decisions it has no stake in.
    fn user_pref(&self, key: &str) -> Result<Option<String>, ConfigError>;

    fn set_user_pref(&self, key: &str, value: &str) -> Result<(), ConfigError>;
}
