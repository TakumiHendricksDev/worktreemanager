//! `~/.config/wtm/config.toml` — the app's own configuration.
//!
//! Distinct from a *project* config: this holds the list of registered repositories,
//! UI preferences, and the execution settings that apply everywhere. It is also the
//! weakest project-config layer, via its `[defaults]` table and per-project
//! `[projects.<path>.config]` overrides.
//!
//! # Round-tripping matters
//!
//! This file is meant to be hand-edited, so writing it back must not destroy what the
//! user put there. Unknown tables are preserved through
//! `#[serde(flatten)] extra`, which means a key from a newer version — or a comment's
//! neighbouring value — survives a write from an older one. (TOML comments themselves
//! cannot survive a serde round-trip; the app only rewrites this file for preferences
//! and registration, both of which the UI owns.)

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use wtm_core::error::ConfigError;

/// Which theme the window uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    /// Follow the OS.
    #[default]
    System,
    Light,
    Dark,
}

impl Theme {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "system" => Some(Self::System),
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            _ => None,
        }
    }
}

/// UI preferences.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UiPrefs {
    #[serde(default)]
    pub theme: Theme,
    /// Sidebar width in pixels.
    #[serde(default)]
    pub sidebar_width: Option<u32>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, toml::Value>,
}

/// Execution settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExecPrefs {
    /// Override the resolved `PATH`.
    ///
    /// The escape hatch for the app's most likely production failure: a bundled `.app`
    /// inherits `launchd`'s minimal `PATH`, and while wtm probes a login shell to
    /// recover a usable one, an unusual setup needs a way out that does not involve
    /// waiting for a new release.
    #[serde(default)]
    pub path: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, toml::Value>,
}

/// One registered repository.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectEntry {
    /// Display name override.
    #[serde(default)]
    pub name: Option<String>,
    /// ISO date it was added, for ordering and for display.
    #[serde(default)]
    pub added: Option<String>,
    /// Project-config overrides for this repository, merged as the user layer.
    #[serde(default)]
    pub config: Option<toml::Value>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, toml::Value>,
}

/// The whole app config.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserConfig {
    #[serde(default)]
    pub ui: UiPrefs,
    #[serde(default)]
    pub exec: ExecPrefs,
    /// Registered repositories, keyed by absolute root path.
    #[serde(default)]
    pub projects: BTreeMap<String, ProjectEntry>,
    /// Project-config defaults applied to every repository.
    #[serde(default)]
    pub defaults: Option<toml::Value>,
    /// Anything this version does not know about, preserved across writes.
    #[serde(flatten)]
    pub extra: BTreeMap<String, toml::Value>,
}

impl UserConfig {
    /// Load, treating a missing file as defaults.
    ///
    /// A *malformed* file is an error, unlike the trust store: this one is hand-edited,
    /// so silently resetting it would throw away the user's work. Reporting the syntax
    /// error is what lets them fix it.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => {
                return Err(ConfigError::Io {
                    path: path.to_path_buf(),
                    message: e.to_string(),
                });
            }
        };

        toml::from_str(&text).map_err(|e| ConfigError::Invalid {
            path: path.to_path_buf(),
            layer: wtm_core::error::ConfigLayer::User,
            line: None,
            column: None,
            key: None,
            message: e.message().to_owned(),
        })
    }

    /// Write atomically, so an interrupted save cannot corrupt the file.
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
            message: format!("serialize config: {e}"),
        })?;

        let temporary = path.with_extension("toml.tmp");
        std::fs::write(&temporary, text).map_err(|e| io(&e))?;
        std::fs::rename(&temporary, path).map_err(|e| io(&e))
    }

    /// Registered roots, in a stable order.
    #[must_use]
    pub fn project_roots(&self) -> Vec<PathBuf> {
        self.projects.keys().map(PathBuf::from).collect()
    }

    /// Register `root`, preserving any existing entry's overrides.
    pub fn register(&mut self, root: &Path, added: String) {
        let key = root.to_string_lossy().into_owned();
        let entry = self.projects.entry(key).or_default();
        // Only fill in `added` on first registration, so re-registering does not
        // rewrite history.
        if entry.added.is_none() {
            entry.added = Some(added);
        }
    }

    pub fn unregister(&mut self, root: &Path) {
        self.projects.remove(&root.to_string_lossy().into_owned());
    }

    /// Read a dotted preference key.
    ///
    /// Deliberately stringly-typed at this boundary: the `ConfigStore` port keeps UI
    /// preferences opaque so the domain does not acquire opinions about the frontend.
    #[must_use]
    pub fn pref(&self, key: &str) -> Option<String> {
        match key {
            "ui.theme" => Some(self.ui.theme.as_str().to_owned()),
            "ui.sidebar_width" => self.ui.sidebar_width.map(|w| w.to_string()),
            "exec.path" => self.exec.path.clone(),
            other => self
                .ui
                .extra
                .get(other.strip_prefix("ui.").unwrap_or(other))
                .and_then(|v| v.as_str().map(str::to_owned)),
        }
    }

    /// Write a dotted preference key. Unknown keys land in `ui.extra`, so a frontend
    /// can add a preference without a Rust change.
    pub fn set_pref(&mut self, key: &str, value: &str) {
        match key {
            "ui.theme" => {
                if let Some(theme) = Theme::parse(value) {
                    self.ui.theme = theme;
                } else {
                    tracing::warn!(value, "ignoring unrecognized theme");
                }
            }
            "ui.sidebar_width" => self.ui.sidebar_width = value.parse().ok(),
            "exec.path" => {
                self.exec.path = (!value.is_empty()).then(|| value.to_owned());
            }
            other => {
                let name = other.strip_prefix("ui.").unwrap_or(other).to_owned();
                self.ui
                    .extra
                    .insert(name, toml::Value::String(value.to_owned()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_loads_as_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let config = UserConfig::load(&dir.path().join("absent.toml")).unwrap();
        assert_eq!(config.ui.theme, Theme::System);
        assert!(config.projects.is_empty());
    }

    #[test]
    fn a_malformed_file_is_an_error_rather_than_being_silently_reset() {
        // Unlike the trust store: this file is hand-edited, so resetting it would throw
        // away the user's work.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "this is not { toml").unwrap();
        assert!(matches!(
            UserConfig::load(&path),
            Err(ConfigError::Invalid { .. })
        ));
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        let mut config = UserConfig::default();
        config.ui.theme = Theme::Dark;
        config.ui.sidebar_width = Some(280);
        config.exec.path = Some("/opt/homebrew/bin:/usr/bin".to_owned());
        config.register(Path::new("/Users/dev/repo"), "2026-07-28".to_owned());
        config.save(&path).unwrap();

        let reloaded = UserConfig::load(&path).unwrap();
        assert_eq!(reloaded.ui.theme, Theme::Dark);
        assert_eq!(reloaded.ui.sidebar_width, Some(280));
        assert_eq!(
            reloaded.exec.path.as_deref(),
            Some("/opt/homebrew/bin:/usr/bin")
        );
        assert_eq!(
            reloaded.project_roots(),
            vec![PathBuf::from("/Users/dev/repo")]
        );
    }

    #[test]
    fn unknown_keys_survive_a_write() {
        // A key from a newer version must not be destroyed by an older one.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[ui]\ntheme = 'dark'\nfuture_option = 'keep me'\n").unwrap();

        let config = UserConfig::load(&path).unwrap();
        config.save(&path).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("future_option"),
            "unknown keys must be preserved:\n{text}"
        );
        assert!(text.contains("keep me"));
    }

    #[test]
    fn registering_twice_does_not_duplicate_or_rewrite_the_added_date() {
        let mut config = UserConfig::default();
        config.register(Path::new("/r"), "2026-01-01".to_owned());
        config.register(Path::new("/r"), "2026-07-28".to_owned());

        assert_eq!(config.projects.len(), 1);
        assert_eq!(config.projects["/r"].added.as_deref(), Some("2026-01-01"));
    }

    #[test]
    fn registering_preserves_existing_project_overrides() {
        let mut config = UserConfig::default();
        config.projects.insert(
            "/r".to_owned(),
            ProjectEntry {
                config: Some(
                    toml::from_str::<toml::Table>("[naming]\nbranch = 'x'\n")
                        .map(toml::Value::Table)
                        .unwrap(),
                ),
                ..ProjectEntry::default()
            },
        );
        config.register(Path::new("/r"), "2026-07-28".to_owned());
        assert!(
            config.projects["/r"].config.is_some(),
            "overrides must survive registration"
        );
    }

    #[test]
    fn preferences_round_trip_through_the_stringly_typed_api() {
        let mut config = UserConfig::default();
        assert_eq!(config.pref("ui.theme").as_deref(), Some("system"));

        config.set_pref("ui.theme", "dark");
        assert_eq!(config.pref("ui.theme").as_deref(), Some("dark"));

        config.set_pref("ui.sidebar_width", "320");
        assert_eq!(config.pref("ui.sidebar_width").as_deref(), Some("320"));

        config.set_pref("exec.path", "/x");
        assert_eq!(config.pref("exec.path").as_deref(), Some("/x"));
    }

    #[test]
    fn an_unrecognized_theme_is_ignored_rather_than_corrupting_the_setting() {
        let mut config = UserConfig::default();
        config.set_pref("ui.theme", "dark");
        config.set_pref("ui.theme", "chartreuse");
        assert_eq!(
            config.ui.theme,
            Theme::Dark,
            "a bad value must not clobber a good one"
        );
    }

    #[test]
    fn an_unknown_preference_key_is_stored_so_the_frontend_can_add_one_freely() {
        let mut config = UserConfig::default();
        config.set_pref("ui.detail_tab", "terminal");
        assert_eq!(config.pref("ui.detail_tab").as_deref(), Some("terminal"));
    }

    #[test]
    fn clearing_the_exec_path_override_removes_it() {
        // An empty string must not become an empty PATH, which would break every spawn.
        let mut config = UserConfig::default();
        config.set_pref("exec.path", "/x");
        config.set_pref("exec.path", "");
        assert_eq!(config.exec.path, None);
    }

    #[test]
    fn save_leaves_no_temporary_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        UserConfig::default().save(&path).unwrap();
        assert!(!dir.path().join("config.toml.tmp").exists());
    }

    #[test]
    fn theme_parsing_is_symmetric() {
        for theme in [Theme::System, Theme::Light, Theme::Dark] {
            assert_eq!(Theme::parse(theme.as_str()), Some(theme));
        }
        assert_eq!(Theme::parse("nope"), None);
    }
}
