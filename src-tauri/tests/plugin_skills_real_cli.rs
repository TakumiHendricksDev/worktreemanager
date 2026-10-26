//! A run role naming a skill from an installed Claude plugin, against the real CLIs.
//!
//! `flow_session.rs` shows what lands on disk: the named skill copied, and nothing else the plugin
//! ships. What only the real CLIs can show is what a session under the run's profile makes of that
//! copy — that each CLI lists the skill under the name the run's skill check looks for, that the
//! model can invoke it, and that nothing else from the plugins reaches the session. The fixture's
//! plugins ship everything a plugin can: another skill, a second plugin installed for everyone,
//! hooks, an MCP server, an agent, a command, and Codex metadata that hides the skill and asks for a
//! server. Every hook and server touches a marker file if it runs, so "absent" here means it never
//! ran, not that nobody mentioned it.
//!
//! Two tests, for the reason `review_real_cli.rs` gives:
//!
//! - **The lists** need no model turn. They run whenever a CLI is installed, and skip, saying so,
//!   when one is not.
//! - **The invocation** spends one model turn per provider, so it runs only with
//!   `WTM_REAL_CLI_TURNS=1`, and says so otherwise.
//!
//! The plugins are the test's own. The app's `skill_homes` points at a home made here, so your real
//! plugins are never an input, while the CLIs themselves still start with your sign-ins.

#![allow(clippy::unwrap_used, clippy::print_stderr, clippy::too_many_lines)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use wtm_agent::authority::Authority;
use wtm_agent::session::AgentSink;
use wtm_app_lib::app::{App, FlowRole};
use wtm_app_lib::flow_session::{self, BridgeEndpoint, Prepared, RoleSession};
use wtm_app_lib::handoff::{Action, Request, Response};
use wtm_config::AppPaths;
use wtm_core::flow::FlowId;
use wtm_core::model::{AgentEvent, ExitOutcome, ProjectId, SessionId};
use wtm_core::ports::exec::{CancelToken, Invocation};
use wtm_testkit::GitFixture;

mod common;
use common::plugins::{FixtureHomes, checklist_word};

/// The one skill every role here names.
const NAMED: &str = "fixture-review";

struct Harness {
    app: Arc<App>,
    project: String,
    worktree: String,
    plugins: FixtureHomes,
    _fixture: GitFixture,
    _data: tempfile::TempDir,
}

fn harness() -> Harness {
    let fixture = GitFixture::new();
    // Under the target directory, as `flow_session.rs` explains for its canaries.
    let data = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let mut plugins = FixtureHomes::new(data.path());
    let mut app = App::with_paths(AppPaths::rooted(data.path())).unwrap();
    app.skill_homes = Some(plugins.homes.clone());
    let app = Arc::new(app);
    let root = app.register(fixture.root()).unwrap();
    // The named skill's plugin is installed for this repository alone, beside a skill the role does
    // not name; a second plugin is installed for everyone and named by nobody.
    plugins.install(
        "fixture-tools",
        "local",
        Some(&root),
        &[NAMED, "fixture-other"],
    );
    plugins.install("fixture-extra", "user", None, &["fixture-extra-review"]);
    let project = ProjectId::from_root(&root).to_string();
    fixture.add_worktree("review-wt", "feature/review");
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
        plugins,
        _fixture: fixture,
        _data: data,
    }
}

fn installed(app: &App, provider: &str) -> bool {
    let entry = wtm_agent::entry(provider).unwrap();
    let found = app.agent_executable(entry).is_some();
    if !found {
        eprintln!(
            "SKIPPING: wtm cannot find {}'s CLI here, so whether a run role can use a plugin's \
             skill on it was NOT checked",
            entry.label
        );
    }
    found
}

fn spec(h: &Harness, provider: &str) -> RoleSession {
    RoleSession {
        project: h.project.clone(),
        worktree: h.worktree.clone(),
        role: FlowRole {
            flow: FlowId(1),
            role: "conventions".to_owned(),
            authority: Authority::Reviewer,
            browser: None,
            design: false,
        },
        provider: provider.to_owned(),
        model: match provider {
            "claude" => Some("haiku".to_owned()),
            _ => None,
        },
        effort: match provider {
            "codex" => Some("low".to_owned()),
            _ => None,
        },
        brief: format!(
            "This run checks that your skill works. Invoke the skill `{NAMED}` as your message \
             says and reply with the word it gives you. There is nothing to submit."
        ),
        resume: None,
        cwd: None,
        skills: vec![NAMED.to_owned()],
    }
}

/// Everything a session emitted.
#[derive(Default)]
struct Heard {
    events: Mutex<Vec<AgentEvent>>,
}

impl Heard {
    fn skills(&self) -> Option<Vec<String>> {
        self.events
            .lock()
            .iter()
            .rev()
            .find_map(|event| match event {
                AgentEvent::SkillsListed { skills } => {
                    Some(skills.iter().map(|s| s.name.clone()).collect())
                }
                _ => None,
            })
    }

    fn turn_over(&self) -> bool {
        self.events.lock().iter().any(|event| {
            matches!(
                event,
                AgentEvent::TurnFinished { .. } | AgentEvent::Failed { .. }
            )
        })
    }

    /// Whether a call of the tool `name` started and finished without an error.
    fn ran_tool(&self, name: &str) -> bool {
        let events = self.events.lock();
        events.iter().any(|event| match event {
            AgentEvent::ToolStarted { id, name: tool, .. } if tool == name => {
                events.iter().any(|later| {
                    matches!(later, AgentEvent::ToolFinished { id: done, ok: true, .. } if done == id)
                })
            }
            _ => false,
        })
    }

    fn said(&self) -> String {
        self.events
            .lock()
            .iter()
            .filter_map(|event| match event {
                AgentEvent::MessageDelta { text } | AgentEvent::Message { text } => {
                    Some(text.as_str())
                }
                _ => None,
            })
            .collect()
    }
}

impl AgentSink for Heard {
    fn on_event(&self, _session: &SessionId, event: &AgentEvent) {
        self.events.lock().push(event.clone());
    }
    fn on_exit(&self, _session: &SessionId, _outcome: &ExitOutcome) {}
    fn on_ready(&self, _session: &SessionId) {}
}

/// A socket of the test's own, answering run tool calls from the ledger, as the app's would.
fn serve(socket: &Path, app: Arc<App>) {
    let listener = UnixListener::bind(socket).unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let app = Arc::clone(&app);
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    return;
                }
                let response = match serde_json::from_str::<Request>(&line) {
                    Ok(Request {
                        action: Action::Flow,
                        flow: Some(call),
                        token,
                        ..
                    }) => wtm_app_lib::flow_tools::answer(&app, &token, &call),
                    _ => Response::failed("this test's socket answers run tools only"),
                };
                let mut stream = stream;
                let _ = stream.write_all(serde_json::to_string(&response).unwrap().as_bytes());
                let _ = stream.write_all(b"\n");
            });
        }
    });
}

/// A role's session opened exactly as a run opens it, minus the canary: the same profile, the
/// same argv and environment, the same copied skills.
struct Opened {
    prepared: Prepared,
    session: wtm_agent::AgentSession,
    heard: Arc<Heard>,
    _socket: tempfile::TempDir,
}

fn open(h: &Harness, provider: &str) -> Opened {
    let spec = spec(h, provider);
    // A short path: a Unix socket's path is limited to about a hundred bytes.
    let socket_dir = tempfile::Builder::new()
        .prefix("wtm-ps")
        .tempdir_in(std::env::temp_dir())
        .unwrap();
    let socket = socket_dir.path().join("h.sock");
    serve(&socket, Arc::clone(&h.app));
    let endpoint = BridgeEndpoint {
        program: PathBuf::from(env!("CARGO_BIN_EXE_wtm")),
        socket,
    };
    let prepared = flow_session::prepare_with(&h.app, &spec, &endpoint).unwrap();
    h.app.flow_ledger.brief(&spec.role, &spec.brief);
    let mut request = prepared.request.clone();
    request.executable = Some(
        h.app
            .agent_executable(prepared.entry)
            .unwrap()
            .to_string_lossy()
            .into_owned(),
    );
    let heard = Arc::new(Heard::default());
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
    for _ in 0..600 {
        if heard.skills().is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Opened {
        prepared,
        session,
        heard,
        _socket: socket_dir,
    }
}

/// The agent types Claude Code's own `initialize` reply lists for a run's argv: what `Task` could
/// start. wtm's driver reads only the commands from that reply, so this asks the CLI directly.
fn claude_agents(h: &Harness, prepared: &Prepared) -> Vec<String> {
    let mut request = prepared.request.clone();
    request.executable = Some(
        h.app
            .agent_executable(prepared.entry)
            .unwrap()
            .to_string_lossy()
            .into_owned(),
    );
    let argv = prepared.entry.provider.argv(&request);
    let inv = Invocation::new(argv, Path::new(&request.cwd), 120_000)
        .with_scrub(request.scrub.clone())
        .with_stdin(
            serde_json::json!({
                "type": "control_request",
                "request_id": "agents",
                "request": { "subtype": "initialize" },
            })
            .to_string()
                + "\n",
        );
    let out = h
        .app
        .runner
        .run_allow_failure(&inv, &CancelToken::new())
        .unwrap();
    let reply = out
        .stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|line| {
            line.pointer("/response/request_id")
                .and_then(|id| id.as_str())
                == Some("agents")
        })
        .unwrap_or_else(|| panic!("Claude Code did not answer `initialize`: {}", out.stdout));
    reply
        .pointer("/response/response/agents")
        .and_then(|agents| agents.as_array())
        .unwrap()
        .iter()
        .filter_map(|agent| agent.get("name").and_then(|name| name.as_str()))
        .map(str::to_owned)
        .collect()
}

fn assert_nothing_ran(h: &Harness, provider: &str) {
    assert_eq!(
        h.plugins.ran(),
        Vec::<String>::new(),
        "{provider}: a plugin hook or server ran in a run session"
    );
}

#[test]
fn each_real_cli_lists_the_plugin_skill_a_run_role_names_and_nothing_else_from_any_plugin() {
    let h = harness();
    if installed(&h.app, "claude") {
        let opened = open(&h, "claude");
        let listed = opened.heard.skills().expect("Claude listed its skills");
        let agents = claude_agents(&h, &opened.prepared);
        let _ = opened.session.close();
        h.app.handoff.forget_unbound(&opened.prepared.token);

        assert!(
            listed.iter().any(|s| s == "wtm:fixture-review"),
            "Claude lists the role's plugin skill under the run's own plugin: {listed:?}"
        );
        let leaked: Vec<&String> = listed
            .iter()
            .filter(|s| s.contains("fixture") && *s != "wtm:fixture-review")
            .collect();
        assert!(
            leaked.is_empty(),
            "no other skill or command from either plugin: {leaked:?}"
        );
        assert!(
            agents.iter().all(|agent| !agent.contains("fixture")),
            "and none of the plugin's agents: {agents:?}"
        );
        assert_nothing_ran(&h, "Claude");
    }
    if installed(&h.app, "codex") {
        let opened = open(&h, "codex");
        let listed = opened.heard.skills().expect("Codex listed its skills");
        let _ = opened.session.close();
        h.app.handoff.forget_unbound(&opened.prepared.token);

        assert!(
            listed.iter().any(|s| s == NAMED),
            "Codex lists the role's plugin skill from the run's own home: {listed:?}"
        );
        let leaked: Vec<&String> = listed
            .iter()
            .filter(|s| s.contains("fixture") && *s != NAMED)
            .collect();
        assert!(
            leaked.is_empty(),
            "no other skill from either plugin: {leaked:?}"
        );
        assert_nothing_ran(&h, "Codex");
    }
}

#[test]
fn each_real_cli_invokes_the_plugin_skill_a_run_role_names_and_runs_none_of_the_plugins_hooks_or_servers()
 {
    if std::env::var("WTM_REAL_CLI_TURNS").as_deref() != Ok("1") {
        eprintln!(
            "SKIPPING the plugin skill invocation: it spends a model turn on your Claude and Codex \
             plans. Set WTM_REAL_CLI_TURNS=1 to run it."
        );
        return;
    }
    let h = harness();
    let word = checklist_word(NAMED);
    for (provider, prompt) in [
        (
            "claude",
            format!(
                "Use the Skill tool to invoke the skill wtm:{NAMED}, and do exactly what it says. \
                 Then reply with only the word it told you to reply with."
            ),
        ),
        (
            "codex",
            format!(
                "Use the skill named {NAMED} from your skills list: read its SKILL.md and do \
                 exactly what it says. Then reply with only the word it told you to reply with."
            ),
        ),
    ] {
        if !installed(&h.app, provider) {
            continue;
        }
        let opened = open(&h, provider);
        opened.session.send_turn(&prompt, &[]).unwrap();
        for _ in 0..3000 {
            if opened.heard.turn_over() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let said = opened.heard.said();
        let _ = opened.session.close();
        h.app.handoff.forget_unbound(&opened.prepared.token);
        eprintln!("{provider} said: {said}");

        assert!(opened.heard.turn_over(), "{provider}'s turn did not end");
        if provider == "claude" {
            // By the Skill tool, not by a model that found the file some other way. Claude runs a
            // skill without asking only when its frontmatter is instructions alone, so this is
            // also the copy having dropped what it must.
            assert!(
                opened.heard.ran_tool("Skill"),
                "Claude's Skill call was refused or never made"
            );
        }
        assert!(
            said.contains(&word),
            "{provider} read the copied skill's own checklist, which only invoking the skill \
             would show it: {said}"
        );
        assert_nothing_ran(&h, provider);
    }
}
