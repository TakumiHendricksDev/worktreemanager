//! The composition root.
//!
//! This is the *only* place in the workspace where a concrete adapter is chosen. Every
//! layer beneath holds `Arc<dyn Port>`, which is what makes the domain testable and the
//! adapters swappable — and what this file exists to pay for.
//!
//! Note that `Runner` and `PtyHostImpl` are built from **one** resolved `PATH` via
//! `wtm_exec::adapters`. Constructing them separately would allow two different resolved
//! paths, and the symptom would be a command that works in the terminal pane but not in a
//! captured preflight check — a genuinely baffling bug.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use wtm_config::{AppPaths, FileConfigStore, RealFileStore};
use wtm_core::error::{ConfigError, WtmError};
use wtm_core::model::{Project, WorkingTreeStatus, Worktree};
use wtm_core::ports::clock::Clock;
use wtm_core::ports::config::ConfigStore;
use wtm_core::ports::exec::CommandRunner;
use wtm_core::ports::fs::FileStore;
use wtm_core::ports::git::Git;
use wtm_core::ports::template::TemplateEngine;
use wtm_exec::{PtyHostImpl, ResolvedPath};
use wtm_git::GitCli;

use crate::display;
use crate::view::{DoctorView, ProjectView, ToolView, TrustPromptView, WorktreeView};

/// Tools a project config commonly invokes, reported by the diagnostics panel.
///
/// Not a dependency — wtm works fine without any of them — but when a project's config
/// calls one and it is missing, this is the fastest route to understanding why.
const KNOWN_TOOLS: &[&str] = &["git", "just", "acli", "docker", "gh", "bun", "npm"];

/// Everything the app needs, wired once at startup.
pub struct App {
    pub git: Arc<dyn Git>,
    pub runner: Arc<dyn CommandRunner>,
    pub pty: Arc<PtyHostImpl>,
    pub engine: Arc<dyn TemplateEngine>,
    pub files: Arc<dyn FileStore>,
    pub clock: Arc<dyn Clock>,
    pub config: Arc<FileConfigStore>,
    resolved_path: ResolvedPath,
    /// `os.*` template tokens, resolved once — they cannot change while running.
    os_tokens: BTreeMap<String, String>,
}

impl std::fmt::Debug for App {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("App")
            .field("resolved_path", &self.resolved_path.value)
            .finish_non_exhaustive()
    }
}

impl App {
    /// Build the app.
    ///
    /// # Errors
    ///
    /// If the config directory cannot be determined.
    pub fn new() -> Result<Self, ConfigError> {
        let paths = AppPaths::discover()?;
        Self::with_paths(paths)
    }

    /// Build with explicit config paths. Used by tests.
    ///
    /// # Errors
    ///
    /// Currently infallible in practice — a malformed user config degrades to defaults
    /// rather than failing, because refusing to start over a stray character in a
    /// preferences file would be worse than ignoring it. The `Result` stays because
    /// construction reads the filesystem and the next thing added here plausibly will
    /// fail; widening the signature later would churn every call site.
    #[allow(clippy::unnecessary_wraps)]
    pub fn with_paths(paths: AppPaths) -> Result<Self, ConfigError> {
        // Read the PATH override *before* building the runner, since the override is the
        // documented escape hatch for a bundled app that cannot see Homebrew.
        let path_override = wtm_config::UserConfig::load(&paths.config_file)
            .map(|config| config.exec.path)
            .unwrap_or_default();

        // One resolved PATH, shared. See the module docs.
        let (resolved_path, runner, pty, clock) = wtm_exec::adapters(path_override.as_deref());

        tracing::info!(
            path = %resolved_path.value,
            source = ?resolved_path.source,
            "resolved execution PATH"
        );

        let runner: Arc<dyn CommandRunner> = Arc::new(runner);
        let clock: Arc<dyn Clock> = Arc::new(clock);
        let git: Arc<dyn Git> = Arc::new(GitCli::new(Arc::clone(&runner)));
        let engine: Arc<dyn TemplateEngine> = Arc::new(wtm_render::Engine::new());

        let config = Arc::new(FileConfigStore::new(
            paths,
            Arc::clone(&git),
            Arc::clone(&engine),
            Arc::clone(&clock),
        ));

        Ok(Self {
            git,
            runner,
            pty: Arc::new(pty),
            engine,
            files: Arc::new(RealFileStore::new()),
            clock,
            config,
            resolved_path,
            os_tokens: wtm_exec::os_tokens(),
        })
    }

    #[must_use]
    pub fn os_tokens(&self) -> &BTreeMap<String, String> {
        &self.os_tokens
    }

    /// Every registered project, each annotated with whether it is usable.
    ///
    /// A project whose config fails to load is still listed, with the reason attached.
    /// Hiding it would leave the user with a repo that silently vanished from the
    /// sidebar and no way to find out why.
    pub fn projects(&self) -> Result<Vec<ProjectView>, ConfigError> {
        let mut out = Vec::new();

        for root in self.config.projects()? {
            out.push(match self.config.load(&root) {
                Ok(project) => crate::view::project_view(&project),
                Err(err) => {
                    let (problem, trust) = match &err {
                        ConfigError::Untrusted {
                            path,
                            commands,
                            content_hash,
                        } => (
                            "This project's configuration declares commands that need your \
                             approval before it can be used."
                                .to_owned(),
                            Some(TrustPromptView {
                                path: path.to_string_lossy().into_owned(),
                                commands: commands.clone(),
                                content_hash: content_hash.clone(),
                            }),
                        ),
                        other => (other.to_string(), None),
                    };

                    ProjectView {
                        id: root.to_string_lossy().into_owned(),
                        name: root.file_name().map_or_else(
                            || root.to_string_lossy().into_owned(),
                            |n| n.to_string_lossy().into_owned(),
                        ),
                        root: root.to_string_lossy().into_owned(),
                        usable: false,
                        problem: Some(problem),
                        trust,
                    }
                }
            });
        }

        Ok(out)
    }

    /// Resolve a project id back to its loaded config.
    pub fn project(&self, project_id: &str) -> Result<Project, WtmError> {
        let root = PathBuf::from(project_id);
        if !self.files.is_dir(&root) {
            return Err(WtmError::UnknownProject(project_id.to_owned()));
        }
        Ok(self.config.load(&root)?)
    }

    /// List a project's worktrees, rendered for display.
    ///
    /// Prunes first: git keeps reporting worktrees whose directories were deleted by
    /// hand, and a stale entry would pollute the sidebar, the branch-conflict check and
    /// any "is this path free" test.
    pub fn worktrees(&self, project: &Project) -> Result<Vec<WorktreeView>, WtmError> {
        if let Err(err) = self.git.prune_worktrees(&project.root) {
            // Not fatal: a read-only or locked repo can still be listed.
            tracing::warn!(error = %err, "worktree prune failed; listing anyway");
        }

        let worktrees = self.git.list_worktrees(&project.root)?;
        let base = self.base_branch(project, &worktrees);

        // Read once for the whole list, not once per row. A missing or unreadable app
        // config means nothing is starred, which is the right answer anyway.
        let favorites: BTreeSet<String> = self
            .config
            .favorites(&project.root)
            .unwrap_or_else(|err| {
                tracing::warn!(error = %err, "cannot read favorites; treating none as starred");
                Vec::new()
            })
            .into_iter()
            .collect();

        Ok(worktrees
            .iter()
            .map(|worktree| {
                let status = self.status_of(worktree, base.as_deref());
                display::worktree_view(
                    project,
                    worktree,
                    status,
                    favorites.contains(worktree.id.as_str()),
                    self.files.as_ref(),
                    self.engine.as_ref(),
                    &self.os_tokens,
                )
            })
            .collect())
    }

    /// Star or unstar a worktree.
    ///
    /// Takes the id verbatim: [`WorktreeId`](wtm_core::model::WorktreeId) *is* the absolute
    /// path, so what gets stored is exactly the string the next `worktrees` call will
    /// compare against. Resolving it through git first would cost a process spawn to
    /// re-derive a value we already hold.
    pub fn set_favorite(
        &self,
        project: &Project,
        worktree_id: &str,
        favorite: bool,
    ) -> Result<(), WtmError> {
        Ok(self
            .config
            .set_favorite(&project.root, worktree_id, favorite)?)
    }

    /// Status plus divergence for one worktree.
    ///
    /// Failures degrade to "clean" rather than propagating: a worktree whose directory is
    /// gone should still appear in the list (flagged prunable), and a `git status` error
    /// is not a reason to fail the whole refresh.
    fn status_of(&self, worktree: &Worktree, base: Option<&str>) -> WorkingTreeStatus {
        let mut status = self.git.status(&worktree.path).unwrap_or_default();

        if let (Some(branch), Some(base)) = (worktree.branch(), base)
            && let Ok((ahead, behind)) = self.git.ahead_behind(&worktree.path, branch, base)
        {
            status.ahead = ahead;
            status.behind = behind;
        }

        status
    }

    /// The ref to measure divergence against.
    ///
    /// Uses the project's configured default base when it resolves, else the main
    /// worktree's own branch. Without a base there is nothing meaningful to be "ahead of",
    /// so `None` simply means no ahead/behind is shown.
    fn base_branch(&self, project: &Project, worktrees: &[Worktree]) -> Option<String> {
        let configured = project
            .field(&project.create.base_field)
            .and_then(|field| field.default.as_ref())
            .map(wtm_core::model::FieldDefault::as_string)
            .filter(|base| {
                // Only use it if it actually resolves. A config can name `origin/develop`
                // in a clone that has never fetched, and measuring against a ref that does
                // not exist would report every worktree as wildly diverged.
                self.git
                    .rev_parse(&project.root, base)
                    .ok()
                    .flatten()
                    .is_some()
            });

        configured.or_else(|| {
            worktrees
                .first()
                .and_then(|main| main.branch().map(|b| b.as_str().to_owned()))
        })
    }

    /// Diagnostics for the panel that answers "why can't it find `just`?".
    pub fn doctor(&self) -> DoctorView {
        DoctorView {
            resolved_path: self.resolved_path.value.clone(),
            path_source: format!("{:?}", self.resolved_path.source),
            config_dir: self
                .config
                .paths()
                .config_dir
                .to_string_lossy()
                .into_owned(),
            tools: KNOWN_TOOLS
                .iter()
                .map(|name| ToolView {
                    name: (*name).to_owned(),
                    path: self
                        .runner
                        .which(name)
                        .map(|p| p.to_string_lossy().into_owned()),
                })
                .collect(),
        }
    }

    /// Find a worktree by id within a project.
    pub fn worktree(&self, project: &Project, worktree_id: &str) -> Result<Worktree, WtmError> {
        self.git
            .list_worktrees(&project.root)?
            .into_iter()
            .find(|w| w.id.as_str() == worktree_id)
            .ok_or_else(|| WtmError::UnknownWorktree(worktree_id.to_owned()))
    }

    /// Register a repository, resolving whatever path the user picked to its root.
    ///
    /// Accepting a subdirectory is deliberate: people drag in a folder they happen to be
    /// looking at, and resolving to the toplevel is what they meant.
    pub fn register(&self, path: &Path) -> Result<PathBuf, WtmError> {
        let expanded = expand_tilde(path);
        let root = self.git.repo_root(&expanded)?;
        self.config.register_project(&root)?;
        Ok(root)
    }
}

impl App {
    /// Build the create pipeline.
    ///
    /// Constructed per call rather than stored: it is a handful of `Arc` clones, and keeping it
    /// stateless means there is no cached project config to go stale between invocations.
    #[must_use]
    pub fn create_pipeline(&self) -> wtm_core::usecase::CreatePipeline {
        wtm_core::usecase::CreatePipeline {
            git: Arc::clone(&self.git),
            runner: Arc::clone(&self.runner),
            pty: Arc::clone(&self.pty) as Arc<dyn wtm_core::ports::pty::PtyHost>,
            engine: Arc::clone(&self.engine),
            files: Arc::clone(&self.files),
            clock: Arc::clone(&self.clock),
        }
    }

    #[must_use]
    pub fn remove_pipeline(&self) -> wtm_core::usecase::RemovePipeline {
        wtm_core::usecase::RemovePipeline {
            git: Arc::clone(&self.git),
            runner: Arc::clone(&self.runner),
            pty: Arc::clone(&self.pty) as Arc<dyn wtm_core::ports::pty::PtyHost>,
            engine: Arc::clone(&self.engine),
        }
    }
}

/// Expand a leading `~` using the real `HOME`.
///
/// Done here rather than in the frontend because the webview has no `HOME` to expand against.
/// A shell would have done this before the path ever reached an argument, so a typed path that
/// starts with `~` is what a person naturally writes.
fn expand_tilde(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if (text == "~" || text.starts_with("~/"))
        && let Some(home) = std::env::var_os("HOME")
    {
        let home = PathBuf::from(home);
        return if text == "~" {
            home
        } else {
            home.join(text.trim_start_matches("~/"))
        };
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn building_the_app_resolves_a_usable_path() {
        let dir = tempfile::tempdir().expect("temp dir");
        let app = App::with_paths(AppPaths::rooted(dir.path())).expect("app should build");

        let doctor = app.doctor();
        assert!(!doctor.resolved_path.is_empty());
        assert!(
            doctor
                .tools
                .iter()
                .any(|t| t.name == "git" && t.path.is_some()),
            "git must be findable: {:?}",
            doctor.tools
        );
    }

    #[test]
    fn a_fresh_install_has_no_registered_projects() {
        let dir = tempfile::tempdir().expect("temp dir");
        let app = App::with_paths(AppPaths::rooted(dir.path())).expect("app should build");
        assert!(app.projects().expect("projects").is_empty());
    }

    #[test]
    fn registering_a_subdirectory_records_the_repo_root() {
        let fixture = wtm_testkit::GitFixture::new();
        let nested = fixture.root().join("nested/deeper");
        std::fs::create_dir_all(&nested).expect("create nested dirs");

        let dir = tempfile::tempdir().expect("temp dir");
        let app = App::with_paths(AppPaths::rooted(dir.path())).expect("app should build");

        let root = app.register(&nested).expect("register");
        assert_eq!(
            std::fs::canonicalize(&root).expect("canonicalize"),
            std::fs::canonicalize(fixture.root()).expect("canonicalize"),
            "a subdirectory must resolve to the repo root"
        );
        assert_eq!(app.projects().expect("projects").len(), 1);
    }

    #[test]
    fn a_leading_tilde_is_expanded_against_the_real_home() {
        // A typed path starting with `~` is what a person writes, and the webview cannot
        // expand it.
        let home = PathBuf::from(std::env::var_os("HOME").expect("HOME"));
        assert_eq!(expand_tilde(Path::new("~")), home);
        assert_eq!(
            expand_tilde(Path::new("~/Sites/repo")),
            home.join("Sites/repo")
        );
        // Anything else is untouched — including a `~` that is not at the start.
        assert_eq!(
            expand_tilde(Path::new("/abs/path")),
            PathBuf::from("/abs/path")
        );
        assert_eq!(
            expand_tilde(Path::new("rel/path")),
            PathBuf::from("rel/path")
        );
        assert_eq!(expand_tilde(Path::new("/x/~/y")), PathBuf::from("/x/~/y"));
        assert_eq!(expand_tilde(Path::new("~user/x")), PathBuf::from("~user/x"));
    }

    #[test]
    fn registering_a_non_repository_fails() {
        let plain = tempfile::tempdir().expect("temp dir");
        let dir = tempfile::tempdir().expect("temp dir");
        let app = App::with_paths(AppPaths::rooted(dir.path())).expect("app should build");
        assert!(app.register(plain.path()).is_err());
    }

    #[test]
    fn worktrees_are_listed_and_rendered_with_defaults_only() {
        // End to end through the real adapters, with no project config at all.
        let fixture = wtm_testkit::GitFixture::new();
        fixture.add_worktree("ACME-1-alpha", "task/ACME-1-alpha");
        fixture.add_detached_worktree("scratch");

        let dir = tempfile::tempdir().expect("temp dir");
        let app = App::with_paths(AppPaths::rooted(dir.path())).expect("app should build");
        let root = app.register(fixture.root()).expect("register");
        let project = app
            .project(&root.to_string_lossy())
            .expect("project should load");

        let views = app.worktrees(&project).expect("worktrees");
        assert_eq!(views.len(), 3);

        let main = &views[0];
        assert!(main.is_main);
        assert_eq!(main.branch.as_deref(), Some("main"));
        assert_eq!(
            main.title, main.dirname,
            "the default title is the directory name"
        );

        let alpha = views
            .iter()
            .find(|v| v.dirname == "ACME-1-alpha")
            .expect("alpha");
        assert_eq!(alpha.branch.as_deref(), Some("task/ACME-1-alpha"));
        assert_eq!(alpha.issue_key.as_deref(), Some("ACME-1"));

        let detached = views
            .iter()
            .find(|v| v.dirname == "scratch")
            .expect("scratch");
        assert_eq!(
            detached.branch, None,
            "a detached worktree must not invent a branch"
        );
        assert_eq!(detached.subtitle, "(detached)");
    }

    #[test]
    fn a_stale_worktree_entry_is_pruned_before_listing() {
        let fixture = wtm_testkit::GitFixture::new();
        let doomed = fixture.add_worktree("doomed", "task/doomed");
        fixture.orphan_worktree(&doomed);

        let dir = tempfile::tempdir().expect("temp dir");
        let app = App::with_paths(AppPaths::rooted(dir.path())).expect("app should build");
        let root = app.register(fixture.root()).expect("register");
        let project = app
            .project(&root.to_string_lossy())
            .expect("project should load");

        let views = app.worktrees(&project).expect("worktrees");
        assert!(
            views.iter().all(|v| v.dirname != "doomed"),
            "a deleted worktree must not linger in the sidebar"
        );
    }

    #[test]
    fn an_untrusted_project_is_still_listed_with_a_trust_prompt() {
        // Hiding it would leave a repo that silently vanished and no way to find out why.
        let fixture = wtm_testkit::GitFixture::new();
        std::fs::write(
            fixture.root().join("wtm.toml"),
            "[setup]\nrun = ['./bin/setup.sh']\n",
        )
        .expect("write config");

        let dir = tempfile::tempdir().expect("temp dir");
        let app = App::with_paths(AppPaths::rooted(dir.path())).expect("app should build");
        app.register(fixture.root()).expect("register");

        let projects = app.projects().expect("projects");
        assert_eq!(projects.len(), 1);
        assert!(!projects[0].usable);
        let trust = projects[0]
            .trust
            .as_ref()
            .expect("a trust prompt must be offered");
        assert!(trust.commands.iter().any(|c| c[0] == "./bin/setup.sh"));
    }

    #[test]
    fn an_unknown_project_id_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().expect("temp dir");
        let app = App::with_paths(AppPaths::rooted(dir.path())).expect("app should build");
        assert!(matches!(
            app.project("/definitely/not/here"),
            Err(WtmError::UnknownProject(_))
        ));
    }
}
