//! A review recipe against the real CLIs: a Claude conventions role and a Codex correctness role,
//! configured independently, each with a skill the fixture repository ships.
//!
//! The engine's logic is covered by `review_engine.rs` with fake agents. What only the real CLIs can
//! show is that the pieces meet: that a run session's locked-down profile still finds the skill its
//! role names (Claude's settings sources are off, so wtm hands it the skill through a plugin of its
//! own; Codex finds a repository skill under the run's own home by itself), that the bridge the CLI
//! starts reaches the engine, and that what each model hands back passes `flow_submit`'s schema.
//!
//! Two tests, for the same reason `flow_session.rs` splits its canaries:
//!
//! - **The skill lists** need no model turn. They run whenever a CLI is installed, and skip, saying
//!   so, when one is not.
//! - **The review itself** spends real turns on your plans — two reviews and Claude's canary turn —
//!   so it runs only with `WTM_REAL_CLI_TURNS=1`, and says so otherwise. It uses the CLIs' own
//!   sign-ins, refuses to run on an API key exactly as the app does (F3's preflight), and prints
//!   what each role found.
//!
//! The run's bridge is the built `wtm` binary, and the socket it calls home on is this test's own:
//! the test binary is not a bridge, and the socket in your config directory belongs to whatever wtm
//! you have running.

#![allow(clippy::unwrap_used, clippy::print_stderr, clippy::too_many_lines)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

use parking_lot::Mutex;
use wtm_agent::authority::Authority;
use wtm_agent::session::AgentSink;
use wtm_app_lib::app::{App, FlowRole};
use wtm_app_lib::flow::host::HeadlessHost;
use wtm_app_lib::flow::{Engine, FlowView, Notice, StartReview, Tuning};
use wtm_app_lib::flow_session::{self, BridgeEndpoint, RoleSession};
use wtm_app_lib::handoff::{Action, Request, Response};
use wtm_config::AppPaths;
use wtm_core::flow::{FlowId, InboxItem, Phase, StepStatus, Submission};
use wtm_core::model::{AgentEvent, ExitOutcome, ProjectId, SessionId};
use wtm_testkit::GitFixture;

/// The conventions skill, as a repository writes one for Claude.
const CONVENTIONS_SKILL: &str = "---
name: fixture-conventions
description: Check the change against this repository's naming conventions.
---
Functions in this repository are named in snake_case. Report every function the change adds or
renames whose name is not snake_case, with its file and line.
";

/// The correctness skill, where Codex looks for a repository's skills.
const CORRECTNESS_SKILL: &str = "---
name: fixture-correctness
description: Look for off-by-one errors in the change.
---
Read each loop and slice the change adds. Report any that reads one element too many or too few,
with its file and line, and say what you read that confirms it.
";

/// A change with one naming problem and one off-by-one, so each role has something to find.
const CHANGE: &str = "def sumFirst(values, n):
    \"\"\"Return the sum of the first n values.\"\"\"
    total = 0
    for i in range(n + 1):
        total += values[i]
    return total
";

const RECIPE: &str = r#"
[defaults.workflow.role.conventions]
objective = "Check the change against this repository's naming conventions"
provider = "claude"
model = "sonnet"
skills = ["fixture-conventions"]
inputs = ["diff", "instructions"]
timeout_ms = 600000

[defaults.workflow.role.correctness]
objective = "Find bugs the change introduces"
provider = "codex"
effort = "low"
skills = ["/fixture-correctness"]
inputs = ["diff"]
timeout_ms = 600000

[[defaults.workflow.recipe.standard.stage]]
roles = ["conventions", "correctness"]
"#;

struct Harness {
    app: Arc<App>,
    project: String,
    worktree: String,
    worktree_path: PathBuf,
    _fixture: GitFixture,
    _data: tempfile::TempDir,
}

fn harness() -> Harness {
    let fixture = GitFixture::new();
    for (path, body) in [
        (
            ".claude/skills/fixture-conventions/SKILL.md",
            CONVENTIONS_SKILL,
        ),
        (
            ".agents/skills/fixture-correctness/SKILL.md",
            CORRECTNESS_SKILL,
        ),
    ] {
        fixture.write(path, body);
    }
    fixture.git(&["add", "."]);
    fixture.git(&["commit", "-m", "Add the repository's review skills"]);

    // Under the target directory: a canary target under a temp root proves nothing and is refused.
    let data = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    std::fs::write(data.path().join("config.toml"), RECIPE).unwrap();
    let app = Arc::new(App::with_paths(AppPaths::rooted(data.path())).unwrap());
    let root = app.register(fixture.root()).unwrap();
    let project = ProjectId::from_root(&root).to_string();
    let worktree_path = fixture.add_worktree("sums-wt", "feature/sums");
    std::fs::create_dir_all(worktree_path.join("src")).unwrap();
    std::fs::write(worktree_path.join("src/sums.py"), CHANGE).unwrap();
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
    Harness {
        app,
        project,
        worktree,
        worktree_path,
        _fixture: fixture,
        _data: data,
    }
}

fn installed(app: &App, provider: &str) -> bool {
    let entry = wtm_agent::entry(provider).unwrap();
    let found = app.agent_executable(entry).is_some();
    if !found {
        eprintln!(
            "skipping: wtm cannot find {}'s CLI here, so the review recipe was not run on it",
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

/// Keeps a session's skill list, and says when it arrived.
#[derive(Default)]
struct Skills {
    listed: Mutex<Option<Vec<String>>>,
}

impl AgentSink for Skills {
    fn on_event(&self, _session: &SessionId, event: &AgentEvent) {
        if let AgentEvent::SkillsListed { skills } = event {
            *self.listed.lock() = Some(skills.iter().map(|s| s.name.clone()).collect());
        }
    }
    fn on_exit(&self, _session: &SessionId, _outcome: &ExitOutcome) {}
    fn on_ready(&self, _session: &SessionId) {}
}

/// Open a role's session exactly as a run would, minus the canary and with no turn, and return
/// what its CLI listed as its skills.
fn skills_of(h: &Harness, provider: &str, role: &str, skills: &[&str]) -> Vec<String> {
    let spec = RoleSession {
        project: h.project.clone(),
        worktree: h.worktree.clone(),
        role: FlowRole {
            flow: FlowId(1),
            role: role.to_owned(),
            authority: Authority::Reviewer,
            browser: None,
        },
        provider: provider.to_owned(),
        model: None,
        effort: None,
        brief: String::new(),
        resume: None,
        cwd: None,
        skills: skills.iter().map(|s| (*s).to_owned()).collect(),
    };
    let endpoint = BridgeEndpoint {
        program: PathBuf::from(env!("CARGO_BIN_EXE_wtm")),
        socket: std::env::temp_dir().join("wtm-review-real-cli-unused.sock"),
    };
    let prepared = flow_session::prepare_with(&h.app, &spec, &endpoint).unwrap();
    let mut request = prepared.request.clone();
    request.executable = Some(
        h.app
            .agent_executable(prepared.entry)
            .unwrap()
            .to_string_lossy()
            .into_owned(),
    );
    let heard = Arc::new(Skills::default());
    let sink: Arc<dyn AgentSink> = heard.clone();
    let session = wtm_agent::AgentSession::open(
        prepared.entry.provider,
        &request,
        Arc::clone(&h.app.pipe) as Arc<dyn wtm_core::ports::pipe::PipeHost>,
        &sink,
        120_000,
        None,
    )
    .unwrap();
    let mut listed = None;
    for _ in 0..600 {
        listed.clone_from(&heard.listed.lock());
        if listed.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let _ = session.close();
    h.app.handoff.forget_unbound(&prepared.token);
    listed.unwrap_or_else(|| panic!("{provider} never listed its skills"))
}

#[test]
fn each_real_cli_lists_the_skill_its_role_names_under_the_run_profile_and_without_a_turn() {
    let h = harness();
    if installed(&h.app, "claude") {
        let listed = skills_of(&h, "claude", "conventions", &["fixture-conventions"]);
        assert!(
            listed.iter().any(|s| s == "wtm:fixture-conventions"),
            "Claude lists the role's skill from wtm's plugin: {listed:?}"
        );
        assert!(
            !listed.iter().any(|s| s == "fixture-conventions"),
            "and not from the repository, whose settings the run keeps out: {listed:?}"
        );
    }
    if installed(&h.app, "codex") {
        let listed = skills_of(&h, "codex", "correctness", &["fixture-correctness"]);
        assert!(
            listed.iter().any(|s| s == "fixture-correctness"),
            "Codex finds the repository's skill under the run's own home: {listed:?}"
        );
    }
}

/// Prints what the engine filed, so a run's inbox is in the test's output.
struct Printed;

impl Notice for Printed {
    fn changed(&self, _flow: FlowId) {}
    fn filed(&self, item: &InboxItem) {
        eprintln!("inbox: {:?} {}: {}", item.kind, item.title, item.body);
    }
}

#[test]
fn one_recipe_runs_a_claude_conventions_role_and_a_codex_correctness_role_on_their_own_profiles() {
    if std::env::var("WTM_REAL_CLI_TURNS").as_deref() != Ok("1") {
        eprintln!(
            "skipping the real review recipe: it spends model turns on your Claude and Codex \
             plans. Set WTM_REAL_CLI_TURNS=1 to run it."
        );
        return;
    }
    let h = harness();
    if !(installed(&h.app, "claude") && installed(&h.app, "codex")) {
        return;
    }

    // A short path: a Unix socket's path is limited to about a hundred bytes.
    let socket_dir = tempfile::Builder::new()
        .prefix("wtm-rc")
        .tempdir_in(std::env::temp_dir())
        .unwrap();
    let socket = socket_dir.path().join("h.sock");
    let host = HeadlessHost::new(
        Arc::clone(&h.app),
        BridgeEndpoint {
            program: PathBuf::from(env!("CARGO_BIN_EXE_wtm")),
            socket: socket.clone(),
        },
    );
    let engine = Engine::new(
        Arc::clone(&h.app),
        Arc::new(host),
        Arc::clone(&h.app.clock),
        Arc::new(Printed),
        Tuning::default(),
    );
    serve(&socket, Arc::clone(&h.app), Arc::downgrade(&engine));

    let flow = engine
        .start(&StartReview {
            project: h.project.clone(),
            worktree: h.worktree.clone(),
            recipe: "standard".to_owned(),
            base: None,
        })
        .unwrap_or_else(|refusal| panic!("the run did not start: {refusal}"));

    let mut view: Option<FlowView> = None;
    // Fifteen minutes: two short reviews, a canary turn, and room for a slow provider.
    for _ in 0..900 {
        let now = engine.view(flow).unwrap();
        let settled = now.flow.state.phase == Phase::Consolidated && !now.running;
        let stopped = now.flow.state.end.is_some() || now.flow.state.hold.is_some();
        view = Some(now);
        if settled || stopped {
            break;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    let view = view.unwrap();
    for step in &view.steps {
        eprintln!(
            "{} on {}: {:?}{}",
            step.role,
            step.provider,
            step.status,
            step.detail
                .as_deref()
                .map_or_else(String::new, |d| format!(" ({d})"))
        );
        if let Some(Submission::Findings(report)) = &step.result {
            for finding in &report.findings {
                eprintln!(
                    "  [{:?} {:?}] {} — {}:{}",
                    finding.severity,
                    finding.class,
                    finding.title,
                    finding.path.as_deref().unwrap_or("-"),
                    finding.line.map_or_else(String::new, |l| l.to_string())
                );
            }
        }
    }

    assert_eq!(
        view.flow.state.phase,
        Phase::Consolidated,
        "{:#?}",
        view.flow.state
    );
    assert!(view.flow.state.hold.is_none(), "{:#?}", view.flow.state);
    for (role, provider) in [("conventions", "Claude"), ("correctness", "Codex")] {
        let step = view.steps.iter().find(|s| s.role == role).unwrap();
        assert_eq!(step.provider.to_string(), provider);
        assert_eq!(step.status, StepStatus::Done, "{role}: {:?}", step.detail);
        assert!(
            matches!(step.result, Some(Submission::Findings(_))),
            "{role} handed in findings that passed the schema"
        );
        assert!(
            view.invocations
                .iter()
                .any(|i| i.step == step.id
                    && i.status == wtm_core::flow::InvocationStatus::Submitted),
            "{role}"
        );
    }
    // Both read the snapshot checkout, not the worktree.
    for invocation in &view.invocations {
        assert!(
            Path::new(&invocation.cwd).starts_with(h.app.config.paths().flow_dir.clone()),
            "{}",
            invocation.cwd
        );
    }
    assert!(h.worktree_path.join("src/sums.py").exists());
}
