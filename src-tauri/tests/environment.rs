//! A worktree's environment, end to end, against real processes and real ports.
//!
//! What is real: the app and its config store, a git repository with a worktree whose `.env`
//! holds the port, a small Python HTTP server as the service, `ps`, `lsof` and `curl` as the
//! supervisor runs them, the SQLite store, and the inbox and approvals the review engine owns.
//! Nothing here is faked except agent sessions, which an environment never opens.
//!
//! Each test is a promise someone would notice broken:
//! - a service that starts, answers, shows its log and opens only an address that answered;
//! - a port someone else holds refusing the start, naming the holder, and leaving it alone;
//! - health that is honest: a service that does not answer is unhealthy, one that exits is failed;
//! - a process recognised after a relaunch only while its pid and start time both match, and a
//!   stranger under a reused pid never signalled;
//! - quitting ending what wtm holds while leaving a detached service for the next launch to list;
//! - a destructive command on a shared resource waiting for an approval each time, running once per
//!   approval, and refusing an approval of something that has changed since;
//! - idle stop stopping only after the time you chose, and never once you came back.
//!
//! Python is the one tool here a Mac might lack; without it each test says so and returns.

#![allow(clippy::unwrap_used, clippy::too_many_lines, clippy::print_stderr)]

use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use wtm_agent::session::AgentSink;
use wtm_app_lib::app::App;
use wtm_app_lib::flow::env::{EnvNotice, EnvTuning, EnvironmentView, Supervisor, port_in_use};
use wtm_app_lib::flow::host::RoleHost;
use wtm_app_lib::flow::{Engine, Notice, Refusal, Tuning};
use wtm_config::AppPaths;
use wtm_core::flow::{
    ApprovalIntent, CommandRunState, FlowId, InboxItem, InboxKind, ProcessIdentity, ServiceRecord,
    ServiceState,
};
use wtm_core::model::{ProjectId, SessionId};
use wtm_core::ports::clock::Clock;
use wtm_core::ports::service::ServiceHost;
use wtm_core::ports::{EnvStore, InboxStore, OutboxStore};
use wtm_testkit::GitFixture;

// ═══════════════════════════════ the fixture ═══════════════════════════════

/// A server that answers `/healthz` with 200, or with 503 when started `sick`, and prints a line
/// per request so its log has something in it.
///
/// A plain `TCPServer`, not `http.server.HTTPServer`: that one's `server_bind` asks DNS for the
/// host's name between `bind` and `listen`, and on a CI runner whose reverse lookups time out the
/// server was not listening for longer than any health deadline here, which read as a broken
/// supervisor.
const SERVER: &str = r#"
import http.server, socketserver, sys
port, mode = int(sys.argv[1]), (sys.argv[2] if len(sys.argv) > 2 else "ok")
class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        code = 503 if (self.path == "/healthz" and mode == "sick") else 200
        self.send_response(code)
        self.end_headers()
        self.wfile.write(b"hello from the fixture")
    def log_message(self, *args):
        print("GET", self.path, flush=True)
print("listening on", port, flush=True)
class Server(socketserver.TCPServer):
    allow_reuse_address = True
Server(("127.0.0.1", port), Handler).serve_forever()
"#;

const CONFIG: &str = r#"
[[defaults.display.source]]
id = "env"
kind = "dotenv"
path = "{{ worktree.path }}/.env"

[[defaults.environment.service]]
id = "web"
start = ["python3", "server.py", "{{ env.WEB_PORT }}"]
ports = ["{{ env.WEB_PORT }}"]
health = { url = "http://localhost:{{ env.WEB_PORT }}/healthz", timeout_ms = 8000 }
preview = "http://localhost:{{ env.WEB_PORT }}/"

[[defaults.environment.service]]
id = "sick"
start = ["python3", "server.py", "{{ env.SICK_PORT }}", "sick"]
ports = ["{{ env.SICK_PORT }}"]
health = { url = "http://localhost:{{ env.SICK_PORT }}/healthz", timeout_ms = 1500 }

[[defaults.environment.service]]
id = "crash"
start = ["sh", "-c", "echo boom; exit 4"]
health = { command = ["false"], timeout_ms = 5000 }

[[defaults.environment.service]]
id = "box"
detached = true
start = ["sh", "-c", "python3 server.py {{ env.BOX_PORT }} > box.out 2>&1 & echo $! > box.pid"]
stop = ["sh", "-c", "kill $(cat box.pid)"]
ports = ["{{ env.BOX_PORT }}"]
health = { url = "http://127.0.0.1:{{ env.BOX_PORT }}/healthz", timeout_ms = 8000 }

[[defaults.environment.resource]]
id = "db"
label = "shared database"
isolation = "shared"

[[defaults.environment.resource]]
id = "scratch"
isolation = "per_worktree"

[[defaults.environment.command]]
id = "reset"
label = "Reset the database"
run = ["sh", "-c", "echo reset {{ env.DB_NAME }} >> resets.log"]
destructive = true
resources = ["db"]

[[defaults.environment.command]]
id = "wipe"
run = ["sh", "-c", "echo wiped >> wipes.log"]
destructive = true
resources = ["scratch"]
"#;

/// An engine with no agents: an environment opens no sessions.
struct NoRoles;

impl RoleHost for NoRoles {
    fn preflight(&self, _: &[&str]) -> Result<(), Refusal> {
        Ok(())
    }
    fn open(
        &self,
        _: &wtm_app_lib::flow_session::RoleSession,
        _: Arc<dyn AgentSink>,
    ) -> Result<SessionId, String> {
        Err("no agents in an environment test".to_owned())
    }
    fn send(&self, _: &SessionId, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn close(&self, _: &SessionId) {}
}

#[derive(Default)]
struct Heard {
    filed: Mutex<Vec<InboxItem>>,
    changes: Mutex<usize>,
}

impl Notice for Heard {
    fn changed(&self, _: FlowId) {}
    fn filed(&self, item: &InboxItem) {
        self.filed.lock().push(item.clone());
    }
}

impl EnvNotice for Heard {
    fn changed(&self, _: &str, _: &str) {
        *self.changes.lock() += 1;
    }
}

struct Harness {
    app: Arc<App>,
    engine: Arc<Engine>,
    heard: Arc<Heard>,
    supervisor: Arc<Supervisor>,
    project: String,
    worktree: String,
    path: PathBuf,
    ports: [u16; 3],
    _fixture: GitFixture,
    _data: tempfile::TempDir,
}

/// A port nothing listens on right now.
fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn harness() -> Option<Harness> {
    harness_with(EnvTuning::default())
}

fn harness_with(tuning: EnvTuning) -> Option<Harness> {
    let fixture = GitFixture::new();
    let data = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    std::fs::write(data.path().join("config.toml"), CONFIG).unwrap();
    let app = Arc::new(App::with_paths(AppPaths::rooted(data.path())).unwrap());
    if app.runner.which("python3").is_none() {
        eprintln!(
            "SKIPPED: python3 is not on the PATH, and the fixture's service is a Python server"
        );
        return None;
    }
    let root = app.register(fixture.root()).unwrap();
    let project = ProjectId::from_root(&root).to_string();
    let path = fixture.add_worktree("acme-env", "feature/env");
    let ports = [free_port(), free_port(), free_port()];
    std::fs::write(path.join("server.py"), SERVER).unwrap();
    std::fs::write(
        path.join(".env"),
        format!(
            "WEB_PORT={}\nSICK_PORT={}\nBOX_PORT={}\nDB_NAME=app_one\n",
            ports[0], ports[1], ports[2]
        ),
    )
    .unwrap();
    let worktree = app
        .git
        .list_worktrees(&root)
        .unwrap()
        .into_iter()
        .find(|w| !w.is_main)
        .unwrap()
        .id
        .as_str()
        .to_owned();
    let heard = Arc::new(Heard::default());
    let engine = Engine::new(
        Arc::clone(&app),
        Arc::new(NoRoles),
        Arc::clone(&app.clock),
        Arc::clone(&heard) as Arc<dyn Notice>,
        Tuning::default(),
    );
    let supervisor = supervisor_for(&app, &engine, &heard, tuning);
    Some(Harness {
        app,
        engine,
        heard,
        supervisor,
        project,
        worktree,
        path,
        ports,
        _fixture: fixture,
        _data: data,
    })
}

fn supervisor_for(
    app: &Arc<App>,
    engine: &Arc<Engine>,
    heard: &Arc<Heard>,
    tuning: EnvTuning,
) -> Arc<Supervisor> {
    Supervisor::new(
        Arc::clone(app),
        Arc::clone(engine),
        Arc::clone(&app.launcher) as Arc<dyn ServiceHost>,
        Arc::clone(&app.runner),
        Arc::clone(&app.clock) as Arc<dyn Clock>,
        Arc::clone(heard) as Arc<dyn EnvNotice>,
        tuning,
    )
}

/// Wait for `ok`, for at most twenty seconds.
fn until(what: &str, mut ok: impl FnMut() -> bool) {
    for _ in 0..400 {
        if ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out waiting for {what}");
}

impl Harness {
    fn view(&self) -> EnvironmentView {
        self.supervisor.view(&self.project, &self.worktree).unwrap()
    }

    fn state(&self, service: &str) -> ServiceState {
        self.service(service).state
    }

    fn service(&self, service: &str) -> wtm_app_lib::flow::env::ServiceView {
        self.view()
            .services
            .into_iter()
            .find(|s| s.id == service)
            .unwrap()
    }

    fn start(&self, service: &str) -> Result<EnvironmentView, String> {
        self.supervisor
            .start(&self.project, &self.worktree, Some(service))
    }

    fn stop(&self, service: &str) {
        self.supervisor
            .stop(&self.project, &self.worktree, Some(service))
            .unwrap();
    }

    /// Wait on the store rather than the view: a view lists the worktrees with `git` each time,
    /// and four hundred of those on a loaded CI runner took longer than the wait itself.
    fn wait_for(&self, service: &str, state: ServiceState) {
        for _ in 0..400 {
            if self.record(service).state == state {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!(
            "timed out waiting for `{service}` to be {state:?}; it is {:?}",
            self.record(service)
        );
    }

    fn db(&self) -> Arc<wtm_appdb::AppDb> {
        self.app.appdb.open().unwrap()
    }

    fn record(&self, service: &str) -> ServiceRecord {
        self.db()
            .services(Some(&self.worktree))
            .unwrap()
            .into_iter()
            .find(|r| r.service == service)
            .unwrap()
    }
}

// ═══════════════════════════════ services ═══════════════════════════════

#[test]
fn a_service_starts_becomes_healthy_logs_what_it_prints_opens_only_a_verified_preview_and_stops() {
    let Some(h) = harness() else { return };
    assert_eq!(h.state("web"), ServiceState::Stopped);
    assert_eq!(
        h.service("web").ports[0].port,
        Some(h.ports[0]),
        "read from .env"
    );

    h.start("web").unwrap();
    h.wait_for("web", ServiceState::Healthy);
    let web = h.service("web");
    assert!(
        web.detail.as_deref().unwrap().starts_with("answered 200"),
        "{web:?}"
    );
    let pid = web
        .pid
        .expect("an attached service has a process wtm holds");

    let preview = h
        .supervisor
        .preview(&h.project, &h.worktree, "web")
        .unwrap();
    assert_eq!(preview.url, format!("http://localhost:{}/", h.ports[0]));
    assert_eq!(preview.status, 200);

    let log = h.supervisor.log(&h.project, &h.worktree, "web").unwrap();
    assert!(log.text.contains("listening on"), "{}", log.text);
    assert!(log.text.contains("GET /healthz"), "{}", log.text);
    assert!(
        !log.path.starts_with(&*h.path.to_string_lossy()),
        "logs live outside the worktree, where `git clean` cannot reach them"
    );

    h.stop("web");
    assert_eq!(h.state("web"), ServiceState::Stopped);
    until("the server's process to end", || {
        !wtm_exec::signal::group_alive(pid)
    });
    assert!(!port_in_use(h.ports[0]), "its port is free again");
    assert!(
        h.supervisor
            .preview(&h.project, &h.worktree, "web")
            .is_err(),
        "a stopped service has nothing to open"
    );
}

#[test]
fn a_port_held_by_another_process_refuses_the_start_and_names_its_owner_without_ending_it() {
    let Some(h) = harness() else { return };
    // This test's own process holds the port, as another worktree's server or your own terminal
    // would.
    let squatter = TcpListener::bind(("127.0.0.1", h.ports[0])).unwrap();

    let refused = h.start("web").unwrap_err();
    assert!(
        refused.contains(&format!("port {} is in use by", h.ports[0])),
        "{refused}"
    );
    let web = h.service("web");
    assert_eq!(web.state, ServiceState::Stopped, "nothing was started");
    assert_eq!(web.pid, None);
    let owners = &web.conflicts[0].owners;
    assert!(
        owners.iter().any(|o| o.pid == Some(std::process::id())),
        "lsof names this test as the holder: {owners:?}"
    );

    // Left alone: the holder still has its port, and still takes connections on it.
    assert!(port_in_use(h.ports[0]));
    squatter.set_nonblocking(true).unwrap();
    assert!(
        squatter.accept().is_ok(),
        "the probe's own connection is waiting in the holder's queue"
    );
}

#[test]
fn a_service_that_does_not_answer_in_time_is_unhealthy_and_one_that_exits_is_failed_with_its_log() {
    let Some(h) = harness() else { return };

    h.start("sick").unwrap();
    assert_eq!(h.state("sick"), ServiceState::Starting);
    h.wait_for("sick", ServiceState::Unhealthy);
    let sick = h.service("sick");
    assert!(
        sick.detail.as_deref().unwrap().contains("answered 503"),
        "{sick:?}"
    );
    assert!(sick.pid.is_some(), "unhealthy is still running");
    h.stop("sick");

    h.start("crash").unwrap();
    h.wait_for("crash", ServiceState::Failed);
    let crash = h.service("crash");
    assert_eq!(crash.detail.as_deref(), Some("it exited with code 4"));
    assert_eq!(crash.pid, None);
    let log = h.supervisor.log(&h.project, &h.worktree, "crash").unwrap();
    assert!(
        log.text.contains("boom"),
        "a failed start shows its log: {}",
        log.text
    );
}

#[test]
fn a_recorded_process_is_adopted_after_a_relaunch_only_while_its_pid_and_start_time_match() {
    let Some(h) = harness() else { return };
    h.start("web").unwrap();
    h.wait_for("web", ServiceState::Healthy);
    let pid = h.service("web").pid.unwrap();

    // A crash: the supervisor that started it is gone without stopping anything, and a new launch
    // opens the same database.
    let relaunched = supervisor_for(&h.app, &h.engine, &h.heard, EnvTuning::default());
    let adopted = relaunched.recover().unwrap();
    assert_eq!(adopted, [format!("{}:web", h.worktree)]);
    let view = relaunched.view(&h.project, &h.worktree).unwrap();
    let web = view.services.iter().find(|s| s.id == "web").unwrap();
    assert_eq!(web.state, ServiceState::Healthy);
    assert!(web.left_running);
    assert_eq!(web.pid, Some(pid));

    relaunched
        .stop(&h.project, &h.worktree, Some("web"))
        .unwrap();
    until("the adopted server to end", || {
        !wtm_exec::signal::group_alive(pid)
    });
}

#[test]
fn a_stranger_under_a_recorded_pid_is_never_signalled_and_the_service_is_recorded_as_stopped() {
    let Some(h) = harness() else { return };
    // Some other program, running under a pid wtm once recorded for `web`, which started at a
    // different time than the record says.
    let stranger = h
        .app
        .launcher
        .launch(
            &wtm_core::ports::exec::Invocation::new(
                vec!["sleep".to_owned(), "300".to_owned()],
                &h.path,
                1,
            ),
            &h.path.join("stranger.log"),
            Box::new(|_| {}),
        )
        .unwrap();
    h.db()
        .save_service(&ServiceRecord {
            project: h.project.clone(),
            worktree: h.worktree.clone(),
            service: "web".to_owned(),
            state: ServiceState::Healthy,
            process: Some(ProcessIdentity {
                pid: stranger.pid,
                started: "Thu Jan  1 00:00:00 1970".to_owned(),
            }),
            detached: false,
            log: "/nowhere".to_owned(),
            detail: None,
            started_at: Some(1),
            updated_at: 1,
        })
        .unwrap();

    assert!(h.supervisor.recover().unwrap().is_empty());
    let web = h.record("web");
    assert_eq!(web.state, ServiceState::Stopped);
    assert!(web.detail.unwrap().contains("left alone"));
    assert!(
        wtm_exec::signal::process_alive(stranger.pid),
        "the stranger was not signalled"
    );

    // And Stop on that record ends nothing either.
    h.stop("web");
    assert!(wtm_exec::signal::process_alive(stranger.pid));
    assert_eq!(
        h.app.launcher.terminate(&stranger),
        wtm_core::ports::Termination::Ended
    );
}

#[test]
fn quitting_ends_what_wtm_holds_and_leaves_a_detached_service_for_the_next_launch_to_list() {
    let Some(h) = harness() else { return };
    h.start("web").unwrap();
    h.start("box").unwrap();
    h.wait_for("web", ServiceState::Healthy);
    h.wait_for("box", ServiceState::Healthy);
    let web = h.service("web").pid.unwrap();
    let box_pid: i32 = std::fs::read_to_string(h.path.join("box.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert_eq!(
        h.service("box").pid,
        None,
        "wtm holds no process for a detached service"
    );

    let groups = h.supervisor.shutdown();
    assert_eq!(groups, [web], "only what wtm holds, verified");
    wtm_exec::signal::terminate_groups(&groups);
    assert!(!wtm_exec::signal::group_alive(web));
    assert_eq!(h.record("web").state, ServiceState::Stopped);
    assert_eq!(h.record("box").state, ServiceState::Healthy, "left running");
    assert!(port_in_use(h.ports[2]), "the detached server is still up");

    let next = supervisor_for(&h.app, &h.engine, &h.heard, EnvTuning::default());
    assert_eq!(next.recover().unwrap(), [format!("{}:box", h.worktree)]);
    let listed = next.view(&h.project, &h.worktree).unwrap();
    let boxed = listed.services.iter().find(|s| s.id == "box").unwrap();
    assert!(boxed.left_running, "listed on the next launch");
    assert!(
        boxed
            .detail
            .as_deref()
            .unwrap()
            .starts_with("left running when wtm last closed"),
        "{boxed:?}"
    );

    next.stop(&h.project, &h.worktree, Some("box")).unwrap();
    until(
        "the detached server to be stopped by its own `stop`",
        || !wtm_exec::signal::process_alive(u32::try_from(box_pid).unwrap()),
    );
}

#[test]
fn removing_a_worktree_stops_everything_it_runs_detached_services_too() {
    let Some(h) = harness() else { return };
    h.start("web").unwrap();
    h.start("box").unwrap();
    h.wait_for("web", ServiceState::Healthy);
    h.wait_for("box", ServiceState::Healthy);

    h.supervisor.stop_worktree(&h.worktree);
    assert_eq!(h.record("web").state, ServiceState::Stopped);
    assert_eq!(h.record("box").state, ServiceState::Stopped);
    until("both ports to be free", || {
        !port_in_use(h.ports[0]) && !port_in_use(h.ports[2])
    });
    assert!(h.path.exists(), "stopping services removes nothing");
}

/// A host whose process is gone before `launch` returns: the exit is reported on the reaping thread,
/// and nothing says that thread waits for the caller. The worst case, made certain.
struct ExitsAtOnce;

impl ServiceHost for ExitsAtOnce {
    fn launch(
        &self,
        _: &wtm_core::ports::exec::Invocation,
        _: &std::path::Path,
        on_exit: wtm_core::ports::OnExit,
    ) -> Result<ProcessIdentity, wtm_core::error::ExecError> {
        on_exit(wtm_core::model::ExitOutcome::Failed { code: 4 });
        Ok(ProcessIdentity {
            pid: 999_999,
            started: "gone already".to_owned(),
        })
    }
    fn identify(&self, _: u32) -> Option<ProcessIdentity> {
        None
    }
    fn terminate(&self, _: &ProcessIdentity) -> wtm_core::ports::Termination {
        wtm_core::ports::Termination::Gone
    }
}

#[test]
fn a_service_that_exits_before_its_start_has_returned_is_failed_and_never_left_starting() {
    let Some(h) = harness() else { return };
    let supervisor = Supervisor::new(
        Arc::clone(&h.app),
        Arc::clone(&h.engine),
        Arc::new(ExitsAtOnce),
        Arc::clone(&h.app.runner),
        Arc::clone(&h.app.clock) as Arc<dyn Clock>,
        Arc::clone(&h.heard) as Arc<dyn EnvNotice>,
        EnvTuning::default(),
    );
    supervisor
        .start(&h.project, &h.worktree, Some("crash"))
        .unwrap();
    let crash = h.record("crash");
    assert_eq!(crash.state, ServiceState::Failed);
    assert_eq!(crash.detail.as_deref(), Some("it exited with code 4"));
    assert_eq!(
        crash.process, None,
        "a process that is gone is not recorded as held"
    );
}

// ═══════════════════════════════ commands ═══════════════════════════════

#[test]
fn a_destructive_command_on_a_shared_resource_waits_for_an_approval_each_time_and_runs_once_per_approval()
 {
    let Some(h) = harness() else { return };
    let resets = h.path.join("resets.log");

    let view = h
        .supervisor
        .run_command(&h.project, &h.worktree, "reset")
        .unwrap();
    let run = &view.runs[0];
    assert_eq!(run.state, CommandRunState::AwaitingApproval);
    assert!(!resets.exists(), "nothing runs before the approval");
    let filed = h.heard.filed.lock().clone();
    assert_eq!(filed.len(), 1);
    assert_eq!(filed[0].kind, InboxKind::Approval);
    assert_eq!(filed[0].worktree.as_deref(), Some(h.worktree.as_str()));
    assert!(
        filed[0].title.contains("shared database"),
        "{}",
        filed[0].title
    );

    let again = h
        .supervisor
        .run_command(&h.project, &h.worktree, "reset")
        .unwrap_err();
    assert!(again.contains("already waiting"), "{again}");

    let pending = h.supervisor.pending(run.flow).unwrap().unwrap();
    assert_eq!(pending.run.id, run.id);
    assert_eq!(pending.shared, ["`shared database`"]);
    assert!(
        pending
            .run
            .argv
            .iter()
            .any(|w| w.contains("{{ env.DB_NAME }}")),
        "the approval shows the declared command, never a value from .env"
    );

    h.supervisor.approve(run.id).unwrap();
    until("the approved run to finish", || {
        h.view().runs[0].state == CommandRunState::Succeeded
    });
    assert_eq!(std::fs::read_to_string(&resets).unwrap(), "reset app_one\n");
    let approvals = h.db().approvals(run.flow).unwrap();
    assert_eq!(approvals.len(), 1);
    assert_eq!(approvals[0].approval.intent, ApprovalIntent::Command);
    assert!(
        h.db().inbox(Some(run.flow), None).unwrap().is_empty(),
        "the approval closed its inbox item"
    );
    assert!(
        h.supervisor.approve(run.id).is_err(),
        "an approval covers one run"
    );

    // The next run asks again, and a no runs nothing.
    let view = h
        .supervisor
        .run_command(&h.project, &h.worktree, "reset")
        .unwrap();
    assert_eq!(view.runs[0].state, CommandRunState::AwaitingApproval);
    h.supervisor.decline(view.runs[0].id).unwrap();
    assert_eq!(h.view().runs[0].state, CommandRunState::Declined);
    assert_eq!(std::fs::read_to_string(&resets).unwrap(), "reset app_one\n");
}

#[test]
fn an_approval_of_a_command_whose_values_changed_since_it_asked_runs_nothing() {
    let Some(h) = harness() else { return };
    let view = h
        .supervisor
        .run_command(&h.project, &h.worktree, "reset")
        .unwrap();
    // The worktree's .env now names another database than the one you were asked about.
    let env = std::fs::read_to_string(h.path.join(".env"))
        .unwrap()
        .replace("app_one", "app_two");
    std::fs::write(h.path.join(".env"), env).unwrap();

    let refused = h.supervisor.approve(view.runs[0].id).unwrap_err();
    assert!(refused.contains("changed since it asked"), "{refused}");
    assert_eq!(h.view().runs[0].state, CommandRunState::Stale);
    assert!(!h.path.join("resets.log").exists());
    assert!(h.db().approvals(view.runs[0].flow).unwrap().is_empty());
}

#[test]
fn a_destructive_command_on_this_worktrees_own_resource_runs_without_asking() {
    let Some(h) = harness() else { return };
    h.supervisor
        .run_command(&h.project, &h.worktree, "wipe")
        .unwrap();
    until("the run to finish", || {
        h.view().runs[0].state == CommandRunState::Succeeded
    });
    assert!(h.heard.filed.lock().is_empty(), "nothing to approve");
    assert_eq!(
        std::fs::read_to_string(h.path.join("wipes.log")).unwrap(),
        "wiped\n"
    );
}

#[test]
fn a_command_a_quit_cut_off_is_said_to_have_stopped_at_the_next_launch_rather_than_left_running() {
    let Some(h) = harness() else { return };
    h.supervisor
        .run_command(&h.project, &h.worktree, "wipe")
        .unwrap();
    until("the run to finish", || {
        h.view().runs[0].state == CommandRunState::Succeeded
    });
    // Another run of it was under way when wtm went away: the record still says running.
    let db = h.app.appdb.get().unwrap();
    let first = &h.view().runs[0];
    let flow = wtm_core::ports::EnvStore::command_run(db.as_ref(), first.id)
        .unwrap()
        .unwrap()
        .flow;
    let cut = wtm_core::ports::EnvStore::record_command_run(
        db.as_ref(),
        &wtm_core::flow::NewCommandRun {
            flow,
            command: "wipe".to_owned(),
            label: "Wipe".to_owned(),
            argv: vec!["true".to_owned()],
            binding: wtm_core::flow::Binding::default(),
            state: CommandRunState::Running,
        },
        1,
    )
    .unwrap();

    let next = supervisor_for(&h.app, &h.engine, &h.heard, EnvTuning::default());
    next.recover().unwrap();
    let after = wtm_core::ports::EnvStore::command_run(db.as_ref(), cut.id)
        .unwrap()
        .unwrap();
    assert_eq!(after.state, CommandRunState::Failed);
    assert!(
        after
            .output
            .as_deref()
            .is_some_and(|o| o.contains("whether it finished is not known")),
        "{after:?}"
    );
}

// ═══════════════════════════════ idle stop ═══════════════════════════════

#[test]
fn idle_stop_stops_a_projects_services_after_the_time_you_chose_and_never_once_you_came_back() {
    let Some(h) = harness_with(EnvTuning {
        idle_minute_ms: 300,
    }) else {
        return;
    };
    h.start("web").unwrap();
    h.wait_for("web", ServiceState::Healthy);

    // Off unless opted into.
    h.supervisor.focused(false);
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(h.state("web"), ServiceState::Healthy);

    h.supervisor.set_idle_stop(&h.project, Some(1)).unwrap();
    // Away, and back before the minute is up.
    h.supervisor.focused(false);
    std::thread::sleep(Duration::from_millis(100));
    h.supervisor.focused(true);
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(h.state("web"), ServiceState::Healthy);

    // Away for good.
    h.supervisor.focused(false);
    h.wait_for("web", ServiceState::Stopped);
    assert!(
        h.service("web")
            .detail
            .unwrap()
            .contains("in the background")
    );
    assert!(h.path.exists());
}

#[test]
fn an_unknown_service_or_command_is_refused_by_name() {
    let Some(h) = harness() else { return };
    assert!(h.start("nope").unwrap_err().contains("no service `nope`"));
    assert!(
        h.supervisor
            .run_command(&h.project, &h.worktree, "nope")
            .unwrap_err()
            .contains("no command `nope`")
    );
}

#[test]
fn focus_quit_and_removal_create_no_database_for_someone_who_never_used_the_work_view() {
    let Some(h) = harness() else { return };
    let flow_db = h.app.config.paths().flow_db.clone();
    assert!(!flow_db.exists(), "nothing has asked for it yet");

    h.supervisor.focused(false);
    h.supervisor.focused(true);
    h.supervisor.stop_worktree(&h.worktree);
    assert!(h.supervisor.shutdown().is_empty());
    std::thread::sleep(Duration::from_millis(200));
    assert!(!flow_db.exists(), "{} was created", flow_db.display());
}
