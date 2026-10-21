//! Opening a workflow run's agent sessions: headless, behind the run's rail, under an authority
//! profile, and only after a canary has shown the profile's sandbox is real.
//!
//! # Headless, but a pane
//!
//! A run session is an ordinary agent session in every way the app already knows: it is opened by
//! `App::open_agent`, its events stream to a pane, its approvals show on that pane, it closes with
//! its worktree. What is different is that nobody opened it by hand, so it is announced the way a
//! delegated child is — `agent:spawned` with a run id — and never given a tile. The window puts it
//! behind the run's rail, where it can be inspected, and nowhere else. The engine drives it with
//! turns and interrupts; nothing here sends one.
//!
//! # The order things happen in, and why
//!
//! 1. **Preflight** (F3): the CLI must bill a plan, and `gh` gets an empty config directory.
//! 2. **Prepare**: the role's token, the bridge that carries it, the authority profile, and the
//!    profile's files, written at 0600 in a directory only the app writes. Nothing a repository
//!    wrote is an input: no `[agent.<id>]` `extra_args`, `env` or `mcp`, no repository mode.
//! 3. **Canary**: a write outside the worktree, under the same profile, which must fail. Neither CLI
//!    says whether its sandbox engaged, and Claude ignores a sandbox object it cannot read without a
//!    word, so the only way to know is to try. A run whose canary does not fail cleanly does not
//!    start, and says why.
//! 4. **Open and announce.**
//!
//! A failure at any step after the token is issued forgets the token, so a run that did not start
//! leaves nothing that could be presented at the socket.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use wtm_agent::authority::{
    self, ClaudeRun, CodexRun, RunAuthority, RunBridge, RunPlaces, RunProfile,
};
use wtm_core::model::{AgentEvent, ExitOutcome, SessionId};
use wtm_core::ports::exec::{CancelToken, Invocation};

use crate::app::{App, FlowRole, SessionScope};
use crate::handoff;

/// What the engine asks for: one role's session in one worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleSession {
    pub project: String,
    /// The worktree's id, which is its path.
    pub worktree: String,
    pub role: FlowRole,
    /// `claude` or `codex`. Cursor roles are deferred; see `wtm_agent::authority::profile`.
    pub provider: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    /// What `flow_context` hands the role, fenced.
    pub brief: String,
    /// A provider conversation to continue, when a question's answer resumes a role.
    pub resume: Option<String>,
}

/// A session cleared to open: its request, its scope, and what its profile put on disk.
#[derive(Debug)]
pub struct Prepared {
    pub entry: &'static wtm_agent::ProviderEntry,
    pub request: wtm_agent::SessionRequest,
    pub scope: SessionScope,
    pub places: RunPlaces,
    pub profile: RunProfile,
    /// The token the bridge carries, so a failure later can forget it.
    pub token: String,
}

/// What every run session is told about where it is, appended to the CLI's own prompt.
///
/// Short, because the facts that bind it are enforced and do not need persuading, and because the
/// role's actual instructions arrive through `flow_context`, fenced, where they can quote an issue
/// without the issue speaking as the app.
const RUN_INSTRUCTIONS: &str = "You are running as one role in a Worktree Manager workflow run, \
     with no one watching this session live. Call `flow_context` before anything else: it has your \
     brief. Your only route to the app is the `flow_*` tools. Your sandbox, not this message, decides \
     what you can reach: package registries are the only network, and git, GitHub, the tracker and \
     other agents are not reachable from your shell. Report with `flow_progress`, ask with \
     `flow_ask`, and hand in your result with `flow_submit`.";

/// How long a canary may take before the run refuses: a CLI start and, for Claude, one short turn.
const CANARY_TIMEOUT_MS: u64 = 120_000;

/// A role name as a path segment. Recipe role names are short identifiers; anything else is refused
/// rather than escaped, because the name also lands in a directory a sandbox rule names.
fn segment(role: &str) -> Result<&str, String> {
    let ok = !role.is_empty()
        && role.len() <= 64
        && role
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        Ok(role)
    } else {
        Err(format!(
            "`{role}` cannot name a run role: use letters, digits, `-` and `_`"
        ))
    }
}

/// The user's home, as the profile's credential-store rules need it.
fn home() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "wtm cannot tell where your home folder is".to_owned())
}

/// Where the user's Codex sign-in is, which a run's own Codex home links to.
fn codex_auth(app: &App, home: &Path) -> PathBuf {
    let env = app.pipe.path().child_env();
    env.get("CODEX_HOME")
        .map_or_else(|| home.join(".codex"), PathBuf::from)
        .join("auth.json")
}

/// Every place a role's profile names, for a worktree at `worktree_path` in `project`.
///
/// # Errors
///
/// When the role name cannot be a path segment, or the worktree is not in a repository.
pub fn places(
    app: &App,
    project: &wtm_core::model::Project,
    worktree_path: &Path,
    role: &FlowRole,
) -> Result<RunPlaces, String> {
    let paths = app.config.paths();
    let session_dir =
        paths
            .run_sessions_dir
            .join(format!("flow-{}-{}", role.flow, segment(&role.role)?));
    Ok(RunPlaces {
        worktree: worktree_path.to_path_buf(),
        repo_root: project.root.clone(),
        git: wtm_git::checkpoint::git_dirs(app.runner.as_ref(), worktree_path)
            .map_err(|e| format!("wtm could not find that worktree's git directories: {e}"))?,
        home: home()?,
        session_dir,
        sessions_root: paths.run_sessions_dir.clone(),
        gh_config_dir: paths.run_gh_config_dir.clone(),
    })
}

/// Everything before the canary: the checks, the token, the profile and its files.
///
/// No preflight, which [`open`] runs first: preparing needs no CLI, which is what lets the profile a
/// session gets be tested on a machine without one.
///
/// # Errors
///
/// Every reason the session cannot open, in a sentence.
pub fn prepare(app: &Arc<App>, spec: &RoleSession) -> Result<Prepared, String> {
    let project = app
        .project(&spec.project)
        .map_err(|e| format!("that worktree's project is no longer registered: {e}"))?;
    let worktree = app
        .worktree(&project, &spec.worktree)
        .map_err(|e| format!("that worktree is no longer available: {e}"))?;
    if !app.files.exists(&worktree.path) {
        return Err(format!(
            "`{}` no longer exists on disk — the worktree may need pruning",
            worktree.path.display()
        ));
    }
    let entry = wtm_agent::entry(&spec.provider).ok_or_else(|| {
        format!(
            "`{}` is not an agent this build of wtm knows how to drive",
            spec.provider
        )
    })?;
    // The repository's say over which agents run in it holds for runs as for panes. Its say over
    // *how* they run does not: nothing else from `[agent.<id>]` is read below.
    if !project.offers_agent(entry.id) {
        return Err(format!(
            "this repository's `wtm.toml` does not offer `{}`",
            entry.id
        ));
    }
    let places = places(app, &project, &worktree.path, &spec.role)?;
    let scope = SessionScope::run(&spec.project, &spec.worktree, spec.role.clone());

    let socket = crate::bridge::socket_path()?;
    let program = std::env::current_exe()
        .map_err(|e| format!("wtm could not find its own executable for the run's bridge: {e}"))?;
    let token = app.handoff.issue(handoff::Caller {
        scope: scope.clone(),
        provider: entry.id.to_owned(),
        effort: spec.effort.clone(),
        mode: None,
        session: None,
    });
    let bridge = RunBridge {
        name: handoff::SERVER_NAME.to_owned(),
        server: wtm_agent::McpServer {
            command: program.to_string_lossy().into_owned(),
            args: vec![crate::bridge::ARGV_FLAG.to_owned()],
            env: [
                (handoff::TOKEN_ENV, token.clone()),
                (handoff::SOCKET_ENV, socket.to_string_lossy().into_owned()),
                (
                    crate::flow_tools::ENV,
                    spec.role.authority.as_str().to_owned(),
                ),
            ]
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect(),
        },
        tools: crate::flow_tools::tools_for(spec.role.authority)
            .into_iter()
            .map(str::to_owned)
            .collect(),
    };

    let built = authority::profile(
        entry.id,
        spec.role.authority,
        &places,
        &bridge,
        &codex_auth(app, &places.home),
    )
    .and_then(|profile| {
        write_private(&places, &profile)?;
        Ok(profile)
    });
    let profile = match built {
        Ok(profile) => profile,
        Err(error) => {
            app.handoff.forget_unbound(&token);
            return Err(error);
        }
    };

    let request = wtm_agent::SessionRequest {
        cwd: worktree.path.to_string_lossy().into_owned(),
        executable: None,
        model: spec.model.clone(),
        effort: spec.effort.clone(),
        // The profile decides; see `SessionRequest::run`.
        mode: None,
        fast: None,
        extra_args: Vec::new(),
        env: std::collections::BTreeMap::new(),
        scrub: profile.scrub.clone(),
        run: Some(profile.run.clone()),
        resume: spec.resume.clone(),
        fork: None,
        ephemeral: false,
        // Kept on the request so `open_agent` can bind the token to the session it starts. Neither
        // provider puts it on the argv for a run; it is in the file the profile wrote.
        mcp: [(handoff::SERVER_NAME.to_owned(), bridge.server)]
            .into_iter()
            .collect(),
        instructions: Some(RUN_INSTRUCTIONS.to_owned()),
    };
    Ok(Prepared {
        entry,
        request,
        scope,
        places,
        profile,
        token,
    })
}

/// Make the profile's directories and files, private to the user, and its links.
///
/// # Why each file is removed before it is written
///
/// The directory is the app's, but a file in it could have been replaced by a link — to the user's
/// real Codex config, say — and writing through that link would overwrite whatever it points at.
/// Removing the name and creating it afresh, refusing an existing one, writes only here. The
/// directories themselves are refused if they are links, for `run_profile::prepare_gh_config_dir`'s
/// reason.
fn write_private(places: &RunPlaces, profile: &RunProfile) -> Result<(), String> {
    use std::io::Write as _;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let fail = |what: &str, path: &Path, e: &dyn std::fmt::Display| {
        format!("wtm could not {what} {}: {e}", path.display())
    };
    for dir in std::iter::once(&places.sessions_root).chain(&profile.dirs) {
        match std::fs::symlink_metadata(dir) {
            Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
                return Err(format!(
                    "{} should be a folder that wtm owns, and it is not a plain folder. wtm left \
                     it alone; remove it and try again.",
                    dir.display()
                ));
            }
            Ok(_) => {}
            Err(_) => std::fs::create_dir_all(dir).map_err(|e| fail("create", dir, &e))?,
        }
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| fail("restrict", dir, &e))?;
    }
    for file in &profile.files {
        let _ = std::fs::remove_file(&file.path);
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&file.path)
            .map_err(|e| fail("write", &file.path, &e))?;
        out.write_all(file.contents.as_bytes())
            .map_err(|e| fail("write", &file.path, &e))?;
    }
    for (link, target) in &profile.links {
        let _ = std::fs::remove_file(link);
        std::os::unix::fs::symlink(target, link).map_err(|e| fail("link", link, &e))?;
    }
    Ok(())
}

/// Directories a run may write to by design, so a canary there would prove nothing.
fn temp_roots(app: &App) -> Vec<PathBuf> {
    let mut roots = vec![
        PathBuf::from("/tmp"),
        PathBuf::from("/private/tmp"),
        std::env::temp_dir(),
    ];
    if let Some(dir) = app.pipe.path().child_env().get("TMPDIR") {
        roots.push(PathBuf::from(dir));
    }
    let resolved: Vec<PathBuf> = roots.iter().filter_map(|r| r.canonicalize().ok()).collect();
    roots.extend(resolved);
    roots
}

/// Show that `prepared`'s sandbox refuses a write outside the worktree, or say why the run may not
/// start.
///
/// # Errors
///
/// Why the canary did not pass, which is why the run does not start.
pub fn canary(app: &Arc<App>, prepared: &Prepared) -> Result<(), String> {
    let target = prepared
        .places
        .session_dir
        .join(format!("canary-{}", uuid::Uuid::new_v4()));
    if let Some(problem) =
        authority::canary_place_problem(&prepared.places, &target, &temp_roots(app))
    {
        return Err(problem);
    }
    let program = app
        .agent_executable(prepared.entry)
        .ok_or_else(|| format!("wtm can't find {}'s CLI", prepared.entry.label))?
        .to_string_lossy()
        .into_owned();
    let verdict = match &prepared.profile.run {
        RunAuthority::Claude(run) => claude_canary(app, prepared, run, &program, &target),
        RunAuthority::Codex(run) => codex_canary(app, prepared, run, &program, &target),
    };
    // Whatever the verdict, a file the canary left is the app's to remove: it is in the app's own
    // directory, and only a sandbox that failed could have written it.
    if target.exists() {
        let _ = std::fs::remove_file(&target);
    }
    verdict
}

fn claude_canary(
    app: &Arc<App>,
    prepared: &Prepared,
    run: &ClaudeRun,
    program: &str,
    target: &Path,
) -> Result<(), String> {
    // First the cheap half, with no model turn: the CLI's own account of the sandbox these settings
    // give it. This is what catches a settings object it silently could not read.
    let status = Invocation::new(
        authority::claude_sandbox_status_argv(program, run),
        &prepared.places.worktree,
        30_000,
    )
    .with_scrub(prepared.request.scrub.clone());
    let out = app
        .runner
        .run_allow_failure(&status, &CancelToken::new())
        .map_err(|e| format!("Claude Code would not report its sandbox: {e}"))?;
    authority::read_claude_sandbox_status(&out.stdout)?;

    // Then the write, which needs Bash and therefore a turn: Claude has no way to run a command in
    // its sandbox without a model asking for it.
    let request = wtm_agent::SessionRequest {
        cwd: prepared.request.cwd.clone(),
        executable: Some(program.to_owned()),
        model: Some(authority::CLAUDE_CANARY_MODEL.to_owned()),
        scrub: prepared.request.scrub.clone(),
        run: Some(RunAuthority::Claude(authority::claude_canary(run))),
        ..wtm_agent::SessionRequest::default()
    };
    let events = canary_turn(
        app,
        prepared.entry,
        &request,
        &authority::claude_canary_prompt(target),
    )?;
    authority::judge_claude_canary(&events, target, target.exists())
}

fn codex_canary(
    app: &Arc<App>,
    prepared: &Prepared,
    run: &CodexRun,
    program: &str,
    target: &Path,
) -> Result<(), String> {
    // A throwaway app server with the run's own environment — its Codex home and its scrub — asked
    // to run one command under the run's own sandbox policy. No thread and no model call.
    let argv = prepared.entry.provider.argv(&wtm_agent::SessionRequest {
        executable: Some(program.to_owned()),
        run: Some(RunAuthority::Codex(run.clone())),
        ..wtm_agent::SessionRequest::default()
    });
    let inv = Invocation::new(
        argv,
        &prepared.places.worktree,
        crate::commands::SHELL_TIMEOUT_MS,
    )
    .with_scrub(prepared.request.scrub.clone());
    let command = vec![
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        authority::canary_command(target),
    ];
    let frames =
        wtm_agent::codex::exec_frames(&command, &prepared.request.cwd, &run.sandbox_policy);
    let reply =
        crate::commands::drive_codex(app, &inv, &frames, "command/exec", CANARY_TIMEOUT_MS)?;
    authority::judge_codex_canary(&reply, target.exists())
}

/// What a canary session emitted, and whether its turn is over.
#[derive(Default)]
struct Watch {
    events: Mutex<Vec<AgentEvent>>,
    done: Mutex<bool>,
}

impl wtm_agent::session::AgentSink for Watch {
    fn on_event(&self, _session: &SessionId, event: &AgentEvent) {
        // A permission prompt ends the canary as surely as the turn ending does: under these
        // profiles sandboxed Bash does not ask, and nobody is there to answer.
        let over = matches!(
            event,
            AgentEvent::TurnFinished { .. }
                | AgentEvent::Failed { .. }
                | AgentEvent::LimitReached { .. }
                | AgentEvent::ApprovalRequested { .. }
        );
        self.events.lock().push(event.clone());
        if over {
            *self.done.lock() = true;
        }
    }
    fn on_exit(&self, _session: &SessionId, _outcome: &ExitOutcome) {
        *self.done.lock() = true;
    }
    fn on_ready(&self, _session: &SessionId) {}
}

/// Open a session nobody sees, send it `prompt`, and return what it emitted once its turn is over.
fn canary_turn(
    app: &Arc<App>,
    entry: &'static wtm_agent::ProviderEntry,
    request: &wtm_agent::SessionRequest,
    prompt: &str,
) -> Result<Vec<AgentEvent>, String> {
    let watch = Arc::new(Watch::default());
    let sink: Arc<dyn wtm_agent::session::AgentSink> = Arc::clone(&watch) as _;
    let session = wtm_agent::AgentSession::open(
        entry.provider,
        request,
        Arc::clone(&app.pipe) as Arc<dyn wtm_core::ports::pipe::PipeHost>,
        &sink,
        crate::commands::SHELL_TIMEOUT_MS,
        None,
    )
    .map_err(|e| format!("the sandbox check could not start {}: {e}", entry.label))?;
    let outcome = session
        .send_turn(prompt, &[])
        .map_err(|e| format!("the sandbox check could not send its turn: {e}"))
        .and_then(|()| {
            let deadline = app.clock.monotonic_ms() + CANARY_TIMEOUT_MS;
            while !*watch.done.lock() {
                if app.clock.monotonic_ms() >= deadline {
                    return Err("the sandbox check did not finish in two minutes".to_owned());
                }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Ok(())
        });
    let _ = session.close();
    outcome?;
    let events = watch.events.lock().clone();
    if let Some(AgentEvent::Failed { message } | AgentEvent::LimitReached { message, .. }) =
        events.iter().find(|event| {
            matches!(
                event,
                AgentEvent::Failed { .. } | AgentEvent::LimitReached { .. }
            )
        })
    {
        return Err(format!("the sandbox check's turn failed: {message}"));
    }
    Ok(events)
}

/// Open one role's session: preflight, prepare, canary, then the session itself, announced behind
/// its run's rail.
///
/// # Errors
///
/// Every reason the session did not open. Nothing is left running, and no token stays issued.
pub fn open(
    handle: &tauri::AppHandle,
    app: &Arc<App>,
    spec: &RoleSession,
) -> Result<SessionId, String> {
    crate::run_profile::preflight(app, &[spec.provider.as_str()])?;
    let prepared = prepare(app, spec)?;
    if let Err(error) = canary(app, &prepared) {
        app.handoff.forget_unbound(&prepared.token);
        tracing::warn!(flow = %spec.role.flow, role = %spec.role.role, %error, "a run's sandbox canary failed");
        return Err(format!(
            "The {} role did not start, because its sandbox check failed: {error}",
            spec.role.role
        ));
    }
    let sink: Arc<dyn wtm_agent::session::AgentSink> =
        crate::agent_bridge::AgentEventSink::new(handle.clone());
    let session = app
        .open_agent(
            prepared.entry,
            &prepared.request,
            prepared.scope.clone(),
            &sink,
        )
        .map_err(|error| {
            app.handoff.forget_unbound(&prepared.token);
            format!(
                "could not start the {} role's session: {error}",
                spec.role.role
            )
        })?;
    app.flow_ledger.brief(&spec.role, &spec.brief);
    crate::agent_bridge::announce_spawn(
        handle,
        &crate::agent_bridge::SpawnedSession {
            session: session.as_str().to_owned(),
            project: spec.project.clone(),
            worktree: spec.worktree.clone(),
            provider: prepared.entry.id.to_owned(),
            model: prepared.request.model.clone(),
            effort: prepared.request.effort.clone(),
            mode: Some(mode_label(&prepared.profile.run).to_owned()),
            // No parent: a run session is nobody's delegation. `flow` is what keeps it off the
            // tiles and behind the run's rail instead; see `sessions.svelte.ts`'s `adoptSpawned`.
            parent_session: None,
            run: Some(rail(spec.role.flow)),
            title: Some(spec.role.role.clone()),
            opened_by: None,
            flow: Some(spec.role.clone()),
        },
    );
    Ok(session)
}

/// The rail a run's sessions are grouped under: one per run.
#[must_use]
pub fn rail(flow: wtm_core::flow::FlowId) -> String {
    format!("flow-{flow}")
}

/// What a run session's pane shows as its mode. Display only: the mode is the profile's, and the
/// providers ignore a picker's change to it.
const fn mode_label(run: &RunAuthority) -> &'static str {
    match run {
        RunAuthority::Claude(run) => run.permission_mode,
        RunAuthority::Codex(run) => run.sandbox,
    }
}
