//! The composition root.
//!
//! This is the *only* place in the workspace where a concrete adapter is chosen. Every
//! layer beneath holds `Arc<dyn Port>`, which is what makes the domain testable and the
//! adapters swappable — and what this file exists to pay for.
//!
//! Note that `Runner`, `PtyHostImpl` and `PipeHostImpl` are built from **one** resolved
//! `PATH` via `wtm_exec::adapters`. Constructing them separately would allow different
//! resolved paths, and the symptom would be a command that works in the terminal pane but
//! not in a captured preflight check, or an agent CLI the picker lists and the spawn cannot
//! find — genuinely baffling bugs.

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
use wtm_exec::{PipeHostImpl, PtyHostImpl, ResolvedPath};
use wtm_git::GitCli;

use crate::display;
use crate::view::{DoctorView, PaletteView, ProjectView, ToolView, TrustPromptView, WorktreeView};

/// Tools a project config commonly invokes, reported by the diagnostics panel.
///
/// Not a dependency — wtm works fine without any of them — but when a project's config
/// calls one and it is missing, this is the fastest route to understanding why.
const KNOWN_TOOLS: &[&str] = &["git", "just", "acli", "docker", "gh", "bun", "npm"];

/// How many finished sessions the pty registry keeps.
///
/// Not about transcripts, despite what `reap_finished`'s own doc suggests — a `Session`
/// holds no output buffer at all; the transcript lives in the pane's xterm instance. What a
/// finished entry actually holds is a `Box<dyn MasterPty>` and its writer, which measures as
/// two file descriptors apiece, forever. Nothing in the app called `reap_finished` before the
/// terminal dock existed, and a dock that opens and closes shells all day is what turns that
/// into a leak with a name.
///
/// Four rather than zero for one reason: a pane that has not yet processed `pty:exit` may
/// still have a `pty_write` or `pty_resize` in flight, and a `NoSuchSession` error for a
/// session that ended a moment ago reads like a bug in the log. Four is eight descriptors.
const KEEP_FINISHED_SESSIONS: usize = 4;

/// A worktree's terminal-dock shell: which project it belongs to, and its session.
type Shell = (String, wtm_core::model::SessionId);

/// One live agent session and what it belongs to.
///
/// A struct where [`Shell`] is a tuple, because this has four fields rather than two and
/// `(String, String, String, AgentSession)` at the call site is unreadable.
struct AgentEntry {
    project: String,
    worktree: String,
    provider: String,
    /// The id the *provider* knows this conversation by, once it has said.
    ///
    /// Empty until then. Kept so `resumable` can exclude a conversation that is already on screen —
    /// offering to resume one would hand the CLI two clients for one thread.
    provider_session: String,
    /// Behind an `Arc` so a lookup can hand the session out and drop the map's lock.
    ///
    /// Not for sharing — nothing holds a second long-lived reference. It exists so
    /// [`App::agent_session`] can return an owned handle rather than a borrow, which is what lets
    /// the guard die before the caller runs anything. See that function.
    session: Arc<wtm_agent::AgentSession>,
}

/// What the frontend needs to know about a live agent session.
///
/// Kept out of `view.rs` because it is not itself an IPC type — [`crate::view::AgentSessionView`]
/// is, and it is built from this. The split exists so `App` does not have to import the view
/// module to answer a question about its own state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSessionFacts {
    pub session: String,
    pub project: String,
    pub worktree: String,
    pub provider: String,
}

/// Everything the app needs, wired once at startup.
pub struct App {
    pub git: Arc<dyn Git>,
    pub runner: Arc<dyn CommandRunner>,
    /// The same runner as above, un-abstracted, for the one call that is not on the port:
    /// [`wtm_exec::Runner::launch_detached`]. See [`Self::with_paths`].
    pub launcher: Arc<wtm_exec::Runner>,
    pub pty: Arc<PtyHostImpl>,
    /// Agent sessions. A separate host from `pty` rather than a mode of it, because these
    /// children speak a line protocol rather than talking to a person — see the port docs for
    /// why a terminal would corrupt that. Held concretely, like `pty`, because `reap_finished`
    /// and `kill_all` are adapter housekeeping rather than domain capabilities.
    pub pipe: Arc<PipeHostImpl>,
    pub engine: Arc<dyn TemplateEngine>,
    pub files: Arc<dyn FileStore>,
    pub clock: Arc<dyn Clock>,
    pub config: Arc<FileConfigStore>,
    resolved_path: ResolvedPath,
    /// `os.*` template tokens, resolved once — they cannot change while running.
    os_tokens: BTreeMap<String, String>,
    /// Which session is a worktree's **terminal-dock shell**, keyed by its `WorktreeId`.
    ///
    /// # Why an index here rather than a query on the pty host
    ///
    /// `PtySession::worktree` is already recorded and `PtyHost::has_session_for` already
    /// answers "is anything running for this worktree" — but *anything* is the problem.
    /// `run_action` and the setup stage tag their sessions with the same worktree id, so a
    /// lookup by worktree alone would hand the dock the session of a running `just test` and
    /// let the user type into it. The worktree was never a unique key.
    ///
    /// The alternative was a session *kind* threaded through `PtyHost::spawn`. Rejected for
    /// the same reason [`Self::palettes`] is assembled here rather than in the domain: "which
    /// session is the UI's terminal" is a frontend concept `wtm-core` has no stake in, and
    /// keeping it in the composition root means `wtm-core` still compiles for `wasm32`.
    ///
    /// Liveness is never read from here. Every lookup intersects with `PtyHostImpl::sessions`,
    /// which reports running sessions only, so an entry for a shell the user exited answers
    /// "no shell" without anybody having to remember to clean up.
    shells: parking_lot::Mutex<BTreeMap<String, Shell>>,
    /// Live agent sessions, keyed by **session id**.
    ///
    /// Keyed differently from [`Self::shells`] on purpose. That map is keyed by worktree because
    /// a worktree has one dock shell; several agent sessions in one worktree is the whole point
    /// of this feature, so the unique thing is the session and the worktree is a field on it.
    ///
    /// Here rather than on the port for the same reason `shells` is: "which sessions does this
    /// pane show" is a frontend concept `wtm-core` has no stake in, and keeping it in the
    /// composition root is what preserves the `wasm32` check. Liveness is never read from it —
    /// see `open_agent`.
    /// Tokens issued to sessions so their MCP bridge can be traced back to a worktree.
    ///
    /// On `App` rather than in the listener because it is written on the command path — a token is
    /// minted while a session's config is built — and read on the socket thread. One owner for both
    /// is what keeps that from needing a channel.
    pub handoff: crate::handoff::Hub,
    agents: parking_lot::Mutex<BTreeMap<wtm_core::model::SessionId, AgentEntry>>,
    /// Where the resume list lives, and the lock that serializes writes to it.
    ///
    /// Held rather than re-derived because it is written on every turn's first event, and a
    /// read-modify-write of a shared file needs a lock even when only one thread is expected to be
    /// doing it — two panes becoming ready at the same moment is the ordinary case.
    sessions_file: PathBuf,
    resume: parking_lot::Mutex<()>,
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
        // Captured before `paths` is moved into the config store below.
        let sessions_file = paths.sessions_file.clone();

        // Read the PATH override *before* building the runner, since the override is the
        // documented escape hatch for a bundled app that cannot see Homebrew.
        let path_override = wtm_config::UserConfig::load(&paths.config_file)
            .map(|config| config.exec.path)
            .unwrap_or_default();

        // One resolved PATH, shared. See the module docs.
        let (resolved_path, runner, pty, pipe, clock) =
            wtm_exec::adapters(path_override.as_deref());

        tracing::info!(
            path = %resolved_path.value,
            source = ?resolved_path.source,
            "resolved execution PATH"
        );

        // The same object behind two handles. `runner` is the port every use-case sees;
        // `launcher` is the concrete adapter, kept because `launch_detached` is
        // deliberately *not* on the port — no use-case launches a desktop application, and
        // putting it there would hand the domain an opinion about GUIs. Sharing one `Arc`
        // rather than constructing two runners preserves the invariant in this module's
        // header: one resolved PATH, so a program can never be findable by one and not the
        // other.
        let launcher = Arc::new(runner);
        let runner: Arc<dyn CommandRunner> = Arc::clone(&launcher) as Arc<dyn CommandRunner>;
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
            launcher,
            pty: Arc::new(pty),
            pipe: Arc::new(pipe),
            engine,
            files: Arc::new(RealFileStore::new()),
            clock,
            config,
            resolved_path,
            os_tokens: wtm_exec::os_tokens(),
            shells: parking_lot::Mutex::new(BTreeMap::new()),
            handoff: crate::handoff::Hub::default(),
            agents: parking_lot::Mutex::new(BTreeMap::new()),
            sessions_file: sessions_file.clone(),
            resume: parking_lot::Mutex::new(()),
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

    /// One environment value for one worktree, read fresh from disk.
    ///
    /// The counterpart to `WorktreeView::env` carrying names only: this is the single way a
    /// value crosses into the frontend, and it does so one key at a time on an explicit
    /// click. Read through the project's *declared* display sources, so it cannot be turned
    /// into "read me any key of any file" — only what the config already exposes.
    ///
    /// The value is deliberately never logged, at any level.
    pub fn env_value(
        &self,
        project: &Project,
        worktree_id: &str,
        key: &str,
    ) -> Result<String, WtmError> {
        let worktree = self.worktree(project, worktree_id)?;

        let mut ctx = display::base_context(project, &self.os_tokens);
        display::add_worktree_tokens(&mut ctx, &worktree);
        let sources =
            display::read_sources(project, self.files.as_ref(), self.engine.as_ref(), &ctx);

        project
            .display
            .sources
            .first()
            .and_then(|source| sources.get(&source.id))
            .and_then(|values| values.get(key))
            .cloned()
            // Reports the key, never a partial value.
            .ok_or_else(|| WtmError::UnknownEnvKey(key.to_owned()))
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

    /// The palettes the user declared in `[ui.palettes]`, validated for Settings.
    ///
    /// Reads the config file directly rather than going through `ConfigStore`, which is the
    /// same thing `with_paths` does for `exec.path` above. The port's own documentation is
    /// the reason: it keeps preferences deliberately untyped because "UI preferences are the
    /// frontend's business, and threading a `UiPrefs` struct through the domain would couple
    /// this crate to decisions it has no stake in." A palette is exactly such a decision, so
    /// it stays out of `wtm-core` and is assembled here in the composition root instead.
    ///
    /// A fresh read per call. This is opened from a settings dialog, so the cost is a file
    /// read nobody will measure, and the payoff is that hand-editing the config and
    /// reopening Settings shows the change without a restart.
    pub fn palettes(&self) -> Vec<PaletteView> {
        let config =
            wtm_config::UserConfig::load(&self.config.paths().config_file).unwrap_or_default();

        config
            .ui
            .palettes
            .iter()
            .map(|(id, def)| {
                let name = def.name.clone().unwrap_or_else(|| id.clone());
                let error = palette_problem(def);
                PaletteView {
                    id: id.clone(),
                    name,
                    hue: def.hue.unwrap_or_default(),
                    chroma: def.chroma.unwrap_or(1.0),
                    brand: if error.is_none() {
                        def.brand.clone()
                    } else {
                        Vec::new()
                    },
                    error,
                }
            })
            .collect()
    }

    /// Find a worktree by id within a project.
    pub fn worktree(&self, project: &Project, worktree_id: &str) -> Result<Worktree, WtmError> {
        self.git
            .list_worktrees(&project.root)?
            .into_iter()
            .find(|w| w.id.as_str() == worktree_id)
            .ok_or_else(|| WtmError::UnknownWorktree(worktree_id.to_owned()))
    }

    /// How the opener catalogue interrogates this machine.
    ///
    /// Borrows the app's own resolved `PATH`, so the picker cannot claim a tool is present
    /// that a spawn would then fail to find — the two would otherwise drift the moment
    /// `exec.path` is set.
    #[must_use]
    pub fn probe(&self) -> AppProbe<'_> {
        AppProbe { app: self }
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

/// The real machine, as the opener catalogue sees it.
///
/// A separate type rather than `impl Probe for App` so the trait stays satisfiable by a
/// fake: the catalogue's behaviour — "PyCharm is offered when installed" — must be
/// testable on a machine that does not have PyCharm.
#[derive(Debug)]
pub struct AppProbe<'a> {
    app: &'a App,
}

impl crate::openers::Probe for AppProbe<'_> {
    fn which(&self, program: &str) -> bool {
        self.app.runner.which(program).is_some()
    }

    fn app_bundle(&self, name: &str) -> bool {
        wtm_exec::app_bundle(name).is_some()
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

// ═══════════════════════════ the terminal dock's shells ═══════════════════════════

impl App {
    /// Open — or re-attach to — the interactive shell for one worktree.
    ///
    /// Idempotent by worktree: a second call while the shell is still running returns the
    /// same session rather than leaving two login shells in one directory fighting over the
    /// same history file.
    ///
    /// # Lock order
    ///
    /// [`Self::shells`] is held across the spawn, deliberately. Two open requests genuinely
    /// race on a double click — commands run on a blocking thread pool, so this is not
    /// theoretical — and checking, spawning and recording under one lock is what makes "one
    /// shell per worktree" true rather than merely likely. The order is always `shells` then
    /// the pty registry; nothing in `wtm-exec` knows this map exists, so there is no other
    /// direction it can be taken in.
    ///
    /// # Errors
    ///
    /// If the program cannot be found on the resolved `PATH`, or the pty cannot be opened.
    pub fn open_shell(
        &self,
        worktree: &Worktree,
        project_id: &str,
        argv: Vec<String>,
        rows: u16,
        cols: u16,
        sink: Arc<dyn wtm_core::ports::pty::PtySink>,
    ) -> Result<wtm_core::model::SessionId, wtm_core::error::ExecError> {
        use wtm_core::ports::pty::PtyHost;

        let mut shells = self.shells.lock();

        if let Some((_, session)) = shells
            .get(worktree.id.as_str())
            .filter(|(_, session)| self.is_running(session))
        {
            let session = session.clone();
            // The re-attaching pane has just measured itself, and that is not the size this
            // session was spawned at. Resizing here rather than waiting for the frontend's
            // first `ResizeObserver` fire is what stops a reattached shell wrapping at the
            // old width.
            if let Err(error) = self.pty.resize(&session, rows, cols) {
                tracing::debug!(%session, %error, "could not resize a shell on reattach");
            }
            return Ok(session);
        }

        // Before the spawn, so descriptors a finished session is still holding are released
        // before a new pair is allocated. This is the app's only caller.
        self.pty.reap_finished(KEEP_FINISHED_SESSIONS);

        // Entries whose shell has since exited are dead weight. Pruned on each open, which is
        // the only moment the map grows, so it stays the size of what is actually running.
        let running = self.running_sessions();
        shells.retain(|_, (_, session)| running.contains(session.as_str()));

        let inv = wtm_core::ports::exec::Invocation::new(
            argv,
            worktree.path.clone(),
            crate::commands::SHELL_TIMEOUT_MS,
        );
        // `inv.env` stays empty on purpose: `PtyHostImpl::spawn` already lays down
        // `child_env()` — so `PATH` and `LOGIN_PATH` are the resolved ones — and a `TERM`, and
        // a login shell is about to re-source the user's profile over the top of both anyway.
        let spawned = self
            .pty
            .spawn(&inv, rows, cols, Some(worktree.id.as_str()), sink)?;

        shells.insert(
            worktree.id.as_str().to_owned(),
            (project_id.to_owned(), spawned.session.clone()),
        );
        Ok(spawned.session)
    }

    /// Every worktree with a live dock shell, for a webview that has just reloaded.
    #[must_use]
    pub fn live_shells(&self) -> Vec<(String, Shell)> {
        let running = self.running_sessions();
        self.shells
            .lock()
            .iter()
            .filter(|(_, (_, session))| running.contains(session.as_str()))
            .map(|(worktree, shell)| (worktree.clone(), shell.clone()))
            .collect()
    }

    /// Kill a worktree's shell and forget it. `false` if it had none running.
    ///
    /// Forgetting is the load-bearing half. `PtyHost::kill` returns as soon as the group has
    /// been signalled, not once the child has been reaped, so an [`Self::open_shell`] issued
    /// immediately afterwards would still see the dying session as running and hand back the
    /// id of a shell that is about to close. Dropping the entry makes restart two ordinary
    /// calls with no ordering requirement on the frontend and no polling — which
    /// `ARCHITECTURE.md` §8 bans outright.
    pub fn close_shell(&self, worktree_id: &str) -> bool {
        use wtm_core::ports::pty::PtyHost;

        let Some((_, session)) = self.shells.lock().remove(worktree_id) else {
            return false;
        };
        match self.pty.kill(&session) {
            Ok(()) => true,
            Err(error) => {
                // Already finished: the user typed `exit` before reaching for the control.
                tracing::debug!(%session, %error, "no live shell to close");
                false
            }
        }
    }

    /// The ids of every session the pty host still reports as running.
    fn running_sessions(&self) -> BTreeSet<String> {
        use wtm_core::ports::pty::PtyHost;

        self.pty
            .sessions()
            .into_iter()
            .map(|s| s.session.as_str().to_owned())
            .collect()
    }

    /// Whether `session` is still running, according to the pty host.
    ///
    /// The single source of truth for liveness. [`Self::shells`] records only *which* session
    /// is a worktree's shell; asking it whether that shell is alive would be believing a cache
    /// over the process table.
    fn is_running(&self, session: &wtm_core::model::SessionId) -> bool {
        use wtm_core::ports::pty::PtyHost;

        self.pty.sessions().iter().any(|s| &s.session == session)
    }

    // ─────────────────────────── agent sessions ───────────────────────────

    /// Start an agent session in `worktree`.
    ///
    /// # Why this index is keyed by session and not by worktree
    ///
    /// [`Self::shells`] is keyed by worktree because a worktree has exactly one dock shell, and
    /// the whole difficulty there was that the worktree *was not* a unique key against the pty
    /// host's other sessions. Here the shape is different by design: several agent sessions in
    /// one worktree is the feature, so the session id is the key and the worktree is a field.
    ///
    /// Liveness is still never read from this map — [`Self::live_agents`] intersects it with what
    /// the pipe host reports as running, so an entry for a session whose CLI exited answers
    /// "not running" with nobody having to remember to clean up. Same rule as the shells map, for
    /// the same reason.
    ///
    /// # Errors
    ///
    /// If the provider is unknown to this build, or the CLI cannot be spawned.
    pub fn open_agent(
        &self,
        entry: &'static wtm_agent::ProviderEntry,
        req: &wtm_agent::SessionRequest,
        worktree: &Worktree,
        project_id: &str,
        events: &Arc<dyn wtm_agent::session::AgentSink>,
    ) -> Result<wtm_core::model::SessionId, wtm_core::error::ExecError> {
        // Before the spawn, so descriptors a finished session still holds are released before a
        // new set is allocated. The pty host's only caller does the same, for the same reason.
        self.pipe.reap_finished(KEEP_FINISHED_SESSIONS);

        let session = wtm_agent::AgentSession::open(
            entry.provider,
            req,
            Arc::clone(&self.pipe) as Arc<dyn wtm_core::ports::pipe::PipeHost>,
            events,
            // The same inert one-week deadline the dock's shell uses. `PipeHost` has no `wait`,
            // so nothing enforces it — see the port's docs.
            crate::commands::SHELL_TIMEOUT_MS,
            Some(worktree.id.as_str()),
        )?;

        let id = session.id().clone();
        let mut agents = self.agents.lock();

        // Entries whose CLI has since exited are dead weight. Pruned on each open, which is the
        // only moment the map grows, so it stays the size of what is actually running.
        let running = self.running_agents();
        agents.retain(|session, _| running.contains(session.as_str()));

        agents.insert(
            id.clone(),
            AgentEntry {
                project: project_id.to_owned(),
                worktree: worktree.id.as_str().to_owned(),
                provider: entry.id.to_owned(),
                provider_session: String::new(),
                session: Arc::new(session),
            },
        );
        Ok(id)
    }

    /// Every agent session still running, with what it belongs to.
    ///
    /// All projects, not one — the same choice [`Self::live_shells`] makes, so a project switch
    /// needs no second round trip and the frontend filters by what is in its listing.
    #[must_use]
    pub fn live_agents(&self) -> Vec<AgentSessionFacts> {
        let running = self.running_agents();
        self.agents
            .lock()
            .iter()
            .filter(|(session, _)| running.contains(session.as_str()))
            .map(|(session, entry)| AgentSessionFacts {
                session: session.as_str().to_owned(),
                project: entry.project.clone(),
                worktree: entry.worktree.clone(),
                provider: entry.provider.clone(),
            })
            .collect()
    }

    /// The session behind an id, with the map's lock already released.
    ///
    /// A separate function, and that is the entire point: the guard dies at this closing brace, so
    /// a caller physically cannot still be holding it while it uses what came back. Doing the
    /// lookup inline and using the result in the same scope is the bug this shape exists to make
    /// unwriteable — see [`Self::with_agent`].
    ///
    /// # Errors
    ///
    /// If no session with that id is in the map.
    fn agent_session(
        &self,
        session: &str,
    ) -> Result<Arc<wtm_agent::AgentSession>, wtm_core::error::ExecError> {
        let id = wtm_core::model::SessionId::new(session);
        self.agents
            .lock()
            .get(&id)
            .map(|entry| Arc::clone(&entry.session))
            .ok_or_else(|| wtm_core::error::ExecError::NoSuchSession(session.to_owned()))
    }

    /// Run `f` against a live agent session.
    ///
    /// Takes a closure rather than handing out the session, so a caller cannot keep an
    /// `&AgentSession` past the lookup and use one this map has already replaced.
    ///
    /// # Why the lookup is a separate function
    ///
    /// This held `agents` across `f` for one release, and it deadlocked every Send.
    /// `AgentSink::on_event` runs **synchronously on the calling thread** — `wtm-agent` guarantees
    /// it, and `session_wiring.rs` pins it — and the first step of a turn is `Emit(UserEcho)`.
    /// [`AgentEventSink::title`](crate::agent_bridge) answers that by calling
    /// [`Self::live_agents`], which locks this same map. `parking_lot::Mutex` is not reentrant, so
    /// the second acquisition parked the thread forever: no turn reached the CLI's stdin, and
    /// because the guard was never dropped, every later `with_agent`, `open_agent` and
    /// `close_agent` queued up behind it.
    ///
    /// So this must not be flattened back into one function, however much it reads like an
    /// indirection. A sink that reaches back into `App` is the design, not an accident — the
    /// resumable list is written from events — and the only durable defence is that there is no
    /// live guard to re-enter.
    ///
    /// # Errors
    ///
    /// If no session with that id is in the map, or if `f` fails.
    pub fn with_agent<T>(
        &self,
        session: &str,
        f: impl FnOnce(&wtm_agent::AgentSession) -> Result<T, wtm_core::error::ExecError>,
    ) -> Result<T, wtm_core::error::ExecError> {
        let session = self.agent_session(session)?;
        f(&session)
    }

    /// End an agent session and forget it.
    ///
    /// Forgetting is the load-bearing half, exactly as it is in [`Self::close_shell`]: `close`
    /// returns once the group has been signalled rather than once the child is reaped, so a
    /// re-open would otherwise be handed the id of a dying session.
    pub fn close_agent(&self, session: &str) -> bool {
        let id = wtm_core::model::SessionId::new(session);
        let Some(entry) = self.agents.lock().remove(&id) else {
            return false;
        };
        match entry.session.close() {
            Ok(()) => true,
            Err(error) => {
                // Already finished: the CLI exited before the user reached for the control.
                tracing::debug!(%session, %error, "no live agent session to close");
                false
            }
        }
    }

    /// Every agent session the *worktree* has, live or not, for teardown.
    ///
    /// Used when a worktree is removed: its sessions have to end before `git worktree remove`
    /// runs, for the same reason the dock shell does — an agent mid-turn is writing into the
    /// directory git is about to refuse to delete.
    #[must_use]
    pub fn agents_in(&self, worktree_id: &str) -> Vec<String> {
        self.agents
            .lock()
            .iter()
            .filter(|(_, entry)| entry.worktree == worktree_id)
            .map(|(session, _)| session.as_str().to_owned())
            .collect()
    }

    /// Record a session as resumable.
    ///
    /// Called when a session reports the id its provider knows it by — which is the first moment
    /// resuming is possible, and before the first reply, so a session that fails mid-turn is still
    /// in the list. Errors are logged rather than surfaced: failing to write a resume entry must not
    /// fail the session it belongs to.
    pub fn remember_session(&self, record: wtm_config::SessionRecord) {
        let _guard = self.resume.lock();
        let mut store = wtm_config::SessionStore::load(&self.sessions_file);
        store.remember(record);
        if let Err(error) = store.save(&self.sessions_file) {
            tracing::warn!(%error, "could not write the resume list");
        }
    }

    /// Forget a conversation, because the user closed it for good.
    pub fn forget_session(&self, provider: &str, provider_session: &str) {
        let _guard = self.resume.lock();
        let mut store = wtm_config::SessionStore::load(&self.sessions_file);
        store.forget(provider, provider_session);
        if let Err(error) = store.save(&self.sessions_file) {
            tracing::warn!(%error, "could not write the resume list");
        }
    }

    /// Give a remembered session a label, if it does not have one.
    ///
    /// Its own method rather than a `remember` with a title, because the caller cannot use
    /// [`Self::resumable`] to find the record: that deliberately excludes sessions that are running,
    /// and the moment a title becomes available is the moment the session *is* running. Reading the
    /// store directly is the only way to reach it.
    ///
    /// Only names an unnamed one. A later turn rewriting the label would move it out from under a
    /// list the user has learned to read.
    pub fn title_session(&self, provider: &str, provider_session: &str, title: &str) {
        let _guard = self.resume.lock();
        let mut store = wtm_config::SessionStore::load(&self.sessions_file);
        let Some(record) = store
            .sessions
            .iter_mut()
            .find(|r| r.provider == provider && r.provider_session == provider_session)
        else {
            return;
        };
        if record.title.is_some() {
            return;
        }
        record.title = Some(title.to_owned());
        record.updated = Some(self.clock.now_iso());
        if let Err(error) = store.save(&self.sessions_file) {
            tracing::warn!(%error, "could not write the resume list");
        }
    }

    /// Forget everything belonging to a worktree that has been removed.
    ///
    /// Without this, a removed worktree leaves entries pointing at a path that no longer exists, and
    /// each would fail on click with an error about a missing directory.
    pub fn forget_worktree_sessions(&self, worktree_id: &str) {
        // Handoff tokens go for the same reason and one step sooner: each names this worktree, so a
        // handoff through a surviving one would open a pane and only then discover the directory is
        // gone. Failing before the pane is the better order.
        self.handoff.forget_worktree(worktree_id);

        let _guard = self.resume.lock();
        let mut store = wtm_config::SessionStore::load(&self.sessions_file);
        store.forget_worktree(worktree_id);
        if let Err(error) = store.save(&self.sessions_file) {
            tracing::warn!(%error, "could not write the resume list");
        }
    }

    /// Where a project's briefs live.
    #[must_use]
    pub fn brief_dir(&self, project_id: &str) -> PathBuf {
        wtm_config::briefs::project_dir(&self.config.paths().config_dir, project_id)
    }

    /// Store a plan, returning its id.
    ///
    /// # Errors
    ///
    /// If the directory cannot be created or either file cannot be written.
    pub fn save_brief(
        &self,
        project_id: &str,
        meta: &wtm_config::BriefMeta,
        markdown: &str,
    ) -> Result<String, ConfigError> {
        wtm_config::briefs::save(&self.brief_dir(project_id), meta, markdown)
    }

    /// What can be resumed in a worktree, newest first, excluding anything already running.
    ///
    /// The exclusion is the point: an entry for a live session would offer to resume a conversation
    /// that is on screen two inches away, and accepting would give the CLI two clients for one
    /// thread.
    #[must_use]
    pub fn resumable(&self, worktree_id: &str) -> Vec<wtm_config::SessionRecord> {
        let live: BTreeSet<String> = self
            .agents
            .lock()
            .values()
            .map(|entry| entry.provider_session.clone())
            .filter(|id| !id.is_empty())
            .collect();

        let _guard = self.resume.lock();
        wtm_config::SessionStore::load(&self.sessions_file)
            .in_worktree(worktree_id)
            .into_iter()
            .filter(|record| !live.contains(&record.provider_session))
            .cloned()
            .collect()
    }

    /// The provider's own id for a running session, once it has said.
    #[must_use]
    pub fn provider_session_of(&self, session: &str) -> Option<String> {
        let id = wtm_core::model::SessionId::new(session);
        self.agents
            .lock()
            .get(&id)
            .map(|entry| entry.provider_session.clone())
            .filter(|id| !id.is_empty())
    }

    /// Note the provider's own id for a running session, so `resumable` can exclude it.
    pub fn note_provider_session(&self, session: &str, provider_session: &str) {
        let id = wtm_core::model::SessionId::new(session);
        if let Some(entry) = self.agents.lock().get_mut(&id) {
            provider_session.clone_into(&mut entry.provider_session);
        }
    }

    fn running_agents(&self) -> BTreeSet<String> {
        use wtm_core::ports::pipe::PipeHost;

        self.pipe
            .sessions()
            .into_iter()
            .map(|s| s.session.as_str().to_owned())
            .collect()
    }
}

/// Why a declared palette cannot be used, or `None` if it can.
///
/// Every message names the key and what was wrong with it, because the audience is someone
/// looking at a TOML file they just edited — "invalid palette" would send them back to the
/// README to guess which of four fields it meant.
///
/// The bounds are the ones the stylesheet's oklch ramp needs. Hue wraps at 360 in CSS, so a
/// value outside 0–360 renders rather than fails, but it renders as some *other* hue than the
/// one that was typed — silently, which is worse than being told.
fn palette_problem(def: &wtm_config::PaletteDef) -> Option<String> {
    let Some(hue) = def.hue else {
        return Some("`hue` is required — an oklch hue angle from 0 to 360".to_owned());
    };
    if !(0.0..=360.0).contains(&hue) {
        return Some(format!("`hue` is {hue}, which is outside 0–360"));
    }

    let chroma = def.chroma.unwrap_or(1.0);
    if !(0.0..=2.0).contains(&chroma) {
        return Some(format!("`chroma` is {chroma}, which is outside 0–2"));
    }

    if def.brand.len() != 4 {
        return Some(format!(
            "`brand` needs exactly 4 colours (300, 400, 500, 600); found {}",
            def.brand.len()
        ));
    }
    for colour in &def.brand {
        if !is_hex_colour(colour) {
            return Some(format!("`{colour}` in `brand` is not a #rrggbb colour"));
        }
    }

    None
}

/// `#rrggbb`, and nothing else.
///
/// Deliberately strict: no three-digit shorthand, no eight-digit alpha, no named colours.
/// The value is interpolated straight into a custom property, and the narrow form is the one
/// the built-in palettes use and the docs show. Accepting more would mean an alpha channel
/// reaching a token that every surface in the app composites against.
fn is_hex_colour(value: &str) -> bool {
    value.len() == 7 && value.starts_with('#') && value[1..].bytes().all(|b| b.is_ascii_hexdigit())
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

    // ─────────────────── the terminal dock's shells ───────────────────
    //
    // These run against the real `PtyHostImpl`, because `App.pty` is the concrete type
    // rather than `Arc<dyn PtyHost>` — which is also why `FakePty::sessions()` returning
    // an empty list needs no fixing for them.

    /// A registered project with one extra worktree, plus the app that owns it.
    fn app_with_worktree() -> (wtm_testkit::GitFixture, tempfile::TempDir, App, Project) {
        let fixture = wtm_testkit::GitFixture::new();
        fixture.add_worktree("shell-a", "task/shell-a");
        fixture.add_worktree("shell-b", "task/shell-b");

        let dir = tempfile::tempdir().expect("temp dir");
        let app = App::with_paths(AppPaths::rooted(dir.path())).expect("app should build");
        let root = app.register(fixture.root()).expect("register");
        let project = app
            .project(&root.to_string_lossy())
            .expect("project should load");
        (fixture, dir, app, project)
    }

    /// Address a worktree by the id the listing reports, never by a path built here.
    ///
    /// On macOS a temp directory is reached through a symlink (`/var` → `/private/var`), so a
    /// locally-constructed id is a spelling the app never uses and every lookup misses.
    fn worktree_named(app: &App, project: &Project, dirname: &str) -> Worktree {
        let id = app
            .worktrees(project)
            .expect("worktrees")
            .into_iter()
            .find(|v| v.dirname == dirname)
            .expect("worktree should be listed")
            .id;
        app.worktree(project, &id).expect("worktree by id")
    }

    fn sleeper() -> Vec<String> {
        vec!["sleep".to_owned(), "30".to_owned()]
    }

    fn sink() -> Arc<dyn wtm_core::ports::pty::PtySink> {
        Arc::new(wtm_testkit::NullPtySink)
    }

    #[test]
    fn a_second_request_for_a_worktrees_shell_reuses_the_running_one_instead_of_spawning_another() {
        let (_fixture, _dir, app, project) = app_with_worktree();
        let worktree = worktree_named(&app, &project, "shell-a");
        let project_id = project.root.to_string_lossy().into_owned();

        let first = app
            .open_shell(&worktree, &project_id, sleeper(), 24, 80, sink())
            .expect("first shell");
        let second = app
            .open_shell(&worktree, &project_id, sleeper(), 30, 100, sink())
            .expect("second shell");

        assert_eq!(
            first, second,
            "a double click must not leave two login shells in one directory"
        );
        assert_eq!(app.pty.kill_all(), 1, "only one session should exist");
    }

    #[test]
    fn a_shell_that_has_exited_is_not_offered_for_reuse() {
        use wtm_core::ports::pty::PtyHost;

        let (_fixture, _dir, app, project) = app_with_worktree();
        let worktree = worktree_named(&app, &project, "shell-a");
        let project_id = project.root.to_string_lossy().into_owned();

        let dead = app
            .open_shell(
                &worktree,
                &project_id,
                vec!["true".to_owned()],
                24,
                80,
                sink(),
            )
            .expect("short-lived shell");
        app.pty
            .wait(&dead, &wtm_core::ports::exec::CancelToken::new())
            .expect("wait");

        let fresh = app
            .open_shell(&worktree, &project_id, sleeper(), 24, 80, sink())
            .expect("replacement shell");

        assert_ne!(
            dead, fresh,
            "trusting the index without checking liveness hands the dock a dead shell forever"
        );
        app.pty.kill_all();
    }

    /// **The test that protects why `App::shells` exists.**
    ///
    /// Sessions are tagged with a worktree id, and `run_action` and the setup stage tag theirs
    /// with the *same* one — so a lookup by worktree alone would adopt a running build as the
    /// dock's shell and let the user type into it.
    ///
    /// Specifically, this goes red when [`App::open_shell`]'s reuse check is replaced by a
    /// worktree-keyed query on the pty host — `sessions().find(|s| s.worktree == …)`, which is
    /// the obvious way to delete the index and looks like it cannot be wrong. Verified by
    /// making that edit and watching this fail with the two ids equal. Note that swapping
    /// `is_running` for `has_session_for` while *keeping* the index does **not** trip it: the
    /// map lookup still gates the reuse, so the bug needs the index gone.
    #[test]
    fn an_action_running_in_a_worktree_is_never_mistaken_for_its_shell() {
        use wtm_core::ports::pty::PtyHost;

        let (_fixture, _dir, app, project) = app_with_worktree();
        let worktree = worktree_named(&app, &project, "shell-a");
        let project_id = project.root.to_string_lossy().into_owned();

        // Stands in for `run_action`: same worktree tag, not the dock's shell.
        let action = app
            .pty
            .spawn(
                &wtm_core::ports::exec::Invocation::new(sleeper(), &worktree.path, 60_000),
                24,
                80,
                Some(worktree.id.as_str()),
                sink(),
            )
            .expect("action session")
            .session;
        assert!(
            app.pty.has_session_for(worktree.id.as_str()),
            "the trap this test exists for: the worktree now has a session that is not a shell"
        );

        let shell = app
            .open_shell(&worktree, &project_id, sleeper(), 24, 80, sink())
            .expect("shell");

        assert_ne!(
            action, shell,
            "the action's session must not become the dock's"
        );
        let listed = app.live_shells();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].1.1, shell);
        app.pty.kill_all();
    }

    #[test]
    fn closing_a_shell_forgets_it_so_a_restart_gets_a_new_session() {
        let (_fixture, _dir, app, project) = app_with_worktree();
        let worktree = worktree_named(&app, &project, "shell-a");
        let project_id = project.root.to_string_lossy().into_owned();

        let first = app
            .open_shell(&worktree, &project_id, sleeper(), 24, 80, sink())
            .expect("first shell");
        assert!(app.close_shell(worktree.id.as_str()));

        // Deliberately without waiting for the kill to be reaped: that is the race, and
        // forgetting the entry is what removes it.
        let second = app
            .open_shell(&worktree, &project_id, sleeper(), 24, 80, sink())
            .expect("restarted shell");

        assert_ne!(
            first, second,
            "restart handed back the id of a shell that was already dying"
        );
        assert!(
            !app.close_shell("/not/a/worktree"),
            "closing something with no shell is not an error"
        );
        app.pty.kill_all();
    }

    #[test]
    fn only_worktrees_with_a_running_shell_are_listed() {
        use wtm_core::ports::pty::PtyHost;

        let (_fixture, _dir, app, project) = app_with_worktree();
        let alive = worktree_named(&app, &project, "shell-a");
        let doomed = worktree_named(&app, &project, "shell-b");
        let project_id = project.root.to_string_lossy().into_owned();

        app.open_shell(&alive, &project_id, sleeper(), 24, 80, sink())
            .expect("long-lived shell");
        let short = app
            .open_shell(
                &doomed,
                &project_id,
                vec!["true".to_owned()],
                24,
                80,
                sink(),
            )
            .expect("short-lived shell");
        app.pty
            .wait(&short, &wtm_core::ports::exec::CancelToken::new())
            .expect("wait");

        let listed = app.live_shells();
        assert_eq!(listed.len(), 1, "got {listed:?}");
        assert_eq!(listed[0].0, alive.id.as_str());
        assert_eq!(listed[0].1.0, project_id);
        app.pty.kill_all();
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

    fn palette(hue: Option<f64>, chroma: Option<f64>, brand: &[&str]) -> wtm_config::PaletteDef {
        wtm_config::PaletteDef {
            name: None,
            hue,
            chroma,
            brand: brand.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    const OK_BRAND: &[&str] = &["#88c0d0", "#81a1c1", "#5e81ac", "#4c688f"];

    #[test]
    fn a_fully_specified_palette_has_no_problem() {
        assert_eq!(
            palette_problem(&palette(Some(245.0), Some(0.8), OK_BRAND)),
            None
        );
    }

    #[test]
    fn chroma_defaults_to_the_reference_rather_than_being_required() {
        // Hue is the one thing a palette cannot be guessed from, so it is the one required
        // field. A palette that only says "make it blue" is a reasonable thing to write.
        assert_eq!(palette_problem(&palette(Some(245.0), None, OK_BRAND)), None);
    }

    #[test]
    fn a_palette_without_a_hue_says_which_field_is_missing() {
        let problem = palette_problem(&palette(None, Some(1.0), OK_BRAND)).expect("a problem");
        assert!(problem.contains("hue"), "unhelpful message: {problem}");
    }

    #[test]
    fn a_hue_outside_the_circle_is_rejected_rather_than_left_to_wrap() {
        // CSS wraps it, so this would render — as some other hue than the one typed, with no
        // indication anything was wrong. Being told beats being surprised.
        assert!(palette_problem(&palette(Some(400.0), None, OK_BRAND)).is_some());
        assert!(palette_problem(&palette(Some(-10.0), None, OK_BRAND)).is_some());
    }

    #[test]
    fn a_brand_ramp_must_have_exactly_four_steps() {
        let short = palette_problem(&palette(Some(245.0), None, &OK_BRAND[..3])).expect("problem");
        assert!(short.contains('4'), "should say how many: {short}");
        assert!(palette_problem(&palette(Some(245.0), None, &[])).is_some());
    }

    #[test]
    fn shorthand_and_alpha_hex_are_both_refused() {
        // The value is interpolated straight into a custom property. Eight-digit hex would
        // put an alpha channel behind every surface that composites against the accent.
        for bad in ["#abc", "#88c0d0ff", "rebeccapurple", "88c0d0"] {
            let brand = &[bad, "#81a1c1", "#5e81ac", "#4c688f"];
            assert!(
                palette_problem(&palette(Some(245.0), None, brand)).is_some(),
                "{bad} should be refused"
            );
        }
    }

    #[test]
    fn a_broken_palette_is_still_listed_so_it_can_be_explained() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(
            dir.path().join("config.toml"),
            "[ui.palettes.oops]\nhue = 900\nbrand = [\"#88c0d0\"]\n",
        )
        .expect("write config");

        let app = App::with_paths(AppPaths::rooted(dir.path())).expect("app should build");
        let palettes = app.palettes();

        assert_eq!(palettes.len(), 1, "dropping it would be a mystery");
        assert_eq!(palettes[0].id, "oops");
        assert!(palettes[0].error.is_some());
        assert!(
            palettes[0].brand.is_empty(),
            "an unusable ramp must not reach the stylesheet"
        );
    }

    #[test]
    fn a_palette_with_no_name_is_listed_under_its_key() {
        let dir = tempfile::tempdir().expect("temp dir");
        std::fs::write(
            dir.path().join("config.toml"),
            "[ui.palettes.nord]\nhue = 245\n\
             brand = [\"#88c0d0\", \"#81a1c1\", \"#5e81ac\", \"#4c688f\"]\n",
        )
        .expect("write config");

        let app = App::with_paths(AppPaths::rooted(dir.path())).expect("app should build");
        let palettes = app.palettes();
        assert_eq!(palettes[0].name, "nord");
        // An omitted chroma defaults to the reference rather than to zero, which would
        // silently turn a declared palette monochrome.
        assert!((palettes[0].chroma - 1.0).abs() < f64::EPSILON);
        assert!(palettes[0].error.is_none());
    }
}
