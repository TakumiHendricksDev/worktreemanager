//! Where wtm keeps its own files.
//!
//! # `~/.config/wtm`, not `~/Library/Application Support`
//!
//! A deliberate deviation from Apple's convention, using `etcetera`'s XDG strategy.
//! This is a developer tool whose configuration is hand-edited, diffed and very often
//! kept in a dotfiles repo; burying it under `Application Support` — a path with a
//! space in it, that no shell completion reaches comfortably — would be hostile to the
//! only person who ever opens it. `dirs` cannot express this, which is why `etcetera`
//! is the dependency.
//!
//! `XDG_CONFIG_HOME` is honoured, so the location stays overridable.

use std::path::{Path, PathBuf};

use etcetera::BaseStrategy;
use wtm_core::error::ConfigError;

/// Directory name under the config root.
pub const APP_DIR: &str = "wtm";

/// The user's config file.
pub const CONFIG_FILENAME: &str = "config.toml";

/// The trust store, kept separate from `config.toml`.
///
/// Separate on purpose: `config.toml` is meant to be hand-edited and shared between
/// machines, while the trust store is machine-local security state that is only ever
/// written by the app. Mixing them would invite copying approvals between machines
/// along with preferences.
pub const TRUST_FILENAME: &str = "trust.toml";

/// Resolved application paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    pub config_dir: PathBuf,
    pub config_file: PathBuf,
    pub trust_file: PathBuf,
}

impl AppPaths {
    /// Resolve from the environment.
    ///
    /// # Errors
    ///
    /// If no home directory can be determined.
    pub fn discover() -> Result<Self, ConfigError> {
        let strategy = etcetera::choose_base_strategy().map_err(|e| ConfigError::Io {
            path: PathBuf::from("~"),
            message: format!("cannot determine the config directory: {e}"),
        })?;
        Ok(Self::rooted(&strategy.config_dir().join(APP_DIR)))
    }

    /// Build paths under an explicit directory. Used by tests and by an override.
    #[must_use]
    pub fn rooted(config_dir: &Path) -> Self {
        Self {
            config_dir: config_dir.to_path_buf(),
            config_file: config_dir.join(CONFIG_FILENAME),
            trust_file: config_dir.join(TRUST_FILENAME),
        }
    }

    /// Create the config directory if it does not exist.
    pub fn ensure_dir(&self) -> Result<(), ConfigError> {
        std::fs::create_dir_all(&self.config_dir).map_err(|e| ConfigError::Io {
            path: self.config_dir.clone(),
            message: e.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rooted_paths_sit_under_the_given_directory() {
        let paths = AppPaths::rooted(Path::new("/tmp/cfg/wtm"));
        assert_eq!(paths.config_file, PathBuf::from("/tmp/cfg/wtm/config.toml"));
        assert_eq!(paths.trust_file, PathBuf::from("/tmp/cfg/wtm/trust.toml"));
    }

    #[test]
    fn the_trust_store_is_a_separate_file_from_the_config() {
        // Machine-local security state must not travel with hand-edited preferences.
        let paths = AppPaths::rooted(Path::new("/tmp/cfg/wtm"));
        assert_ne!(paths.config_file, paths.trust_file);
    }

    #[test]
    fn discovery_lands_in_an_xdg_style_path_not_application_support() {
        let paths = AppPaths::discover().expect("a home directory should exist");
        let rendered = paths.config_dir.to_string_lossy().into_owned();
        assert!(rendered.ends_with("/wtm"), "got {rendered}");
        assert!(
            !rendered.contains("Application Support"),
            "should use the XDG strategy, got {rendered}"
        );
    }

    #[test]
    fn ensure_dir_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::rooted(&dir.path().join("nested/wtm"));
        paths.ensure_dir().unwrap();
        paths.ensure_dir().unwrap();
        assert!(paths.config_dir.is_dir());
    }
}
