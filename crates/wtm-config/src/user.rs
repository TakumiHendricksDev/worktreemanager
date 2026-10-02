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

use crate::sidebar::SidebarLayout;

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

/// A user-defined colour palette.
///
/// The app ships nine of these compiled into the stylesheet. This is the same shape,
/// declared in TOML, for someone who wants a tenth:
///
/// ```toml
/// [ui.palettes.nord]
/// name   = "Nord"
/// hue    = 245
/// chroma = 0.8
/// brand  = ["#88c0d0", "#81a1c1", "#5e81ac", "#4c688f"]
/// ```
///
/// `hue` and `chroma` drive the neutral ramp, which the stylesheet derives in oklch — so a
/// custom palette gets the same thirteen greys as a built-in one, at the same lightness.
/// `brand` is the accent ramp at 300/400/500/600; dark mode uses the first two and light
/// mode the last two, which is the constraint to check when picking them.
///
/// Deliberately not validated here. This crate's job is to round-trip the file, and a
/// palette with a bad hex is not a corrupt config — the rest of it must still load. The
/// frontend validates and falls back to the default, which is where a colour can actually
/// be checked against the surface it will sit on.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PaletteDef {
    /// What Settings calls it. Falls back to the table key when absent.
    #[serde(default)]
    pub name: Option<String>,
    /// oklch hue angle, 0–360.
    #[serde(default)]
    pub hue: Option<f64>,
    /// Multiplier on the neutral ramp's chroma. 1 is the reference; 0 is achromatic.
    #[serde(default)]
    pub chroma: Option<f64>,
    /// The accent ramp: `#rrggbb` at 300, 400, 500, 600.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub brand: Vec<String>,
}

/// UI preferences.
///
/// `palettes`, a table, is declared below the plain values for the reason given on
/// `ProjectEntry`: it is the order TOML writes them in, though `toml` 1.x no longer fails
/// `save` when it is not.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UiPrefs {
    #[serde(default)]
    pub theme: Theme,
    /// Which colour palette is selected, by id. `None` means the app's default.
    ///
    /// A plain string rather than an enum: the set of valid values includes whatever the
    /// user declared in `palettes` below, so this crate cannot know it. The frontend
    /// resolves an unrecognised id to the default rather than erroring — a palette that
    /// was renamed, or a config copied from a newer build, should not stop the app.
    #[serde(default)]
    pub palette: Option<String>,
    /// Sidebar width in pixels.
    #[serde(default)]
    pub sidebar_width: Option<u32>,
    /// User-defined palettes, keyed by id. Empty for almost everyone.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub palettes: BTreeMap<String, PaletteDef>,
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
///
/// Plain values are declared above tables (`sidebar`, `config`) because that is the order
/// TOML writes them in. It used to be load-bearing — the old serializer failed `save` on a
/// value after a table — and no longer is: `toml` 1.x reorders them itself. Checked by
/// swapping `favorites` below `sidebar` and watching `a_layout_saves_beside_a_project_config_table`
/// still pass; that test is what would catch a serializer that stopped doing this.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectEntry {
    /// Display name override.
    #[serde(default)]
    pub name: Option<String>,
    /// ISO date it was added, for ordering and for display.
    #[serde(default)]
    pub added: Option<String>,
    /// Starred worktrees, as written before favorites became a sidebar group.
    ///
    /// Read, never written: [`UserConfig::sidebar`] folds these into the Favorites group, and
    /// the first layout saved for the project clears them. Kept as a field rather than
    /// migrated on load because loading must not rewrite a hand-edited file — and an older
    /// build reading the file after a downgrade still finds the stars it knows about until
    /// then.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub favorites: Vec<String>,
    /// How the sidebar arranges this project's worktrees.
    ///
    /// Stored per project for the reason favorites were: a worktree path is only meaningful
    /// relative to its repository, and unregistering a project should take its layout with
    /// it. Kept out of the file entirely when it says nothing, so a project nobody has
    /// arranged looks exactly as it did before this existed.
    #[serde(default, skip_serializing_if = "SidebarLayout::is_empty")]
    pub sidebar: SidebarLayout,
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

        let temporary = crate::fs::unique_temp_path(path);
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

    /// The sidebar layout for `root`, normalized, with any legacy favorites folded in.
    ///
    /// An unregistered root reads as the empty layout rather than an error: the sidebar only
    /// ever asks about a project it is showing, so absent means the answer is "nothing".
    #[must_use]
    pub fn sidebar(&self, root: &Path) -> SidebarLayout {
        self.projects
            .get(root.to_string_lossy().as_ref())
            .map_or_else(SidebarLayout::default, |entry| {
                entry.sidebar.with_legacy_favorites(&entry.favorites)
            })
            .normalized()
    }

    /// Replace the sidebar layout for `root`. Returns the normalized layout as stored, or
    /// `None` when `root` is not registered.
    ///
    /// A missing project entry is a no-op rather than an insertion: `projects` is also the
    /// registration list, so creating an entry here would make a stray write register a
    /// phantom repository. Only a registered project's worktrees are reachable in the UI, so
    /// absent means something is already wrong upstream.
    ///
    /// Clears the legacy `favorites` list. The frontend only writes a layout it read through
    /// [`sidebar`](Self::sidebar), which had those stars folded in, so they are part of what
    /// is being written — keeping the old list as well would resurrect a star the user has
    /// since removed.
    pub fn set_sidebar(&mut self, root: &Path, layout: &SidebarLayout) -> Option<SidebarLayout> {
        let entry = self.projects.get_mut(root.to_string_lossy().as_ref())?;
        entry.sidebar = layout.compacted();
        entry.favorites.clear();
        Some(entry.sidebar.normalized())
    }

    /// Take `worktree` out of the layout for `root`. Returns whether anything changed.
    pub fn forget_in_sidebar(&mut self, root: &Path, worktree: &str) -> bool {
        let Some(entry) = self.projects.get_mut(root.to_string_lossy().as_ref()) else {
            return false;
        };
        let before = entry.favorites.len();
        entry.favorites.retain(|f| f != worktree);
        let changed = entry.sidebar.forget(worktree) || entry.favorites.len() != before;
        entry.sidebar = entry.sidebar.compacted();
        changed
    }

    /// Read a dotted preference key.
    ///
    /// Deliberately stringly-typed at this boundary: the `ConfigStore` port keeps UI
    /// preferences opaque so the domain does not acquire opinions about the frontend.
    #[must_use]
    pub fn pref(&self, key: &str) -> Option<String> {
        match key {
            "ui.theme" => Some(self.ui.theme.as_str().to_owned()),
            "ui.palette" => self.ui.palette.clone(),
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
            /* Not validated against the known palettes, because this crate does not know
            them — nine live in the stylesheet and the rest in `ui.palettes`. An empty
            value clears back to the default, which is how Settings offers "use the
            default" without a sentinel id. */
            "ui.palette" => {
                self.ui.palette = (!value.is_empty()).then(|| value.to_owned());
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
    use crate::sidebar::FAVORITES;

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

    fn layout(groups: &[(&str, &[&str])]) -> SidebarLayout {
        SidebarLayout {
            groups: groups
                .iter()
                .map(|(id, worktrees)| crate::sidebar::SidebarGroup {
                    id: (*id).to_owned(),
                    name: format!("{id} name"),
                    collapsed: false,
                    worktrees: worktrees.iter().map(|&w| w.to_owned()).collect(),
                })
                .collect(),
            ..SidebarLayout::default()
        }
    }

    fn members(layout: &SidebarLayout, id: &str) -> Vec<String> {
        layout
            .groups
            .iter()
            .find(|g| g.id == id)
            .map(|g| g.worktrees.clone())
            .unwrap_or_default()
    }

    #[test]
    fn a_sidebar_layout_round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        let mut config = UserConfig::default();
        config.register(Path::new("/r"), "2026-07-28".to_owned());
        let mut written = layout(&[(FAVORITES, &["/r-f"]), ("reviews", &["/r-b", "/r-a"])]);
        written.groups[1].collapsed = true;
        written
            .origins
            .insert("/r-f".to_owned(), "reviews".to_owned());
        config.set_sidebar(Path::new("/r"), &written).unwrap();
        config.save(&path).unwrap();

        let reloaded = UserConfig::load(&path).unwrap().sidebar(Path::new("/r"));
        assert_eq!(reloaded, written.normalized());
        assert_eq!(
            members(&reloaded, "reviews"),
            ["/r-b", "/r-a"],
            "a hand-set order must not be re-sorted"
        );
    }

    #[test]
    fn a_layout_saves_beside_a_project_config_table() {
        // `sidebar` and `config` are both tables under the project, beside a plain array. The
        // serializer that used to fail `save` on a value after a table would fail here, and
        // nowhere at compile time.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        let mut config = UserConfig::default();
        config.projects.insert(
            "/r".to_owned(),
            ProjectEntry {
                favorites: vec!["/r-legacy".to_owned()],
                config: Some(
                    toml::from_str::<toml::Table>("[naming]\nbranch = 'x'\n")
                        .map(toml::Value::Table)
                        .unwrap(),
                ),
                ..ProjectEntry::default()
            },
        );
        config.projects.get_mut("/r").unwrap().sidebar = layout(&[("g", &["/r-a"])]);
        config
            .save(&path)
            .expect("a layout beside a config table must serialize");

        let reloaded = UserConfig::load(&path).unwrap();
        assert!(reloaded.projects["/r"].config.is_some());
        assert_eq!(members(&reloaded.sidebar(Path::new("/r")), "g"), ["/r-a"]);
    }

    #[test]
    fn legacy_favorites_read_as_the_favorites_group() {
        // A config written before favorites were a group must come up with the same stars.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[projects.\"/r\"]\nfavorites = [\"/r-a\", \"/r-b\"]\n",
        )
        .unwrap();

        let sidebar = UserConfig::load(&path).unwrap().sidebar(Path::new("/r"));
        assert_eq!(members(&sidebar, FAVORITES), ["/r-a", "/r-b"]);
    }

    #[test]
    fn saving_a_layout_retires_the_legacy_favorites_list() {
        // Otherwise unstarring a migrated favorite would not stick: the old list would put it
        // back on the next read.
        let mut config = UserConfig::default();
        config.projects.insert(
            "/r".to_owned(),
            ProjectEntry {
                favorites: vec!["/r-a".to_owned(), "/r-b".to_owned()],
                ..ProjectEntry::default()
            },
        );

        let mut read = config.sidebar(Path::new("/r"));
        read.groups[0].worktrees.retain(|w| w != "/r-a");
        config.set_sidebar(Path::new("/r"), &read).unwrap();

        assert!(config.projects["/r"].favorites.is_empty());
        assert_eq!(
            members(&config.sidebar(Path::new("/r")), FAVORITES),
            ["/r-b"]
        );
    }

    #[test]
    fn a_project_nobody_has_arranged_writes_no_sidebar_or_favorites_key() {
        // The setting should be invisible until it is used — including after a write of the
        // layout a fresh project reads as, which is the two empty built-ins.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");

        let mut config = UserConfig::default();
        config.register(Path::new("/r"), "2026-07-28".to_owned());
        let untouched = config.sidebar(Path::new("/r"));
        config.set_sidebar(Path::new("/r"), &untouched).unwrap();
        config.save(&path).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("sidebar"), "unexpected table in:\n{text}");
        assert!(!text.contains("favorites"), "unexpected key in:\n{text}");
    }

    #[test]
    fn arranging_an_unregistered_project_does_not_register_it() {
        // `projects` doubles as the registration list, so an insert here would put a
        // phantom repository in the sidebar.
        let mut config = UserConfig::default();
        assert!(
            config
                .set_sidebar(Path::new("/never-added"), &layout(&[("g", &["/x"])]))
                .is_none()
        );
        assert!(config.projects.is_empty());
    }

    #[test]
    fn forgetting_a_worktree_clears_it_from_the_layout_and_the_legacy_list() {
        let mut config = UserConfig::default();
        config.projects.insert(
            "/r".to_owned(),
            ProjectEntry {
                favorites: vec!["/r-a".to_owned()],
                sidebar: layout(&[("g", &["/r-a", "/r-b"])]),
                ..ProjectEntry::default()
            },
        );

        assert!(config.forget_in_sidebar(Path::new("/r"), "/r-a"));
        let sidebar = config.sidebar(Path::new("/r"));
        assert!(members(&sidebar, FAVORITES).is_empty());
        assert_eq!(members(&sidebar, "g"), ["/r-b"]);
        assert!(!config.forget_in_sidebar(Path::new("/r"), "/r-a"));
    }

    #[test]
    fn unregistering_takes_the_layout_with_it() {
        let mut config = UserConfig::default();
        config.register(Path::new("/r"), "2026-07-28".to_owned());
        config.set_sidebar(Path::new("/r"), &layout(&[(FAVORITES, &["/r-a"])]));
        config.unregister(Path::new("/r"));
        assert!(members(&config.sidebar(Path::new("/r")), FAVORITES).is_empty());
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

    #[test]
    fn clearing_the_palette_returns_to_the_default() {
        let mut config = UserConfig::default();
        config.set_pref("ui.palette", "harbor");
        assert_eq!(config.pref("ui.palette").as_deref(), Some("harbor"));
        config.set_pref("ui.palette", "");
        assert_eq!(config.ui.palette, None);
    }

    #[test]
    fn an_unrecognized_palette_is_stored_rather_than_rejected() {
        // The valid set includes whatever is in `[ui.palettes]` plus nine this crate cannot
        // see, so refusing an unknown id here would refuse legitimate ones.
        let mut config = UserConfig::default();
        config.set_pref("ui.palette", "something-from-a-newer-build");
        assert_eq!(
            config.pref("ui.palette").as_deref(),
            Some("something-from-a-newer-build")
        );
    }

    #[test]
    fn a_hand_written_palette_survives_a_write() {
        // The whole point of declaring one in TOML: the app rewrites this file whenever any
        // preference changes, and a palette that did not round-trip would vanish the first
        // time someone resized the sidebar.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[ui.palettes.nord]\n\
             name = \"Nord\"\n\
             hue = 245\n\
             chroma = 0.8\n\
             brand = [\"#88c0d0\", \"#81a1c1\", \"#5e81ac\", \"#4c688f\"]\n",
        )
        .unwrap();

        let mut config = UserConfig::load(&path).unwrap();
        config.set_pref("ui.sidebar_width", "300");
        config.save(&path).unwrap();

        let reloaded = UserConfig::load(&path).unwrap();
        let nord = reloaded.ui.palettes.get("nord").expect("nord survives");
        assert_eq!(nord.name.as_deref(), Some("Nord"));
        assert_eq!(nord.hue, Some(245.0));
        assert_eq!(nord.chroma, Some(0.8));
        assert_eq!(nord.brand.len(), 4);
    }

    #[test]
    fn a_notification_preference_round_trips_without_a_rust_field() {
        // The mechanism `set_pref`'s doc promises, exercised by the preference that relies on it:
        // `ui.notify` has no field on `UiPrefs` and needs none, so the frontend added a setting with
        // no change to this crate at all.
        //
        // The reload matters as much as the value. `ui.extra` serializes as plain keys under `[ui]`,
        // and TOML requires those *before* any nested table — so an unknown key landing after
        // `[ui.palettes]` would write a file that no longer parses, and the failure would be at
        // runtime on the next launch rather than here.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[ui.palettes.nord]\n\
             name = \"Nord\"\n\
             hue = 245\n",
        )
        .unwrap();

        let mut config = UserConfig::load(&path).unwrap();
        assert_eq!(config.pref("ui.notify"), None, "unset means ask");

        config.set_pref("ui.notify", "on");
        config.save(&path).unwrap();

        let reloaded = UserConfig::load(&path).unwrap();
        assert_eq!(reloaded.pref("ui.notify").as_deref(), Some("on"));
        assert!(
            reloaded.ui.palettes.contains_key("nord"),
            "the table after the new key must still parse"
        );
    }

    #[test]
    fn a_config_with_no_palettes_writes_no_palettes_table() {
        // Same argument as `sidebar`: a file nobody has customized must look exactly as it
        // did before this feature existed.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        UserConfig::default().save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("palettes"), "unexpected table in:\n{text}");
    }

    #[test]
    fn one_broken_palette_does_not_stop_the_config_loading() {
        // Validation belongs to the frontend layer, which can check a colour against the
        // surface it will sit on. Here, a nonsense palette must still parse.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[ui]\ntheme = \"dark\"\n\n[ui.palettes.broken]\nbrand = [\"not-a-colour\"]\n",
        )
        .unwrap();

        let config = UserConfig::load(&path).expect("the rest of the file still loads");
        assert_eq!(config.ui.theme, Theme::Dark);
        assert!(config.ui.palettes.contains_key("broken"));
    }
}
