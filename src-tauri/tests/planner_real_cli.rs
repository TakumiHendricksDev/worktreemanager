//! A planner's question and the resume of its conversation, against the real Claude and Codex
//! CLIs.
//!
//! `planning.rs` holds the engine to the rules with fake agents. What only the real CLIs can show is
//! that the machinery meets them: that a run session under the read-only profile can ask with
//! `flow_ask`, that the engine's interrupt really ends a turn the CLI is in the middle of, that the
//! session can be closed with the question open, and that a new session asked to continue the
//! conversation — `--resume` for Claude, `thread/resume` for Codex — reports the same provider
//! conversation and carries on from the answer to a plan that passes the schema.
//!
//! It spends real turns on your plans — a canary turn for Claude, and at least two turns per
//! provider — so it runs only with `WTM_REAL_CLI_TURNS=1`, and says so otherwise. It skips a
//! provider whose CLI is not installed, saying so, and refuses to run on an API key exactly as the
//! app does (F3's preflight).
//!
//! The run's bridge is the built `wtm` binary, and the socket it calls home on is this test's own:
//! the test binary is not a bridge, and the socket in your config directory belongs to whatever wtm
//! you have running. The tracker is a fake: nothing here reads or writes a real one.

#![allow(clippy::unwrap_used, clippy::print_stderr, clippy::too_many_lines)]

use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::Duration;

use wtm_app_lib::app::{App, WORK_VIEW_PREF};
use wtm_app_lib::flow::host::HeadlessHost;
use wtm_app_lib::flow::planner::PlanView;
use wtm_app_lib::flow::start::{StartOutcome, StartWork};
use wtm_app_lib::flow::{Engine, Notice, Tuning};
use wtm_app_lib::flow_session::BridgeEndpoint;
use wtm_app_lib::handoff::{Action, Request, Response};
use wtm_app_lib::tracker;
use wtm_config::AppPaths;
use wtm_core::flow::{
    FlowId, Hold, InboxItem, Issue, IssueStatus, Phase, Sprint, SprintState, StatusCategory,
};
use wtm_core::model::ProjectId;
use wtm_core::ports::config::{ConfigStore, TrustDecision};
use wtm_core::ports::{Tracker, TrackerWriter};
use wtm_core::usecase::SyncReason;
use wtm_testkit::{FakeTracker, GitFixture};

const CONFIG: &str = r#"
[[field]]
key = "issue"
label = "Issue"
kind = "text"
required = true
pattern = "^[A-Z]+-[0-9]+$"

[[field]]
key = "base"
label = "Base branch"
kind = "text"
required = true
default = "main"

[naming]
branch = "feature/{{ issue }}"
directory = "{{ issue }}"
branch_must_match = "^[A-Za-z0-9][A-Za-z0-9._/-]*$"
"#;

const KEY: &str = "ACME-7";

/// What makes the issue ambiguous on purpose, and the instruction that makes asking about it the
/// role's first move rather than the model's judgement call, so the test is about the machinery.
const INSTRUCTIONS: &str = "This issue is deliberately ambiguous about the export format. Before \
     you plan, ask exactly one question with `flow_ask`: whether the export is CSV or XLSX, with \
     options [\"CSV\", \"XLSX\"]. Do not decide it yourself. After the answer arrives, keep the \
     investigation short and hand in a small plan.";

fn workflow(root: &str, provider: &str) -> String {
    let model = match provider {
        "claude" => "model = \"sonnet\"\n",
        _ => "effort = \"low\"\n",
    };
    format!(
        "\n[projects.\"{root}\".config.workflow]\n\
         tracker = \"jira\"\n\
         planner = \"planner\"\n\
         [projects.\"{root}\".config.workflow.statuses]\n\
         todo = \"To Do\"\n\
         started = \"In Progress\"\n\
         [projects.\"{root}\".config.workflow.queue]\n\
         query = \"assignee = currentUser()\"\n\
         board = 3\n\
         [projects.\"{root}\".config.workflow.role.planner]\n\
         objective = \"Investigate the issue and plan its implementation\"\n\
         instructions = {INSTRUCTIONS:?}\n\
         provider = \"{provider}\"\n\
         {model}\
         inputs = [\"issue\"]\n\
         output = \"plan\"\n\
         timeout_ms = 600000\n",
    )
}

fn installed(app: &App, provider: &str) -> bool {
    let entry = wtm_agent::entry(provider).unwrap();
    let found = app.agent_executable(entry).is_some();
    if !found {
        eprintln!(
            "skipping {}: wtm cannot find its CLI here, so its question and resume were not run",
            entry.label
        );
    }
    found
}

/// A socket of the test's own, answering run tool calls with the engine, as the app's does.
fn serve(socket: &Path, app: Arc<App>, engine: Weak<Engine>) {
    let listener = UnixListener::bind(socket).unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let app = Arc::clone(&app);
            let engine = engine.clone();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    return;
                }
                let response = match serde_json::from_str::<Request>(&line) {
                    Ok(request) => match (request.action, &request.flow) {
                        (Action::Flow, Some(call)) => wtm_app_lib::flow_tools::answer_with(
                            &app,
                            engine.upgrade().as_deref(),
                            &request.token,
                            call,
                        ),
                        _ => Response::failed("this test's socket answers run tools only"),
                    },
                    Err(_) => Response::failed("that request was not understood"),
                };
                let mut stream = stream;
                let _ = stream.write_all(serde_json::to_string(&response).unwrap().as_bytes());
                let _ = stream.write_all(b"\n");
            });
        }
    });
}

struct Printed;

impl Notice for Printed {
    fn changed(&self, _flow: FlowId) {}
    fn filed(&self, item: &InboxItem) {
        eprintln!("inbox: {:?} {}", item.kind, item.title);
    }
}

fn wait(engine: &Engine, flow: FlowId, what: &str, done: impl Fn(&PlanView) -> bool) -> PlanView {
    // Ten minutes: a canary turn, an investigation, and room for a slow provider.
    for _ in 0..600 {
        let view = engine.plan_view(flow).unwrap();
        let failed = view
            .step
            .as_ref()
            .is_some_and(|s| s.status == wtm_core::flow::StepStatus::Failed)
            || view.flow.state.end.is_some();
        if (!view.running && done(&view)) || failed {
            return view;
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    panic!("the planner never {what}");
}

/// Ask, close, answer, resume: one provider, end to end.
fn question_and_resume(provider: &str) {
    let fixture = GitFixture::new();
    fixture.commit("wtm.toml", CONFIG, "add wtm config");
    fixture.commit(
        "src/export.py",
        "def export(units):\n    \"\"\"Write the unit list somewhere.\"\"\"\n    return units\n",
        "an export stub",
    );
    // Under the target directory: a canary target under a temp root proves nothing and is refused.
    let data = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let paths = AppPaths::rooted(data.path());
    let config_file = paths.config_file.clone();
    let mut app = App::with_paths(paths).unwrap();
    if !installed(&app, provider) {
        return;
    }
    let tracker = Arc::new(
        FakeTracker::new()
            .with_sprint(Sprint {
                id: 41,
                name: "Sprint 41".to_owned(),
                state: SprintState::Active,
                goal: None,
                starts_at: None,
                ends_at: None,
                board: Some(3),
            })
            .with_issue(
                Issue {
                    key: KEY.to_owned(),
                    summary: "Let people export the unit list".to_owned(),
                    status: IssueStatus {
                        name: "To Do".to_owned(),
                        category: StatusCategory::ToDo,
                    },
                    kind: Some("Story".to_owned()),
                    priority: None,
                    assignee: None,
                    labels: Vec::new(),
                },
                &[41],
            ),
    );
    tracker.describe(
        KEY,
        "People want to download the unit list as a spreadsheet from src/export.py.",
    );
    app.tracker = Arc::clone(&tracker) as Arc<dyn Tracker>;
    app.tracker_writer = Arc::clone(&tracker) as Arc<dyn TrackerWriter>;
    let root = app.register(fixture.root()).unwrap();
    app.config
        .set_trust(&root.join("wtm.toml"), TrustDecision::Approve)
        .unwrap();
    app.config.set_user_pref(WORK_VIEW_PREF, "on").unwrap();
    let mut text = std::fs::read_to_string(&config_file).unwrap_or_default();
    text.write_str(&workflow(&root.display().to_string(), provider))
        .unwrap();
    std::fs::write(&config_file, text).unwrap();
    let app = Arc::new(app);
    let project = ProjectId::from_root(&root).to_string();
    tracker::sync(&app, &project, SyncReason::Demand).unwrap();

    // A short path: a Unix socket's path is limited to about a hundred bytes.
    let socket_dir = tempfile::Builder::new()
        .prefix("wtm-pl")
        .tempdir_in(std::env::temp_dir())
        .unwrap();
    let socket = socket_dir.path().join("h.sock");
    let host = HeadlessHost::new(
        Arc::clone(&app),
        BridgeEndpoint {
            program: PathBuf::from(env!("CARGO_BIN_EXE_wtm")),
            socket: socket.clone(),
        },
    );
    let engine = Engine::new(
        Arc::clone(&app),
        Arc::new(host),
        Arc::clone(&app.clock),
        Arc::new(Printed),
        Tuning::default(),
    );
    serve(&socket, Arc::clone(&app), Arc::downgrade(&engine));

    let flow = match engine
        .start_work(&StartWork {
            project: project.clone(),
            key: KEY.to_owned(),
            choice: None,
            by_drop: false,
            confirmed: false,
        })
        .unwrap_or_else(|refusal| panic!("{provider}: the start was refused: {refusal}"))
    {
        StartOutcome::Started { flow, .. } => flow,
        other => panic!("{provider}: expected a start, got {other:?}"),
    };

    let asked = wait(&engine, flow, "asked", |v| {
        matches!(v.flow.state.hold, Some(Hold::AwaitingAnswer))
    });
    for session in &asked.sessions {
        eprintln!(
            "{provider} session {}: {:?} conversation {:?} ({})",
            session.attempt,
            session.status,
            session.provider_session,
            session.detail.as_deref().unwrap_or("")
        );
    }
    assert!(
        matches!(asked.flow.state.hold, Some(Hold::AwaitingAnswer)),
        "{provider}: the planner did not ask: {:?} {:?}",
        asked.flow.state,
        asked.step.as_ref().and_then(|s| s.detail.clone())
    );
    let question = asked.questions[0].question.clone();
    eprintln!(
        "{provider} asked: {} (options {:?})",
        question.text, question.options
    );
    let first = asked.sessions.last().unwrap().clone();
    let conversation = first
        .provider_session
        .clone()
        .unwrap_or_else(|| panic!("{provider} reported no conversation id"));
    let live = first
        .session
        .as_deref()
        .is_some_and(|session| app.with_agent(session, |_| Ok(())).is_ok());
    assert!(
        !live,
        "{provider}: the asking session was closed, not left waiting"
    );

    let choice = question
        .options
        .iter()
        .position(|o| o.eq_ignore_ascii_case("CSV"));
    engine
        .answer(question.id, choice, "CSV, in the list's column order.")
        .unwrap();
    let planned = wait(&engine, flow, "planned after the answer", |v| {
        v.flow.state.phase == Phase::Planned
    });
    for session in &planned.sessions {
        eprintln!(
            "{provider} session {}: {:?} conversation {:?} ({})",
            session.attempt,
            session.status,
            session.provider_session,
            session.detail.as_deref().unwrap_or("")
        );
    }
    assert_eq!(
        planned.flow.state.phase,
        Phase::Planned,
        "{provider}: {:?} {:?}",
        planned.flow.state,
        planned.step.as_ref().and_then(|s| s.detail.clone())
    );
    let resumed = &planned.sessions[asked.sessions.len()];
    assert_ne!(resumed.session, first.session, "{provider}: a new session");
    assert_eq!(
        resumed.provider_session.as_deref(),
        Some(conversation.as_str()),
        "{provider}: the answer continued the same provider conversation"
    );
    let plan = &planned.plans[0].plan;
    eprintln!("{provider} planned: {}", plan.summary);
    assert!(
        tracker.comments_sent().is_empty(),
        "{provider}: nothing was posted to the tracker"
    );
}

#[test]
fn a_real_claude_planner_asks_is_closed_and_the_answer_resumes_its_conversation() {
    if std::env::var("WTM_REAL_CLI_TURNS").as_deref() != Ok("1") {
        eprintln!(
            "skipping the real Claude planner: it spends model turns on your Claude plan. Set \
             WTM_REAL_CLI_TURNS=1 to run it."
        );
        return;
    }
    question_and_resume("claude");
}

#[test]
fn a_real_codex_planner_asks_is_closed_and_the_answer_resumes_its_thread() {
    if std::env::var("WTM_REAL_CLI_TURNS").as_deref() != Ok("1") {
        eprintln!(
            "skipping the real Codex planner: it spends model turns on your Codex plan. Set \
             WTM_REAL_CLI_TURNS=1 to run it."
        );
        return;
    }
    question_and_resume("codex");
}
