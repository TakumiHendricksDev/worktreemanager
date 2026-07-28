//! The `ConfigStore` implementation.
//!
//! # Load order
//!
//! 1. Resolve the layer paths — the local one keyed on `git rev-parse
//!    --git-common-dir`, so every worktree of a repo shares it.
//! 2. Read what exists; a missing layer is normal, not an error.
//! 3. **Trust check.** Before anything is interpreted, hash the on-disk layers and
//!    refuse if they declare commands that have not been approved *at these exact
//!    bytes*.
//! 4. Merge weakest-to-strongest on `toml::Value`, then deserialize once.
//! 5. Validate semantically, so a bad template is an error here rather than a strange
//!    worktree later.
//!
//! Step 3 comes before step 4 for a reason: the trust prompt has to show the user what
//! *the file* says, and a merged view would attribute a command to the wrong file.
//!
//! # Caching
//!
//! Resolved projects are cached against the layer files' content hashes, so an edit
//! invalidates the cache automatically. Without it, listing worktrees would re-read,
//! re-merge and re-validate on every refresh.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use toml::Value;
use wtm_core::error::{ConfigError, ConfigLayer};
use wtm_core::model::{Project, ProjectId};
use wtm_core::ports::clock::Clock;
use wtm_core::ports::config::{ConfigStore, LayerProvenance, TrustDecision};
use wtm_core::ports::git::Git;
use wtm_core::ports::template::TemplateEngine;

use crate::layers::{self, LayerPaths, LoadedLayer};
use crate::paths::AppPaths;
use crate::trust::{self, TrustStore};
use crate::user::UserConfig;
use crate::validate::{self, Origin};

/// A resolved project plus the hashes it was resolved from.
#[derive(Debug, Clone)]
struct CacheEntry {
    project: Project,
    /// `(path, hash)` for every on-disk layer that contributed.
    fingerprint: Vec<(PathBuf, String)>,
}

/// Filesystem-backed [`ConfigStore`].
pub struct FileConfigStore {
    paths: AppPaths,
    git: Arc<dyn Git>,
    engine: Arc<dyn TemplateEngine>,
    clock: Arc<dyn Clock>,
    cache: Mutex<BTreeMap<PathBuf, CacheEntry>>,
}

impl std::fmt::Debug for FileConfigStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileConfigStore")
            .field("paths", &self.paths)
            .finish_non_exhaustive()
    }
}

impl FileConfigStore {
    #[must_use]
    pub fn new(
        paths: AppPaths,
        git: Arc<dyn Git>,
        engine: Arc<dyn TemplateEngine>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            paths,
            git,
            engine,
            clock,
            cache: Mutex::new(BTreeMap::new()),
        }
    }

    #[must_use]
    pub fn paths(&self) -> &AppPaths {
        &self.paths
    }

    /// Read the user config, treating absence as defaults.
    pub fn user_config(&self) -> Result<UserConfig, ConfigError> {
        UserConfig::load(&self.paths.config_file)
    }

    fn save_user_config(&self, config: &UserConfig) -> Result<(), ConfigError> {
        self.paths.ensure_dir()?;
        config.save(&self.paths.config_file)
    }

    /// Starred worktree paths for `repo_root`.
    ///
    /// Deliberately an inherent method rather than part of the [`ConfigStore`] port. A
    /// favorite is a sidebar-ordering preference: no use-case reads it, no plan depends on
    /// it, and putting it on the port would hand the domain an opinion about the UI. The
    /// composition root reaches for the concrete store, which is the one place allowed to.
    pub fn favorites(&self, repo_root: &Path) -> Result<Vec<String>, ConfigError> {
        Ok(self.user_config()?.favorites(repo_root).to_vec())
    }

    /// Star or unstar one worktree, by its absolute path.
    ///
    /// Writing the whole config back for one boolean is fine here: the file is small, the
    /// write is atomic, and a star is a deliberate click rather than something that
    /// happens in a loop.
    pub fn set_favorite(
        &self,
        repo_root: &Path,
        worktree: &str,
        favorite: bool,
    ) -> Result<(), ConfigError> {
        let mut config = self.user_config()?;
        if !config
            .projects
            .contains_key(repo_root.to_string_lossy().as_ref())
        {
            tracing::warn!(
                repo = %repo_root.display(),
                "ignoring a favorite for an unregistered project"
            );
            return Ok(());
        }
        // Already in the requested state: no change, so no write.
        if !config.set_favorite(repo_root, worktree, favorite) {
            return Ok(());
        }
        self.save_user_config(&config)
    }

    fn trust_store(&self) -> TrustStore {
        TrustStore::load(&self.paths.trust_file)
    }

    /// Resolve the paths of all four layers for `repo_root`.
    fn layer_paths(&self, repo_root: &Path) -> Result<LayerPaths, ConfigError> {
        // `--git-common-dir`, never `--git-dir`. See the module docs in `layers`.
        let common = self
            .git
            .git_common_dir(repo_root)
            .map_err(|e| ConfigError::Io {
                path: repo_root.to_path_buf(),
                message: format!("cannot resolve the git directory: {e}"),
            })?;
        Ok(LayerPaths::new(repo_root, &common, &self.paths.config_file))
    }

    /// Read every layer that exists, weakest first.
    fn read_layers(&self, repo_root: &Path) -> Result<Vec<LoadedLayer>, ConfigError> {
        let paths = self.layer_paths(repo_root)?;
        let mut out = Vec::with_capacity(4);

        out.push(LoadedLayer {
            layer: ConfigLayer::BuiltIn,
            path: None,
            value: layers::parse(
                Path::new("<built-in>"),
                ConfigLayer::BuiltIn,
                layers::BUILT_IN_DEFAULTS,
            )?,
            source: None,
        });

        // The user layer is an app config; only its project-relevant parts apply.
        if let Some(source) = read_optional(&paths.user)? {
            let user = layers::parse(&paths.user, ConfigLayer::User, &source)?;
            out.push(LoadedLayer {
                layer: ConfigLayer::User,
                path: Some(paths.user.clone()),
                value: layers::project_overrides_from_user(&user, repo_root),
                source: Some(source),
            });
        }

        for (layer, path) in [
            (ConfigLayer::Repo, &paths.repo),
            (ConfigLayer::Local, &paths.local),
        ] {
            if let Some(source) = read_optional(path)? {
                out.push(LoadedLayer {
                    layer,
                    path: Some(path.clone()),
                    value: layers::parse(path, layer, &source)?,
                    source: Some(source),
                });
            }
        }

        Ok(out)
    }

    /// Refuse to interpret a layer whose declared commands are not approved.
    ///
    /// Checked per *file*, before merging, so the prompt can attribute each command to
    /// the file that actually declares it.
    fn enforce_trust(&self, loaded: &[LoadedLayer]) -> Result<(), ConfigError> {
        let store = self.trust_store();

        for layer in loaded {
            // The user's own config is not a trust boundary: it is theirs, it lives
            // outside any repository, and prompting for it would train people to click
            // through the prompt that does matter.
            if layer.layer == ConfigLayer::User || layer.layer == ConfigLayer::BuiltIn {
                continue;
            }
            let (Some(path), Some(source)) = (&layer.path, &layer.source) else {
                continue;
            };

            let declared = declared_commands_in(&layer.value);
            if declared.is_empty() {
                // Nothing to execute, so nothing to approve.
                continue;
            }
            if store.is_approved(path, source) {
                continue;
            }

            return Err(ConfigError::Untrusted {
                path: path.clone(),
                commands: declared,
                content_hash: trust::content_hash(source),
            });
        }

        Ok(())
    }

    /// Deserialize and validate a merged value.
    fn finish(
        &self,
        repo_root: &Path,
        merged: Value,
        origin: &Origin,
    ) -> Result<Project, ConfigError> {
        let mut project: Project = merged.try_into().map_err(|e: toml::de::Error| {
            let span = e.span();
            ConfigError::Invalid {
                path: origin.path.clone(),
                layer: origin.layer,
                line: span.map(|_| 0),
                column: None,
                key: None,
                message: e.message().to_owned(),
            }
        })?;

        project.id = ProjectId::from_root(repo_root);
        project.root = repo_root.to_path_buf();

        validate::validate(&project, self.engine.as_ref(), origin)?;
        Ok(project)
    }

    /// `(path, hash)` for each on-disk layer, used as the cache key.
    fn fingerprint(loaded: &[LoadedLayer]) -> Vec<(PathBuf, String)> {
        loaded
            .iter()
            .filter_map(|l| {
                let path = l.path.clone()?;
                let source = l.source.as_ref()?;
                Some((path, trust::content_hash(source)))
            })
            .collect()
    }
}

fn read_optional(path: &Path) -> Result<Option<String>, ConfigError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(ConfigError::Io {
            path: path.to_path_buf(),
            message: e.to_string(),
        }),
    }
}

/// Every argv a raw config value would run.
///
/// Deliberately operates on `toml::Value` rather than a deserialized `Project`: the
/// trust prompt has to work even when the config does not fully validate, because an
/// invalid config can still contain a `run` array, and refusing to show it would be
/// exactly backwards.
fn declared_commands_in(value: &Value) -> Vec<Vec<String>> {
    let mut out: Vec<Vec<String>> = Vec::new();
    collect_runs(value, &mut out);
    out
}

fn collect_runs(value: &Value, out: &mut Vec<Vec<String>>) {
    match value {
        Value::Table(table) => {
            if let Some(Value::Array(items)) = table.get("run") {
                let argv: Vec<String> = items
                    .iter()
                    .map(|item| {
                        item.as_str()
                            .map_or_else(|| item.to_string(), str::to_owned)
                    })
                    .collect();
                if !argv.is_empty() && !out.contains(&argv) {
                    out.push(argv);
                }
            }
            for nested in table.values() {
                collect_runs(nested, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_runs(item, out);
            }
        }
        _ => {}
    }
}

impl ConfigStore for FileConfigStore {
    fn load(&self, repo_root: &Path) -> Result<Project, ConfigError> {
        let loaded = self.read_layers(repo_root)?;
        let fingerprint = Self::fingerprint(&loaded);

        if let Some(entry) = self.cache.lock().get(repo_root)
            && entry.fingerprint == fingerprint
        {
            return Ok(entry.project.clone());
        }

        self.enforce_trust(&loaded)?;

        // Report errors against the most specific file that contributed, since that is
        // the one most likely to hold the mistake.
        let origin = loaded
            .iter()
            .rev()
            .find_map(|l| {
                l.path.clone().map(|path| Origin {
                    path,
                    layer: l.layer,
                })
            })
            .unwrap_or_else(|| Origin {
                path: PathBuf::from("<built-in>"),
                layer: ConfigLayer::BuiltIn,
            });

        let project = self.finish(repo_root, layers::merge_all(&loaded), &origin)?;

        self.cache.lock().insert(
            repo_root.to_path_buf(),
            CacheEntry {
                project: project.clone(),
                fingerprint,
            },
        );

        Ok(project)
    }

    fn provenance(&self, repo_root: &Path) -> Result<Vec<LayerProvenance>, ConfigError> {
        Ok(self
            .read_layers(repo_root)?
            .into_iter()
            .map(|l| LayerProvenance {
                layer: l.layer,
                path: l.path,
            })
            .collect())
    }

    fn set_trust(&self, path: &Path, decision: TrustDecision) -> Result<(), ConfigError> {
        let source = read_optional(path)?.ok_or_else(|| ConfigError::Io {
            path: path.to_path_buf(),
            message: "cannot record a decision for a file that no longer exists".to_owned(),
        })?;

        let mut store = self.trust_store();
        store.record(path, &source, decision, Some(self.clock.now_iso()));
        self.paths.ensure_dir()?;
        store.save(&self.paths.trust_file)?;

        // A decision changes what `load` may do, so drop the cache.
        self.cache.lock().clear();
        Ok(())
    }

    fn is_trusted(&self, path: &Path) -> Result<bool, ConfigError> {
        let Some(source) = read_optional(path)? else {
            return Ok(false);
        };
        Ok(self.trust_store().is_approved(path, &source))
    }

    fn projects(&self) -> Result<Vec<PathBuf>, ConfigError> {
        Ok(self.user_config()?.project_roots())
    }

    fn register_project(&self, repo_root: &Path) -> Result<(), ConfigError> {
        // Verify it is a repository before recording it, so a typo fails here rather
        // than becoming a permanently broken sidebar entry.
        let root = self.git.repo_root(repo_root).map_err(|e| ConfigError::Io {
            path: repo_root.to_path_buf(),
            message: format!("not a git repository: {e}"),
        })?;

        let mut config = self.user_config()?;
        config.register(&root, self.clock.today());
        self.save_user_config(&config)
    }

    fn unregister_project(&self, repo_root: &Path) -> Result<(), ConfigError> {
        let mut config = self.user_config()?;
        config.unregister(repo_root);
        self.save_user_config(&config)?;
        self.cache.lock().remove(repo_root);
        Ok(())
    }

    fn user_pref(&self, key: &str) -> Result<Option<String>, ConfigError> {
        Ok(self.user_config()?.pref(key))
    }

    fn set_user_pref(&self, key: &str, value: &str) -> Result<(), ConfigError> {
        let mut config = self.user_config()?;
        config.set_pref(key, value);
        self.save_user_config(&config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wtm_render::Engine;
    use wtm_testkit::{FakeClock, FakeGit};

    struct Harness {
        store: FileConfigStore,
        repo: PathBuf,
        _config_dir: tempfile::TempDir,
        _repo_dir: tempfile::TempDir,
    }

    impl Harness {
        fn new() -> Self {
            let config_dir = tempfile::tempdir().unwrap();
            let repo_dir = tempfile::tempdir().unwrap();
            let repo = repo_dir.path().to_path_buf();
            std::fs::create_dir_all(repo.join(".git")).unwrap();

            let git = Arc::new(FakeGit::with_main(repo.clone(), "main"));
            let store = FileConfigStore::new(
                AppPaths::rooted(config_dir.path()),
                git,
                Arc::new(Engine::new()),
                Arc::new(FakeClock::new()),
            );

            Self {
                store,
                repo,
                _config_dir: config_dir,
                _repo_dir: repo_dir,
            }
        }

        fn write_repo_config(&self, contents: &str) {
            std::fs::write(self.repo.join("wtm.toml"), contents).unwrap();
        }

        fn write_local_config(&self, contents: &str) {
            std::fs::write(self.repo.join(".git/wtm.local.toml"), contents).unwrap();
        }

        fn approve(&self, filename: &str) {
            let path = if filename == "wtm.toml" {
                self.repo.join("wtm.toml")
            } else {
                self.repo.join(".git").join(filename)
            };
            self.store.set_trust(&path, TrustDecision::Approve).unwrap();
        }
    }

    #[test]
    fn a_repo_with_no_config_loads_the_built_in_defaults() {
        // The zero-configuration promise.
        let h = Harness::new();
        let project = h.store.load(&h.repo).unwrap();
        assert!(
            project.field("name").is_some(),
            "the default form must be present"
        );
        assert!(project.field("base").is_some());
        assert_eq!(project.root, h.repo);
    }

    #[test]
    fn a_config_with_no_commands_needs_no_trust_prompt() {
        // Only executable content is a trust boundary.
        let h = Harness::new();
        h.write_repo_config("[naming]\nbranch = '{{ name | slugify }}-x'\n");
        let project = h.store.load(&h.repo).unwrap();
        assert!(project.naming.branch.ends_with("-x"));
    }

    /// The security gate.
    #[test]
    fn a_config_declaring_a_command_is_refused_until_approved() {
        let h = Harness::new();
        h.write_repo_config("[setup]\nrun = ['./bin/setup.sh', '{{ worktree.path }}']\n");

        match h.store.load(&h.repo).unwrap_err() {
            ConfigError::Untrusted { commands, path, .. } => {
                assert_eq!(path, h.repo.join("wtm.toml"));
                assert!(
                    commands
                        .iter()
                        .any(|c| c.first().map(String::as_str) == Some("./bin/setup.sh")),
                    "the prompt must show what would run: {commands:?}"
                );
            }
            other => panic!("expected Untrusted, got {other:?}"),
        }

        h.approve("wtm.toml");
        let project = h.store.load(&h.repo).unwrap();
        assert!(project.setup.is_some());
    }

    #[test]
    fn editing_an_approved_config_re_arms_the_prompt() {
        let h = Harness::new();
        h.write_repo_config("[setup]\nrun = ['./bin/setup.sh']\n");
        h.approve("wtm.toml");
        h.store.load(&h.repo).unwrap();

        // Someone (or a `git pull`) changes the command.
        h.write_repo_config("[setup]\nrun = ['curl', 'evil.example']\n");
        assert!(
            matches!(h.store.load(&h.repo), Err(ConfigError::Untrusted { .. })),
            "a changed command must not inherit the old approval"
        );
    }

    #[test]
    fn the_trust_prompt_lists_commands_from_every_site() {
        let h = Harness::new();
        h.write_repo_config(
            "[[field]]\nkey = 'env'\nlabel = 'Env'\nkind = 'select'\n\
             [field.options]\nkind = 'command'\nrun = ['./bin/envs.sh']\n\n\
             [[lookup]]\nid = 'jira'\nrun = ['acli', 'jira', 'view']\n\n\
             [setup]\nrun = ['./bin/setup.sh']\n\n\
             [[action]]\nid = 'start'\nlabel = 'Start'\nrun = ['just', 'start']\n",
        );

        match h.store.load(&h.repo).unwrap_err() {
            ConfigError::Untrusted { commands, .. } => {
                let programs: Vec<&str> = commands
                    .iter()
                    .filter_map(|c| c.first().map(String::as_str))
                    .collect();
                for expected in ["./bin/envs.sh", "acli", "./bin/setup.sh", "just"] {
                    assert!(
                        programs.contains(&expected),
                        "missing {expected} in {programs:?}"
                    );
                }
            }
            other => panic!("expected Untrusted, got {other:?}"),
        }
    }

    #[test]
    fn an_invalid_config_still_shows_its_commands_in_the_prompt() {
        // Trust is checked before validation on purpose: refusing to say what a broken
        // config would run is exactly backwards.
        let h = Harness::new();
        h.write_repo_config("[setup]\nrun = ['./bin/setup.sh']\n\n[naming]\nbranch = '{{ oops'\n");
        assert!(matches!(
            h.store.load(&h.repo),
            Err(ConfigError::Untrusted { .. })
        ));
    }

    #[test]
    fn the_local_layer_overrides_the_repo_layer() {
        let h = Harness::new();
        h.write_repo_config("[naming]\nbranch = 'from-repo'\ndirectory = 'from-repo'\n");
        h.write_local_config("[naming]\nbranch = 'from-local'\n");

        let project = h.store.load(&h.repo).unwrap();
        assert_eq!(project.naming.branch, "from-local");
        assert_eq!(
            project.naming.directory, "from-repo",
            "untouched keys survive"
        );
    }

    #[test]
    fn both_repo_and_local_layers_are_trust_checked_independently() {
        let h = Harness::new();
        h.write_repo_config("[setup]\nrun = ['./bin/setup.sh']\n");
        h.write_local_config("[[action]]\nid = 'x'\nlabel = 'X'\nrun = ['./bin/local.sh']\n");

        h.approve("wtm.toml");
        match h.store.load(&h.repo).unwrap_err() {
            ConfigError::Untrusted { path, .. } => {
                assert!(
                    path.ends_with("wtm.local.toml"),
                    "the local layer needs its own approval"
                );
            }
            other => panic!("expected Untrusted for the local layer, got {other:?}"),
        }

        h.approve("wtm.local.toml");
        h.store.load(&h.repo).unwrap();
    }

    #[test]
    fn provenance_reports_which_layers_contributed() {
        let h = Harness::new();
        assert_eq!(
            h.store.provenance(&h.repo).unwrap().len(),
            1,
            "only the built-in layer with no files present"
        );

        h.write_repo_config("[naming]\nbranch = 'x'\n");
        let layers = h.store.provenance(&h.repo).unwrap();
        assert_eq!(layers.len(), 2);
        assert_eq!(layers[0].layer, ConfigLayer::BuiltIn);
        assert_eq!(layers[1].layer, ConfigLayer::Repo);
    }

    #[test]
    fn a_validation_error_names_the_most_specific_file() {
        let h = Harness::new();
        h.write_repo_config("[naming]\nbranch = 'ok'\n");
        h.write_local_config("[naming]\nbranch = '{{ worktree.path }}'\n");

        match h.store.load(&h.repo).unwrap_err() {
            ConfigError::Invalid { path, layer, .. } => {
                assert!(path.ends_with("wtm.local.toml"), "got {}", path.display());
                assert_eq!(layer, ConfigLayer::Local);
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn a_toml_syntax_error_reports_a_line() {
        let h = Harness::new();
        h.write_repo_config("[naming\nbranch = 'x'\n");
        match h.store.load(&h.repo).unwrap_err() {
            ConfigError::Invalid { line, .. } => assert!(line.is_some(), "expected a line number"),
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn the_cache_is_invalidated_when_a_layer_file_changes() {
        let h = Harness::new();
        h.write_repo_config("[naming]\nbranch = 'first'\n");
        assert_eq!(h.store.load(&h.repo).unwrap().naming.branch, "first");

        h.write_repo_config("[naming]\nbranch = 'second'\n");
        assert_eq!(
            h.store.load(&h.repo).unwrap().naming.branch,
            "second",
            "an edit must invalidate the cache"
        );
    }

    #[test]
    fn adding_a_local_layer_invalidates_the_cache() {
        let h = Harness::new();
        h.write_repo_config("[naming]\nbranch = 'repo'\n");
        h.store.load(&h.repo).unwrap();

        h.write_local_config("[naming]\nbranch = 'local'\n");
        assert_eq!(h.store.load(&h.repo).unwrap().naming.branch, "local");
    }

    #[test]
    fn projects_can_be_registered_and_unregistered() {
        let h = Harness::new();
        assert!(h.store.projects().unwrap().is_empty());

        h.store.register_project(&h.repo).unwrap();
        assert_eq!(h.store.projects().unwrap().len(), 1);

        // Registering twice must not duplicate.
        h.store.register_project(&h.repo).unwrap();
        assert_eq!(h.store.projects().unwrap().len(), 1);

        h.store.unregister_project(&h.repo).unwrap();
        assert!(h.store.projects().unwrap().is_empty());
    }

    #[test]
    fn user_preferences_round_trip() {
        let h = Harness::new();
        // There is always a theme — `system` is a real value, not an absence.
        assert_eq!(
            h.store.user_pref("ui.theme").unwrap().as_deref(),
            Some("system")
        );
        h.store.set_user_pref("ui.theme", "dark").unwrap();
        assert_eq!(
            h.store.user_pref("ui.theme").unwrap().as_deref(),
            Some("dark")
        );
        h.store.set_user_pref("ui.theme", "light").unwrap();
        assert_eq!(
            h.store.user_pref("ui.theme").unwrap().as_deref(),
            Some("light")
        );
    }

    #[test]
    fn declared_commands_finds_run_arrays_at_any_depth() {
        let value = layers::document("[a.b.c]\nrun = ['x', 'y']\n\n[[d]]\nrun = ['z']\n").unwrap();
        let found = declared_commands_in(&value);
        assert!(
            found.contains(&vec!["x".to_owned(), "y".to_owned()]),
            "got {found:?}"
        );
        assert!(found.contains(&vec!["z".to_owned()]), "got {found:?}");
    }

    #[test]
    fn declared_commands_deduplicates() {
        let value = layers::document("[a]\nrun = ['x']\n\n[b]\nrun = ['x']\n").unwrap();
        assert_eq!(declared_commands_in(&value).len(), 1);
    }

    #[test]
    fn favorites_persist_to_disk_and_can_be_removed() {
        let h = Harness::new();
        h.store.register_project(&h.repo).unwrap();

        let a = h.repo.join("../wt-a").to_string_lossy().into_owned();
        h.store.set_favorite(&h.repo, &a, true).unwrap();
        assert_eq!(h.store.favorites(&h.repo).unwrap(), vec![a.clone()]);

        h.store.set_favorite(&h.repo, &a, false).unwrap();
        assert!(h.store.favorites(&h.repo).unwrap().is_empty());
    }

    #[test]
    fn favoriting_does_not_disturb_the_rest_of_the_app_config() {
        // The whole file is rewritten for one boolean, so the round-trip has to be safe.
        let h = Harness::new();
        h.store.register_project(&h.repo).unwrap();
        h.store.set_user_pref("ui.theme", "dark").unwrap();

        h.store.set_favorite(&h.repo, "/wt-a", true).unwrap();

        assert_eq!(
            h.store.user_pref("ui.theme").unwrap().as_deref(),
            Some("dark"),
            "an unrelated preference must survive a favorite"
        );
        assert_eq!(
            h.store.projects().unwrap(),
            vec![h.repo.clone()],
            "registration must survive a favorite"
        );
    }

    #[test]
    fn favoriting_an_unregistered_project_is_ignored_rather_than_an_error() {
        // Reachable only if the UI got ahead of the config; it must not create a phantom
        // project, and it must not fail the click either.
        let h = Harness::new();
        h.store.set_favorite(&h.repo, "/wt-a", true).unwrap();
        assert!(h.store.projects().unwrap().is_empty());
        assert!(h.store.favorites(&h.repo).unwrap().is_empty());
    }

    #[test]
    fn a_project_with_no_config_file_reports_no_favorites() {
        let h = Harness::new();
        assert!(h.store.favorites(&h.repo).unwrap().is_empty());
    }
}
