//! What a workflow run session's CLI is started with, exactly, for each provider and authority.
//!
//! These are snapshots in the plain sense: each test writes out the whole argv, frame or
//! environment a run gets and compares it with the one value it should be. That is deliberate for
//! this file in particular. A run's authority is the sum of a dozen flags, and the way one of them
//! goes wrong is quietly — F2 found a malformed sandbox object is ignored without a word, and the
//! `--disallowedTools` route leaves twenty-two tools behind — so a test that checked for a flag's
//! presence would pass a profile with an extra flag beside it that undoes it. Comparing the whole
//! value is what makes an added flag a diff somebody has to read.
//!
//! The other property here is that nothing a repository wrote reaches a run: its `extra_args`,
//! its mode, its environment and its MCP servers are all on the request, and none of them is on the
//! argv or in the environment.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use wtm_agent::authority::{self, Authority, RunAuthority, RunBridge, RunPlaces};
use wtm_agent::claude::Claude;
use wtm_agent::codex::Codex;
use wtm_agent::provider::{McpServer, Provider, SessionRequest, Step};
use wtm_agent::session::{AgentSession, AgentSink};
use wtm_core::model::{AgentEvent, ExitOutcome, SessionId};
use wtm_core::ports::pipe::PipeHost;
use wtm_testkit::FakePipe;

fn places() -> RunPlaces {
    RunPlaces {
        worktree: PathBuf::from("/w/acme-wt"),
        repo_root: PathBuf::from("/w/acme"),
        git: vec![
            PathBuf::from("/w/acme/.git"),
            PathBuf::from("/w/acme/.git/worktrees/acme-wt"),
            PathBuf::from("/w/acme-wt/.git"),
        ],
        home: PathBuf::from("/Users/someone"),
        session_dir: PathBuf::from("/data/run-sessions/flow-7-conventions"),
        sessions_root: PathBuf::from("/data/run-sessions"),
        gh_config_dir: PathBuf::from("/data/run-gh-config"),
    }
}

const TOKEN: &str = "run-token-0000";

fn bridge(tools: &[&str]) -> RunBridge {
    RunBridge {
        name: "wtm".to_owned(),
        server: McpServer {
            command: "/Applications/wtm.app/Contents/MacOS/wtm".to_owned(),
            args: vec!["--mcp-bridge".to_owned()],
            env: BTreeMap::from([
                ("WTM_HANDOFF_TOKEN".to_owned(), TOKEN.to_owned()),
                ("WTM_FLOW_TOOLS".to_owned(), "reviewer".to_owned()),
            ]),
        },
        tools: tools.iter().map(|tool| (*tool).to_owned()).collect(),
    }
}

const REVIEWER_TOOLS: &[&str] = &[
    "flow_context",
    "flow_progress",
    "flow_ask",
    "flow_submit",
    "flow_request",
    "flow_run_check",
    "flow_propose_rule",
];

fn profile(provider: &str, authority: Authority) -> authority::RunProfile {
    let mut tools = REVIEWER_TOOLS.to_vec();
    if authority == Authority::Writer {
        tools.push("flow_checkpoint");
    }
    authority::profile(
        provider,
        authority,
        &places(),
        &bridge(&tools),
        Path::new("/Users/someone/.codex/auth.json"),
    )
    .unwrap()
}

/// A request carrying everything a repository could have put on one, so a test can show none of it
/// reaches a run.
fn hostile_request(run: &authority::RunProfile) -> SessionRequest {
    let repository_server = McpServer {
        command: "npx".to_owned(),
        args: vec!["repo-server".to_owned()],
        env: BTreeMap::new(),
    };
    SessionRequest {
        cwd: "/w/acme-wt".to_owned(),
        executable: Some("/opt/bin/agent".to_owned()),
        model: Some("opus".to_owned()),
        mode: Some("bypassPermissions".to_owned()),
        extra_args: vec!["--dangerously-skip-permissions".to_owned()],
        env: BTreeMap::from([("NODE_OPTIONS".to_owned(), "--require /tmp/x.js".to_owned())]),
        scrub: run.scrub.clone(),
        mcp: BTreeMap::from([
            ("wtm".to_owned(), bridge(&[]).server),
            ("repo".to_owned(), repository_server),
        ]),
        run: Some(run.run.clone()),
        instructions: Some("You are the conventions reviewer.".to_owned()),
        ..SessionRequest::default()
    }
}

/// The argv with the per-launch session id replaced, since a fresh v4 UUID is the point of it.
fn stable(mut argv: Vec<String>) -> Vec<String> {
    if let Some(at) = argv.iter().position(|arg| arg == "--session-id") {
        "<uuid>".clone_into(&mut argv[at + 1]);
    }
    argv
}

fn settings_of(argv: &[String]) -> Value {
    let at = argv.iter().position(|arg| arg == "--settings").unwrap();
    serde_json::from_str(&argv[at + 1]).unwrap()
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn a_claude_reviewer_starts_with_exactly_this_argv_whatever_the_repository_asked_for() {
    let run = profile("claude", Authority::Reviewer);
    let argv = stable(Claude.argv(&hostile_request(&run)));
    let settings = settings_of(&argv).to_string();

    assert_eq!(
        argv,
        strings(&[
            "/opt/bin/agent",
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
            "--permission-prompt-tool",
            "stdio",
            "--session-id",
            "<uuid>",
            "--model",
            "opus",
            "--permission-mode",
            "dontAsk",
            "--strict-mcp-config",
            "--setting-sources",
            "",
            "--tools",
            "Read,Bash,Grep,Glob,Task",
            "--settings",
            &settings,
            "--mcp-config",
            "/data/run-sessions/flow-7-conventions/mcp.json",
            "--append-system-prompt",
            "You are the conventions reviewer.",
            "--add-dir",
            "/w/acme-wt",
        ])
    );
    // The token is in the file, not in a process listing.
    assert!(!argv.iter().any(|arg| arg.contains(TOKEN)), "{argv:?}");
}

#[test]
fn a_claude_writer_differs_from_a_reviewer_only_in_its_mode_tools_and_settings() {
    let reviewer = stable(Claude.argv(&hostile_request(&profile("claude", Authority::Reviewer))));
    let writer = stable(Claude.argv(&hostile_request(&profile("claude", Authority::Writer))));

    let differing: Vec<(&str, &str)> = reviewer
        .iter()
        .zip(&writer)
        .filter(|(a, b)| a != b)
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    assert_eq!(reviewer.len(), writer.len());
    assert_eq!(differing.len(), 3, "{differing:?}");
    assert_eq!(differing[0], ("dontAsk", "acceptEdits"));
    assert_eq!(
        differing[1],
        (
            "Read,Bash,Grep,Glob,Task",
            "Read,Edit,Write,Bash,Grep,Glob,Task"
        )
    );
}

#[test]
fn a_claude_reviewers_settings_are_exactly_the_profile_row() {
    let argv = Claude.argv(&hostile_request(&profile("claude", Authority::Reviewer)));
    assert_eq!(
        settings_of(&argv),
        json!({
            "sandbox": {
                "enabled": true,
                "allowUnsandboxedCommands": false,
                "failIfUnavailable": true,
                "autoAllowBashIfSandboxed": true,
                "network": {
                    "allowedDomains": ["registry.npmjs.org", "pypi.org", "files.pythonhosted.org"]
                },
                "filesystem": {
                    "denyWrite": [
                        "/w/acme-wt",
                        "/w/acme/.git",
                        "/w/acme/.git/worktrees/acme-wt",
                        "/w/acme-wt/.git",
                        "/w/acme-wt/.claude",
                        "/w/acme-wt/.codex",
                        "/w/acme-wt/.cursor",
                        "/w/acme-wt/.mcp.json",
                    ],
                    "denyRead": [
                        "/Users/someone/Library/Keychains",
                        "/Users/someone/.config/gh",
                        "/Users/someone/.ssh",
                        "/Users/someone/.codex",
                        "/Users/someone/.config/acli",
                        "/data/run-sessions",
                    ],
                },
            },
            "permissions": {
                "allow": [
                    "mcp__wtm__flow_context",
                    "mcp__wtm__flow_progress",
                    "mcp__wtm__flow_ask",
                    "mcp__wtm__flow_submit",
                    "mcp__wtm__flow_request",
                    "mcp__wtm__flow_run_check",
                    "mcp__wtm__flow_propose_rule",
                ],
                "deny": [
                    "Read(//Users/someone/Library/Keychains/**)",
                    "Read(//Users/someone/.config/gh/**)",
                    "Read(//Users/someone/.ssh/**)",
                    "Read(//Users/someone/.codex/**)",
                    "Read(//Users/someone/.config/acli/**)",
                    "Read(//data/run-sessions/**)",
                ],
            },
        })
    );
}

#[test]
fn ultracode_and_fast_mode_ride_in_the_same_settings_object_as_the_sandbox() {
    let run = profile("claude", Authority::Reviewer);
    let mut request = hostile_request(&run);
    request.effort = Some("ultracode".to_owned());
    request.fast = Some(true);
    let argv = Claude.argv(&request);

    assert_eq!(argv.iter().filter(|arg| *arg == "--settings").count(), 1);
    let settings = settings_of(&argv);
    assert_eq!(settings["ultracode"], true);
    assert_eq!(settings["fastMode"], true);
    assert_eq!(settings["sandbox"]["enabled"], true);
}

#[test]
fn a_claude_role_with_skills_gets_the_skill_tool_and_its_own_plugin_and_nothing_else_changes() {
    let plain = profile("claude", Authority::Reviewer);
    let mut skilled = plain.clone();
    let RunAuthority::Claude(run) = &mut skilled.run else {
        panic!("a Claude profile");
    };
    authority::with_skills(run, Path::new("/data/flow/7/skills/conventions"));
    let before = stable(Claude.argv(&hostile_request(&plain)));
    let after = stable(Claude.argv(&hostile_request(&skilled)));

    let at = after.iter().position(|arg| arg == "--tools").unwrap();
    assert_eq!(
        after[at + 1..at + 6],
        strings(&[
            "Read,Bash,Grep,Glob,Task,Skill",
            "--plugin-dir",
            "/data/flow/7/skills/conventions",
            "--add-dir",
            "/data/flow/7/skills/conventions",
        ])
    );
    // Everything else, the settings object included, is the reviewer's argv unchanged.
    let mut without = after.clone();
    without.drain(at + 2..at + 6);
    without[at + 1] = "Read,Bash,Grep,Glob,Task".to_owned();
    assert_eq!(without, before);

    let RunAuthority::Claude(run) = &skilled.run else {
        panic!("a Claude profile");
    };
    let canary = authority::claude_canary(run);
    assert_eq!(canary.skills, None);
    assert_eq!(canary.tools, ["Bash"]);
}

#[test]
fn a_run_session_never_starts_with_wtms_own_reading_of_skill_directories() {
    // A composer seed is wtm's guess at a pane's skills from disk. A run's skills are checked
    // against what the provider reports, and its profile keeps those directories out, so a seed
    // would make a run session appear to have skills it does not.
    let repo = tempfile::tempdir().unwrap();
    let skill = repo.path().join(".claude/skills/conventions");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: conventions\ndescription: x\n---\nx\n",
    )
    .unwrap();

    let seeded = |run: Option<RunAuthority>| {
        let sink = Arc::new(Recorder::default());
        let sink_dyn: Arc<dyn AgentSink> = sink.clone();
        let session = AgentSession::open(
            &Claude,
            &SessionRequest {
                cwd: repo.path().to_string_lossy().into_owned(),
                executable: Some("/opt/bin/agent".to_owned()),
                run,
                ..SessionRequest::default()
            },
            Arc::new(FakePipe::new()) as Arc<dyn PipeHost>,
            &sink_dyn,
            60_000,
            None,
        )
        .unwrap();
        drop(session);
        let events = sink.events.lock().clone();
        events
            .into_iter()
            .any(|event| matches!(event, AgentEvent::SkillsListed { .. }))
    };

    assert!(seeded(None), "a pane is seeded");
    let run = profile("claude", Authority::Reviewer).run;
    assert!(!seeded(Some(run)), "a run session is not");
}

#[test]
fn a_claude_canary_loads_no_server_and_keeps_no_conversation() {
    let RunAuthority::Claude(run) = profile("claude", Authority::Writer).run else {
        panic!("a Claude profile");
    };
    let canary = authority::claude_canary(&run);
    let argv = stable(Claude.argv(&SessionRequest {
        cwd: "/w/acme-wt".to_owned(),
        model: Some(authority::CLAUDE_CANARY_MODEL.to_owned()),
        run: Some(RunAuthority::Claude(canary)),
        ..SessionRequest::default()
    }));
    let at = argv.iter().position(|arg| arg == "--tools").unwrap();
    assert_eq!(argv[at + 1], "Bash");
    assert!(argv.contains(&"--no-session-persistence".to_owned()));
    assert!(argv.contains(&"--strict-mcp-config".to_owned()));
    assert!(!argv.contains(&"--mcp-config".to_owned()), "{argv:?}");
    assert_eq!(settings_of(&argv), run.settings);
}

#[test]
fn a_codex_run_starts_the_bare_app_server_whatever_the_repository_asked_for() {
    let run = profile("codex", Authority::Writer);
    assert_eq!(
        Codex.argv(&hostile_request(&run)),
        strings(&["/opt/bin/agent", "app-server", "--stdio"])
    );
}

fn frames(steps: &[Step]) -> Vec<Value> {
    steps
        .iter()
        .filter_map(|step| match step {
            Step::Write(frame) => Some(serde_json::from_str(frame).unwrap()),
            _ => None,
        })
        .collect()
}

/// The `thread/start` and first `turn/start` a run session sends.
fn codex_opening(authority: Authority) -> (Value, Value) {
    let run = profile("codex", authority);
    let mut driver = Codex.protocol(&hostile_request(&run));
    driver.open();
    let opened = frames(&driver.on_line(r#"{"id":1,"result":{"userAgent":"wtm/0.160.1"}}"#));
    driver.on_line(r#"{"id":2,"result":{"thread":{"id":"thread-1"}}}"#);
    let turn = frames(&driver.send_turn("review the change", &[]));
    (opened[1].clone(), turn[0].clone())
}

#[test]
fn a_codex_reviewer_opens_read_only_never_asks_and_has_no_network() {
    let (thread, turn) = codex_opening(Authority::Reviewer);
    assert_eq!(
        thread["params"],
        json!({
            "cwd": "/w/acme-wt",
            "model": "opus",
            "approvalPolicy": "never",
            "approvalsReviewer": "user",
            "sandbox": "read-only",
            "developerInstructions": "You are the conventions reviewer.",
        })
    );
    assert_eq!(turn["method"], "turn/start");
    assert_eq!(turn["params"]["approvalPolicy"], "never");
    assert_eq!(
        turn["params"]["sandboxPolicy"],
        json!({ "type": "readOnly", "networkAccess": false })
    );
}

#[test]
fn a_codex_writer_may_write_the_workspace_asks_a_person_and_still_has_no_network() {
    let (thread, turn) = codex_opening(Authority::Writer);
    assert_eq!(thread["params"]["sandbox"], "workspace-write");
    assert_eq!(thread["params"]["approvalPolicy"], "on-request");
    // A person, never Codex's own auto-reviewer, answers a writer's escalation.
    assert_eq!(thread["params"]["approvalsReviewer"], "user");
    assert_eq!(
        turn["params"]["sandboxPolicy"],
        json!({
            "type": "workspaceWrite",
            "writableRoots": [],
            "networkAccess": false,
            "excludeTmpdirEnvVar": false,
            "excludeSlashTmp": false,
        })
    );
}

#[test]
fn a_run_sessions_mode_cannot_be_changed_from_the_picker_on_either_provider() {
    let run = profile("codex", Authority::Reviewer);
    let mut codex = Codex.protocol(&hostile_request(&run));
    codex.open();
    codex.on_line(r#"{"id":1,"result":{}}"#);
    codex.on_line(r#"{"id":2,"result":{"thread":{"id":"thread-1"}}}"#);
    let update = frames(&codex.reconfigure(Some("gpt-5.6"), None, Some("full-access"), None));
    assert_eq!(update.len(), 1);
    assert_eq!(update[0]["params"]["model"], "gpt-5.6");
    assert!(
        update[0]["params"].get("sandboxPolicy").is_none(),
        "{update:?}"
    );
    // And the next turn still carries the profile's policy.
    let turn = frames(&codex.send_turn("go on", &[]));
    assert_eq!(turn[0]["params"]["sandboxPolicy"]["type"], "readOnly");

    let mut claude = Claude.protocol(&hostile_request(&profile("claude", Authority::Reviewer)));
    let steps = frames(&claude.reconfigure(None, None, Some("bypassPermissions"), None));
    assert!(
        !steps
            .iter()
            .any(|frame| frame["request"]["subtype"] == "set_permission_mode"),
        "{steps:?}"
    );
}

#[test]
fn the_files_a_run_needs_carry_its_token_and_only_its_bridge() {
    let claude = profile("claude", Authority::Reviewer);
    assert_eq!(
        claude.dirs,
        [PathBuf::from("/data/run-sessions/flow-7-conventions")]
    );
    assert_eq!(claude.files.len(), 1);
    assert_eq!(
        claude.files[0].path,
        Path::new("/data/run-sessions/flow-7-conventions/mcp.json")
    );
    let document: Value = serde_json::from_str(&claude.files[0].contents).unwrap();
    assert_eq!(
        document["mcpServers"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        ["wtm"]
    );
    assert_eq!(
        document["mcpServers"]["wtm"]["env"]["WTM_HANDOFF_TOKEN"],
        TOKEN
    );
    assert!(claude.links.is_empty());

    let codex = profile("codex", Authority::Reviewer);
    let home = PathBuf::from("/data/run-sessions/flow-7-conventions/codex-home");
    assert_eq!(codex.dirs[1], home);
    assert_eq!(codex.files[0].path, home.join("config.toml"));
    assert!(codex.files[0].contents.contains(TOKEN));
    assert_eq!(
        codex.links,
        [(
            home.join("auth.json"),
            PathBuf::from("/Users/someone/.codex/auth.json")
        )]
    );
}

#[derive(Default)]
struct Quiet;

/// Keeps every event a session emitted, for the tests that look at what it said while opening.
#[derive(Default)]
struct Recorder {
    events: parking_lot::Mutex<Vec<AgentEvent>>,
}

impl AgentSink for Recorder {
    fn on_event(&self, _session: &SessionId, event: &AgentEvent) {
        self.events.lock().push(event.clone());
    }
    fn on_exit(&self, _session: &SessionId, _outcome: &ExitOutcome) {}
    fn on_ready(&self, _session: &SessionId) {}
}

impl AgentSink for Quiet {
    fn on_event(&self, _session: &SessionId, _event: &AgentEvent) {}
    fn on_exit(&self, _session: &SessionId, _outcome: &ExitOutcome) {}
    fn on_ready(&self, _session: &SessionId) {}
}

/// The environment a spawned child sees: the parent, then the invocation's env, then its scrub.
fn child_env(
    provider: &'static (dyn Provider + Sync),
    request: &SessionRequest,
) -> BTreeMap<String, String> {
    let fake = Arc::new(FakePipe::new());
    let sink: Arc<dyn AgentSink> = Arc::new(Quiet);
    AgentSession::open(
        provider,
        request,
        Arc::clone(&fake) as Arc<dyn PipeHost>,
        &sink,
        60_000,
        Some("/w/acme-wt"),
    )
    .unwrap();
    let spawned = fake.spawned();
    let mut env: BTreeMap<String, String> = [
        ("HOME", "/Users/someone"),
        ("PATH", "/usr/bin:/bin"),
        ("SSH_AUTH_SOCK", "/private/tmp/agent.sock"),
        ("GH_TOKEN", "gho_dummy"),
        ("GH_CONFIG_DIR", "/Users/someone/.config/gh"),
        ("GITHUB_TOKEN", "ghp_dummy"),
        ("ANTHROPIC_API_KEY", "sk-ant-dummy"),
        ("OPENAI_API_KEY", "sk-dummy"),
        ("CODEX_HOME", "/Users/someone/.codex"),
        ("JIRA_API_TOKEN", "dummy"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect();
    env.extend(spawned[0].env.clone());
    spawned[0].scrub.apply(&mut env);
    env
}

#[test]
fn a_codex_run_child_gets_its_own_home_and_no_credential_from_a_dirty_parent() {
    let run = profile("codex", Authority::Reviewer);
    let mut request = hostile_request(&run);
    // A repository's environment is on the request and in none of the child's: not its
    // `NODE_OPTIONS`, and not a `CODEX_HOME` pointing back into the worktree.
    request
        .env
        .insert("CODEX_HOME".to_owned(), "/w/acme-wt/.codex".to_owned());
    assert_eq!(
        child_env(&Codex, &request),
        [
            (
                "CODEX_HOME",
                "/data/run-sessions/flow-7-conventions/codex-home"
            ),
            ("GH_CONFIG_DIR", "/data/run-gh-config"),
            ("GIT_TERMINAL_PROMPT", "0"),
            ("HOME", "/Users/someone"),
            ("NO_COLOR", "1"),
            ("PATH", "/usr/bin:/bin"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect::<BTreeMap<_, _>>()
    );
}

#[test]
fn a_claude_run_child_gets_the_same_scrub_and_no_codex_home_of_its_own() {
    let run = profile("claude", Authority::Writer);
    let env = child_env(&Claude, &hostile_request(&run));
    assert_eq!(
        env.into_iter().collect::<Vec<_>>(),
        [
            // The app's own, passed through: not a credential, and a Codex started inside
            // Claude's sandbox reaches no network to use it with.
            ("CODEX_HOME", "/Users/someone/.codex"),
            ("GH_CONFIG_DIR", "/data/run-gh-config"),
            ("GIT_TERMINAL_PROMPT", "0"),
            ("HOME", "/Users/someone"),
            ("NO_COLOR", "1"),
            ("PATH", "/usr/bin:/bin"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect::<Vec<_>>()
    );
}

#[test]
fn a_profile_for_the_other_cli_does_not_start_a_session_at_all() {
    let fake = Arc::new(FakePipe::new());
    let sink: Arc<dyn AgentSink> = Arc::new(Quiet);
    let codex_profile = profile("codex", Authority::Reviewer);
    let refused = AgentSession::open(
        &Claude,
        &hostile_request(&codex_profile),
        Arc::clone(&fake) as Arc<dyn PipeHost>,
        &sink,
        60_000,
        None,
    );
    assert!(refused.is_err());
    assert!(fake.spawned().is_empty(), "nothing may be spawned");
}

#[test]
fn cursor_has_no_profile_and_says_so_by_name() {
    let refused = authority::profile(
        "cursor",
        Authority::Reviewer,
        &places(),
        &bridge(&[]),
        Path::new("/x"),
    );
    assert!(refused.is_err_and(|why| why.contains("Cursor")));
}
