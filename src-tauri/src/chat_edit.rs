//! A conversation edit forks provider history. No display text or provider transcript file is patched.
use crate::{
    agent_bridge::{SpawnedSession, Window},
    app::{AgentStatus, App, SeqEvent},
    commands::{AppState, Branch, Reply, SessionOptions, blocking},
    view::ErrorView,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};
use wtm_core::{
    model::AgentEvent,
    ports::exec::{CancelToken, Invocation},
};

/// How many settled journals are kept.
///
/// A journal exists so that a retry whose reply was lost gets the first outcome back instead of
/// submitting the edit again, and that retry comes from the same dialog moments later. A few
/// dozen is generous for that, and bounds what up to a mebibyte of edited text apiece can cost.
/// A journal whose outcome is unknown is never pruned: it is all that stands between a retry
/// and a second submission.
const SETTLED_JOURNALS_KEPT: usize = 32;

/// The largest edited message accepted. A journal holds it, so it bounds a journal too.
const MAX_EDIT_BYTES: usize = 1024 * 1024;

const UNKNOWN_OUTCOME: &str = "The previous edit has an unknown outcome. It will not be submitted \
    again. Check the conversation history.";
const BUSY: &str = "Session is busy. Wait for it to finish or stop it first.";
const NO_EXACT_BOUNDARY: &str = "This provider adapter does not yet negotiate an exact message \
    boundary. The original conversation is unchanged.";
const HOME_UNSUPPORTED: &str =
    "Editing Home conversations is not supported by this provider adapter yet.";
const TRUNCATED: &str = "The retained message text was truncated. Editing it would lose part of \
    the original input.";
const ATTACHMENTS: &str = "This provider item contains attachments. Editing it without losing \
    those attachments is not supported yet.";
const MID_TURN: &str = "This message was sent inside a running turn. The provider can fork at \
    turn boundaries only.";
const UNPROVEN_START: &str = "The retained history does not prove the exact start of this turn. \
    Reopen the complete conversation first.";
const UNFINISHED: &str = "The turn boundary is not completed yet.";
const NO_EXACT_FORKS: &str =
    "Installed Codex does not advertise exact-boundary forks. Update Codex before editing.";
const UNKNOWN_MODE: &str =
    "The current provider permission mode is unknown; it cannot be inherited safely.";
const CHANGED_DURING_CHECK: &str =
    "Conversation changed while checking provider support. Reopen Edit.";

/// One edit at a time, across every session.
///
/// The journal fences a retry, but a fence that two concurrent requests both read before either
/// wrote is no fence. An edit takes a fork and a send, so serializing them costs nothing anyone
/// can notice.
static EDITS: Mutex<()> = Mutex::new(());

/// The last answer the installed Codex gave about exact-boundary forks.
///
/// Keyed by the resolved executable and its modification time: an upgrade replaces the file, so
/// a new Codex is asked again, and an unchanged one is not asked on every edit. Only a definite
/// answer is kept; a probe that failed for any other reason is tried again next time.
static PROBED: Mutex<Option<(PathBuf, SystemTime, bool)>> = Mutex::new(None);

fn error(message: impl std::fmt::Display) -> ErrorView {
    ErrorView::new("chatEdit", message.to_string())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub id: String,
    pub turn: String,
    pub text: String,
    pub previous: Option<String>,
    pub problem: Option<String>,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub session: String,
    pub revision: u64,
    pub messages: Vec<Message>,
    pub problem: Option<String>,
    pub status: &'static str,
}

pub fn messages(events: &[SeqEvent]) -> Vec<Message> {
    let mut complete = false;
    let mut turns = Vec::<String>::new();
    let mut finished = BTreeSet::new();
    let mut active: Option<String> = None;
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for row in events {
        match &row.event {
            AgentEvent::HistoryBoundary { complete: full } => complete = *full,
            AgentEvent::HistoricalTurn { turn, complete } => {
                if !turns.contains(turn) {
                    turns.push(turn.clone());
                }
                if *complete {
                    finished.insert(turn.clone());
                }
            }
            AgentEvent::TurnStarted { turn } => {
                if !turns.contains(turn) {
                    turns.push(turn.clone());
                }
                active = Some(turn.clone());
            }
            AgentEvent::TurnFinished { turn, .. } => {
                finished.insert(turn.clone());
                if active.as_ref() == Some(turn) {
                    active = None;
                }
            }
            // Codex reports a turn that errored as `Failed` and never finishes it, and neither
            // event names the turn. It is the one that was running: turns do not overlap on one
            // thread. Left open, it and the message after it could never be edited.
            AgentEvent::Failed { .. } | AgentEvent::LimitReached { .. } => {
                if let Some(turn) = active.take() {
                    finished.insert(turn);
                }
            }
            AgentEvent::MessageIdentity {
                message,
                turn,
                text,
                attachments,
                truncated,
            } => {
                if !seen.insert(message.clone()) {
                    continue;
                }
                let position = turns.iter().position(|t| t == turn);
                let previous = position
                    .and_then(|at| at.checked_sub(1))
                    .and_then(|at| turns.get(at))
                    .cloned();
                let problem = if *truncated {
                    Some(TRUNCATED)
                } else if *attachments {
                    Some(ATTACHMENTS)
                } else if out.iter().any(|m: &Message| m.turn == *turn) {
                    Some(MID_TURN)
                } else if position.is_none() || (previous.is_none() && !complete) {
                    Some(UNPROVEN_START)
                } else {
                    None
                };
                out.push(Message {
                    id: message.clone(),
                    turn: turn.clone(),
                    text: text.clone(),
                    previous,
                    problem: problem.map(str::to_owned),
                });
            }
            _ => {}
        }
    }
    for message in &mut out {
        if message.problem.is_none()
            && (!finished.contains(&message.turn)
                || message
                    .previous
                    .as_ref()
                    .is_some_and(|t| !finished.contains(t)))
        {
            message.problem = Some(UNFINISHED.into());
        }
    }
    out
}

/// Whether a session is in the middle of something an edit would race.
///
/// A failed or limit-stopped turn is over, so `Failed` is as editable as `Idle`: refusing it would
/// refuse an edit exactly when one is most wanted, after the turn went wrong. The flag that makes
/// a session report `Failed` stays set until its next turn starts.
fn busy(status: AgentStatus) -> bool {
    match status {
        AgentStatus::Starting | AgentStatus::Working | AgentStatus::NeedsYou => true,
        AgentStatus::Idle | AgentStatus::Failed => false,
    }
}

pub fn snapshot(app: &App, session: &str) -> Reply<Snapshot> {
    let source = app
        .agent_fork_source(session)
        .ok_or_else(|| error("Session is unavailable or still starting"))?;
    let events = app.agent_replay(session);
    let overview = app
        .overview_of(session)
        .ok_or_else(|| error("Session is not live"))?;
    let occupied = busy(overview.status);
    let problem = if occupied {
        Some(BUSY)
    } else if source.provider != wtm_agent::codex::ID {
        Some(NO_EXACT_BOUNDARY)
    } else if source.scope.place().is_none() {
        Some(HOME_UNSUPPORTED)
    } else {
        None
    };
    Ok(Snapshot {
        session: session.into(),
        revision: events.last().map_or(0, |e| e.seq),
        messages: messages(&events),
        problem: problem.map(str::to_owned),
        status: if occupied {
            "busy"
        } else if problem.is_some() {
            "unsupported"
        } else {
            "ready"
        },
    })
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Edit {
    pub id: String,
    pub session: String,
    pub message: String,
    pub revision: u64,
    pub text: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Outcome {
    pub session: String,
    pub original: String,
    pub submitted: bool,
}

/// One edit's record, under its request id.
#[derive(Debug, Serialize, Deserialize)]
struct Journal {
    request: Edit,
    /// `None` while the edit is in flight, and for good if the app stopped before it settled.
    outcome: Option<Outcome>,
    /// Why the edit stopped before anything was submitted. Terminal, like an outcome: a retry
    /// with the same id reports it rather than an outcome nobody knows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    failure: Option<String>,
}
impl Journal {
    fn settled(&self) -> bool {
        self.outcome.is_some() || self.failure.is_some()
    }
}

/// What an earlier attempt with this request id recorded, if there was one.
fn recorded(path: &Path, edit: &Edit) -> Reply<Option<Reply<Outcome>>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(error(format!("Could not read the edit's journal: {e}"))),
    };
    // Unreadable means a crash cut a write short — journals are written whole now, but a build
    // that created them in place could leave an empty one. Whatever it was about to say, the
    // attempt may have reached the provider, so it is an unknown outcome like any other.
    let Ok(journal) = serde_json::from_slice::<Journal>(&bytes) else {
        return Ok(Some(Err(error(UNKNOWN_OUTCOME))));
    };
    if journal.request != *edit {
        return Err(error("This edit id belongs to a different request"));
    }
    Ok(Some(match (journal.outcome, journal.failure) {
        (Some(outcome), _) => Ok(outcome),
        (None, Some(failure)) => Err(error(failure)),
        (None, None) => Err(error(UNKNOWN_OUTCOME)),
    }))
}

fn save(path: &Path, journal: &Journal) -> Reply<()> {
    let bytes = serde_json::to_vec(journal).map_err(error)?;
    crate::app::write_private_atomically(path, &bytes)
        .map_err(|e| error(format!("Could not record the edit's outcome: {e}")))
}

/// Remove the oldest settled journals beyond [`SETTLED_JOURNALS_KEPT`].
///
/// Only files past the newest few are read, so the steady state is one read per edit.
fn prune(folder: &Path) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    let mut journals: Vec<(SystemTime, PathBuf)> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "json"))
        .filter_map(|path| Some((std::fs::metadata(&path).ok()?.modified().ok()?, path)))
        .collect();
    journals.sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    for (_, path) in journals.into_iter().skip(SETTLED_JOURNALS_KEPT) {
        let settled = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Journal>(&bytes).ok())
            .is_some_and(|journal| journal.settled());
        if settled {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Ask the installed Codex whether its `thread/fork` takes a last-turn id.
///
/// Only an edit that forks needs to know, and the answer is cached per executable: generating
/// the schema spawns Codex with a 15-second budget, which is a long time to make every edit wait
/// for an answer that changes only when Codex does.
fn probe(app: &App) -> Reply<()> {
    let entry =
        wtm_agent::entry(wtm_agent::codex::ID).ok_or_else(|| error("Codex is unavailable"))?;
    let executable = app
        .agent_executable(entry)
        .ok_or_else(|| error("Codex is not installed"))?;
    let resolved = executable
        .canonicalize()
        .unwrap_or_else(|_| executable.clone());
    let modified = std::fs::metadata(&resolved).and_then(|m| m.modified()).ok();
    let mut cached = PROBED.lock();
    if let (Some((path, at, supported)), Some(modified)) = (cached.as_ref(), modified)
        && *path == resolved
        && *at == modified
    {
        return if *supported {
            Ok(())
        } else {
            Err(error(NO_EXACT_FORKS))
        };
    }
    let supported = ask(app, &executable)?;
    if let Some(modified) = modified {
        *cached = Some((resolved, modified, supported));
    }
    if supported {
        Ok(())
    } else {
        Err(error(NO_EXACT_FORKS))
    }
}

/// Run the schema generator. `Ok` is a definite answer; `Err` means Codex could not be asked.
fn ask(app: &App, executable: &Path) -> Reply<bool> {
    let scratch = app
        .config
        .paths()
        .config_dir
        .join(crate::app::CODEX_PROBES_DIR);
    // Whatever an earlier probe left behind, one killed halfway included, goes first. Probes are
    // serialized by `PROBED`, so nothing else is using it.
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&scratch)
        .map_err(|e| error(format!("Could not prepare to ask Codex about forks: {e}")))?;
    let invocation = Invocation::new(
        vec![
            executable.to_string_lossy().into_owned(),
            "app-server".into(),
            "generate-json-schema".into(),
            "--out".into(),
            scratch.to_string_lossy().into_owned(),
        ],
        &scratch,
        15_000,
    );
    let answer = app
        .runner
        .run(&invocation, &CancelToken::new())
        .map_err(error)
        .and_then(|output| {
            if !output.is_success() {
                return Err(error(
                    "Installed Codex could not report its fork capabilities.",
                ));
            }
            // A Codex too old to generate this schema at all is as definite a "no" as one whose
            // schema lacks the field, and deserves the same sentence rather than an IO error.
            let Ok(bytes) = std::fs::read(scratch.join("v2/ThreadForkParams.json")) else {
                return Ok(false);
            };
            let schema: Value = serde_json::from_slice(&bytes).map_err(error)?;
            Ok(schema["properties"]["lastTurnId"].is_object())
        });
    let _ = std::fs::remove_dir_all(&scratch);
    answer
}

/// Fork the conversation just before an edited message and send the edited text there.
///
/// The original conversation is never touched. A request id is journalled before anything is
/// spawned, so a retry of the same request returns the first outcome — or the first failure —
/// and an attempt whose outcome is unknown is never submitted again.
///
/// # Errors
///
/// A malformed request, a conversation that changed or cannot be forked, a branch that could not
/// open, or a send or journal write that failed.
pub fn apply(window: &dyn Window, app: &Arc<App>, mut edit: Edit) -> Reply<Outcome> {
    // The canonical spelling, because the id names the journal: a braced or URN spelling of the
    // same request must find the same journal rather than slip past it.
    edit.id = uuid::Uuid::parse_str(&edit.id)
        .map_err(|_| error("Edit request must have a UUID"))?
        .to_string();
    if edit.text.trim().is_empty() || edit.text.len() > MAX_EDIT_BYTES {
        return Err(error("Edited message is empty or too large"));
    }
    let _serial = EDITS.lock();
    let folder = app
        .config
        .paths()
        .config_dir
        .join(crate::app::CHAT_EDITS_DIR);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&folder)
        .map_err(|e| error(format!("Could not prepare the edit journal: {e}")))?;
    let path = folder.join(format!("{}.json", edit.id));
    if let Some(previous) = recorded(&path, &edit)? {
        return previous;
    }

    let before = snapshot(app, &edit.session)?;
    if let Some(problem) = before.problem {
        return Err(error(problem));
    }
    if before.revision != edit.revision {
        return Err(error(
            "Conversation changed. Reopen Edit to review its latest history.",
        ));
    }
    let message = before
        .messages
        .iter()
        .find(|m| m.id == edit.message)
        .ok_or_else(|| error("Message is no longer in retained history"))?;
    if let Some(problem) = &message.problem {
        return Err(error(problem));
    }
    let source = app
        .agent_fork_source(&edit.session)
        .ok_or_else(|| error("Original session closed"))?;
    let (project, worktree) = source.scope.place().ok_or_else(|| error("No workspace"))?;
    // The first message has nothing before it to keep, so editing it is a fresh conversation, and
    // a fresh conversation needs no fork support from the installed Codex.
    let fork = message
        .previous
        .as_ref()
        .map(|_| source.provider_session.clone());
    if fork.is_some() {
        probe(app)?;
    }
    let mut options = SessionOptions::default();
    for row in &app.agent_replay(&edit.session) {
        match &row.event {
            AgentEvent::SessionReady {
                model,
                effort,
                mode,
                ..
            } => {
                options.model.clone_from(model);
                options.effort.clone_from(effort);
                options.mode.clone_from(mode);
            }
            AgentEvent::ModeChanged { mode } => options.mode = Some(mode.clone()),
            _ => {}
        }
    }
    if options.mode.is_none() {
        return Err(error(UNKNOWN_MODE));
    }
    let current = snapshot(app, &edit.session)?;
    if current.revision != edit.revision || current.problem.is_some() {
        return Err(error(CHANGED_DURING_CHECK));
    }

    let mut journal = Journal {
        request: edit.clone(),
        outcome: None,
        failure: None,
    };
    let pending = serde_json::to_vec(&journal).map_err(error)?;
    crate::app::create_private_atomically(&path, &pending).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            error(UNKNOWN_OUTCOME)
        } else {
            error(format!("Could not record the edit before sending it: {e}"))
        }
    })?;
    prune(&folder);

    let spawned = SpawnedSession {
        session: String::new(),
        project: project.to_owned(),
        worktree: worktree.to_owned(),
        provider: source.provider.clone(),
        model: options.model.clone(),
        effort: options.effort.clone(),
        mode: options.mode.clone(),
        parent_session: None,
        run: None,
        title: None,
        opened_by: None,
    };
    let branch = Branch {
        fork,
        through: message.previous.clone(),
        durable: true,
    };
    let opened = crate::commands::open_agent_branch_process(
        window,
        app,
        project,
        worktree,
        &source.provider,
        options,
        branch,
    );
    let session = match opened {
        Ok(session) => session,
        Err(e) => {
            // Nothing was submitted: the edited text is sent only once a branch exists. Recorded
            // as the outcome, so a retry reports this instead of an outcome nobody knows.
            journal.failure = Some(e.message.clone());
            save(&path, &journal)?;
            return Err(e);
        }
    };
    let submitted = (|| {
        journal.outcome = Some(Outcome {
            session: session.clone(),
            original: edit.session.clone(),
            submitted: false,
        });
        save(&path, &journal)?;
        app.send_agent_turn(&session, &edit.text, &[])
            .map_err(error)?;
        app.note_user_turn(&session);
        if let Some(outcome) = &mut journal.outcome {
            outcome.submitted = true;
        }
        save(&path, &journal)?;
        journal
            .outcome
            .clone()
            .ok_or_else(|| error(UNKNOWN_OUTCOME))
    })();
    if submitted.is_err() {
        // The window adopts a branch from this command's reply, and an error carries no session.
        // Announced instead, so a branch that is running is never one nobody can see or close.
        window.announce(&SpawnedSession {
            session: session.clone(),
            ..spawned
        });
    }
    submitted
}

#[tauri::command]
pub async fn chat_edit_snapshot(app: AppState<'_>, session: String) -> Reply<Snapshot> {
    let app = Arc::clone(&app);
    blocking(move || snapshot(&app, &session)).await
}

#[tauri::command]
pub async fn chat_edit(handle: tauri::AppHandle, app: AppState<'_>, edit: Edit) -> Reply<Outcome> {
    let app = Arc::clone(&app);
    blocking(move || apply(&handle, &app, edit)).await
}
