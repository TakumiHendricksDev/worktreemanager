//! The MCP server an agent CLI spawns is the app's own binary, and it has to speak the protocol.
//!
//! This drives the **real executable** rather than calling the functions behind it, because the
//! property that matters is not "the JSON is shaped right" — a unit test proves that more cheaply —
//! but that `wtm --mcp-bridge` starts, answers, and *never paints a window or writes anything to
//! stdout that is not a protocol frame*. Those are facts about `main`, the Tauri builder being
//! skipped, and the process's stdio, and none of them are reachable from inside the library.
//!
//! It is also the test that would catch the worst regression available here. A stray `println!`
//! anywhere on the startup path, or a tracing subscriber that logged to stdout instead of stderr,
//! would corrupt the very first frame — and the symptom in real use is an agent CLI that reports the
//! `wtm` server as failed with no explanation, which is a long way from the cause.
//!
//! # Where the coverage stops, and why
//!
//! A `tools/call` is covered by standing in for the app: `fake_app` binds a socket, and the bridge
//! cannot tell it from the real listener because the wire is one framed JSON line each way. So the
//! whole child half is under test — handshake, tool list, the socket round trip, and each refusal.
//!
//! What is **not** here is the middle: resolving a token to a worktree, opening a pane, waiting for
//! `TurnFinished`. That needs a Tauri runtime for the `AppHandle` a pane is announced on, a
//! registered project, and a second agent CLI installed and logged in — `agent_sessions.rs`-shaped
//! work that cannot be made hermetic. The pieces of it that can be tested without a runtime are unit
//! tests in `handoff.rs` instead.

// The same justification `platform_seams.rs` gives: driving our own binary in a test is not app code
// and needs no deadline, sanitized environment or tracing span.
#![allow(clippy::unwrap_used, clippy::disallowed_methods)]

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use wtm_app_lib::handoff;

/// A bridge process, with its stdio held open.
struct Bridge {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Bridge {
    /// Start `wtm --mcp-bridge` with an agent roster in its environment.
    fn start(agents: &str) -> Self {
        Self::spawn(agents, None)
    }

    /// Start a bridge wired to a socket and a token, so a `tools/call` has somewhere to go.
    fn wired(agents: &str, socket: &std::path::Path, token: &str) -> Self {
        Self::spawn(agents, Some((socket, token)))
    }

    fn spawn(agents: &str, wiring: Option<(&std::path::Path, &str)>) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_wtm"));
        command.arg("--mcp-bridge").env(handoff::AGENTS_ENV, agents);

        match wiring {
            Some((socket, token)) => {
                command
                    .env(handoff::SOCKET_ENV, socket)
                    .env(handoff::TOKEN_ENV, token);
            }
            // Deliberately unset, so a `tools/call` cannot reach a real socket even if this test is
            // run on a machine with the app open.
            None => {
                command
                    .env_remove(handoff::TOKEN_ENV)
                    .env_remove(handoff::SOCKET_ENV);
            }
        }

        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("the app binary should start as a bridge");

        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin,
            stdout,
        }
    }

    /// Send a request and read its reply.
    fn call(&mut self, id: i32, method: &str, params: &serde_json::Value) -> serde_json::Value {
        let frame = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        writeln!(self.stdin, "{frame}").unwrap();
        self.stdin.flush().unwrap();

        let mut line = String::new();
        self.stdout
            .read_line(&mut line)
            .expect("the bridge should answer a request");
        serde_json::from_str(&line).unwrap_or_else(|e| {
            panic!("the bridge wrote something that is not a JSON frame ({e}): {line:?}")
        })
    }

    /// Send a notification, which must not be answered.
    fn notify(&mut self, method: &str) {
        let frame = serde_json::json!({ "jsonrpc": "2.0", "method": method });
        writeln!(self.stdin, "{frame}").unwrap();
        self.stdin.flush().unwrap();
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn the_app_binary_answers_an_initialize_and_declares_its_tools() {
    let mut bridge = Bridge::start("codex:Codex,claude:Claude Code");

    let reply = bridge.call(
        1,
        "initialize",
        &serde_json::json!({
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "test", "version": "0" },
        }),
    );

    assert_eq!(reply["id"], 1, "the reply must be correlated: {reply}");
    // Echoed rather than asserted equal to a constant: the server is meant to follow the client's
    // revision, and pinning its own would break on the first CLI that moves ahead of it.
    assert_eq!(reply["result"]["protocolVersion"], "2025-06-18");
    assert!(
        reply["result"]["capabilities"]["tools"].is_object(),
        "a server with a tool must declare the tools capability: {reply}"
    );
}

#[test]
fn the_bridge_offers_exactly_the_agents_it_was_told_about() {
    // The whole discoverability mechanism. The model picks an agent from this `enum`, so a roster
    // that arrives empty or wrong is the difference between "let Codex review this" working and the
    // model inventing an id that gets refused.
    let mut bridge = Bridge::start("codex:Codex,claude:Claude Code");
    bridge.call(1, "initialize", &serde_json::json!({}));
    bridge.notify("notifications/initialized");

    let reply = bridge.call(2, "tools/list", &serde_json::json!({}));
    let tools = reply["result"]["tools"].as_array().unwrap();
    assert_eq!(
        tools.len(),
        2,
        "one-child handoff and parallel delegation: {reply}"
    );

    let tool = &tools[0];
    assert_eq!(tool["name"], "ask_agent");
    assert_eq!(tools[1]["name"], "spawn_agents");

    let choices = tool["inputSchema"]["properties"]["agent"]["enum"]
        .as_array()
        .expect("the agent parameter must be a closed set");
    assert_eq!(choices.len(), 2, "both agents should be offered: {tool}");
    assert!(choices.contains(&serde_json::json!("codex")));
    assert!(choices.contains(&serde_json::json!("claude")));
    assert_eq!(
        tools[1]["inputSchema"]["properties"]["tasks"]["items"]["properties"]["agent"]["enum"],
        serde_json::json!(["codex", "claude"]),
        "parallel children must use the same closed provider roster: {reply}"
    );

    // The labels belong in the prose, so a model choosing between ids knows which is which.
    let description = tool["inputSchema"]["properties"]["agent"]["description"]
        .as_str()
        .unwrap();
    assert!(
        description.contains("Codex") && description.contains("Claude Code"),
        "the human labels should reach the model: {description}"
    );

    assert!(
        tool["inputSchema"]["required"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("prompt")),
        "a handoff with no prompt is not a handoff: {tool}"
    );
}

#[test]
fn a_repository_that_offers_one_agent_advertises_only_that_one() {
    // `[agent.codex] enabled = false` has to reach the tool schema, not just the launcher. A tool
    // offering an agent the repository refuses would produce a handoff that opens nothing and
    // returns an error the model has to interpret.
    let mut bridge = Bridge::start("claude:Claude Code");
    bridge.call(1, "initialize", &serde_json::json!({}));

    let reply = bridge.call(2, "tools/list", &serde_json::json!({}));
    let choices = reply["result"]["tools"][0]["inputSchema"]["properties"]["agent"]["enum"]
        .as_array()
        .unwrap();
    assert_eq!(choices, &vec![serde_json::json!("claude")]);
}

#[test]
fn the_tool_description_names_the_phrasings_a_user_actually_types() {
    // The description is the only thing that makes this feature discoverable — there is no other
    // signal telling a model that "get a second opinion from Codex" is a tool call rather than
    // something to decline. Asserting on the phrasings is unusual, and it is here because deleting
    // them would silently make the feature much harder to reach while every other test stayed green.
    let mut bridge = Bridge::start("codex:Codex");
    bridge.call(1, "initialize", &serde_json::json!({}));

    let reply = bridge.call(2, "tools/list", &serde_json::json!({}));
    let description = reply["result"]["tools"][0]["description"]
        .as_str()
        .unwrap()
        .to_lowercase();

    for phrase in ["review", "second opinion", "wait"] {
        assert!(
            description.contains(phrase),
            "the description should mention {phrase:?}: {description}"
        );
    }
    // The one thing a caller gets wrong on its own: the far side cannot see the conversation, so a
    // prompt that says "review the above" arrives meaningless.
    assert!(
        description.contains("does not share your conversation"),
        "the description must say the prompt has to stand alone: {description}"
    );
}

#[test]
fn a_notification_is_not_answered() {
    // `notifications/initialized` has no id, and replying to it is a protocol error that some
    // clients treat as fatal. Proved by asking a real question afterwards and checking the answer
    // that comes back is *that* question's — a spurious reply would shift every subsequent read by
    // one and this assertion would see the wrong id.
    let mut bridge = Bridge::start("codex:Codex");
    bridge.call(1, "initialize", &serde_json::json!({}));
    bridge.notify("notifications/initialized");

    let reply = bridge.call(7, "ping", &serde_json::json!({}));
    assert_eq!(reply["id"], 7, "reads are out of step: {reply}");
}

#[test]
fn an_unknown_method_is_refused_rather_than_ignored() {
    // Silence would wedge a client that is blocking on a reply. `-32601` is the standard code, so a
    // client can tell "this server does not do that" from "this server is broken".
    let mut bridge = Bridge::start("codex:Codex");
    bridge.call(1, "initialize", &serde_json::json!({}));

    let reply = bridge.call(3, "resources/list", &serde_json::json!({}));
    assert_eq!(reply["error"]["code"], -32_601, "{reply}");
}

#[test]
fn a_handoff_with_an_empty_prompt_fails_as_a_tool_error_not_a_protocol_error() {
    // The distinction is what the model sees. A JSON-RPC error reads as "the server is broken"; an
    // `isError` result reads as "that call did not work, here is why" — which the model can act on
    // by asking the user or retrying with a real prompt.
    let mut bridge = Bridge::start("codex:Codex");
    bridge.call(1, "initialize", &serde_json::json!({}));

    let reply = bridge.call(
        4,
        "tools/call",
        &serde_json::json!({ "name": "ask_agent", "arguments": { "agent": "codex", "prompt": "  " } }),
    );

    assert!(
        reply.get("error").is_none(),
        "an empty prompt is a tool failure, not a malformed request: {reply}"
    );
    assert_eq!(reply["result"]["isError"], true, "{reply}");
    let text = reply["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("prompt"),
        "the message should say what: {text}"
    );
}

#[test]
fn a_handoff_without_a_token_says_so_rather_than_reaching_for_a_socket() {
    // The ordering that makes this test meaningful: the token is checked before the socket is
    // touched, so a bridge started outside a session fails with an explanation instead of a connect
    // error naming a path the user has no reason to know about.
    let mut bridge = Bridge::start("codex:Codex");
    bridge.call(1, "initialize", &serde_json::json!({}));

    let reply = bridge.call(
        5,
        "tools/call",
        &serde_json::json!({
            "name": "ask_agent",
            "arguments": { "agent": "codex", "prompt": "review this" },
        }),
    );

    assert_eq!(reply["result"]["isError"], true, "{reply}");
    let text = reply["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("token"),
        "the message should name the missing piece: {text}"
    );
}

/// Stand in for the running app: accept one handoff, hand back a canned answer.
///
/// Returns the request the bridge actually sent, so the test can assert the wire round trip rather
/// than only the reply. A thread is necessary rather than convenient — the bridge blocks on this
/// socket while the test blocks on the bridge's stdout, so somebody has to be the other end.
fn fake_app(
    socket: &std::path::Path,
    reply: handoff::Response,
) -> std::thread::JoinHandle<handoff::Request> {
    let listener = std::os::unix::net::UnixListener::bind(socket).expect("bind a test socket");
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().expect("the bridge should connect");
        let mut line = String::new();
        BufReader::new(&stream)
            .read_line(&mut line)
            .expect("the bridge should send a request");

        let mut out = &stream;
        let body = serde_json::to_string(&reply).unwrap();
        out.write_all(body.as_bytes()).unwrap();
        out.write_all(b"\n").unwrap();
        out.flush().unwrap();

        serde_json::from_str(&line).expect("the request should be JSON")
    })
}

#[test]
fn a_handoff_carries_the_token_and_prompt_over_the_socket_and_returns_the_answer() {
    // The whole child half in one test: MCP in, framed JSON out over a Unix socket, the app's answer
    // back, MCP content out. Everything except what the app does in the middle, which needs a Tauri
    // runtime and a real second CLI.
    //
    // The token assertion is the load-bearing one. It is what ties a handoff to a worktree, and a
    // bridge that dropped it would still connect, still return text, and open every pane in the
    // wrong place.
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("h.sock");
    let app = fake_app(
        &socket,
        handoff::Response::ok("Two problems with it.".to_owned()),
    );

    let mut bridge = Bridge::wired("codex:Codex", &socket, "token-xyz");
    bridge.call(1, "initialize", &serde_json::json!({}));

    let reply = bridge.call(
        2,
        "tools/call",
        &serde_json::json!({
            "name": "ask_agent",
            "arguments": { "agent": "codex", "prompt": "Review this plan." },
        }),
    );

    let sent = app.join().expect("the fake app should not panic");
    assert_eq!(
        sent.token, "token-xyz",
        "the session token must reach the app"
    );
    assert_eq!(sent.agent, "codex");
    assert_eq!(sent.prompt, "Review this plan.");

    assert_eq!(reply["result"]["isError"], false, "{reply}");
    assert_eq!(
        reply["result"]["content"][0]["text"], "Two problems with it.",
        "the other agent's words should reach the caller verbatim: {reply}"
    );
}

#[test]
fn a_refusal_from_the_app_reaches_the_caller_as_a_tool_error() {
    // The other direction of the same wire. A repository that does not offer the requested agent is
    // refused by the app, and that has to arrive as `isError` — not as text the model would read as
    // findings and summarise to the user as if the review had happened.
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("h.sock");
    let app = fake_app(
        &socket,
        handoff::Response::failed("this repository's `wtm.toml` does not offer `codex`"),
    );

    let mut bridge = Bridge::wired("codex:Codex", &socket, "token-xyz");
    bridge.call(1, "initialize", &serde_json::json!({}));
    let reply = bridge.call(
        2,
        "tools/call",
        &serde_json::json!({
            "name": "ask_agent",
            "arguments": { "agent": "codex", "prompt": "Review this plan." },
        }),
    );
    app.join().unwrap();

    assert_eq!(reply["result"]["isError"], true, "{reply}");
    let text = reply["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("does not offer"),
        "the reason should survive: {text}"
    );
}

#[test]
fn a_handoff_with_no_app_listening_explains_itself_rather_than_hanging() {
    // The app quit while a CLI kept running — an ordinary way to end a session. The failure has to
    // name the cause, because "connection refused" on a path under `~/.config` is not something a
    // user can act on without being told the app has to be open.
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("absent.sock");

    let mut bridge = Bridge::wired("codex:Codex", &socket, "token-xyz");
    bridge.call(1, "initialize", &serde_json::json!({}));
    let reply = bridge.call(
        2,
        "tools/call",
        &serde_json::json!({
            "name": "ask_agent",
            "arguments": { "agent": "codex", "prompt": "Review this plan." },
        }),
    );

    assert_eq!(reply["result"]["isError"], true, "{reply}");
    let text = reply["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("still running"),
        "the message should point at the app being closed: {text}"
    );
}

#[test]
fn calling_a_tool_this_server_does_not_have_is_a_tool_error() {
    let mut bridge = Bridge::start("codex:Codex");
    bridge.call(1, "initialize", &serde_json::json!({}));

    let reply = bridge.call(
        6,
        "tools/call",
        &serde_json::json!({ "name": "run_anything", "arguments": {} }),
    );
    assert_eq!(reply["result"]["isError"], true, "{reply}");
}
