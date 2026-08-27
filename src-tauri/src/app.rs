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

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use wtm_config::{AppPaths, FileConfigStore, RealFileStore};
use wtm_core::error::{ConfigError, WtmError};
use wtm_core::model::{
    AgentEvent, DatabaseEngine, DatabaseScope, Project, WorkingTreeStatus, Worktree,
};
use wtm_core::ports::clock::Clock;
use wtm_core::ports::config::ConfigStore;
use wtm_core::ports::database::{DatabaseConnection, DatabaseHost};
use wtm_core::ports::exec::CommandRunner;
use wtm_core::ports::fs::FileStore;
use wtm_core::ports::git::Git;
use wtm_core::ports::template::TemplateEngine;
use wtm_exec::{PipeHostImpl, PtyHostImpl, ResolvedPath};
use wtm_git::GitCli;

use crate::display;
use crate::view::{
    DatabaseConnectionView, DoctorView, PaletteView, ProjectView, ToolView, TrustPromptView,
    WorktreeView,
};

/// Tools a project config commonly invokes, reported by the diagnostics panel.
///
/// Not a dependency — wtm works fine without any of them — but when a project's config
/// calls one and it is missing, this is the fastest route to understanding why.
const KNOWN_TOOLS: &[&str] = &["git", "just", "acli", "docker", "gh", "bun", "npm"];

/// A credential-free connection label for the picker.
fn database_target(connection: &DatabaseConnection) -> String {
    if let Some(path) = &connection.path {
        return path.to_string_lossy().into_owned();
    }
    if connection.url.is_some() && connection.host.is_none() {
        return "configured URL".to_owned();
    }
    let host = connection.host.as_deref().unwrap_or("localhost");
    let default_port = match connection.engine {
        DatabaseEngine::Postgres => 5432,
        DatabaseEngine::Mysql => 3306,
        DatabaseEngine::Sqlite => 0,
    };
    let port = connection.port.unwrap_or(default_port);
    connection.name.as_ref().map_or_else(
        || format!("{host}:{port}"),
        |name| format!("{host}:{port}/{name}"),
    )
}

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

/// One terminal-dock shell and what it belongs to.
struct ShellEntry {
    project: String,
    worktree: String,
}

/// What the frontend needs to know about a live dock shell.
///
/// Kept out of `view.rs` for the same reason [`AgentSessionFacts`] is: the view type is built from
/// this, and `App` should not have to import the view module to describe its own state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellFacts {
    pub session: String,
    pub project: String,
    pub worktree: String,
}

/// One live agent session and what it belongs to.
struct AgentEntry {
    project: String,
    worktree: String,
    provider: String,
    /// The id the *provider* knows this conversation by, once it has said.
    ///
    /// Empty until then. Kept so `resumable` can exclude a conversation that is already on screen —
    /// offering to resume one would hand the CLI two clients for one thread.
    provider_session: String,
    /// First user prompt, cached even when it arrives before the provider handshake.
    title: Option<String>,
    /// Side-question forks are live long enough to stream one answer, but are never resumable.
    ephemeral: bool,
    /// Behind an `Arc` so a lookup can hand the session out and drop the map's lock.
    ///
    /// Not for sharing — nothing holds a second long-lived reference. It exists so
    /// [`App::agent_session`] can return an owned handle rather than a borrow, which is what lets
    /// the guard die before the caller runs anything. See that function.
    session: Arc<wtm_agent::AgentSession>,
    /// Clipboard files named in turns sent to this session.
    ///
    /// Providers receive their paths and may read them after `send_turn` returns, so deleting
    /// them at that boundary races images and reliably breaks non-image attachments. Keeping
    /// ownership here gives them the same lifetime as the session that can still refer to them.
    staged_attachments: BTreeSet<PathBuf>,
    /// Everything this session has already emitted, so a reload can repaint it.
    ///
    /// # Why the backend holds a transcript at all
    ///
    /// A webview reload throws away the frontend's panes while the CLIs keep running, and
    /// `list_agent_sessions` re-attaches them. It used to re-attach them to *empty* panes: the
    /// events had been emitted to a window that no longer existed, and nothing had kept them. A
    /// live session with a blank transcript is the "my sessions changed after I refreshed"
    /// complaint in its purest form.
    ///
    /// This is **memory only, and never written anywhere**. The rule in
    /// `wtm_config::sessions` — no transcript — is about a second secret-bearing *file* the user
    /// does not know exists, since agent output quotes whatever it read. A buffer that dies with
    /// the process it belongs to is not that, and it holds exactly what the pane on screen already
    /// holds.
    replay: ReplayBuffer,
}

impl Drop for AgentEntry {
    fn drop(&mut self) {
        for path in &self.staged_attachments {
            let _ = std::fs::remove_file(path);
        }
    }
}

pub(crate) fn staged_attachment_dir() -> PathBuf {
    std::env::temp_dir().join("wtm-agent-attachments")
}

fn owned_staged_attachment(path: &Path) -> bool {
    path.parent() == Some(staged_attachment_dir().as_path())
}

/// One buffered event and its position in the session's stream.
///
/// The number is what makes re-attaching race-free. The frontend subscribes to `agent:event`
/// before it asks for the buffer, so an event can arrive twice — once live, once in the snapshot.
/// Deduplicating on identity would be wrong (a session can legitimately emit the same delta twice)
/// and deduplicating on arrival order would need the two paths to agree about a clock they do not
/// share. A counter the emitter owns is the one thing both sides can compare.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SeqEvent {
    pub seq: u64,
    pub event: AgentEvent,
}

/// How many events one session's replay buffer keeps.
///
/// The frontend keeps `MAX_EVENTS = 100_000` per pane and this only has to survive long enough to
/// repaint one, so it is deliberately smaller: a reload after a very long session comes back with
/// its recent history rather than all of it, which is the same trade the frontend already makes.
/// Overflow drops the oldest.
const MAX_REPLAY: usize = 40_000;

/// How much serialized transcript data one live session may keep for repainting a webview.
///
/// An event count cannot bound memory when a Codex patch update contains a complete-file diff.
/// Thirty-two MiB is enough recent prose and command output to make a reload useful while keeping
/// one forgotten session from retaining hundreds of cumulative diff snapshots indefinitely.
const MAX_REPLAY_BYTES: usize = 32 * 1024 * 1024;

/// How much serialized transcript data every live session may keep, together.
///
/// The per-session cap still applies; this stops a handful of long-running agents from
/// retaining 32 MiB each indefinitely.
const MAX_REPLAY_BYTES_GLOBAL: usize = 128 * 1024 * 1024;

#[derive(Debug, Clone)]
struct BufferedEvent {
    seq_event: SeqEvent,
    bytes: usize,
}

/// A session's recent events, numbered.
///
/// A type rather than two fields on [`AgentEntry`] because the counter and the queue have one
/// invariant between them and it is the whole point: **the number never goes backwards, including
/// across an overflow that discards the events it counted.** Deriving the next number from the
/// queue's length or its last entry would break the moment the front was dropped, and the failure
/// would be a reload silently discarding live events whose sequence had been reused.
#[derive(Debug, Default)]
struct ReplayBuffer {
    events: VecDeque<BufferedEvent>,
    bytes: usize,
    next: u64,
}

impl ReplayBuffer {
    /// Buffer an event and give it its number.
    fn push(&mut self, event: &AgentEvent) -> u64 {
        let seq = self.next;
        self.next += 1;

        // Patches and other snapshots replace earlier state. Keeping every cumulative Codex diff
        // is both misleading on replay and the source of an otherwise unbounded memory multiplier.
        if let Some(index) = self
            .events
            .iter()
            .position(|entry| replaces(&entry.seq_event.event, event))
            && let Some(removed) = self.events.remove(index)
        {
            self.bytes = self.bytes.saturating_sub(removed.bytes);
        }

        let mut buffered = event.clone();
        let mut bytes = serialized_len(&buffered);
        if bytes > MAX_REPLAY_BYTES {
            buffered = AgentEvent::Notice {
                level: wtm_core::model::NoticeLevel::Warn,
                message: "One oversized transcript event was omitted from reload history."
                    .to_owned(),
            };
            bytes = serialized_len(&buffered);
        }

        self.bytes = self.bytes.saturating_add(bytes);
        self.events.push_back(BufferedEvent {
            seq_event: SeqEvent {
                seq,
                event: buffered,
            },
            bytes,
        });
        while self.events.len() > MAX_REPLAY || self.bytes > MAX_REPLAY_BYTES {
            if let Some(removed) = self.events.pop_front() {
                self.bytes = self.bytes.saturating_sub(removed.bytes);
            }
        }
        seq
    }

    fn pop_oldest(&mut self) -> bool {
        if let Some(removed) = self.events.pop_front() {
            self.bytes = self.bytes.saturating_sub(removed.bytes);
            true
        } else {
            false
        }
    }

    fn snapshot(&self) -> Vec<SeqEvent> {
        self.events
            .iter()
            .map(|entry| entry.seq_event.clone())
            .collect()
    }
}

fn serialized_len(event: &AgentEvent) -> usize {
    serde_json::to_vec(event).map_or(0, |bytes| bytes.len())
}

/// Whether the newer event is the complete current value of state already in the replay.
fn replaces(previous: &AgentEvent, newer: &AgentEvent) -> bool {
    match (previous, newer) {
        (AgentEvent::Patch { id: left, .. }, AgentEvent::Patch { id: right, .. }) => left == right,
        (AgentEvent::AgendaUpdated { .. }, AgentEvent::AgendaUpdated { .. })
        | (AgentEvent::SkillsListed { .. }, AgentEvent::SkillsListed { .. })
        | (AgentEvent::Usage(_), AgentEvent::Usage(_)) => true,
        _ => false,
    }
}

/// Drop oldest events from other sessions until the global replay budget is met.
fn trim_global_replay(
    agents: &mut BTreeMap<wtm_core::model::SessionId, AgentEntry>,
    keep: &wtm_core::model::SessionId,
) {
    loop {
        let total: usize = agents.values().map(|entry| entry.replay.bytes).sum();
        if total <= MAX_REPLAY_BYTES_GLOBAL {
            return;
        }
        let victim = agents
            .iter()
            .filter(|(id, entry)| *id != keep && !entry.replay.events.is_empty())
            .max_by_key(|(_, entry)| entry.replay.bytes)
            .map(|(id, _)| id.clone());
        let Some(victim) = victim else {
            return;
        };
        let Some(entry) = agents.get_mut(&victim) else {
            return;
        };
        if !entry.replay.pop_oldest() {
            return;
        }
    }
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
    /// The provider's own id for this conversation, empty until it has said.
    ///
    /// Reported so a window that is re-attaching can tell *which* conversation a live session is,
    /// and put it back in the pane it was in rather than beside a restored copy of itself.
    pub provider_session: String,
    pub ephemeral: bool,
}

/// The durable provider conversation behind a live pane, used as the source of a side fork.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentForkSource {
    pub project: String,
    pub worktree: String,
    pub provider: String,
    pub provider_session: String,
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
    /// Live database connections. Concrete ownership belongs in the composition root, like PTYs.
    pub database: Arc<dyn DatabaseHost>,
    resolved_path: ResolvedPath,
    /// `os.*` template tokens, resolved once — they cannot change while running.
    os_tokens: BTreeMap<String, String>,
    /// Which sessions are **terminal-dock shells**, keyed by session id.
    ///
    /// # Why an index here rather than a query on the pty host
    ///
    /// `PtySession::worktree` is already recorded and `PtyHost::has_session_for` already
    /// answers "is anything running for this worktree" — but *anything* is the problem.
    /// `run_action` and the setup stage tag their sessions with the same worktree id, so a
    /// lookup by worktree alone would hand the dock the session of a running `just test` and
    /// let the user type into it. The worktree was never a unique key, and that is still true
    /// now that a worktree may hold several shells — which is why this index survived the
    /// re-keying rather than being replaced by a filter over `sessions()`.
    ///
    /// The alternative was a session *kind* threaded through `PtyHost::spawn`. Rejected for
    /// the same reason [`Self::palettes`] is assembled here rather than in the domain: "which
    /// session is the UI's terminal" is a frontend concept `wtm-core` has no stake in, and
    /// keeping it in the composition root means `wtm-core` still compiles for `wasm32`.
    ///
    /// Keyed by session, like [`Self::agents`]: several shells in one worktree is the point, so
    /// the session is the unique thing and the worktree is a field on it.
    ///
    /// Liveness is never read from here. Every lookup intersects with `PtyHostImpl::sessions`,
    /// which reports running sessions only, so an entry for a shell the user exited answers
    /// "not running" without anybody having to remember to clean up.
    shells: parking_lot::Mutex<BTreeMap<wtm_core::model::SessionId, ShellEntry>>,
    /// Live agent sessions, keyed by **session id**, exactly as the shells map above is.
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
    /// The one recording in progress, if any. See [`crate::dictate`].
    pub dictation: crate::dictate::Dictation,
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
        let database: Arc<dyn DatabaseHost> = Arc::new(wtm_db::Host::new(Arc::clone(&clock)));

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
            database,
            resolved_path,
            os_tokens: wtm_exec::os_tokens(),
            shells: parking_lot::Mutex::new(BTreeMap::new()),
            handoff: crate::handoff::Hub::default(),
            dictation: crate::dictate::Dictation::default(),
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
                            databases,
                            content_hash,
                        } => (
                            "This project's configuration declares capabilities that need your \
                             approval before it can be used."
                                .to_owned(),
                            Some(TrustPromptView {
                                path: path.to_string_lossy().into_owned(),
                                commands: commands.clone(),
                                databases: databases.clone(),
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

    /// Database profiles resolved for one selected worktree, with credentials omitted.
    pub fn database_connections(
        &self,
        project: &Project,
        worktree_id: &str,
    ) -> Result<Vec<DatabaseConnectionView>, WtmError> {
        let worktree = self.worktree(project, worktree_id)?;
        Ok(project
            .database
            .iter()
            .map(
                |(id, spec)| match self.resolve_database(project, &worktree, id) {
                    Ok(connection) => DatabaseConnectionView {
                        id: id.clone(),
                        label: connection.label.clone(),
                        engine: connection.engine,
                        scope: connection.scope,
                        environment: connection.environment,
                        access: connection.access,
                        tls: connection.tls,
                        target: database_target(&connection),
                        available: wtm_db::supports(connection.engine),
                        problem: (!wtm_db::supports(connection.engine)).then(|| {
                            format!(
                                "{} support is not available in this build",
                                connection.engine.as_str()
                            )
                        }),
                    },
                    Err(error) => DatabaseConnectionView {
                        id: id.clone(),
                        label: spec.label.clone().unwrap_or_else(|| id.clone()),
                        engine: spec.engine,
                        scope: spec.scope,
                        environment: spec.environment,
                        access: spec.access,
                        tls: spec.tls,
                        target: "unresolved".to_owned(),
                        available: false,
                        problem: Some(error.to_string()),
                    },
                },
            )
            .collect())
    }

    /// Resolve one profile for the backend. The returned value must never be serialized or logged.
    pub fn resolve_database(
        &self,
        project: &Project,
        worktree: &Worktree,
        profile_id: &str,
    ) -> Result<DatabaseConnection, WtmError> {
        let spec = project.database.get(profile_id).ok_or_else(|| {
            wtm_core::error::DatabaseError::InvalidConnection(format!(
                "unknown database profile `{profile_id}`"
            ))
        })?;
        let mut context = display::base_context(project, &self.os_tokens);
        if spec.scope == DatabaseScope::Worktree {
            display::add_worktree_tokens(&mut context, worktree);
            let sources =
                display::read_sources(project, self.files.as_ref(), self.engine.as_ref(), &context);
            display::add_source_tokens(&mut context, project, &sources);
        }

        let render = |field: &str, template: &Option<String>| -> Result<Option<String>, WtmError> {
            template
                .as_ref()
                .map(|template| {
                    self.engine
                        .render(
                            &format!("database.{profile_id}.{field}"),
                            template,
                            &context,
                        )
                        .map(|value| value.trim().to_owned())
                        .map_err(WtmError::from)
                })
                .transpose()
                .map(|value| value.filter(|value| !value.is_empty()))
        };

        let port = render("port", &spec.port)?
            .map(|value| {
                value.parse::<u16>().map_err(|_| {
                    wtm_core::error::DatabaseError::InvalidConnection(format!(
                        "database.{profile_id}.port did not resolve to a valid port"
                    ))
                })
            })
            .transpose()?;
        let path = render("path", &spec.path)?.map(PathBuf::from).map(|path| {
            if path.is_absolute() {
                path
            } else if spec.scope == DatabaseScope::Worktree {
                worktree.path.join(path)
            } else {
                project.root.join(path)
            }
        });

        Ok(DatabaseConnection {
            profile_id: profile_id.to_owned(),
            label: spec.label.clone().unwrap_or_else(|| profile_id.to_owned()),
            engine: spec.engine,
            scope: spec.scope,
            environment: spec.environment,
            access: spec.access,
            url: render("url", &spec.url)?,
            host: render("host", &spec.host)?,
            port,
            name: render("name", &spec.name)?,
            user: render("user", &spec.user)?,
            password: render("password", &spec.password)?,
            path,
            tls: spec.tls,
        })
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

    /// Resolve the executable that an agent availability row and its eventual spawn both use.
    ///
    /// Most providers have one canonical program on `PATH`. Cursor is the exception: releases
    /// have used both `cursor-agent` and `agent`, and Cursor.app keeps a private `cursor-agent`
    /// under its extension storage. Returning the winning path—not just a boolean—is what keeps a
    /// grey-row fix from becoming a later "program not found" error when the pane actually opens.
    #[must_use]
    pub fn agent_executable(&self, entry: &'static wtm_agent::ProviderEntry) -> Option<PathBuf> {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let candidates = if entry.id == wtm_agent::cursor::ID {
            wtm_agent::cursor::executable_candidates(home.as_deref())
        } else {
            vec![PathBuf::from(entry.provider.program())]
        };

        candidates
            .into_iter()
            .find_map(|candidate| self.runner.which(candidate.to_string_lossy().as_ref()))
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
    /// Open an interactive shell in one worktree.
    ///
    /// **Not idempotent**, the same way [`Self::open_agent`] is not: asking twice opens two login
    /// shells, because several shells in one worktree is the feature. The old behaviour returned
    /// the running session instead, and re-attaching after a webview reload — the case that
    /// justified it — is [`Self::live_shells`]' job rather than this one's.
    ///
    /// Two login shells in one directory do share a history file, which is what the single-shell
    /// rule used to prevent. That is a property of the user's own shell configuration (`zsh` with
    /// `INC_APPEND_HISTORY` handles it; the default does last-writer-wins), and it is a small
    /// enough cost that trading a second terminal for it was the wrong way round.
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

        // Before the spawn, so descriptors a finished session is still holding are released
        // before a new pair is allocated. This is the app's only caller.
        self.pty.reap_finished(KEEP_FINISHED_SESSIONS);

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

        // Taken after the spawn, unlike the version this replaces: with no reuse check there is
        // nothing to make atomic, so there is no reason to hold a lock across a process launch.
        let mut shells = self.shells.lock();

        // Entries whose shell has since exited are dead weight. Pruned on each open, which is
        // the only moment the map grows, so it stays the size of what is actually running.
        let running = self.running_sessions();
        shells.retain(|session, _| running.contains(session.as_str()));

        shells.insert(
            spawned.session.clone(),
            ShellEntry {
                project: project_id.to_owned(),
                worktree: worktree.id.as_str().to_owned(),
            },
        );
        Ok(spawned.session)
    }

    /// Every live dock shell, for a webview that has just reloaded.
    #[must_use]
    pub fn live_shells(&self) -> Vec<ShellFacts> {
        let running = self.running_sessions();
        self.shells
            .lock()
            .iter()
            .filter(|(session, _)| running.contains(session.as_str()))
            .map(|(session, entry)| ShellFacts {
                session: session.as_str().to_owned(),
                project: entry.project.clone(),
                worktree: entry.worktree.clone(),
            })
            .collect()
    }

    /// Every dock shell in one worktree, for tearing the worktree down.
    ///
    /// Mirrors [`Self::agents_in`]. Returns ids rather than killing them here because the two
    /// callers want different things: `remove_worktree` kills, and a caller counting shells
    /// should not have to.
    #[must_use]
    pub fn shells_in(&self, worktree_id: &str) -> Vec<String> {
        self.shells
            .lock()
            .iter()
            .filter(|(_, entry)| entry.worktree == worktree_id)
            .map(|(session, _)| session.as_str().to_owned())
            .collect()
    }

    /// Kill one dock shell and forget it. `false` if it was not running.
    ///
    /// Forgetting is the load-bearing half. `PtyHost::kill` returns as soon as the group has
    /// been signalled, not once the child has been reaped, so a [`Self::live_shells`] taken
    /// immediately afterwards would still see the dying session and a reloading webview would
    /// adopt a pane for a shell that is about to close. Dropping the entry makes that impossible
    /// without any ordering requirement on the frontend and without polling — which
    /// `ARCHITECTURE.md` §8 bans outright.
    pub fn close_shell(&self, session_id: &str) -> bool {
        use wtm_core::ports::pty::PtyHost;

        let session = wtm_core::model::SessionId::new(session_id);
        if self.shells.lock().remove(&session).is_none() {
            return false;
        }
        let closed = match self.pty.kill(&session) {
            Ok(()) => true,
            Err(error) => {
                // Already finished: the user typed `exit` before reaching for the control.
                tracing::debug!(%session, %error, "no live shell to close");
                false
            }
        };
        self.pty.reap_finished(KEEP_FINISHED_SESSIONS);
        closed
    }

    pub(crate) fn reap_hosts(&self) {
        self.pty.reap_finished(KEEP_FINISHED_SESSIONS);
        self.pipe.reap_finished(KEEP_FINISHED_SESSIONS);
    }

    /// The ids of every session the pty host still reports as running.
    ///
    /// The single source of truth for liveness. [`Self::shells`] records only *which* sessions are
    /// dock shells; asking it whether one is alive would be believing a cache over the process
    /// table. (There was a single-session `is_running` beside this, whose only caller was
    /// `open_shell`'s reuse check — it went with the reuse.)
    fn running_sessions(&self) -> BTreeSet<String> {
        use wtm_core::ports::pty::PtyHost;

        self.pty
            .sessions()
            .into_iter()
            .map(|s| s.session.as_str().to_owned())
            .collect()
    }

    // ─────────────────────────── agent sessions ───────────────────────────

    /// Start an agent session in `worktree`.
    ///
    /// # Why this index is keyed by session and not by worktree
    ///
    /// Several sessions in one worktree is the feature, so the session id is the unique thing and
    /// the worktree is a field on the entry. The shells map is keyed the same way and for the same
    /// reason — it was keyed by worktree while a worktree could only have one shell, and re-keying
    /// it is what let a worktree have several.
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

        let executable = self.agent_executable(entry).ok_or_else(|| {
            wtm_core::error::ExecError::ProgramNotFound {
                program: entry.provider.program().to_owned(),
                searched: self.runner.resolved_path(),
            }
        })?;
        let mut request = req.clone();
        request.executable = Some(executable.to_string_lossy().into_owned());

        let session = wtm_agent::AgentSession::open(
            entry.provider,
            &request,
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
                title: None,
                ephemeral: req.ephemeral,
                session: Arc::new(session),
                staged_attachments: BTreeSet::new(),
                replay: ReplayBuffer::default(),
            },
        );
        if let Some(token) = req
            .mcp
            .get(crate::handoff::SERVER_NAME)
            .and_then(|server| server.env.get(crate::handoff::TOKEN_ENV))
        {
            self.handoff.bind_session(token, id.as_str());
        }
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
                provider_session: entry.provider_session.clone(),
                ephemeral: entry.ephemeral,
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

    /// Keep staged attachment files for as long as the session may still read them.
    pub fn remember_staged_attachments(
        &self,
        session: &str,
        attachments: &[wtm_core::model::AgentAttachment],
    ) {
        let id = wtm_core::model::SessionId::new(session);
        let mut agents = self.agents.lock();
        let Some(entry) = agents.get_mut(&id) else {
            return;
        };
        entry.staged_attachments.extend(
            attachments
                .iter()
                .map(|attachment| PathBuf::from(&attachment.path))
                .filter(|path| owned_staged_attachment(path)),
        );
    }

    /// End an agent session and forget it.
    ///
    /// Forgetting is the load-bearing half, exactly as it is in [`Self::close_shell`]: `close`
    /// returns once the group has been signalled rather than once the child is reaped, so a
    /// re-open would otherwise be handed the id of a dying session.
    ///
    /// The handoff token goes in the same moment, even when there is no live process: a token
    /// that outlived its session was one UUID and `Caller` per pane until the worktree died, and
    /// the only thing that can still present it is a CLI we just signalled to exit.
    pub fn close_agent(&self, session: &str) -> bool {
        // Descendants first: `forget_session` drops the parentage map, and doing that
        // before signalling would leave child CLIs running with no owner.
        for child in self.handoff.descendants(session) {
            let _ = self.end_agent_process(&child.session);
            self.handoff.forget_session(&child.session);
        }
        let closed = self.end_agent_process(session);
        self.handoff.forget_session(session);
        self.pipe.reap_finished(KEEP_FINISHED_SESSIONS);
        closed
    }

    fn end_agent_process(&self, session: &str) -> bool {
        let id = wtm_core::model::SessionId::new(session);
        let Some(entry) = self.agents.lock().remove(&id) else {
            return false;
        };
        match entry.session.close() {
            Ok(()) => true,
            Err(error) => {
                tracing::debug!(%session, %error, "no live agent session to close");
                false
            }
        }
    }

    /// Signal every dock shell and agent in a worktree with one grace period.
    ///
    /// Used by worktree removal: calling [`Self::close_shell`] / [`Self::close_agent`] in a
    /// loop would pay 400 ms per session.
    pub fn terminate_sessions_in(&self, worktree_id: &str) {
        let shells: Vec<wtm_core::model::SessionId> = self
            .shells_in(worktree_id)
            .into_iter()
            .map(wtm_core::model::SessionId::new)
            .collect();
        let agents: Vec<wtm_core::model::SessionId> = self
            .agents_in(worktree_id)
            .into_iter()
            .map(wtm_core::model::SessionId::new)
            .collect();
        let mut pids = self.pty.take_pids(&shells);
        pids.extend(self.pipe.take_pids(&agents));
        wtm_exec::signal::terminate_groups(&pids);
        {
            let mut map = self.shells.lock();
            for session in &shells {
                map.remove(session);
            }
        }
        {
            let mut map = self.agents.lock();
            for session in &agents {
                map.remove(session);
                self.handoff.forget_session(session.as_str());
            }
        }
        self.reap_hosts();
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

    #[must_use]
    pub fn session_title_of(&self, session: &str) -> Option<String> {
        let id = wtm_core::model::SessionId::new(session);
        self.agents
            .lock()
            .get(&id)
            .and_then(|entry| entry.title.clone())
    }

    /// Cache the first prompt immediately, then persist it once the provider id is known.
    pub fn title_live_session(&self, session: &str, title: &str) {
        let id = wtm_core::model::SessionId::new(session);
        let destination = {
            let mut agents = self.agents.lock();
            let Some(entry) = agents.get_mut(&id) else {
                return;
            };
            if entry.ephemeral || entry.title.is_some() {
                return;
            }
            entry.title = Some(title.to_owned());
            (!entry.provider_session.is_empty())
                .then(|| (entry.provider.clone(), entry.provider_session.clone()))
        };
        if let Some((provider, provider_session)) = destination {
            self.title_session(&provider, &provider_session, title);
        }
    }

    /// Resolve everything needed to fork a live agent without exposing provider ids to the UI.
    #[must_use]
    pub fn agent_fork_source(&self, session: &str) -> Option<AgentForkSource> {
        let id = wtm_core::model::SessionId::new(session);
        self.agents.lock().get(&id).and_then(|entry| {
            if entry.provider_session.is_empty() {
                return None;
            }
            Some(AgentForkSource {
                project: entry.project.clone(),
                worktree: entry.worktree.clone(),
                provider: entry.provider.clone(),
                provider_session: entry.provider_session.clone(),
            })
        })
    }

    /// Buffer an event for a session and give it its sequence number.
    ///
    /// Called on the reader thread for every event, before it is emitted, so the number the window
    /// receives is the number the buffer holds. A session that is no longer in the map — one being
    /// closed while its last events drain — gets `None` and is simply not buffered.
    pub fn record_agent_event(&self, session: &str, event: &AgentEvent) -> Option<u64> {
        let id = wtm_core::model::SessionId::new(session);
        let mut agents = self.agents.lock();
        let entry = agents.get_mut(&id)?;
        let seq = entry.replay.push(event);
        trim_global_replay(&mut agents, &id);
        Some(seq)
    }

    /// Everything a session has emitted that is still buffered, oldest first.
    #[must_use]
    pub fn agent_replay(&self, session: &str) -> Vec<SeqEvent> {
        let id = wtm_core::model::SessionId::new(session);
        self.agents
            .lock()
            .get(&id)
            .map(|entry| entry.replay.snapshot())
            .unwrap_or_default()
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
    fn only_direct_children_of_the_staging_directory_are_owned_by_a_session() {
        let root = staged_attachment_dir();
        assert!(owned_staged_attachment(&root.join("generated-file")));
        assert!(!owned_staged_attachment(
            &root.join("nested/generated-file")
        ));
        assert!(!owned_staged_attachment(&root.join("../outside-file")));
    }

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

    fn notice(message: &str) -> AgentEvent {
        AgentEvent::Notice {
            level: wtm_core::model::NoticeLevel::Info,
            message: message.to_owned(),
        }
    }

    #[test]
    fn a_replay_buffer_numbers_events_in_the_order_it_received_them() {
        let mut buffer = ReplayBuffer::default();
        assert_eq!(buffer.push(&notice("one")), 0);
        assert_eq!(buffer.push(&notice("two")), 1);

        let snapshot = buffer.snapshot();
        assert_eq!(snapshot.iter().map(|e| e.seq).collect::<Vec<_>>(), [0, 1]);
    }

    #[test]
    fn a_sequence_number_is_never_reused_after_the_buffer_overflows() {
        // The invariant the type exists for. The frontend skips any live event whose number it has
        // already drawn, so a reused number would make a reload silently discard real events —
        // and it would only happen to the people with the longest sessions.
        let mut buffer = ReplayBuffer::default();
        for index in 0..MAX_REPLAY + 10 {
            buffer.push(&notice(&index.to_string()));
        }

        let snapshot = buffer.snapshot();
        assert_eq!(snapshot.len(), MAX_REPLAY, "the buffer stays bounded");
        assert_eq!(
            snapshot[0].seq, 10,
            "the oldest events went, not their numbers"
        );
        assert_eq!(
            buffer.push(&notice("next")),
            u64::try_from(MAX_REPLAY + 10).expect("fits"),
        );
    }

    #[test]
    fn a_patch_snapshot_replaces_the_previous_snapshot_with_a_new_sequence() {
        let mut buffer = ReplayBuffer::default();
        buffer.push(&AgentEvent::Patch {
            id: "same-file".to_owned(),
            unified_diff: "first".to_owned(),
        });
        assert_eq!(
            buffer.push(&AgentEvent::Patch {
                id: "same-file".to_owned(),
                unified_diff: "latest".to_owned(),
            }),
            1,
        );

        let snapshot = buffer.snapshot();
        assert_eq!(snapshot.len(), 1);
        assert_eq!(snapshot[0].seq, 1);
        assert!(matches!(
            &snapshot[0].event,
            AgentEvent::Patch { unified_diff, .. } if unified_diff == "latest"
        ));
    }

    #[test]
    fn replay_history_is_bounded_by_serialized_bytes() {
        let mut buffer = ReplayBuffer::default();
        for index in 0..40 {
            buffer.push(&notice(&format!("{index}:{}", "x".repeat(1024 * 1024))));
        }

        assert!(buffer.bytes <= MAX_REPLAY_BYTES);
        assert!(buffer.snapshot().len() < 40, "the oldest large events went");
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
    fn a_second_shell_request_in_the_same_worktree_spawns_a_second_session() {
        // The inverse of what this used to assert. Reuse was the old contract — a second call
        // returned the running session — and the whole point of keying `shells` by session is
        // that a worktree can now hold several. `open_agent` has always worked this way.
        let (_fixture, _dir, app, project) = app_with_worktree();
        let worktree = worktree_named(&app, &project, "shell-a");
        let project_id = project.root.to_string_lossy().into_owned();

        let first = app
            .open_shell(&worktree, &project_id, sleeper(), 24, 80, sink())
            .expect("first shell");
        let second = app
            .open_shell(&worktree, &project_id, sleeper(), 30, 100, sink())
            .expect("second shell");

        assert_ne!(first, second, "asking twice is asking for two");
        assert_eq!(app.pty.kill_all(), 2, "both should be running");
    }

    /// **The test that protects why `App::shells` exists.**
    ///
    /// Sessions are tagged with a worktree id, and `run_action` and the setup stage tag theirs
    /// with the *same* one — so a lookup by worktree alone would adopt a running build as a dock
    /// shell and let the user type into it.
    ///
    /// The property moved when shells stopped being one-per-worktree: there is no reuse check
    /// left to get wrong, so what this now guards is **adoption**. It goes red if
    /// [`App::live_shells`] is ever derived from the pty host by worktree —
    /// `sessions().filter(|s| s.worktree == …)`, which is the obvious way to delete the index and
    /// looks like it cannot be wrong — because the action would then be listed as a shell and a
    /// reloading webview would mount a terminal pane onto a running build.
    #[test]
    fn an_action_running_in_a_worktree_is_never_mistaken_for_its_shell() {
        use wtm_core::ports::pty::PtyHost;

        let (_fixture, _dir, app, project) = app_with_worktree();
        let worktree = worktree_named(&app, &project, "shell-a");
        let project_id = project.root.to_string_lossy().into_owned();

        // Stands in for `run_action`: same worktree tag, not a dock shell.
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
        assert_eq!(listed.len(), 1, "got {listed:?}");
        assert_eq!(listed[0].session, shell.as_str());
        assert!(
            !app.shells_in(worktree.id.as_str())
                .contains(&action.as_str().to_owned()),
            "teardown must not go looking for the action either"
        );
        app.pty.kill_all();
    }

    #[test]
    fn closing_a_shell_forgets_it_so_a_reload_does_not_adopt_a_dying_session() {
        let (_fixture, _dir, app, project) = app_with_worktree();
        let worktree = worktree_named(&app, &project, "shell-a");
        let project_id = project.root.to_string_lossy().into_owned();

        let shell = app
            .open_shell(&worktree, &project_id, sleeper(), 24, 80, sink())
            .expect("shell");
        assert!(app.close_shell(shell.as_str()));

        // Deliberately without waiting for the kill to be reaped: that is the race. `PtyHost::kill`
        // returns once the group is signalled, so the pty host still reports this session as
        // running for a moment, and dropping the entry is the only thing keeping a reloading
        // webview from mounting a pane onto a shell that is closing.
        let listed = app.live_shells();
        assert!(
            !listed.iter().any(|s| s.session == shell.as_str()),
            "got {listed:?}"
        );

        assert!(
            !app.close_shell("not-a-session"),
            "closing something that was never a shell is not an error"
        );
        app.pty.kill_all();
    }

    #[test]
    fn closing_one_of_two_shells_in_a_worktree_leaves_the_other_running() {
        // The failure this rules out is a session-keyed close that still reaches for the worktree
        // — one Kill press taking somebody's dev server with it.
        let (_fixture, _dir, app, project) = app_with_worktree();
        let worktree = worktree_named(&app, &project, "shell-a");
        let project_id = project.root.to_string_lossy().into_owned();

        let doomed = app
            .open_shell(&worktree, &project_id, sleeper(), 24, 80, sink())
            .expect("first shell");
        let survivor = app
            .open_shell(&worktree, &project_id, sleeper(), 24, 80, sink())
            .expect("second shell");

        assert!(app.close_shell(doomed.as_str()));

        let listed = app.live_shells();
        assert_eq!(listed.len(), 1, "got {listed:?}");
        assert_eq!(listed[0].session, survivor.as_str());
        app.pty.kill_all();
    }

    #[test]
    fn every_shell_in_a_worktree_is_found_for_teardown_and_none_from_another_worktree() {
        // `remove_worktree` closes what this returns, and a shell it misses is a dev server
        // holding untracked files that make `git worktree remove` refuse.
        let (_fixture, _dir, app, project) = app_with_worktree();
        let doomed = worktree_named(&app, &project, "shell-a");
        let other = worktree_named(&app, &project, "shell-b");
        let project_id = project.root.to_string_lossy().into_owned();

        let first = app
            .open_shell(&doomed, &project_id, sleeper(), 24, 80, sink())
            .expect("first shell");
        let second = app
            .open_shell(&doomed, &project_id, sleeper(), 24, 80, sink())
            .expect("second shell");
        let elsewhere = app
            .open_shell(&other, &project_id, sleeper(), 24, 80, sink())
            .expect("unrelated shell");

        let found = app.shells_in(doomed.id.as_str());
        assert_eq!(found.len(), 2, "got {found:?}");
        assert!(found.contains(&first.as_str().to_owned()));
        assert!(found.contains(&second.as_str().to_owned()));
        assert!(
            !found.contains(&elsewhere.as_str().to_owned()),
            "another worktree's shell must survive this one's removal"
        );
        app.pty.kill_all();
    }

    #[test]
    fn every_running_shell_is_listed_and_an_exited_one_is_not() {
        use wtm_core::ports::pty::PtyHost;

        let (_fixture, _dir, app, project) = app_with_worktree();
        let alive = worktree_named(&app, &project, "shell-a");
        let doomed = worktree_named(&app, &project, "shell-b");
        let project_id = project.root.to_string_lossy().into_owned();

        // Two in one worktree, to prove the listing is per shell rather than per worktree.
        let first = app
            .open_shell(&alive, &project_id, sleeper(), 24, 80, sink())
            .expect("long-lived shell");
        let second = app
            .open_shell(&alive, &project_id, sleeper(), 24, 80, sink())
            .expect("second long-lived shell");
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
        assert_eq!(listed.len(), 2, "got {listed:?}");
        let sessions: Vec<&str> = listed.iter().map(|s| s.session.as_str()).collect();
        assert!(sessions.contains(&first.as_str()));
        assert!(sessions.contains(&second.as_str()));
        assert!(
            !sessions.contains(&short.as_str()),
            "an exited shell is not a shell"
        );
        assert!(listed.iter().all(|s| s.project == project_id));
        assert!(listed.iter().all(|s| s.worktree == alive.id.as_str()));
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
