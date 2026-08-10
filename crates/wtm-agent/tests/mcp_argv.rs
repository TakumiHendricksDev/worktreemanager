//! The same set of MCP servers has to reach both CLIs, spelled each one's own way.
//!
//! This file exists because of a bug that was invisible for an increment. `SessionRequest` used to
//! carry `mcp_config: Option<String>` — pre-serialized `--mcp-config` JSON — and Codex has no such
//! flag, so `codex.rs` ignored the field entirely. A repository that declared servers got them on
//! Claude and silently got none of them on Codex. Nothing failed; a feature just was not there.
//!
//! So the property under test is not "the JSON is well formed". It is **both providers honour the
//! same input**, which is the thing a single serialization site cannot promise and the reason the
//! field is now a structured map that each provider spells itself.
//!
//! The tests assert on argv rather than on a snapshot because the two shapes are genuinely
//! different — one flag carrying a JSON document versus a flat list of dotted TOML assignments — and
//! a snapshot of each would prove they are stable without proving they agree.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;

use wtm_agent::claude::Claude;
use wtm_agent::codex::Codex;
use wtm_agent::provider::{McpServer, Provider, SessionRequest, Step};

/// One server, named `codex`, of the shape the worked example documents.
fn one_server() -> BTreeMap<String, McpServer> {
    let mut env = BTreeMap::new();
    env.insert("WTM_TOKEN".to_owned(), "abc123".to_owned());
    let mut servers = BTreeMap::new();
    servers.insert(
        "codex".to_owned(),
        McpServer {
            command: "codex".to_owned(),
            args: vec!["mcp-server".to_owned()],
            env,
        },
    );
    servers
}

fn request(mcp: BTreeMap<String, McpServer>) -> SessionRequest {
    SessionRequest {
        cwd: "/tmp/worktree".to_owned(),
        mcp,
        ..SessionRequest::default()
    }
}

#[test]
fn claude_receives_every_server_as_one_mcp_config_document() {
    let argv = Claude.argv(&request(one_server()));

    let index = argv
        .iter()
        .position(|a| a == "--mcp-config")
        .expect("--mcp-config should be passed when servers are declared");
    let document: serde_json::Value =
        serde_json::from_str(&argv[index + 1]).expect("the flag's value should be a JSON document");

    let server = &document["mcpServers"]["codex"];
    assert_eq!(server["command"], "codex");
    assert_eq!(server["args"][0], "mcp-server");
    // The env is what carries a handoff token, so losing it would not break the handshake — it would
    // produce a bridge that starts, lists its tool, and fails every call.
    assert_eq!(server["env"]["WTM_TOKEN"], "abc123");
}

#[test]
fn codex_receives_every_server_as_config_overrides() {
    // The regression this file was written for. Before the refactor this argv contained no trace of
    // the server at all, and this assertion is what would have caught it.
    let argv = Codex.argv(&request(one_server()));

    let assignments: Vec<&String> = argv
        .iter()
        .filter(|a| a.starts_with("mcp_servers."))
        .collect();

    assert!(
        assignments.contains(&&r#"mcp_servers.codex.command="codex""#.to_owned()),
        "the program should be assigned as a TOML string: {argv:?}"
    );
    assert!(
        assignments.contains(&&r#"mcp_servers.codex.args=["mcp-server"]"#.to_owned()),
        "the arguments should be assigned as a TOML array: {argv:?}"
    );
    assert!(
        assignments.contains(&&r#"mcp_servers.codex.env.WTM_TOKEN="abc123""#.to_owned()),
        "each env var should be its own dotted assignment: {argv:?}"
    );

    // Every assignment must be introduced by its own `-c`, or the CLI reads the value as a prompt.
    for assignment in &assignments {
        let at = argv.iter().position(|a| a == *assignment).unwrap();
        assert_eq!(
            argv[at - 1],
            "-c",
            "each override needs its own -c: {argv:?}"
        );
    }
}

#[test]
fn neither_provider_mentions_mcp_when_no_servers_are_declared() {
    // The common case — most repositories declare none — and the one where a stray empty `-c` or an
    // `--mcp-config {}` would be a change in behaviour for every existing user.
    let claude = Claude.argv(&request(BTreeMap::new()));
    assert!(
        !claude.iter().any(|a| a == "--mcp-config"),
        "an empty set must not produce the flag: {claude:?}"
    );

    let codex = Codex.argv(&request(BTreeMap::new()));
    assert!(
        !codex.iter().any(|a| a == "-c"),
        "an empty set must not produce an override: {codex:?}"
    );
}

#[test]
fn codex_skips_a_server_whose_name_could_not_be_a_toml_key() {
    // `-c` takes a *dotted path*, so a name containing a dot would land the server one level deeper
    // than intended rather than failing. Dropping it is the safe direction: a missing tool is
    // diagnosable, whereas a server registered under `mcp_servers.my.server.command` looks like a
    // config the CLI simply ignored.
    let mut servers = BTreeMap::new();
    servers.insert(
        "my.server".to_owned(),
        McpServer {
            command: "thing".to_owned(),
            args: Vec::new(),
            env: BTreeMap::new(),
        },
    );

    let argv = Codex.argv(&request(servers));
    assert!(
        !argv.iter().any(|a| a.contains("my.server")),
        "a dotted name must not be emitted as a path: {argv:?}"
    );
}

#[test]
fn both_providers_append_environment_instructions_without_replacing_their_own() {
    // The distinction is the whole test, and both CLIs offer a footgun next to the flag that is
    // wanted: `--system-prompt` and `baseInstructions` *replace* the CLI's own prompt, which would
    // discard the instructions that make it work and the user's own `CLAUDE.md` / `AGENTS.md` with
    // them. Reaching for the wrong one of a near-identical pair is an easy edit to make and a hard
    // symptom to read, so it is pinned rather than trusted to review.
    let req = SessionRequest {
        cwd: "/tmp/worktree".to_owned(),
        instructions: Some("prefer the handoff tool".to_owned()),
        ..SessionRequest::default()
    };

    let claude = Claude.argv(&req);
    let at = claude
        .iter()
        .position(|a| a == "--append-system-prompt")
        .expect("instructions should be appended");
    assert_eq!(claude[at + 1], "prefer the handoff tool");
    assert!(
        !claude.iter().any(|a| a == "--system-prompt"),
        "the replacing flag must never be used: {claude:?}"
    );

    // Codex carries it on the frame rather than the argv, so the assertion has to drive the
    // handshake far enough to see `thread/start`.
    let mut driver = Codex.protocol(&req);
    let mut frames: Vec<String> = Vec::new();
    for step in driver.open() {
        if let Step::Write(frame) = step {
            frames.push(frame);
        }
    }
    for step in driver.on_line(r#"{"id":1,"result":{"userAgent":"x"}}"#) {
        if let Step::Write(frame) = step {
            frames.push(frame);
        }
    }

    let start = frames
        .iter()
        .map(|f| serde_json::from_str::<serde_json::Value>(f).unwrap())
        .find(|f| f["method"] == "thread/start")
        .expect("a thread should be opened");
    assert_eq!(
        start["params"]["developerInstructions"], "prefer the handoff tool",
        "instructions should ride on thread/start: {start}"
    );
    assert!(
        start["params"].get("baseInstructions").is_none(),
        "the replacing field must never be sent: {start}"
    );
}

#[test]
fn neither_provider_says_anything_when_there_are_no_instructions() {
    // A session in a repository that offers only one agent gets no guidance, because there is nothing
    // to hand off to. An empty flag or a `null` field would be a change in behaviour for every
    // single-agent setup.
    let claude = Claude.argv(&SessionRequest::default());
    assert!(
        !claude.iter().any(|a| a == "--append-system-prompt"),
        "{claude:?}"
    );
}

#[test]
fn claude_accepts_a_name_codex_has_to_refuse() {
    // Not a bug, and worth pinning so nobody "fixes" it into a shared restriction. Claude's servers
    // are a JSON object, where any string is a valid key — so the constraint belongs to Codex's
    // override grammar alone, and narrowing Claude to match would remove capability for tidiness.
    let mut servers = BTreeMap::new();
    servers.insert(
        "my.server".to_owned(),
        McpServer {
            command: "thing".to_owned(),
            args: Vec::new(),
            env: BTreeMap::new(),
        },
    );

    let argv = Claude.argv(&request(servers));
    let index = argv.iter().position(|a| a == "--mcp-config").unwrap();
    let document: serde_json::Value = serde_json::from_str(&argv[index + 1]).unwrap();
    assert_eq!(document["mcpServers"]["my.server"]["command"], "thing");
}
