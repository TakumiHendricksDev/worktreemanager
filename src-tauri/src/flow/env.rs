//! The environment supervisor: starting, watching and stopping a worktree's services, and running
//! its commands.
//!
//! The rules it follows are `wtm_core::flow::env`'s; this is where they meet processes, ports, a
//! log file and the inbox. ARCHITECTURE §6j says why it is shaped this way. In short:
//!
//! - **A service is a process group wtm owns, keyed by `(worktree, service)`**, started through
//!   [`ServiceHost`] and recorded in `env_services` with the pid *and* the start time that let a
//!   relaunch recognise it ([`Supervisor::recover`]). Nothing is signalled without both matching.
//! - **Ports are read, never allocated.** A service's `ports` render from the worktree's own
//!   `.env`. Before a start each is bind-probed; one in use is reported with whoever holds it
//!   (`lsof`, and `docker ps` when the project asks), and the start is refused. wtm never ends a
//!   process it did not start.
//! - **Health is asked on a schedule that ends** (`probe_delays`), on a thread per start. There is
//!   no timer afterwards: a service that exits is heard by its parent the moment it does, and one
//!   that stops answering is found by **Check again**, by a restart, or when the Environment tab
//!   is opened again.
//! - **A destructive command on a shared resource waits for an approval** of exactly what it will
//!   run, filed in the inbox against the worktree's environment flow, and an approval covers one
//!   run.
//! - **Quit ends the groups wtm holds and leaves detached services running**; the next launch lists
//!   those, and adopts or forgets the rest ([`Supervisor::recover`]). Idle stop is yours to turn
//!   on, per project.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use parking_lot::Mutex;
use serde::Serialize;
use wtm_appdb::AppDb;
use wtm_core::error::ExecError;
use wtm_core::flow::{
    CommandRequest, CommandRun, CommandRunChange, CommandRunId, CommandRunState, EnvCommandSpec,
    FlowEvent, FlowId, FlowKind, InboxKind, InboxTarget, Isolation, NewApproval, NewCommandRun,
    NewInboxItem, NewWorkItem, Phase, PortConflict, PortOwner, Recovery, ServiceEvent,
    ServiceRecord, ServiceSpec, ServiceState, WorkLink, command_binding, containers_on,
    healthy_status, is_docker_forwarder, loopback_url, needs_approval, parse_lsof, port_number,
    probe_delays,
};
use wtm_core::model::{ExitOutcome, Project, ProjectId, Worktree};
use wtm_core::ports::exec::{CancelToken, CommandRunner, Invocation};
use wtm_core::ports::service::{ServiceHost, Termination};
use wtm_core::ports::template::Context;
use wtm_core::ports::{
    Clock, EnvStore, FlowStore, FlowStoreError, InboxStore, OutboxStore, RunStore,
};

use super::{Engine, text};
use crate::app::App;
use crate::display;

/// How much of a log the Environment tab shows: the end, where a failure is.
const LOG_TAIL_BYTES: u64 = 64 * 1024;

/// How much of a command's output is kept with its run.
const OUTPUT_TAIL_CHARS: usize = 4_000;

/// How long one health question may take. A local server answers in milliseconds; one that takes
/// five seconds to say anything is not up in any sense that matters for testing.
const PROBE_TIMEOUT_MS: u64 = 5_000;

/// How long `lsof` and `docker ps` may take to say who holds a port.
const OWNER_LOOKUP_TIMEOUT_MS: u64 = 5_000;

/// How long a command's runs are listed for in the Environment tab.
const RECENT_RUNS: usize = 5;

/// A service of a worktree: `(worktree id, service id)`.
type Key = (String, String);

/// Who hears that a worktree's environment changed. The app emits `env:changed`; a test records it.
pub trait EnvNotice: Send + Sync {
    fn changed(&self, project: &str, worktree: &str);
}

/// What tests shorten.
#[derive(Debug, Clone)]
pub struct EnvTuning {
    /// How long a minute of idle stop is, in milliseconds.
    pub idle_minute_ms: u64,
}

impl Default for EnvTuning {
    fn default() -> Self {
        Self {
            idle_minute_ms: 60_000,
        }
    }
}

/// What the supervisor holds for a service beyond its row.
#[derive(Debug, Clone, Default)]
struct Live {
    /// Which start this is. A health answer or an exit from an earlier start is ignored.
    generation: u64,
    stopping: bool,
    /// Left running when wtm last closed, and taken back by this launch.
    recovered: bool,
    /// The ports that stopped the last start, and who holds them.
    conflicts: Vec<PortConflict>,
}

/// How a health question is asked, rendered.
#[derive(Debug, Clone)]
enum Probe {
    Url(String),
    Command { argv: Vec<String>, cwd: PathBuf },
}

// ═══════════════════════════════ views ═══════════════════════════════

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentView {
    pub project: String,
    pub worktree: String,
    /// Whether the project declares anything. The tab and the header control show only when it
    /// does.
    pub declared: bool,
    pub services: Vec<ServiceView>,
    pub resources: Vec<ResourceView>,
    pub commands: Vec<CommandView>,
    /// The worktree's latest command runs, newest first.
    pub runs: Vec<CommandRunView>,
    pub idle_stop_minutes: Option<u32>,
    /// Why the config could not be read, when it could not: untrusted, or invalid.
    pub problem: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceView {
    pub id: String,
    pub label: String,
    pub state: ServiceState,
    pub detail: Option<String>,
    /// Each declared port, rendered: the number, or why it did not render to one.
    pub ports: Vec<PortView>,
    /// What its health check asks, rendered: a URL, or a command's program.
    pub health: String,
    /// Its preview, rendered, when it renders to an address on this machine.
    pub preview: Option<String>,
    pub detached: bool,
    pub pid: Option<u32>,
    /// Left running when wtm last closed, and still running.
    pub left_running: bool,
    pub conflicts: Vec<PortConflict>,
    pub started_at: Option<u64>,
    pub updated_at: Option<u64>,
    pub has_log: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PortView {
    pub port: Option<u16>,
    pub problem: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceView {
    pub id: String,
    pub label: String,
    pub isolation: Isolation,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandView {
    pub id: String,
    pub label: String,
    /// As declared, templates and all: the rendered argv can hold a value from `.env`.
    pub argv: Vec<String>,
    pub destructive: bool,
    pub resources: Vec<String>,
    pub needs_approval: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandRunView {
    pub id: CommandRunId,
    pub flow: FlowId,
    pub command: String,
    pub label: String,
    pub argv: Vec<String>,
    pub state: CommandRunState,
    pub exit_code: Option<i32>,
    pub output: Option<String>,
    pub requested_at: u64,
    pub finished_at: Option<u64>,
}

impl From<CommandRun> for CommandRunView {
    fn from(run: CommandRun) -> Self {
        Self {
            id: run.id,
            flow: run.flow,
            command: run.command,
            label: run.label,
            argv: run.argv,
            state: run.state,
            exit_code: run.exit_code,
            output: run.output,
            requested_at: run.requested_at,
            finished_at: run.finished_at,
        }
    }
}

/// A command waiting for your approval, as the inbox shows it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingCommandView {
    pub run: CommandRunView,
    pub project: String,
    pub worktree: String,
    /// The worktree's directory name, for a sentence.
    pub worktree_name: String,
    /// The shared resources that make it wait, by label.
    pub shared: Vec<String>,
}

/// A preview address that answered just now.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifiedPreview {
    pub url: String,
    pub status: u16,
    pub ms: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceLog {
    pub path: String,
    pub text: String,
    /// Only the end of a longer log is shown.
    pub truncated: bool,
}

// ═══════════════════════════════ the supervisor ═══════════════════════════════

pub struct Supervisor {
    app: Arc<App>,
    engine: Arc<Engine>,
    host: Arc<dyn ServiceHost>,
    runner: Arc<dyn CommandRunner>,
    clock: Arc<dyn Clock>,
    notice: Arc<dyn EnvNotice>,
    tuning: EnvTuning,
    live: Mutex<BTreeMap<Key, Live>>,
    /// One start, stop or restart at a time per worktree, so two clicks cannot both start `web`.
    worktree_locks: Mutex<BTreeMap<String, Arc<Mutex<()>>>>,
    generation: AtomicU64,
    /// Bumped whenever a window gains or loses focus. An idle stop that wakes to a different number
    /// was overtaken by you coming back.
    focus: AtomicU64,
    quitting: AtomicBool,
    myself: Weak<Self>,
}

impl std::fmt::Debug for Supervisor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Supervisor").finish_non_exhaustive()
    }
}

impl Supervisor {
    #[must_use]
    pub fn new(
        app: Arc<App>,
        engine: Arc<Engine>,
        host: Arc<dyn ServiceHost>,
        runner: Arc<dyn CommandRunner>,
        clock: Arc<dyn Clock>,
        notice: Arc<dyn EnvNotice>,
        tuning: EnvTuning,
    ) -> Arc<Self> {
        Arc::new_cyclic(|myself| Self {
            app,
            engine,
            host,
            runner,
            clock,
            notice,
            tuning,
            live: Mutex::new(BTreeMap::new()),
            worktree_locks: Mutex::new(BTreeMap::new()),
            generation: AtomicU64::new(0),
            focus: AtomicU64::new(0),
            quitting: AtomicBool::new(false),
            myself: myself.clone(),
        })
    }

    fn db(&self) -> Result<Arc<AppDb>, String> {
        self.engine.db()
    }

    fn now(&self) -> u64 {
        self.clock.now_unix_ms()
    }

    /// The database, only if it already exists. What runs without a click — a window losing focus,
    /// a quit, a worktree's removal — must not create one for someone who never opened the Work
    /// view, and with no database there is nothing wtm started to act on.
    fn existing_db(&self) -> Option<Arc<AppDb>> {
        if !self.app.files.exists(&self.app.config.paths().flow_db) {
            return None;
        }
        self.db().ok()
    }

    fn lock_for(&self, worktree: &str) -> Arc<Mutex<()>> {
        Arc::clone(
            self.worktree_locks
                .lock()
                .entry(worktree.to_owned())
                .or_default(),
        )
    }

    fn resolve(&self, project_id: &str, worktree_id: &str) -> Result<(Project, Worktree), String> {
        let project = self.app.project(project_id).map_err(text)?;
        let worktree = self
            .app
            .worktree(&project, worktree_id)
            .map_err(|e| format!("that worktree is not available: {e}"))?;
        Ok((project, worktree))
    }

    /// The template context of a worktree: its facts and its `.env`, as a display link sees them.
    fn context(&self, project: &Project, worktree: &Worktree) -> Context {
        let mut ctx = display::base_context(project, self.app.os_tokens());
        display::add_worktree_tokens(&mut ctx, worktree);
        let sources = display::read_sources(
            project,
            self.app.files.as_ref(),
            self.app.engine.as_ref(),
            &ctx,
        );
        display::add_source_tokens(&mut ctx, project, &sources);
        ctx
    }

    fn render(&self, ctx: &Context, key: &str, template: &str) -> Result<String, String> {
        self.app
            .engine
            .render(key, template, ctx)
            .map_err(|e| format!("`{key}` did not render: {e}"))
    }

    fn render_argv(
        &self,
        project: &Project,
        ctx: &Context,
        key: &str,
        argv: &[String],
    ) -> Result<Vec<String>, String> {
        let rendered = argv
            .iter()
            .enumerate()
            .map(|(i, word)| self.render(ctx, &format!("{key}[{i}]"), word))
            .collect::<Result<Vec<_>, _>>()?;
        wtm_config::check_forbidden(project, &rendered).map_err(text)?;
        Ok(rendered)
    }

    fn log_path(&self, worktree: &Worktree, service: &str) -> PathBuf {
        let id = worktree.id.as_str();
        let digest = wtm_core::flow::sha256_of(id.as_bytes());
        let short = digest
            .trim_start_matches("sha256:")
            .get(..10)
            .unwrap_or("0");
        self.app
            .config
            .paths()
            .env_dir
            .join(format!("{}-{short}", worktree.dirname()))
            .join(format!("{service}.log"))
    }

    fn record(&self, key: &Key) -> Option<ServiceRecord> {
        self.db()
            .ok()?
            .services(Some(&key.0))
            .ok()?
            .into_iter()
            .find(|r| r.service == key.1)
    }

    fn save(&self, record: &ServiceRecord) {
        let Ok(db) = self.db() else { return };
        if let Err(e) = db.save_service(record) {
            tracing::warn!(service = %record.service, error = %e, "could not record a service");
        }
        self.notice.changed(&record.project, &record.worktree);
    }

    /// Move a service by `event`, if its state has that step, keeping its process unless told to
    /// let it go.
    fn transition(
        &self,
        key: &Key,
        event: ServiceEvent,
        detail: Option<String>,
        clear_process: bool,
    ) {
        let Some(mut record) = self.record(key) else {
            return;
        };
        let Ok(next) = record.state.after(event) else {
            tracing::debug!(service = %key.1, from = ?record.state, ?event, "ignored a late event");
            return;
        };
        record.state = next;
        record.detail = detail;
        if clear_process {
            record.process = None;
        }
        record.updated_at = self.now();
        self.save(&record);
    }

    /// Whether `generation` is still the live start of `key`.
    fn current(&self, key: &Key, generation: u64) -> bool {
        self.live
            .lock()
            .get(key)
            .is_some_and(|live| live.generation == generation && !live.stopping)
    }

    // ═══════════════════════════════ reading ═══════════════════════════════

    /// A worktree's environment as the Environment tab shows it.
    ///
    /// # Errors
    ///
    /// When the worktree cannot be found. A config that cannot be read is a `problem` in the view,
    /// not an error, so the tab can say why.
    pub fn view(&self, project_id: &str, worktree_id: &str) -> Result<EnvironmentView, String> {
        let project = match self.app.project(project_id) {
            Ok(project) => project,
            Err(e) => {
                return Ok(EnvironmentView {
                    project: project_id.to_owned(),
                    worktree: worktree_id.to_owned(),
                    declared: false,
                    services: Vec::new(),
                    resources: Vec::new(),
                    commands: Vec::new(),
                    runs: Vec::new(),
                    idle_stop_minutes: None,
                    problem: Some(e.to_string()),
                });
            }
        };
        let worktree = self
            .app
            .worktree(&project, worktree_id)
            .map_err(|e| format!("that worktree is not available: {e}"))?;
        self.refresh_recovered(worktree_id);
        let env = &project.environment;
        let ctx = self.context(&project, &worktree);
        let db = self.db().ok();
        let records: BTreeMap<String, ServiceRecord> = db
            .as_ref()
            .and_then(|db| db.services(Some(worktree_id)).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|r| (r.service.clone(), r))
            .collect();
        let live = self.live.lock().clone();

        let services = env
            .services
            .iter()
            .map(|spec| {
                let record = records.get(&spec.id);
                let key = (worktree_id.to_owned(), spec.id.clone());
                let held = live.get(&key);
                let ports = spec
                    .ports
                    .iter()
                    .enumerate()
                    .map(|(i, template)| {
                        match self
                            .render(&ctx, &format!("service.{}.ports[{i}]", spec.id), template)
                            .and_then(|r| port_number(&r))
                        {
                            Ok(port) => PortView {
                                port: Some(port),
                                problem: None,
                            },
                            Err(problem) => PortView {
                                port: None,
                                problem: Some(problem),
                            },
                        }
                    })
                    .collect();
                let health = match (&spec.health.url, &spec.health.command) {
                    (Some(url), _) => self
                        .render(&ctx, "health.url", url)
                        .unwrap_or_else(|_| url.clone()),
                    (None, Some(argv)) => argv.join(" "),
                    (None, None) => String::new(),
                };
                let preview = spec.preview.as_ref().and_then(|template| {
                    self.render(&ctx, "preview", template)
                        .ok()
                        .and_then(|url| loopback_url(&url).ok())
                        .map(|url| url.as_str().to_owned())
                });
                let log = self.log_path(&worktree, &spec.id);
                let state = record.map_or(ServiceState::Stopped, |r| r.state);
                ServiceView {
                    id: spec.id.clone(),
                    label: spec.label().to_owned(),
                    state,
                    detail: record.and_then(|r| r.detail.clone()),
                    ports,
                    health,
                    preview,
                    detached: spec.detached,
                    pid: record
                        .filter(|r| !r.detached)
                        .and_then(|r| r.process.as_ref().map(|p| p.pid)),
                    left_running: held.is_some_and(|l| l.recovered) && state.is_running(),
                    conflicts: held.map(|l| l.conflicts.clone()).unwrap_or_default(),
                    started_at: record.and_then(|r| r.started_at),
                    updated_at: record.map(|r| r.updated_at),
                    has_log: log.exists(),
                }
            })
            .collect();

        let runs = db
            .as_ref()
            .and_then(|db| {
                db.environment_flow(worktree_id)
                    .ok()
                    .flatten()
                    .map(|f| (db, f))
            })
            .and_then(|(db, flow)| db.command_runs(flow, RECENT_RUNS).ok())
            .unwrap_or_default()
            .into_iter()
            .map(CommandRunView::from)
            .collect();

        Ok(EnvironmentView {
            project: project_id.to_owned(),
            worktree: worktree_id.to_owned(),
            declared: !env.is_empty(),
            services,
            resources: env
                .resources
                .iter()
                .map(|r| ResourceView {
                    id: r.id.clone(),
                    label: r.label().to_owned(),
                    isolation: r.isolation,
                })
                .collect(),
            commands: env
                .commands
                .iter()
                .map(|c| CommandView {
                    id: c.id.clone(),
                    label: c.label().to_owned(),
                    argv: c.run.clone(),
                    destructive: c.destructive,
                    resources: c
                        .resources
                        .iter()
                        .map(|id| {
                            env.resource(id)
                                .map_or(id.as_str(), |r| r.label())
                                .to_owned()
                        })
                        .collect(),
                    needs_approval: needs_approval(env, c),
                })
                .collect(),
            runs,
            idle_stop_minutes: db
                .as_ref()
                .and_then(|db| db.idle_stop(&ProjectId::from_root(&project.root)).ok())
                .flatten(),
            problem: None,
        })
    }

    /// A service taken back at launch has no thread waiting on it, because it is not this run's
    /// child. Whether it is still there is checked here, when someone looks, which is the on-demand
    /// check ARCHITECTURE §8 allows in place of a timer.
    fn refresh_recovered(&self, worktree_id: &str) {
        let recovered: Vec<Key> = self
            .live
            .lock()
            .iter()
            .filter(|(key, live)| key.0 == worktree_id && live.recovered)
            .map(|(key, _)| key.clone())
            .collect();
        for key in recovered {
            let Some(record) = self.record(&key) else {
                continue;
            };
            if record.detached || !record.state.is_running() {
                continue;
            }
            let gone = record
                .process
                .as_ref()
                .is_none_or(|p| self.host.identify(p.pid).as_ref() != Some(p));
            if gone {
                self.live.lock().remove(&key);
                self.transition(
                    &key,
                    ServiceEvent::Exited,
                    Some("it exited after wtm took it back at launch".to_owned()),
                    true,
                );
            }
        }
    }

    // ═══════════════════════════════ starting ═══════════════════════════════

    /// Start one service, or every one not running.
    ///
    /// # Errors
    ///
    /// Why a service did not start, in a sentence: a port in use (with its owner), a port that
    /// does not render, a program that is not there. Services already started stay started.
    pub fn start(
        &self,
        project_id: &str,
        worktree_id: &str,
        service: Option<&str>,
    ) -> Result<EnvironmentView, String> {
        let (project, worktree) = self.resolve(project_id, worktree_id)?;
        let lock = self.lock_for(worktree_id);
        let _one = lock.lock();
        let ctx = self.context(&project, &worktree);
        let specs = Self::chosen(&project, service)?;
        let mut problems = Vec::new();
        for spec in specs {
            let key = (worktree_id.to_owned(), spec.id.clone());
            if self.record(&key).is_some_and(|r| r.state.is_running()) {
                continue;
            }
            if let Err(problem) = self.start_one(&project, &worktree, &ctx, spec) {
                problems.push(format!("`{}`: {problem}", spec.id));
            }
        }
        if problems.is_empty() {
            self.view(project_id, worktree_id)
        } else {
            Err(problems.join("; "))
        }
    }

    fn chosen<'a>(
        project: &'a Project,
        service: Option<&str>,
    ) -> Result<Vec<&'a ServiceSpec>, String> {
        let env = &project.environment;
        match service {
            Some(id) => env
                .service(id)
                .map(|spec| vec![spec])
                .ok_or_else(|| format!("this project declares no service `{id}`")),
            None if env.services.is_empty() => {
                Err("this project declares no `[[environment.service]]`".to_owned())
            }
            None => Ok(env.services.iter().collect()),
        }
    }

    fn start_one(
        &self,
        project: &Project,
        worktree: &Worktree,
        ctx: &Context,
        spec: &ServiceSpec,
    ) -> Result<(), String> {
        let worktree_id = worktree.id.as_str().to_owned();
        let key = (worktree_id.clone(), spec.id.clone());
        let project_root = project.root.to_string_lossy().into_owned();
        let log = self.log_path(worktree, &spec.id);
        let base = ServiceRecord {
            project: project_root.clone(),
            worktree: worktree_id.clone(),
            service: spec.id.clone(),
            state: ServiceState::Stopped,
            process: None,
            detached: spec.detached,
            log: log.to_string_lossy().into_owned(),
            detail: None,
            started_at: None,
            updated_at: self.now(),
        };
        let refuse = |detail: String, state: ServiceState| {
            self.save(&ServiceRecord {
                state,
                detail: Some(detail.clone()),
                ..base.clone()
            });
            detail
        };

        let mut ports = Vec::new();
        for (i, template) in spec.ports.iter().enumerate() {
            let port = self
                .render(
                    ctx,
                    &format!("environment.service.{}.ports[{i}]", spec.id),
                    template,
                )
                .and_then(|r| port_number(&r))
                .map_err(|problem| {
                    refuse(
                        format!("port `{template}` {problem}"),
                        ServiceState::Stopped,
                    )
                })?;
            ports.push(port);
        }
        let conflicts: Vec<PortConflict> = ports
            .iter()
            .filter(|port| port_in_use(**port))
            .map(|port| PortConflict {
                port: *port,
                owners: self.owners(*port, project.environment.docker),
            })
            .collect();
        self.live
            .lock()
            .entry(key.clone())
            .or_default()
            .conflicts
            .clone_from(&conflicts);
        if !conflicts.is_empty() {
            let detail = conflict_sentence(&conflicts);
            return Err(refuse(detail, ServiceState::Stopped));
        }

        let argv = self
            .render_argv(
                project,
                ctx,
                &format!("environment.service.{}.start", spec.id),
                &spec.start,
            )
            .map_err(|e| refuse(e, ServiceState::Failed))?;
        let probe = self
            .probe_of(project, worktree, ctx, spec)
            .map_err(|e| refuse(e, ServiceState::Failed))?;
        append_header(&log, &format!("started {}", argv.join(" ")));

        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.live.lock().insert(
            key.clone(),
            Live {
                generation,
                ..Live::default()
            },
        );
        let started_at = Some(self.now());

        // Starting is recorded before anything runs, because an attached service's exit is heard on
        // the thread that reaps it, which may run before `launch` has even returned. Recorded
        // first, an instant exit moves Starting to Failed, and nothing below can move it back.
        self.save(&ServiceRecord {
            state: ServiceState::Starting,
            started_at,
            ..base.clone()
        });
        let process = if spec.detached {
            let inv = Invocation::new(argv, &worktree.path, spec.health.timeout_ms());
            let out = self.runner.run_allow_failure(&inv, &CancelToken::new());
            append_output(&log, &out);
            match out {
                Ok(out) if out.is_success() => {}
                Ok(out) => {
                    self.live.lock().remove(&key);
                    return Err(refuse(
                        format!(
                            "`start` exited {}: {}",
                            out.code,
                            last_line(&out.stderr, &out.stdout)
                        ),
                        ServiceState::Failed,
                    ));
                }
                Err(e) => {
                    self.live.lock().remove(&key);
                    return Err(refuse(e.to_string(), ServiceState::Failed));
                }
            }
            match &spec.logs {
                Some(logs) => {
                    let argv = self
                        .render_argv(
                            project,
                            ctx,
                            &format!("environment.service.{}.logs", spec.id),
                            logs,
                        )
                        .map_err(|e| refuse(e, ServiceState::Failed))?;
                    // Its exit says nothing about the service, so nobody listens for it.
                    self.host
                        .launch(
                            &Invocation::new(argv, &worktree.path, 1),
                            &log,
                            Box::new(|_| {}),
                        )
                        .map_err(
                            |e| tracing::warn!(error = %e, "could not follow a service's logs"),
                        )
                        .ok()
                }
                None => None,
            }
        } else {
            let inv = Invocation::new(argv, &worktree.path, 1);
            let weak = self.myself.clone();
            let exited = key.clone();
            let on_exit = Box::new(move |outcome: ExitOutcome| {
                if let Some(this) = weak.upgrade() {
                    this.exited(&exited, generation, &outcome);
                }
            });
            match self.host.launch(&inv, &log, on_exit) {
                Ok(identity) => Some(identity),
                Err(e) => {
                    self.live.lock().remove(&key);
                    return Err(refuse(start_failure(&e), ServiceState::Failed));
                }
            }
        };

        // The process joins the record only while this start is still the live one, under the lock
        // `exited` takes first: an exit that got there before this has already said Failed.
        {
            let live = self.live.lock();
            if live.get(&key).is_some_and(|l| l.generation == generation) {
                if let Some(mut record) = self.record(&key) {
                    record.process = process;
                    record.updated_at = self.now();
                    self.save(&record);
                }
            } else if let Some(process) = &process {
                // A detached service's log follower outlived the start it belonged to.
                let _ = self.host.terminate(process);
            }
        }
        self.watch(key, generation, probe, spec.health.timeout_ms());
        Ok(())
    }

    fn probe_of(
        &self,
        project: &Project,
        worktree: &Worktree,
        ctx: &Context,
        spec: &ServiceSpec,
    ) -> Result<Probe, String> {
        match (&spec.health.url, &spec.health.command) {
            (Some(url), _) => {
                let rendered = self.render(ctx, &format!("service.{}.health.url", spec.id), url)?;
                loopback_url(&rendered)
                    .map(|url| Probe::Url(url.as_str().to_owned()))
                    .map_err(text)
            }
            (None, Some(argv)) => Ok(Probe::Command {
                argv: self.render_argv(
                    project,
                    ctx,
                    &format!("environment.service.{}.health.command", spec.id),
                    argv,
                )?,
                cwd: worktree.path.clone(),
            }),
            (None, None) => Err("it declares no health check".to_owned()),
        }
    }

    /// Ask a started service whether it is up, on the schedule, until it answers, its deadline
    /// passes, or it stops being this start.
    fn watch(&self, key: Key, generation: u64, probe: Probe, timeout_ms: u64) {
        let Some(this) = self.myself.upgrade() else {
            return;
        };
        let spawned = std::thread::Builder::new()
            .name(format!("health-{}", key.1))
            .spawn(move || {
                let began = this.clock.monotonic_ms();
                let mut last = "it did not answer".to_owned();
                for delay in probe_delays(timeout_ms) {
                    std::thread::sleep(Duration::from_millis(delay));
                    if !this.current(&key, generation) {
                        return;
                    }
                    let left = timeout_ms.saturating_sub(this.clock.monotonic_ms() - began);
                    match this.ask(&probe, left.clamp(500, PROBE_TIMEOUT_MS)) {
                        Ok(answer) => {
                            if this.current(&key, generation) {
                                this.transition(&key, ServiceEvent::Answered, Some(answer), false);
                            }
                            return;
                        }
                        Err(why) => last = why,
                    }
                    if this.clock.monotonic_ms() - began >= timeout_ms {
                        break;
                    }
                }
                if this.current(&key, generation) {
                    this.transition(
                        &key,
                        ServiceEvent::Unanswered,
                        Some(format!("no answer within {}: {last}", seconds(timeout_ms))),
                        false,
                    );
                }
            });
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "could not watch a service's health");
        }
    }

    /// One health question.
    fn ask(&self, probe: &Probe, timeout_ms: u64) -> Result<String, String> {
        match probe {
            Probe::Url(url) => {
                let (status, ms) = self.fetch(url, timeout_ms)?;
                if healthy_status(status) {
                    Ok(if ms == 0 {
                        format!("answered {status} in under 1 ms")
                    } else {
                        format!("answered {status} in {ms} ms")
                    })
                } else {
                    Err(format!("answered {status}"))
                }
            }
            Probe::Command { argv, cwd } => {
                let inv = Invocation::new(argv.clone(), cwd, timeout_ms);
                match self.runner.run_allow_failure(&inv, &CancelToken::new()) {
                    Ok(out) if out.is_success() => Ok(format!("`{}` exited 0", argv[0])),
                    Ok(out) => Err(format!(
                        "`{}` exited {}: {}",
                        argv[0],
                        out.code,
                        last_line(&out.stderr, &out.stdout)
                    )),
                    Err(e) => Err(e.to_string()),
                }
            }
        }
    }

    /// GET a loopback URL with the user's own `curl`: the status and how long it took.
    ///
    /// `curl` rather than a socket of our own, for ARCHITECTURE §6a's reason: wtm links no HTTP
    /// client. `--noproxy '*'`, because a proxy configured for the world has no business with
    /// `localhost`, and a corporate one would answer for a server that is not there.
    fn fetch(&self, url: &str, timeout_ms: u64) -> Result<(u16, u64), String> {
        let url = loopback_url(url).map_err(text)?;
        let seconds = format!("{}.{}", timeout_ms / 1000, (timeout_ms % 1000) / 100);
        let inv = Invocation::new(
            [
                "curl",
                "--silent",
                "--show-error",
                "--output",
                "/dev/null",
                "--write-out",
                "%{http_code} %{time_total}",
                "--max-time",
                &seconds,
                "--noproxy",
                "*",
                url.as_str(),
            ]
            .map(str::to_owned)
            .to_vec(),
            "/",
            timeout_ms + 2_000,
        );
        let out = self
            .runner
            .run_allow_failure(&inv, &CancelToken::new())
            .map_err(text)?;
        if !out.is_success() {
            return Err(match out.code {
                7 => "nothing is listening there yet".to_owned(),
                28 => "it did not answer in time".to_owned(),
                _ => last_line(&out.stderr, &out.stdout),
            });
        }
        let mut words = out.stdout.split_whitespace();
        let status = words
            .next()
            .and_then(|w| w.parse::<u16>().ok())
            .ok_or_else(|| format!("curl said `{}`", out.stdout.trim()))?;
        let ms = words.next().map_or(0, seconds_text_to_ms);
        Ok((status, ms))
    }

    /// Who holds `port`: `lsof`'s listeners, the container Docker publishes it from when the
    /// project asks or the listener is Docker's own forwarder, and which of them wtm started.
    fn owners(&self, port: u16, docker: bool) -> Vec<PortOwner> {
        let run = |argv: &[&str]| {
            let inv = Invocation::new(
                argv.iter().map(|w| (*w).to_owned()).collect(),
                "/",
                OWNER_LOOKUP_TIMEOUT_MS,
            );
            self.runner
                .run_allow_failure(&inv, &CancelToken::new())
                .ok()
                .filter(wtm_core::ports::exec::Output::is_success)
                .map(|out| out.stdout)
        };
        let mut owners = run(&[
            "lsof",
            "-nP",
            &format!("-iTCP:{port}"),
            "-sTCP:LISTEN",
            "-Fpc",
        ])
        .map(|out| parse_lsof(&out))
        .unwrap_or_default();
        if (docker || owners.iter().any(|o| is_docker_forwarder(&o.command)))
            && let Some(out) = run(&[
                "docker",
                "ps",
                "--format",
                "{{.Names}}\t{{.Image}}\t{{.Ports}}",
            ])
        {
            owners.extend(containers_on(&out, port));
        }
        let ours: Vec<ServiceRecord> = self
            .db()
            .ok()
            .and_then(|db| db.services(None).ok())
            .unwrap_or_default()
            .into_iter()
            .filter(|r| r.state.is_running())
            .collect();
        for owner in &mut owners {
            let Some(pid) = owner.pid else { continue };
            let group = self.group_of(pid);
            owner.service = ours
                .iter()
                .find(|r| {
                    r.process
                        .as_ref()
                        .is_some_and(|p| p.pid == pid || Some(p.pid) == group)
                })
                .map(|r| wtm_core::flow::OwnedService {
                    worktree: r.worktree.clone(),
                    service: r.service.clone(),
                });
        }
        owners
    }

    fn group_of(&self, pid: u32) -> Option<u32> {
        let inv = Invocation::new(
            vec![
                "ps".to_owned(),
                "-o".to_owned(),
                "pgid=".to_owned(),
                "-p".to_owned(),
                pid.to_string(),
            ],
            "/",
            OWNER_LOOKUP_TIMEOUT_MS,
        );
        self.runner
            .run_allow_failure(&inv, &CancelToken::new())
            .ok()
            .and_then(|out| out.stdout.trim().parse().ok())
    }

    /// A process this run started exited.
    fn exited(&self, key: &Key, generation: u64, outcome: &ExitOutcome) {
        {
            let mut live = self.live.lock();
            let Some(entry) = live.get(key) else { return };
            if entry.generation != generation || entry.stopping {
                return;
            }
            live.remove(key);
        }
        if self.quitting.load(Ordering::SeqCst) {
            return;
        }
        let detail = match outcome {
            ExitOutcome::Success => "it exited on its own, with code 0".to_owned(),
            ExitOutcome::Failed { code } => format!("it exited with code {code}"),
            ExitOutcome::Signalled { signal } => format!("it was ended by signal {signal}"),
            ExitOutcome::TimedOut { .. } | ExitOutcome::Cancelled => "it was ended".to_owned(),
        };
        self.transition(key, ServiceEvent::Exited, Some(detail), true);
    }

    // ═══════════════════════════════ stopping ═══════════════════════════════

    /// Stop one service, or every running one.
    ///
    /// # Errors
    ///
    /// When the worktree or the service cannot be found.
    pub fn stop(
        &self,
        project_id: &str,
        worktree_id: &str,
        service: Option<&str>,
    ) -> Result<EnvironmentView, String> {
        let (project, worktree) = self.resolve(project_id, worktree_id)?;
        let lock = self.lock_for(worktree_id);
        let _one = lock.lock();
        let ctx = self.context(&project, &worktree);
        let wanted: Option<BTreeSet<&str>> = match service {
            Some(id) => Some(
                Self::chosen(&project, Some(id))?
                    .iter()
                    .map(|s| s.id.as_str())
                    .collect(),
            ),
            None => None,
        };
        let records = self.db()?.services(Some(worktree_id)).map_err(text)?;
        for record in records {
            if wanted
                .as_ref()
                .is_some_and(|w| !w.contains(record.service.as_str()))
            {
                continue;
            }
            let spec = project.environment.service(&record.service);
            // Stopped by you: the state says so, and a detail repeating it would only be noise.
            self.stop_one(&project, &worktree, Some(&ctx), spec, record, None);
        }
        self.view(project_id, worktree_id)
    }

    /// Stop then start.
    ///
    /// # Errors
    ///
    /// As [`Self::start`].
    pub fn restart(
        &self,
        project_id: &str,
        worktree_id: &str,
        service: Option<&str>,
    ) -> Result<EnvironmentView, String> {
        self.stop(project_id, worktree_id, service)?;
        self.start(project_id, worktree_id, service)
    }

    fn stop_one(
        &self,
        project: &Project,
        worktree: &Worktree,
        ctx: Option<&Context>,
        spec: Option<&ServiceSpec>,
        mut record: ServiceRecord,
        why: Option<&str>,
    ) {
        let key = (record.worktree.clone(), record.service.clone());
        let running = record.state.is_running() || record.process.is_some();
        if let Some(live) = self.live.lock().get_mut(&key) {
            live.stopping = true;
        }
        if running {
            if let (Some(spec), Some(ctx)) = (spec, ctx)
                && let Some(stop) = &spec.stop
            {
                let log = PathBuf::from(&record.log);
                match self.render_argv(
                    project,
                    ctx,
                    &format!("environment.service.{}.stop", spec.id),
                    stop,
                ) {
                    Ok(argv) => {
                        append_header(&log, &format!("stopping: {}", argv.join(" ")));
                        let inv = Invocation::new(
                            argv,
                            &worktree.path,
                            wtm_core::flow::env::STOP_TIMEOUT_MS,
                        );
                        let out = self.runner.run_allow_failure(&inv, &CancelToken::new());
                        append_output(&log, &out);
                    }
                    Err(e) => tracing::warn!(service = %key.1, error = %e, "stop did not render"),
                }
            }
            if let Some(process) = &record.process {
                match self.host.terminate(process) {
                    Termination::Ended | Termination::Gone => {}
                    Termination::NotOurs => tracing::warn!(
                        service = %key.1,
                        pid = process.pid,
                        "the recorded pid belongs to another process now; it was left alone"
                    ),
                }
            }
        }
        self.live.lock().remove(&key);
        if record.state != ServiceState::Stopped || record.process.is_some() {
            record.state = ServiceState::Stopped;
            record.process = None;
            record.detail = why.map(str::to_owned);
            record.updated_at = self.now();
            self.save(&record);
        }
    }

    /// Ask a running service once whether it is up.
    ///
    /// # Errors
    ///
    /// When the service cannot be found or its health check does not render.
    pub fn check(
        &self,
        project_id: &str,
        worktree_id: &str,
        service: &str,
    ) -> Result<EnvironmentView, String> {
        let (project, worktree) = self.resolve(project_id, worktree_id)?;
        let spec = Self::chosen(&project, Some(service))?[0];
        let key = (worktree_id.to_owned(), spec.id.clone());
        self.refresh_recovered(worktree_id);
        if self.record(&key).is_some_and(|r| r.state.is_running()) {
            let ctx = self.context(&project, &worktree);
            let probe = self.probe_of(&project, &worktree, &ctx, spec)?;
            match self.ask(&probe, PROBE_TIMEOUT_MS) {
                Ok(answer) => self.transition(&key, ServiceEvent::Answered, Some(answer), false),
                Err(why) => self.transition(&key, ServiceEvent::Unanswered, Some(why), false),
            }
        }
        self.view(project_id, worktree_id)
    }

    /// The preview address of a healthy service, fetched just now and answering.
    ///
    /// # Errors
    ///
    /// When the service is not healthy, declares no preview, its preview is not an address on this
    /// machine, or it did not answer: the window opens nothing it was not given.
    pub fn preview(
        &self,
        project_id: &str,
        worktree_id: &str,
        service: &str,
    ) -> Result<VerifiedPreview, String> {
        let (project, worktree) = self.resolve(project_id, worktree_id)?;
        let spec = Self::chosen(&project, Some(service))?[0];
        let key = (worktree_id.to_owned(), spec.id.clone());
        if self.record(&key).map(|r| r.state) != Some(ServiceState::Healthy) {
            return Err(format!(
                "`{}` is not healthy, so there is nothing to open",
                spec.id
            ));
        }
        let template = spec
            .preview
            .as_ref()
            .ok_or_else(|| format!("`{}` declares no preview", spec.id))?;
        let ctx = self.context(&project, &worktree);
        let rendered = self.render(&ctx, "preview", template)?;
        let url = loopback_url(&rendered).map_err(text)?;
        let (status, ms) = self.fetch(url.as_str(), PROBE_TIMEOUT_MS)?;
        if !healthy_status(status) {
            return Err(format!(
                "{} answered {status}, so it was not opened",
                url.as_str()
            ));
        }
        Ok(VerifiedPreview {
            url: url.as_str().to_owned(),
            status,
            ms,
        })
    }

    /// The end of a service's log.
    ///
    /// # Errors
    ///
    /// When the service cannot be found, or its log cannot be read.
    pub fn log(
        &self,
        project_id: &str,
        worktree_id: &str,
        service: &str,
    ) -> Result<ServiceLog, String> {
        let (project, worktree) = self.resolve(project_id, worktree_id)?;
        let spec = Self::chosen(&project, Some(service))?[0];
        let path = self.log_path(&worktree, &spec.id);
        let (text, truncated) = tail(&path, LOG_TAIL_BYTES)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        Ok(ServiceLog {
            path: path.to_string_lossy().into_owned(),
            text,
            truncated,
        })
    }

    // ═══════════════════════════════ commands ═══════════════════════════════

    /// The worktree's environment flow, made with its work item if it has none.
    fn environment_flow(&self, project: &Project, worktree: &Worktree) -> Result<FlowId, String> {
        let db = self.db()?;
        let id = worktree.id.as_str();
        if let Some(flow) = db.environment_flow(id).map_err(text)? {
            return Ok(flow);
        }
        let project_id = ProjectId::from_root(&project.root);
        let path = worktree.path.to_string_lossy().into_owned();
        let now = self.now();
        let item = if let Some(item) = db
            .work_item_for_worktree(&project_id, &path)
            .map_err(text)?
        {
            item
        } else {
            let title = worktree
                .branch()
                .map_or_else(|| worktree.dirname().to_owned(), |b| b.as_str().to_owned());
            let item = db
                .create_work_item(
                    &NewWorkItem {
                        project: project_id.clone(),
                        title,
                        issue: None,
                        pull_request: None,
                    },
                    now,
                )
                .map_err(text)?;
            db.link(item.id, &WorkLink::Worktree { path }, now)
                .map_err(text)?;
            item.id
        };
        let flow = match db.start_flow(item, FlowKind::Environment, now) {
            Ok(flow) => {
                db.record(
                    flow.id,
                    &FlowEvent::Advanced {
                        to: Phase::Operating,
                    },
                    now,
                )
                .map_err(text)?;
                flow.id
            }
            Err(FlowStoreError::FlowAlreadyOpen { existing }) => existing,
            Err(e) => return Err(text(e)),
        };
        db.bind_environment_flow(flow, &project_id, id)
            .map_err(text)?;
        Ok(flow)
    }

    /// Run one of the worktree's commands, or, for a destructive one on a shared resource, ask for
    /// your approval in the inbox.
    ///
    /// # Errors
    ///
    /// When the command is not declared, does not render, a guard forbids it, or another command
    /// is already waiting for your approval.
    pub fn run_command(
        &self,
        project_id: &str,
        worktree_id: &str,
        command: &str,
    ) -> Result<EnvironmentView, String> {
        let (project, worktree) = self.resolve(project_id, worktree_id)?;
        let env = &project.environment;
        let spec = env
            .command(command)
            .ok_or_else(|| format!("this project declares no command `{command}`"))?;
        let ctx = self.context(&project, &worktree);
        let argv = self.render_argv(
            &project,
            &ctx,
            &format!("environment.command.{}.run", spec.id),
            &spec.run,
        )?;
        let flow = self.environment_flow(&project, &worktree)?;
        let db = self.db()?;
        let binding = command_binding(&argv, &worktree.path.to_string_lossy());
        let asks = needs_approval(env, spec);
        let run = db
            .record_command_run(
                &NewCommandRun {
                    flow,
                    command: spec.id.clone(),
                    label: spec.label().to_owned(),
                    argv: spec.run.clone(),
                    binding,
                    state: if asks {
                        CommandRunState::AwaitingApproval
                    } else {
                        CommandRunState::Running
                    },
                },
                self.now(),
            )
            .map_err(|e| match e {
                FlowStoreError::CommandWaiting { existing } => format!(
                    "command run {existing} is already waiting for your approval in the inbox; \
                     approve or decline it first"
                ),
                other => text(other),
            })?;
        if asks {
            let shared = shared_labels(&project, spec);
            self.engine.file(&NewInboxItem {
                flow,
                kind: InboxKind::Approval,
                target: InboxTarget::Flow,
                title: format!("Run {} against {}?", spec.label(), shared.join(", ")),
                body: format!(
                    "`{}` in {}. It is destructive, and {} {} shared by every worktree, so it \
                     waits for your approval each time.",
                    spec.run.join(" "),
                    worktree.dirname(),
                    shared.join(", "),
                    if shared.len() == 1 { "is" } else { "are" },
                ),
            });
        } else {
            self.execute(
                run.id,
                flow,
                argv,
                worktree.path.clone(),
                spec.timeout_ms(),
                project_id,
                worktree_id,
            );
        }
        self.engine.notice.changed(flow);
        self.notice.changed(project_id, worktree_id);
        self.view(project_id, worktree_id)
    }

    /// Run an approved or approval-free command on a thread, and record how it ended.
    #[allow(clippy::too_many_arguments)]
    fn execute(
        &self,
        run: CommandRunId,
        flow: FlowId,
        argv: Vec<String>,
        cwd: PathBuf,
        timeout_ms: u64,
        project_id: &str,
        worktree_id: &str,
    ) {
        let Some(this) = self.myself.upgrade() else {
            return;
        };
        let (project_id, worktree_id) = (project_id.to_owned(), worktree_id.to_owned());
        let spawned = std::thread::Builder::new()
            .name(format!("env-command-{run}"))
            .spawn(move || {
                let inv = Invocation::new(argv, cwd, timeout_ms);
                let (state, exit_code, output) =
                    match this.runner.run_allow_failure(&inv, &CancelToken::new()) {
                        Ok(out) => (
                            if out.is_success() {
                                CommandRunState::Succeeded
                            } else {
                                CommandRunState::Failed
                            },
                            Some(out.code),
                            tail_chars(&format!("{}{}", out.stdout, out.stderr)),
                        ),
                        Err(e) => (CommandRunState::Failed, None, e.to_string()),
                    };
                if let Ok(db) = this.db() {
                    let _ = db.change_command_run(
                        run,
                        &CommandRunChange {
                            state: Some(state),
                            exit_code,
                            output: Some(output),
                            ..CommandRunChange::default()
                        },
                        this.now(),
                    );
                }
                this.engine.notice.changed(flow);
                this.notice.changed(&project_id, &worktree_id);
            });
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "could not run an environment command");
        }
    }

    /// The command waiting for your approval in `flow`, if one is.
    ///
    /// # Errors
    ///
    /// When the store cannot be read.
    pub fn pending(&self, flow: FlowId) -> Result<Option<PendingCommandView>, String> {
        let db = self.db()?;
        let Some(run) = db
            .command_runs(flow, RECENT_RUNS)
            .map_err(text)?
            .into_iter()
            .find(|r| r.state == CommandRunState::AwaitingApproval)
        else {
            return Ok(None);
        };
        let Some((project_id, worktree)) = db.environment_place(flow).map_err(text)? else {
            return Ok(None);
        };
        let shared = self
            .app
            .project(project_id.as_str())
            .ok()
            .and_then(|p| {
                p.environment
                    .command(&run.command)
                    .map(|spec| shared_labels(&p, spec))
            })
            .unwrap_or_default();
        Ok(Some(PendingCommandView {
            worktree_name: Path::new(&worktree)
                .file_name()
                .map_or_else(|| worktree.clone(), |n| n.to_string_lossy().into_owned()),
            run: run.into(),
            project: project_id.as_str().to_owned(),
            worktree,
            shared,
        }))
    }

    /// Approve one run of a waiting command, and run it, if what it would run now is exactly what
    /// was asked about.
    ///
    /// # Errors
    ///
    /// When it is not waiting, or what it would run changed since it asked, in which case it is
    /// marked stale and nothing runs.
    pub fn approve(&self, run_id: CommandRunId) -> Result<CommandRunView, String> {
        let db = self.db()?;
        let run = db
            .command_run(run_id)
            .map_err(text)?
            .ok_or_else(|| format!("no command run {run_id}"))?;
        if run.state != CommandRunState::AwaitingApproval {
            return Err(format!(
                "command run {run_id} is not waiting for an approval"
            ));
        }
        let (project_id, worktree_id) = db
            .environment_place(run.flow)
            .map_err(text)?
            .ok_or_else(|| format!("command run {run_id} has no worktree"))?;
        let stale = |why: String| -> Result<CommandRunView, String> {
            let _ = db.change_command_run(
                run_id,
                &CommandRunChange {
                    state: Some(CommandRunState::Stale),
                    output: Some(why.clone()),
                    ..CommandRunChange::default()
                },
                self.now(),
            );
            self.settle(run.flow, project_id.as_str(), &worktree_id);
            Err(why)
        };
        let (project, worktree) = match self.resolve(project_id.as_str(), &worktree_id) {
            Ok(found) => found,
            Err(e) => return stale(e),
        };
        let Some(spec) = project.environment.command(&run.command) else {
            return stale(format!(
                "`{}` is no longer declared; nothing ran",
                run.command
            ));
        };
        let ctx = self.context(&project, &worktree);
        let argv = match self.render_argv(
            &project,
            &ctx,
            &format!("environment.command.{}.run", spec.id),
            &spec.run,
        ) {
            Ok(argv) => argv,
            Err(e) => return stale(e),
        };
        let binding = command_binding(&argv, &worktree.path.to_string_lossy());
        if binding != run.binding {
            return stale(
                "what it would run changed since it asked (its config or this worktree's .env), \
                 so nothing ran; run it again to be asked about what it would run now"
                    .to_owned(),
            );
        }
        let request = CommandRequest {
            project: project_id.as_str().to_owned(),
            worktree: worktree_id.clone(),
            command: spec.id.clone(),
            label: spec.label().to_owned(),
            argv: spec.run.clone(),
            shared: shared_labels(&project, spec),
        };
        let approval = NewApproval::command(run.flow, &request, binding).map_err(text)?;
        let approved = db.approve(&approval, &[], self.now()).map_err(text)?;
        let running = db
            .change_command_run(
                run_id,
                &CommandRunChange {
                    state: Some(CommandRunState::Running),
                    approval: Some(approved.approval.id),
                    ..CommandRunChange::default()
                },
                self.now(),
            )
            .map_err(text)?;
        self.settle(run.flow, project_id.as_str(), &worktree_id);
        self.execute(
            run_id,
            run.flow,
            argv,
            worktree.path.clone(),
            spec.timeout_ms(),
            project_id.as_str(),
            &worktree_id,
        );
        Ok(running.into())
    }

    /// Decline a waiting command. Nothing runs.
    ///
    /// # Errors
    ///
    /// When it is not waiting.
    pub fn decline(&self, run_id: CommandRunId) -> Result<CommandRunView, String> {
        let db = self.db()?;
        let run = db
            .change_command_run(
                run_id,
                &CommandRunChange {
                    state: Some(CommandRunState::Declined),
                    ..CommandRunChange::default()
                },
                self.now(),
            )
            .map_err(text)?;
        if let Some((project, worktree)) = db.environment_place(run.flow).map_err(text)? {
            self.settle(run.flow, project.as_str(), &worktree);
        }
        Ok(run.into())
    }

    /// Close the approval's inbox item and tell both windows' listeners.
    fn settle(&self, flow: FlowId, project: &str, worktree: &str) {
        if let Ok(db) = self.db() {
            let _ = db.resolve_inbox(
                flow,
                InboxKind::Approval,
                Some(&InboxTarget::Flow),
                self.now(),
            );
        }
        self.engine.notice.changed(flow);
        self.notice.changed(project, worktree);
    }

    /// Opt a project into idle stop, or out of it.
    ///
    /// # Errors
    ///
    /// When the store cannot be written.
    pub fn set_idle_stop(&self, project_id: &str, minutes: Option<u32>) -> Result<(), String> {
        let project = ProjectId::from_root(Path::new(project_id));
        self.db()?.set_idle_stop(&project, minutes).map_err(text)
    }

    // ═══════════════════════════════ the app's life ═══════════════════════════════

    /// At launch: take back what the last run left running, and forget what it did not.
    ///
    /// - A service whose process is still running under its pid, with its start time, is adopted:
    ///   kept, asked once whether it is up, and listed as left running.
    /// - One whose pid is gone, or now belongs to something else, is recorded as stopped, and the
    ///   something else is left alone.
    /// - A detached service is asked once; the log follower the last run started is ended, if it
    ///   is still that process. If the service answers, it is listed as left running.
    ///
    /// Opens the database only if it exists. Returns the services it took back.
    ///
    /// # Errors
    ///
    /// When the store cannot be read.
    pub fn recover(&self) -> Result<Vec<String>, String> {
        if !self.app.files.exists(&self.app.config.paths().flow_db) {
            return Ok(Vec::new());
        }
        let db = self.db()?;
        let mut adopted = Vec::new();
        for mut record in db.services(None).map_err(text)? {
            if !record.state.is_running() && record.process.is_none() {
                continue;
            }
            let key = (record.worktree.clone(), record.service.clone());
            let found = record.process.as_ref().map(|p| {
                (
                    p.clone(),
                    wtm_core::flow::env::recover(p, self.host.identify(p.pid).as_ref()),
                )
            });
            if record.detached {
                if let Some((process, Recovery::Adopt)) = &found {
                    let _ = self.host.terminate(process);
                }
                record.process = None;
            } else {
                match found {
                    Some((_, Recovery::Adopt)) => {}
                    Some((_, recovery)) => {
                        record.state = ServiceState::Stopped;
                        record.process = None;
                        record.detail = Some(match recovery {
                            Recovery::Reused => "its process ended while wtm was not running, and \
                                its pid now belongs to another program, which was left alone"
                                .to_owned(),
                            _ => "it ended while wtm was not running".to_owned(),
                        });
                        record.updated_at = self.now();
                        self.save(&record);
                        continue;
                    }
                    None => {
                        record.state = ServiceState::Stopped;
                        record.updated_at = self.now();
                        self.save(&record);
                        continue;
                    }
                }
            }
            let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
            self.live.lock().insert(
                key.clone(),
                Live {
                    generation,
                    recovered: true,
                    ..Live::default()
                },
            );
            let answer = self.ask_recorded(&record);
            match answer {
                Some(Ok(answer)) => {
                    record.state = ServiceState::Healthy;
                    record.detail = Some(format!("left running when wtm last closed; {answer}"));
                }
                Some(Err(why)) if record.detached => {
                    self.live.lock().remove(&key);
                    record.state = ServiceState::Stopped;
                    record.detail = Some(format!("not answering since wtm last ran: {why}"));
                }
                Some(Err(why)) => {
                    record.state = ServiceState::Unhealthy;
                    record.detail = Some(format!("left running when wtm last closed; {why}"));
                }
                None => {}
            }
            record.updated_at = self.now();
            self.save(&record);
            if record.state.is_running() {
                adopted.push(format!("{}:{}", record.worktree, record.service));
            }
        }
        Ok(adopted)
    }

    /// Ask a recorded service its health question, reading its config again. `None` when the
    /// config or the worktree cannot be read: nothing is decided on a guess.
    fn ask_recorded(&self, record: &ServiceRecord) -> Option<Result<String, String>> {
        let (project, worktree) = self.resolve(&record.project, &record.worktree).ok()?;
        let spec = project.environment.service(&record.service)?;
        let ctx = self.context(&project, &worktree);
        let probe = self.probe_of(&project, &worktree, &ctx, spec).ok()?;
        Some(self.ask(&probe, PROBE_TIMEOUT_MS))
    }

    /// The app is quitting. Returns the process groups to end, verified as wtm's own, so the exit
    /// path ends them with everything else under one grace period; records them as stopped; and
    /// leaves detached services running, recorded, for the next launch to list.
    pub fn shutdown(&self) -> Vec<u32> {
        self.quitting.store(true, Ordering::SeqCst);
        self.focus.fetch_add(1, Ordering::SeqCst);
        let Some(db) = self.existing_db() else {
            return Vec::new();
        };
        let Ok(records) = db.services(None) else {
            return Vec::new();
        };
        let mut groups = Vec::new();
        for mut record in records {
            let Some(process) = record.process.clone() else {
                continue;
            };
            if self.host.identify(process.pid).as_ref() == Some(&process) {
                groups.push(process.pid);
            }
            record.process = None;
            if !record.detached {
                record.state = ServiceState::Stopped;
                record.detail = Some("stopped when wtm quit".to_owned());
            }
            record.updated_at = self.now();
            let _ = db.save_service(&record);
        }
        self.live.lock().clear();
        groups
    }

    /// The worktree is about to be removed: stop everything it runs, detached services too. Called
    /// only after removal's own checks passed, so a worktree that removal refuses keeps its
    /// services, and nothing here removes anything.
    pub fn stop_worktree(&self, worktree_id: &str) {
        let Some(db) = self.existing_db() else { return };
        let Ok(records) = db.services(Some(worktree_id)) else {
            return;
        };
        let running: Vec<ServiceRecord> = records
            .into_iter()
            .filter(|r| r.state.is_running() || r.process.is_some())
            .collect();
        let Some(first) = running.first() else {
            return;
        };
        let resolved = self.resolve(&first.project, worktree_id).ok();
        let ctx = resolved.as_ref().map(|(p, w)| self.context(p, w));
        for record in running {
            if let Some((project, worktree)) = &resolved {
                let spec = project.environment.service(&record.service);
                self.stop_one(
                    project,
                    worktree,
                    ctx.as_ref(),
                    spec,
                    record,
                    Some("stopped: the worktree was removed"),
                );
            } else {
                // Its config cannot be read now, so there is no `stop` to run: end what wtm holds,
                // verified as its own, and nothing else.
                if let Some(process) = &record.process {
                    let _ = self.host.terminate(process);
                }
                let key = (record.worktree.clone(), record.service.clone());
                self.live.lock().remove(&key);
                self.save(&ServiceRecord {
                    state: ServiceState::Stopped,
                    process: None,
                    detail: Some("stopped: the worktree was removed".to_owned()),
                    updated_at: self.now(),
                    ..record
                });
            }
        }
    }

    /// A window gained or lost focus. Losing it starts the idle clock for every project that opted
    /// in; gaining it, anywhere, stops every clock.
    ///
    /// One sleeping thread per blur, which wakes at each opted-in project's deadline and gives up
    /// the moment focus has moved since: a deadline, not a poll.
    pub fn focused(&self, focused: bool) {
        let generation = self.focus.fetch_add(1, Ordering::SeqCst) + 1;
        if focused || self.quitting.load(Ordering::SeqCst) {
            return;
        }
        let Some(this) = self.myself.upgrade() else {
            return;
        };
        let spawned = std::thread::Builder::new()
            .name("env-idle".to_owned())
            .spawn(move || this.idle(generation));
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "could not start the idle clock");
        }
    }

    fn idle(&self, generation: u64) {
        let Some(db) = self.existing_db() else { return };
        let projects: BTreeSet<String> = db
            .services(None)
            .unwrap_or_default()
            .into_iter()
            .filter(|r| r.state.is_running())
            .map(|r| r.project)
            .collect();
        let mut due: Vec<(u32, String)> = projects
            .into_iter()
            .filter_map(|project| {
                db.idle_stop(&ProjectId::from_root(Path::new(&project)))
                    .ok()
                    .flatten()
                    .map(|minutes| (minutes, project))
            })
            .collect();
        due.sort();
        let mut slept = 0u64;
        for (minutes, project) in due {
            let deadline = u64::from(minutes) * self.tuning.idle_minute_ms;
            std::thread::sleep(Duration::from_millis(deadline.saturating_sub(slept)));
            slept = deadline;
            if self.focus.load(Ordering::SeqCst) != generation {
                return;
            }
            let worktrees: BTreeSet<String> = db
                .services(None)
                .unwrap_or_default()
                .into_iter()
                .filter(|r| r.project == project && r.state.is_running())
                .map(|r| r.worktree)
                .collect();
            for worktree in worktrees {
                let lock = self.lock_for(&worktree);
                let _one = lock.lock();
                if let Ok((p, w)) = self.resolve(&project, &worktree) {
                    let ctx = self.context(&p, &w);
                    for record in db.services(Some(&worktree)).unwrap_or_default() {
                        if record.state.is_running() {
                            let spec = p.environment.service(&record.service);
                            self.stop_one(
                                &p,
                                &w,
                                Some(&ctx),
                                spec,
                                record,
                                Some(&format!(
                                    "stopped after {minutes} min with wtm in the background"
                                )),
                            );
                        }
                    }
                }
            }
        }
    }
}

// ═══════════════════════════════ helpers ═══════════════════════════════

/// Whether something listens on `port` on this machine.
///
/// A connect to each loopback address first, because it is the one test that is never wrong when
/// it succeeds: something accepted. Then a bind of the wildcard and of loopback, which catches a
/// listener the connect missed, such as one on another address of this machine. `std` sets
/// `SO_REUSEADDR` before a bind, which is why the bind alone would let a wildcard listener and a
/// loopback one coexist and miss it; the connect covers that.
#[must_use]
pub fn port_in_use(port: u16) -> bool {
    let timeout = Duration::from_millis(250);
    let connects = [
        SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
        SocketAddr::from((Ipv6Addr::LOCALHOST, port)),
    ];
    if connects
        .iter()
        .any(|addr| TcpStream::connect_timeout(addr, timeout).is_ok())
    {
        return true;
    }
    [
        SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)),
        SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
    ]
    .iter()
    .any(|addr| {
        matches!(TcpListener::bind(addr), Err(e) if e.kind() == std::io::ErrorKind::AddrInUse)
    })
}

/// A conflict in a sentence, for the service's detail.
fn conflict_sentence(conflicts: &[PortConflict]) -> String {
    conflicts
        .iter()
        .map(|c| {
            let who = c
                .owners
                .iter()
                .map(|o| match (&o.container, &o.service, o.pid) {
                    (Some(container), _, _) => format!("container `{container}`"),
                    (None, Some(ours), _) => {
                        format!("wtm's `{}` in {}", ours.service, ours.worktree)
                    }
                    (None, None, Some(pid)) => format!("`{}` (pid {pid})", o.command),
                    (None, None, None) => format!("`{}`", o.command),
                })
                .collect::<Vec<_>>();
            if who.is_empty() {
                format!(
                    "port {} is in use, by something lsof could not name",
                    c.port
                )
            } else {
                format!("port {} is in use by {}", c.port, who.join(" and "))
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn shared_labels(project: &Project, command: &EnvCommandSpec) -> Vec<String> {
    command
        .resources
        .iter()
        .filter_map(|id| project.environment.resource(id))
        .filter(|r| r.isolation == Isolation::Shared)
        .map(|r| format!("`{}`", r.label()))
        .collect()
}

fn start_failure(error: &ExecError) -> String {
    match error {
        ExecError::ProgramNotFound { program, .. } => {
            format!("`{program}` is not on the PATH wtm runs commands with")
        }
        other => other.to_string(),
    }
}

fn seconds(ms: u64) -> String {
    if ms.is_multiple_of(1000) {
        format!("{} s", ms / 1000)
    } else {
        format!("{}.{} s", ms / 1000, (ms % 1000) / 100)
    }
}

/// `curl`'s `time_total`, seconds with a fraction, as whole milliseconds. Read as text, so no float
/// is involved: `0.012345` is 12 ms.
fn seconds_text_to_ms(text: &str) -> u64 {
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    let millis: String = fraction.chars().chain("000".chars()).take(3).collect();
    whole.parse::<u64>().unwrap_or(0) * 1000 + millis.parse::<u64>().unwrap_or(0)
}

fn last_line(stderr: &str, stdout: &str) -> String {
    stderr
        .lines()
        .chain(stdout.lines())
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("no output")
        .trim()
        .to_owned()
}

fn tail_chars(text: &str) -> String {
    let count = text.chars().count();
    if count <= OUTPUT_TAIL_CHARS {
        return text.to_owned();
    }
    let skip = count - OUTPUT_TAIL_CHARS;
    format!("…{}", text.chars().skip(skip).collect::<String>())
}

fn append(log: &Path, text: &str) {
    if let Some(parent) = log.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
    {
        let _ = file.write_all(text.as_bytes());
    }
}

fn append_header(log: &Path, what: &str) {
    append(log, &format!("\n── wtm: {what} ──\n"));
}

fn append_output(log: &Path, out: &Result<wtm_core::ports::exec::Output, ExecError>) {
    match out {
        Ok(out) => append(log, &format!("{}{}", out.stdout, out.stderr)),
        Err(e) => append(log, &format!("{e}\n")),
    }
}

/// The last `max` bytes of a file, cut at a line start, and whether anything was cut.
fn tail(path: &Path, max: u64) -> std::io::Result<(String, bool)> {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok((String::new(), false));
        }
        Err(e) => return Err(e),
    };
    let len = file.metadata()?.len();
    let start = len.saturating_sub(max);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if start > 0
        && let Some(newline) = text.find('\n')
    {
        text.drain(..=newline);
    }
    Ok((text, start > 0))
}
