//! Checks that need services, and runtime roles, against a real service in a real worktree.
//!
//! What is real: the app, its config, a git worktree whose `.env` holds the ports, a small Python
//! server as the service, the supervisor that starts it and the `curl` that asks its health, the
//! SQLite store, the inbox and the approvals, and the run tokens and run tools. What is fake is the
//! agent sessions ([`FakeHost`]) and the browser pane, which needs a window: the host records the
//! pane the engine asked for and says which store it built it on.
//!
//! Each test is a promise someone would notice broken:
//! - a check whose service is not up is blocked, with the service named, and never run, passed or
//!   quietly skipped;
//! - asking is the default, and approving the start runs the check once its service answers, with
//!   which service answered where kept as evidence; a plan that changed since it was shown runs
//!   nothing;
//! - `auto`, only where your config says, starts the service and runs;
//! - a service that runs and does not answer blocks the check too: health, not a process, is the
//!   gate;
//! - a runtime role runs after the static roles, on the worktree, in an incognito pane on the
//!   verified preview, waits blocked for its environment when it has to, and says so when the
//!   worktree changed under it.
//!
//! Python is the one tool here a Mac might lack; without it each test says so and returns.

#![allow(clippy::unwrap_used, clippy::too_many_lines, clippy::print_stderr)]

mod common;

use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use wtm_app_lib::app::App;
use wtm_app_lib::flow::env::{EnvNotice, EnvTuning, Supervisor};
use wtm_app_lib::flow::host::{PaneStore, RoleHost};
use wtm_app_lib::flow::runtime::Environment;
use wtm_app_lib::flow::{Engine, FlowView, Notice, StartReview, Tuning};
use wtm_config::AppPaths;
use wtm_core::flow::{
    ApprovalSubject, BlockCause, CheckRun, CheckStatus, FlowId, Hold, InboxKind, InboxState,
    InboxTarget, Phase, ServiceRecord, ServiceState, ShotTaker, StepStatus,
};
use wtm_core::model::ProjectId;
use wtm_core::ports::clock::Clock;
use wtm_core::ports::service::ServiceHost;
use wtm_core::ports::{EnvStore, OutboxStore};
use wtm_testkit::GitFixture;

use common::agents::{Act, Behaviour, FakeHost, Heard, Script};

/// Answers `/healthz` with 200, or with 503 when started `sick`, and every other path with a page.
/// A plain `TCPServer`, for the reason `environment.rs` gives.
const SERVER: &str = r#"
import http.server, socketserver, sys
port, mode = int(sys.argv[1]), (sys.argv[2] if len(sys.argv) > 2 else "ok")
class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        code = 503 if (self.path == "/healthz" and mode == "sick") else 200
        self.send_response(code)
        self.end_headers()
        self.wfile.write(b"<h1>fixture</h1>")
    def log_message(self, *args):
        print("GET", self.path, flush=True)
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

[defaults.workflow.check.e2e]
label = "Browser tests"
run = ["sh", "-c", "touch ran-e2e && echo 3 passed"]
needs = ["web"]

[defaults.workflow.check.smoke]
run = ["sh", "-c", "touch ran-smoke"]
needs = ["web"]
start_services = "auto"

[defaults.workflow.check.sickly]
run = ["sh", "-c", "touch ran-sickly"]
needs = ["sick"]

[defaults.workflow.role.checker]
objective = "Run the checks"
provider = "codex"
checks = ["e2e", "smoke", "sickly"]
inputs = ["diff"]
timeout_ms = 60000

[[defaults.workflow.recipe.checks.stage]]
roles = ["checker"]

[defaults.workflow.role.conventions]
objective = "Read the change"
provider = "claude"
inputs = ["diff"]
timeout_ms = 60000

[defaults.workflow.role.clickthrough]
objective = "Click through the page the change touches"
provider = "claude"
kind = "runtime"
service = "web"
checks = ["e2e"]
inputs = ["diff"]
timeout_ms = 60000

[[defaults.workflow.recipe.runtime.stage]]
roles = ["conventions"]

[[defaults.workflow.recipe.runtime.stage]]
roles = ["clickthrough"]
"#;

fn finding(title: &str) -> Value {
    json!({ "findings": [{
        "title": title,
        "severity": "notable",
        "class": "suggestion",
        "body": "Seen on the page.",
    }]})
}

struct Harness {
    app: Arc<App>,
    host: Arc<FakeHost>,
    engine: Arc<Engine>,
    supervisor: Arc<Supervisor>,
    project: String,
    worktree: String,
    path: PathBuf,
    ports: [u16; 2],
    _fixture: GitFixture,
    _data: tempfile::TempDir,
}

struct Quiet;
impl EnvNotice for Quiet {
    fn changed(&self, _: &str, _: &str) {}
}

fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn harness(script: Script) -> Option<Harness> {
    harness_with(script, "")
}

fn harness_with(script: Script, extra_config: &str) -> Option<Harness> {
    let fixture = GitFixture::new();
    let data = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    std::fs::write(
        data.path().join("config.toml"),
        format!("{CONFIG}{extra_config}"),
    )
    .unwrap();
    let app = Arc::new(App::with_paths(AppPaths::rooted(data.path())).unwrap());
    if app.runner.which("python3").is_none() {
        eprintln!(
            "SKIPPED: python3 is not on the PATH, and the fixture's service is a Python server"
        );
        return None;
    }
    let root = app.register(fixture.root()).unwrap();
    let project = ProjectId::from_root(&root).to_string();
    let path = fixture.add_worktree("acme-rt", "feature/page");
    fixture.git_in(&path, &["config", "commit.gpgsign", "false"]);
    std::fs::write(path.join("server.py"), SERVER).unwrap();
    std::fs::write(path.join(".gitignore"), ".env\nran-*\n").unwrap();
    std::fs::write(path.join("page.html"), "<button>Save</button>\n").unwrap();
    fixture.git_in(&path, &["add", "."]);
    fixture.git_in(&path, &["commit", "-m", "Add the page"]);
    let ports = [free_port(), free_port()];
    std::fs::write(
        path.join(".env"),
        format!("WEB_PORT={}\nSICK_PORT={}\n", ports[0], ports[1]),
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

    let host = FakeHost::new(Arc::clone(&app), script);
    let heard = Arc::new(Heard::default());
    let engine = Engine::new(
        Arc::clone(&app),
        Arc::clone(&host) as Arc<dyn RoleHost>,
        Arc::clone(&app.clock) as Arc<dyn Clock>,
        Arc::clone(&heard) as Arc<dyn Notice>,
        Tuning {
            tick_ms: 5,
            skills_wait_ms: 30_000,
            settle_ms: 30_000,
            nudges: 1,
        },
    );
    *host.engine.lock() = Arc::downgrade(&engine);
    let supervisor = Supervisor::new(
        Arc::clone(&app),
        Arc::clone(&engine),
        Arc::clone(&app.launcher) as Arc<dyn ServiceHost>,
        Arc::clone(&app.runner),
        Arc::clone(&app.clock) as Arc<dyn Clock>,
        Arc::new(Quiet) as Arc<dyn EnvNotice>,
        EnvTuning::default(),
    );
    engine.attach_environment(&(Arc::clone(&supervisor) as Arc<dyn Environment>));
    Some(Harness {
        app,
        host,
        engine,
        supervisor,
        project,
        worktree,
        // As git and the app name it: `/var` is `/private/var` on macOS.
        path: path.canonicalize().unwrap(),
        ports,
        _fixture: fixture,
        _data: data,
    })
}

impl Drop for Harness {
    fn drop(&mut self) {
        // Every service a test started, stopped, so no Python server outlives its test.
        let _ = self.supervisor.stop(&self.project, &self.worktree, None);
    }
}

impl Harness {
    fn start(&self, recipe: &str) -> FlowId {
        self.engine
            .start(&StartReview {
                project: self.project.clone(),
                worktree: self.worktree.clone(),
                recipe: recipe.to_owned(),
                base: None,
            })
            .unwrap()
    }

    fn view(&self, flow: FlowId) -> FlowView {
        self.engine.view(flow).unwrap()
    }

    fn until(&self, flow: FlowId, what: &str, done: impl Fn(&FlowView) -> bool) -> FlowView {
        for _ in 0..3_000 {
            let view = self.view(flow);
            if done(&view) {
                return view;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("never {what}: {:#?}", self.view(flow));
    }

    fn consolidated(&self, flow: FlowId) -> FlowView {
        self.until(flow, "consolidated", |v| {
            v.flow.state.phase == Phase::Consolidated && !v.running
        })
    }

    fn checks_of<'a>(view: &'a FlowView, check: &str) -> Vec<&'a CheckRun> {
        view.checks.iter().filter(|c| c.check == check).collect()
    }

    fn record(&self, service: &str) -> Option<ServiceRecord> {
        self.app
            .appdb
            .open()
            .unwrap()
            .services(Some(&self.worktree))
            .unwrap()
            .into_iter()
            .find(|r| r.service == service)
    }

    fn start_service(&self, service: &str, settled: ServiceState) {
        self.supervisor
            .start(&self.project, &self.worktree, Some(service))
            .unwrap();
        for _ in 0..400 {
            if self.record(service).is_some_and(|r| r.state == settled) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!(
            "`{service}` never became {settled:?}: {:?}",
            self.record(service)
        );
    }

    fn answer_containing(&self, needle: &str) -> String {
        self.host
            .answers
            .lock()
            .iter()
            .map(|(_, _, text)| text.clone())
            .find(|text| text.contains(needle))
            .unwrap_or_else(|| panic!("no run tool answered with {needle:?}"))
    }
}

// ═══════════════════════════════ checks that need services ═══════════════════════════════

#[test]
fn a_check_whose_service_is_stopped_is_blocked_with_the_service_named_and_never_run() {
    let Some(h) = harness(Box::new(|_, _| {
        Behaviour::CheckThenSubmit(vec!["e2e"], finding("checked"))
    })) else {
        return;
    };
    let flow = h.start("checks");
    let view = h.consolidated(flow);

    let e2e = Harness::checks_of(&view, "e2e");
    assert_eq!(e2e.len(), 1);
    assert_eq!(e2e[0].status, CheckStatus::Blocked);
    assert_eq!(e2e[0].exit_code, None);
    let detail = e2e[0].detail.as_deref().unwrap();
    assert!(
        detail.starts_with("blocked: environment not started") && detail.contains("`web`"),
        "{detail}"
    );
    assert_eq!(e2e[0].services.len(), 1);
    assert_eq!(e2e[0].services[0].state, ServiceState::Stopped);
    assert!(
        !h.path.join("ran-e2e").exists()
            && !PathBuf::from(&view.invocations[0].cwd)
                .join("ran-e2e")
                .exists(),
        "a blocked check is never started"
    );

    let told = h.answer_containing("`e2e` was not run");
    assert!(told.contains("neither passed nor failed"), "{told}");

    // Asking is the default: one approval, about the run, says what it would start and run.
    let asked: Vec<_> = view
        .inbox
        .iter()
        .filter(|i| i.kind == InboxKind::Approval && i.target == InboxTarget::Flow)
        .collect();
    assert_eq!(asked.len(), 1, "{:#?}", view.inbox);
    assert!(asked[0].title.contains("web"), "{}", asked[0].title);
    let start = view.pending_start.as_ref().unwrap();
    assert_eq!(start.request.services[0].service, "web");
    assert_eq!(start.request.checks[0].check, "e2e");
    assert!(start.binding.starts_with("sha256:"), "{}", start.binding);
    assert!(h.record("web").is_none(), "asking starts nothing");
}

#[test]
fn approving_the_start_runs_the_blocked_check_once_its_service_answers_and_keeps_where_it_answered()
{
    let Some(h) = harness(Box::new(|_, _| {
        Behaviour::CheckThenSubmit(vec!["e2e"], finding("checked"))
    })) else {
        return;
    };
    let flow = h.start("checks");
    let view = h.consolidated(flow);
    let start = view.pending_start.clone().unwrap();

    h.engine.start_environment(flow, &start.binding).unwrap();
    let view = h.until(flow, "the blocked check ran", |v| {
        Harness::checks_of(v, "e2e").len() == 2
    });

    let ran = Harness::checks_of(&view, "e2e")[1];
    assert_eq!(ran.status, CheckStatus::Passed, "{ran:#?}");
    assert_eq!(ran.exit_code, Some(0));
    assert_eq!(
        ran.cwd,
        h.path.to_string_lossy(),
        "it runs where its service runs"
    );
    assert!(h.path.join("ran-e2e").exists());
    let web = &ran.services[0];
    assert_eq!(web.state, ServiceState::Healthy);
    assert_eq!(
        web.url.as_deref(),
        Some(format!("http://localhost:{}/healthz", h.ports[0]).as_str())
    );
    assert!(
        web.detail.as_deref().unwrap().starts_with("answered 200"),
        "{web:?}"
    );
    // The first record stays as it was: it was blocked, and that is history.
    assert_eq!(
        Harness::checks_of(&view, "e2e")[0].status,
        CheckStatus::Blocked
    );
    assert!(view.pending_start.is_none(), "nothing is blocked any more");

    let db = h.app.appdb.open().unwrap();
    let approvals = db.approvals(flow).unwrap();
    assert_eq!(approvals.len(), 1);
    let approved = &approvals[0];
    assert!(
        matches!(&approved.subject, ApprovalSubject::Services(request) if request.checks[0].check == "e2e"),
        "{:?}",
        approved.subject
    );
    let item = view
        .inbox
        .iter()
        .find(|i| i.kind == InboxKind::Approval)
        .map(|i| i.state);
    assert_ne!(item, Some(InboxState::Open), "approving settles the item");
}

#[test]
fn an_approval_of_a_start_that_changed_since_it_was_shown_runs_nothing() {
    let Some(h) = harness(Box::new(|_, _| {
        Behaviour::CheckThenSubmit(vec!["e2e"], finding("checked"))
    })) else {
        return;
    };
    let flow = h.start("checks");
    let shown = h.consolidated(flow).pending_start.unwrap();

    // The port the start would use moves after you looked.
    std::fs::write(
        h.path.join(".env"),
        format!("WEB_PORT={}\nSICK_PORT={}\n", free_port(), h.ports[1]),
    )
    .unwrap();
    let refused = h
        .engine
        .start_environment(flow, &shown.binding)
        .unwrap_err();
    assert!(refused.contains("changed since you looked"), "{refused}");
    std::thread::sleep(Duration::from_millis(300));
    assert!(h.record("web").is_none(), "nothing was started");
    assert!(
        h.app
            .appdb
            .open()
            .unwrap()
            .approvals(flow)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn declining_the_start_leaves_the_check_blocked_and_starts_nothing() {
    let Some(h) = harness(Box::new(|_, _| {
        Behaviour::CheckThenSubmit(vec!["e2e"], finding("checked"))
    })) else {
        return;
    };
    let flow = h.start("checks");
    h.consolidated(flow);
    h.engine.decline_environment(flow).unwrap();
    let view = h.view(flow);
    assert_eq!(
        Harness::checks_of(&view, "e2e")
            .iter()
            .map(|c| c.status)
            .collect::<Vec<_>>(),
        [CheckStatus::Blocked]
    );
    assert!(
        !view
            .inbox
            .iter()
            .any(|i| i.kind == InboxKind::Approval && i.state == InboxState::Open)
    );
    assert!(h.record("web").is_none());
}

#[test]
fn an_auto_check_starts_its_service_waits_for_its_health_and_then_runs() {
    let Some(h) = harness(Box::new(|_, _| {
        Behaviour::CheckThenSubmit(vec!["smoke"], finding("checked"))
    })) else {
        return;
    };
    let flow = h.start("checks");
    let view = h.consolidated(flow);

    let smoke = Harness::checks_of(&view, "smoke");
    assert_eq!(smoke.len(), 1);
    assert_eq!(smoke[0].status, CheckStatus::Passed, "{:#?}", smoke[0]);
    assert_eq!(smoke[0].services[0].state, ServiceState::Healthy);
    assert_eq!(h.record("web").unwrap().state, ServiceState::Healthy);
    assert!(
        !view.inbox.iter().any(|i| i.kind == InboxKind::Approval),
        "a check your config lets start its services asks nothing"
    );
}

#[test]
fn a_check_whose_service_runs_but_does_not_answer_is_blocked_because_health_is_the_gate() {
    let Some(h) = harness(Box::new(|_, _| {
        Behaviour::CheckThenSubmit(vec!["sickly"], finding("checked"))
    })) else {
        return;
    };
    h.start_service("sick", ServiceState::Unhealthy);
    assert!(h.record("sick").unwrap().process.is_some(), "it is running");

    let flow = h.start("checks");
    let view = h.consolidated(flow);
    let sickly = Harness::checks_of(&view, "sickly")[0];
    assert_eq!(sickly.status, CheckStatus::Blocked);
    let detail = sickly.detail.as_deref().unwrap();
    assert!(
        detail.contains("`sick` is not answering") && detail.contains("503"),
        "{detail}"
    );
    assert!(!h.path.join("ran-sickly").exists());
}

// ═══════════════════════════════ runtime roles ═══════════════════════════════

#[test]
fn the_runtime_role_runs_after_the_static_roles_on_the_worktree_in_an_incognito_pane_on_the_verified_preview()
 {
    let Some(h) = harness(Box::new(|_, _| Behaviour::Submit(vec![finding("seen")]))) else {
        return;
    };
    h.start_service("web", ServiceState::Healthy);
    let flow = h.start("runtime");
    let view = h.consolidated(flow);

    assert_eq!(h.host.opened_roles(), ["conventions", "clickthrough"]);
    let opened = h.host.opened.lock().clone();
    let runtime = &opened[1];
    assert_eq!(
        runtime.cwd.as_deref(),
        Some(h.path.as_path()),
        "it reads the worktree"
    );
    assert!(opened[0].browser.is_none(), "a static role drives no pane");

    let panes = h.host.panes.lock().clone();
    assert_eq!(panes.len(), 1);
    assert_eq!(panes[0].url, format!("http://localhost:{}/", h.ports[0]));
    assert_eq!(
        panes[0].sessions_open, 0,
        "the pane is opened before its session"
    );
    assert!(panes[0].closed, "and closed when the role is done");
    assert_eq!(runtime.browser.as_deref(), Some(panes[0].id.as_str()));
    assert!(
        runtime.brief.contains(&panes[0].url) && runtime.brief.contains("private store"),
        "{}",
        runtime.brief
    );

    let shots = &view.screenshots;
    assert_eq!(shots.len(), 1, "{shots:#?}");
    assert_eq!(shots[0].taken_by, ShotTaker::Wtm);
    assert_eq!(shots[0].role.as_deref(), Some("clickthrough"));
    assert!(
        runtime.brief.contains(&shots[0].id.label()),
        "{}",
        runtime.brief
    );
    let step = view
        .steps
        .iter()
        .find(|s| s.role == "clickthrough")
        .unwrap();
    assert_eq!(step.status, StepStatus::Done);
}

#[test]
fn a_runtime_pane_built_on_the_persistent_jar_is_refused_and_closed_before_any_session() {
    let Some(h) = harness(Box::new(|_, _| Behaviour::Submit(vec![finding("seen")]))) else {
        return;
    };
    *h.host.pane_store.lock() = PaneStore::Persistent;
    h.start_service("web", ServiceState::Healthy);
    let flow = h.start("runtime");
    let view = h.consolidated(flow);

    assert_eq!(
        h.host.opened_roles(),
        ["conventions"],
        "no session drove it"
    );
    assert!(h.host.panes.lock().iter().all(|p| p.closed));
    let step = view
        .steps
        .iter()
        .find(|s| s.role == "clickthrough")
        .unwrap();
    assert_eq!(step.status, StepStatus::Failed);
    let why = &view.invocations.last().unwrap().detail;
    assert!(
        why.as_deref()
            .is_some_and(|d| d.contains("not an incognito store")),
        "{why:?}"
    );
}

#[test]
fn a_runtime_role_waits_blocked_for_its_environment_until_you_approve_starting_it() {
    let Some(h) = harness(Box::new(|_, _| Behaviour::Submit(vec![finding("seen")]))) else {
        return;
    };
    let flow = h.start("runtime");
    let waiting = h.until(flow, "held for its environment", |v| {
        matches!(
            v.flow.state.hold,
            Some(Hold::Blocked {
                cause: BlockCause::Env
            })
        )
    });
    let step = waiting
        .steps
        .iter()
        .find(|s| s.role == "clickthrough")
        .unwrap();
    assert_eq!(step.status, StepStatus::Pending);
    assert!(
        step.detail
            .as_deref()
            .unwrap()
            .starts_with("blocked: environment not started"),
        "{:?}",
        step.detail
    );
    assert_eq!(h.host.opened_roles(), ["conventions"], "it has not started");
    let start = waiting.pending_start.clone().unwrap();
    assert_eq!(start.request.roles, ["clickthrough"]);
    assert!(waiting.running, "the run waits rather than ending");

    h.engine.start_environment(flow, &start.binding).unwrap();
    let view = h.consolidated(flow);
    assert_eq!(h.host.opened_roles(), ["conventions", "clickthrough"]);
    assert!(view.flow.state.hold.is_none());
    assert_eq!(h.record("web").unwrap().state, ServiceState::Healthy);
}

#[test]
fn a_runtime_role_whose_worktree_changed_while_it_ran_says_so_and_its_findings_read_stale() {
    let Some(h) = harness(Box::new(|role, _| {
        if role == "clickthrough" {
            Behaviour::Acts(vec![
                Act::Write("page.html", "<button disabled>Save</button>\n"),
                Act::Findings(finding("the Save button does nothing")),
            ])
        } else {
            Behaviour::Submit(vec![json!({ "findings": [] })])
        }
    })) else {
        return;
    };
    h.start_service("web", ServiceState::Healthy);
    let flow = h.start("runtime");
    let view = h.consolidated(flow);

    let step = view
        .steps
        .iter()
        .find(|s| s.role == "clickthrough")
        .unwrap();
    assert_eq!(step.status, StepStatus::Done);
    let detail = step.detail.as_deref().unwrap_or_default();
    assert!(
        detail.contains("the worktree changed while it ran") && detail.contains("page.html"),
        "{detail}"
    );
    let stale = h.engine.staleness(flow).unwrap();
    assert_eq!(view.findings.len(), 1);
    assert_eq!(stale.stale_findings, [view.findings[0].finding.id]);
}

#[test]
fn with_the_projects_playwright_a_runtime_role_runs_its_checks_and_gets_no_pane() {
    let Some(h) = harness(Box::new(|role, _| {
        if role == "clickthrough" {
            Behaviour::CheckThenSubmit(vec!["e2e"], finding("e2e ran"))
        } else {
            Behaviour::Submit(vec![json!({ "findings": [] })])
        }
    })) else {
        return;
    };
    std::fs::write(
        h.path.join("package.json"),
        r#"{"devDependencies": {"@playwright/test": "^1.48.0"}}"#,
    )
    .unwrap();
    h.start_service("web", ServiceState::Healthy);
    let flow = h.start("runtime");
    let view = h.consolidated(flow);

    assert!(
        h.host.panes.lock().is_empty(),
        "Playwright drives its own browser"
    );
    let opened = h.host.opened.lock().clone();
    assert!(opened[1].browser.is_none());
    assert!(
        opened[1].brief.contains("Playwright"),
        "{}",
        opened[1].brief
    );
    let e2e = Harness::checks_of(&view, "e2e");
    assert_eq!(e2e[0].status, CheckStatus::Passed, "{:#?}", e2e[0]);
}
