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

/// The resume list, kept separate from `config.toml` for the same reason the trust store is.
///
/// `config.toml` is hand-edited and often kept in a dotfiles repo; this is machine-local state only
/// ever written by the app, and every entry in it names an absolute path that means nothing on
/// another machine. Mixing them would invite copying a stale session index between machines along
/// with preferences.
pub const SESSIONS_FILENAME: &str = "sessions.toml";

/// The trust store, kept separate from `config.toml`.
///
/// Separate on purpose: `config.toml` is meant to be hand-edited and shared between
/// machines, while the trust store is machine-local security state that is only ever
/// written by the app. Mixing them would invite copying approvals between machines
/// along with preferences.
pub const TRUST_FILENAME: &str = "trust.toml";

/// The handles Home conversations have given out. Beside the resume list, for its reasons.
pub const HANDLES_FILENAME: &str = "home_handles.toml";

/// The Home agent's working directory, under the data root.
pub const HOME_DIRNAME: &str = "home";

/// Resolved application paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    pub config_dir: PathBuf,
    pub config_file: PathBuf,
    pub trust_file: PathBuf,
    pub sessions_file: PathBuf,
    /// See [`HANDLES_FILENAME`].
    pub handles_file: PathBuf,
    /// Where the Home agent runs: a session outside every repository still needs a directory.
    ///
    /// # Not under `config_dir`
    ///
    /// That directory is the one this module's header says is often a dotfiles checkout. An
    /// agent's scratch files do not belong in somebody's dotfiles, and a CLI that looks for the
    /// enclosing git repository — every one of them does — would adopt that checkout as Home's
    /// project and read its `CLAUDE.md` or `AGENTS.md` as instructions. The XDG data directory is
    /// state the app owns, which is what this is. It is also stable, which matters: Claude Code
    /// resumes a conversation by its working directory.
    pub home_dir: PathBuf,
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
        Ok(Self::from_bases(
            &strategy.config_dir().join(APP_DIR),
            &strategy.data_dir().join(APP_DIR),
        ))
    }

    /// Build paths under an explicit directory. Used by tests and by an override.
    ///
    /// The data root goes beneath it as `data/`, so a test's whole footprint is one directory it
    /// can delete. That is the one layout in which Home sits under the config directory, and only
    /// because nothing there is anybody's dotfiles.
    #[must_use]
    pub fn rooted(config_dir: &Path) -> Self {
        Self::from_bases(config_dir, &config_dir.join("data"))
    }

    /// Build paths from a config root and a data root.
    #[must_use]
    pub fn from_bases(config_dir: &Path, data_dir: &Path) -> Self {
        Self {
            config_dir: config_dir.to_path_buf(),
            config_file: config_dir.join(CONFIG_FILENAME),
            trust_file: config_dir.join(TRUST_FILENAME),
            sessions_file: config_dir.join(SESSIONS_FILENAME),
            handles_file: config_dir.join(HANDLES_FILENAME),
            home_dir: data_dir.join(HOME_DIRNAME),
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
    fn the_home_directory_is_outside_the_config_directory() {
        // The config directory is often a dotfiles checkout, and a CLI started inside it would
        // take that checkout for Home's repository. See `AppPaths::home_dir`.
        let paths = AppPaths::from_bases(
            Path::new("/Users/me/.config/wtm"),
            Path::new("/Users/me/.local/share/wtm"),
        );
        assert!(!paths.home_dir.starts_with(&paths.config_dir));
        assert_eq!(
            paths.home_dir,
            PathBuf::from("/Users/me/.local/share/wtm/home")
        );

        let discovered = AppPaths::discover().expect("a home directory should exist");
        assert!(
            !discovered.home_dir.starts_with(&discovered.config_dir),
            "got {}",
            discovered.home_dir.display()
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
