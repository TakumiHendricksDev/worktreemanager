//! The MCP server an agent CLI spawns, and the socket it calls home on.
//!
//! Both ends of one wire protocol, deliberately in one file. The request and response types live in
//! [`crate::handoff`], but the *framing* — newline-delimited JSON in both directions — is a single
//! decision, and splitting the two halves across files is how a serializer and a parser drift.
//!
//! # The child half
//!
//! [`serve_stdio`] is what runs when the app's own binary is re-executed with [`ARGV_FLAG`]. It
//! speaks MCP over stdin and stdout to whichever CLI spawned it, and for the one tool it exposes it
//! opens a socket to the running app and waits.
//!
//! Almost all of MCP is unnecessary here and is not implemented. There are no resources, no prompts,
//! no subscriptions and no server-initiated anything — one tool, one call, one answer. Implementing
//! the rest "for completeness" would be code with no caller, and the handshake is the only part that
//! has to be right.
//!
//! # Why `tools/list` needs no socket
//!
//! Because a CLI issues it during its own startup, and the app may not have finished registering the
//! session that is starting. Answering it from the environment — see
//! [`AGENTS_ENV`](crate::handoff::AGENTS_ENV) — means the tool list cannot race the thing it
//! describes, and it means a bridge whose app has quit still describes itself correctly rather than
//! hanging a CLI's boot.
//!
//! # The app half
//!
//! [`listen`] runs one accept loop on a thread for the app's whole life. A connection is one request
//! and one response, then closed: a handoff happens at human speed and the far side is a process
//! that exists only to make this one call, so a persistent connection would be state to manage for
//! no gain.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};

use crate::app::App;
use crate::handoff::{self, Request, Response, Task};

/// The argument that turns the app binary into an MCP server.
///
/// A flag on the app's own executable rather than a second binary. `current_exe()` resolves
/// correctly both in a `cargo` target directory and inside a bundled `.app`, where a sidecar's path
/// differs and has to be looked up through Tauri's resource resolver. The GUI is never constructed
/// on this path — `main` returns before `run()` — so no window appears and no Tauri runtime starts.
pub const ARGV_FLAG: &str = "--mcp-bridge";

/// The socket filename, under the config directory.
///
/// Beside `config.toml` rather than in a temp directory, because a socket in `/tmp` is subject to
/// whatever cleaner the OS runs and reappearing after one has swept is not something the app would
/// notice. The 104-byte `sun_path` limit on macOS is the reason this is a short name in a path the
/// user already has, rather than anything more descriptive.
pub const SOCKET_FILENAME: &str = "handoff.sock";

/// The MCP revision this server claims when a client does not name one.
///
/// A client's own version is echoed when it sends one, which is what the spec asks for and what
/// keeps this working across a CLI upgrade. This constant is only the fallback.
const PROTOCOL_VERSION: &str = "2025-06-18";

/// The one tool.
const TOOL: &str = "ask_agent";
const SPAWN_TOOL: &str = "spawn_agents";

/// Where the socket lives.
pub fn socket_path() -> Result<PathBuf, String> {
    let paths = wtm_config::AppPaths::discover().map_err(|e| e.to_string())?;
    Ok(paths.config_dir.join(SOCKET_FILENAME))
}

// ═══════════════════════════════ the app half ═══════════════════════════════

/// Serve handoff requests for the life of the app.
///
/// Binds, then spawns one thread for the accept loop and one per connection. A thread per connection
/// rather than a pool because a handoff *blocks for minutes* by design — it is waiting on another
/// agent to finish thinking — so a bounded pool would let two concurrent handoffs starve a third,
/// and the concurrent count is bounded anyway by how many panes a user can have open.
///
/// Failure to bind is logged and otherwise ignored. The app is fully usable without this; what is
/// lost is one feature, and refusing to launch over it would be the wrong trade.
pub fn listen(handle: tauri::AppHandle, app: Arc<App>) {
    let path = match socket_path() {
        Ok(path) => path,
        Err(error) => {
            tracing::warn!(%error, "no handoff socket path; agent handoff is unavailable");
            return;
        }
    };

    // Unlinked first, because `bind` fails on an existing path and a socket file always outlives the
    // process that made it — a crash, or a `kill -9`, leaves one behind. Last writer wins, which is
    // also what happens with two app instances: the second takes the socket and the first's sessions
    // lose their bridge. Documented rather than solved, because a second instance of a
    // single-window desktop app is already a confusing state and a lock file would not make it less
    // so.
    let _ = std::fs::remove_file(&path);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let listener = match UnixListener::bind(&path) {
        Ok(listener) => listener,
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "could not bind the handoff socket");
            return;
        }
    };

    // 0600. The socket is the door into "start an agent in my worktree", so the permission bits are
    // the actual access control and not hygiene — `bind` respects the umask, which is commonly 022
    // and would leave this group- and world-readable.
    if let Err(error) = restrict(&path) {
        tracing::warn!(%error, "could not restrict the handoff socket; not serving");
        let _ = std::fs::remove_file(&path);
        return;
    }

    tracing::info!(path = %path.display(), "serving agent handoffs");

    let spawned = std::thread::Builder::new()
        .name("wtm-handoff".to_owned())
        .spawn(move || {
            for incoming in listener.incoming() {
                match incoming {
                    Ok(stream) => {
                        let handle = handle.clone();
                        let app = Arc::clone(&app);
                        // Errors from the spawn itself are the machine being out of threads, which
                        // is not something this loop can improve on by retrying.
                        let _ = std::thread::Builder::new()
                            .name("wtm-handoff-conn".to_owned())
                            .spawn(move || serve_one(&handle, &app, stream));
                    }
                    Err(error) => {
                        tracing::debug!(%error, "a handoff connection failed to accept");
                    }
                }
            }
        });

    if let Err(error) = spawned {
        tracing::warn!(%error, "could not start the handoff listener thread");
    }
}

/// Set 0600 on the socket.
///
/// `std::os::unix::fs::PermissionsExt` rather than a `nix` call, because this is a plain `chmod` on a
/// path that already exists and needs no syscall the standard library does not have.
fn restrict(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

/// Handle one connection: read a request, run it, write the response.
fn serve_one(handle: &tauri::AppHandle, app: &Arc<App>, stream: UnixStream) {
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(clone) => clone,
        Err(error) => {
            tracing::debug!(%error, "could not read a handoff connection");
            return;
        }
    });

    let mut line = String::new();
    if let Err(error) = reader.read_line(&mut line) {
        tracing::debug!(%error, "a handoff request could not be read");
        return;
    }

    let response = match serde_json::from_str::<Request>(&line) {
        Ok(request) => handoff::run(handle, app, &request),
        Err(error) => {
            tracing::debug!(%error, "a handoff request would not parse");
            Response::failed("that request was not understood")
        }
    };

    let mut stream = stream;
    // Errors ignored: the far side hanging up mid-handoff is an ordinary way for this to end — the
    // CLI it was serving may have been interrupted — and there is nobody left to tell.
    if let Ok(body) = serde_json::to_string(&response) {
        let _ = stream.write_all(body.as_bytes());
        let _ = stream.write_all(b"\n");
        let _ = stream.flush();
    }
}

// ═══════════════════════════════ the child half ═══════════════════════════════

/// Speak MCP on stdio until stdin closes.
///
/// # Panics
///
/// Never deliberately. Every failure here is answered as a JSON-RPC error or ignored, because this
/// process is a CLI's child and a panic would surface to the user as an MCP server that died during
/// startup with no explanation.
pub fn serve_stdio() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();

    let agents = agents_from_env();

    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };

        // A notification has no id and takes no reply. `notifications/initialized` is the one that
        // actually arrives; answering it would be a protocol error.
        let Some(id) = message.get("id").cloned() else {
            continue;
        };
        let method = message.get("method").and_then(Value::as_str).unwrap_or("");
        let params = message.get("params").cloned().unwrap_or(Value::Null);

        let reply = match method {
            "initialize" => Some(result(&id, &initialize(&params))),
            "tools/list" => Some(result(&id, &tools(&agents))),
            "tools/call" => Some(result(&id, &call(&params))),
            // `ping` is in the spec and costs one line. Everything else gets the standard
            // method-not-found rather than silence, so a client waiting on a reply is not wedged.
            "ping" => Some(result(&id, &json!({}))),
            _ => Some(error(&id, -32_601, &format!("no method `{method}`"))),
        };

        if let Some(reply) = reply {
            // A write failure means the CLI has gone. Nothing left to serve.
            if serde_json::to_writer(&mut stdout, &reply).is_err()
                || stdout.write_all(b"\n").is_err()
                || stdout.flush().is_err()
            {
                break;
            }
        }
    }
}

/// The agents this repository offers, as `(id, label)`.
///
/// Parsed from the environment the app set when it built this bridge's config. An empty or absent
/// value is not an error: it means the app offered nothing, and the tool then advertises no `enum`
/// rather than refusing to list itself.
fn agents_from_env() -> Vec<(String, String)> {
    std::env::var(handoff::AGENTS_ENV)
        .unwrap_or_default()
        .split(',')
        .filter_map(|entry| {
            let entry = entry.trim();
            if entry.is_empty() {
                return None;
            }
            let (id, label) = entry.split_once(':')?;
            Some((id.trim().to_owned(), label.trim().to_owned()))
        })
        .collect()
}

fn initialize(params: &Value) -> Value {
    // The client's version is echoed when it names one. A server that insisted on its own would
    // break on the first CLI that moved ahead of this constant, for no benefit — the subset of the
    // protocol used here has not changed across revisions.
    let version = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or(PROTOCOL_VERSION);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "wtm", "version": env!("CARGO_PKG_VERSION") },
    })
}

/// The tool list, with the offered agents baked into the schema.
///
/// # Why the description is written the way it is
///
/// It is the *only* thing that makes this feature discoverable. A user types "can you get Codex to
/// review this plan" at a Claude session, and whether that works depends entirely on whether the
/// model recognises this tool as the answer. So the description names the phrasings people actually
/// use rather than describing the mechanism, and it states the two facts a model needs in order to
/// choose it deliberately: that the other agent runs in the same worktree, and that this call blocks
/// until there is an answer.
///
/// `enum` on `agent` is the other half. Without it a model guesses an id — "codex-cli", "gpt" — and
/// gets an error it has to recover from; with it the choice is closed and the labels tell it which is
/// which.
fn tools(agents: &[(String, String)]) -> Value {
    let ids: Vec<&str> = agents.iter().map(|(id, _)| id.as_str()).collect();
    let roster = agents
        .iter()
        .map(|(id, label)| format!("`{id}` ({label})"))
        .collect::<Vec<_>>()
        .join(", ");

    let mut agent_schema = json!({
        "type": "string",
        "description": if roster.is_empty() {
            "The agent to hand this to, by id.".to_owned()
        } else {
            format!("Which agent to hand this to. Available here: {roster}.")
        },
    });
    if !ids.is_empty() {
        agent_schema["enum"] = json!(ids);
    }

    let task_agent_schema = agent_schema.clone();
    json!({
        "tools": [{
            "name": TOOL,
            "description":
                "Hand a prompt to a different coding agent running in this same worktree, and wait \
                 for its reply. Use this whenever the user asks for another agent by name — \"let \
                 Codex review this plan\", \"ask Claude what it thinks\", \"get a second opinion \
                 from Codex\", \"have the other model check this\" — and when you want an \
                 independent review of a plan, a diff, or a design decision.\n\n\
                 The other agent opens as a live session the user can watch and interact with: it \
                 has its own tools, reads the same files, and may ask the user to approve \
                 something. Its session stays open afterwards so the user can read the whole \
                 exchange.\n\n\
                 This call does not return until that agent finishes its turn, which can take \
                 several minutes for a real review. Send it everything it needs in `prompt` — it \
                 does not share your conversation, so include the plan or the question in full \
                 rather than referring to \"the above\".",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "agent": agent_schema,
                    "model": {
                        "type": "string",
                        "description": "Optional provider model id. Omit to use the configured default.",
                    },
                    "effort": {
                        "type": "string",
                        "description": "Optional reasoning or thought level supported by that model.",
                    },
                    "mode": {
                        "type": "string",
                        "description": "Optional provider mode, such as agent, plan, ask, or read-only.",
                    },
                    "prompt": {
                        "type": "string",
                        "description":
                            "The complete, self-contained prompt. The other agent sees only this — \
                             not your conversation — so include the plan, diff, or question in \
                             full, and say what kind of answer you want back.",
                    },
                },
                "required": ["agent", "prompt"],
            },
        }, {
            "name": SPAWN_TOOL,
            "description":
                "Launch several independent coding-agent sessions in parallel and collect every \
                 result. Use this for review swarms, competing analyses, or delegating distinct \
                 subtasks. Each child opens as a visible WTM session with its own transcript and \
                 approvals. The sessions share this worktree: prefer read-only review prompts when \
                 several children run together, because parallel writers can conflict. Starting \
                 many agents can incur substantial provider usage, so match the requested count and \
                 models exactly rather than expanding the swarm on your own.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tasks": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": 20,
                        "items": {
                            "type": "object",
                            "properties": {
                                "title": {
                                    "type": "string",
                                    "description": "A short label shown in the child-session tree.",
                                },
                                "agent": task_agent_schema,
                                "model": { "type": "string" },
                                "effort": { "type": "string" },
                                "mode": { "type": "string" },
                                "prompt": {
                                    "type": "string",
                                    "description": "A complete, self-contained task for this child.",
                                },
                            },
                            "required": ["agent", "prompt"],
                        },
                    },
                    "concurrency": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": 20,
                        "description": "Maximum children running simultaneously. Defaults to 4.",
                    },
                },
                "required": ["tasks"],
            },
        }],
    })
}

/// Run a `tools/call` by asking the app.
fn call(params: &Value) -> Value {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    if name != TOOL && name != SPAWN_TOOL {
        return tool_error(&format!("no tool named `{name}`"));
    }

    let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
    let (agent, prompt, model, effort, mode, tasks, concurrency) = if name == TOOL {
        let prompt = arguments
            .get("prompt")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if prompt.trim().is_empty() {
            return tool_error("`prompt` is required and must not be empty");
        }
        (
            arguments
                .get("agent")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            prompt,
            arguments
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_owned),
            arguments
                .get("effort")
                .and_then(Value::as_str)
                .map(str::to_owned),
            arguments
                .get("mode")
                .and_then(Value::as_str)
                .map(str::to_owned),
            Vec::new(),
            None,
        )
    } else {
        let tasks = match arguments
            .get("tasks")
            .cloned()
            .map(serde_json::from_value::<Vec<Task>>)
        {
            Some(Ok(tasks)) if !tasks.is_empty() => tasks,
            Some(Err(error)) => return tool_error(&format!("`tasks` was invalid: {error}")),
            _ => return tool_error("`tasks` is required and must not be empty"),
        };
        (
            String::new(),
            String::new(),
            None,
            None,
            None,
            tasks,
            arguments
                .get("concurrency")
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok()),
        )
    };

    let Ok(token) = std::env::var(handoff::TOKEN_ENV) else {
        return tool_error("this bridge was started without a session token");
    };
    let socket = std::env::var(handoff::SOCKET_ENV)
        .map_or_else(|_| socket_path().unwrap_or_default(), PathBuf::from);

    match ask(
        &socket,
        &Request {
            token,
            agent,
            prompt,
            model,
            effort,
            mode,
            tasks,
            concurrency,
        },
    ) {
        Ok(response) if response.ok => json!({
            "content": [{ "type": "text", "text": response.text.unwrap_or_default() }],
            "isError": false,
        }),
        Ok(response) => tool_error(response.error.as_deref().unwrap_or("the handoff failed")),
        Err(error) => tool_error(&error),
    }
}

/// Make the round trip to the app.
///
/// No timeout is set on the read, and that is deliberate: the app is the thing enforcing the
/// deadline, and a shorter one here would abandon a handoff that is still running and still visible
/// to the user, leaving the caller told it failed while the pane carries on working.
fn ask(socket: &Path, request: &Request) -> Result<Response, String> {
    let mut stream = UnixStream::connect(socket).map_err(|e| {
        format!(
            "could not reach Worktree Manager on {} ({e}) — is the app still running?",
            socket.display()
        )
    })?;

    let body = serde_json::to_string(request).map_err(|e| e.to_string())?;
    stream
        .write_all(body.as_bytes())
        .and_then(|()| stream.write_all(b"\n"))
        .and_then(|()| stream.flush())
        .map_err(|e| format!("could not send the handoff: {e}"))?;

    let mut line = String::new();
    BufReader::new(&stream)
        .read_line(&mut line)
        .map_err(|e| format!("could not read the handoff reply: {e}"))?;

    serde_json::from_str(&line).map_err(|e| format!("the handoff reply would not parse: {e}"))
}

/// A tool-level failure.
///
/// `isError` on a successful JSON-RPC result rather than a JSON-RPC error, and the difference
/// matters: a protocol error is the *server* malfunctioning and clients treat it as such, while this
/// is the tool reporting that what it was asked to do did not work. The model sees the text and can
/// tell the user, or try something else.
fn tool_error(message: &str) -> Value {
    json!({
        "content": [{ "type": "text", "text": message }],
        "isError": true,
    })
}

fn result(id: &Value, value: &Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": value })
}

fn error(id: &Value, code: i32, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tool_list_offers_one_child_and_bounded_parallel_runs() {
        let listed = tools(&[
            ("claude".to_owned(), "Claude Code".to_owned()),
            ("cursor".to_owned(), "Cursor Agent".to_owned()),
        ]);
        let entries = listed["tools"]
            .as_array()
            .expect("tools/list must contain an array");

        assert_eq!(entries[0]["name"], TOOL);
        assert_eq!(entries[1]["name"], SPAWN_TOOL);
        assert_eq!(
            entries[1]["inputSchema"]["properties"]["tasks"]["maxItems"],
            20
        );
        assert_eq!(
            entries[1]["inputSchema"]["properties"]["tasks"]["items"]["properties"]["agent"]["enum"],
            json!(["claude", "cursor"])
        );
        assert_eq!(
            entries[1]["inputSchema"]["properties"]["concurrency"]["maximum"],
            20
        );
    }

    #[test]
    fn a_parallel_task_preserves_every_per_child_override() {
        let task: Task = serde_json::from_value(json!({
            "title": "Cheap first pass",
            "agent": "cursor",
            "model": "grok-4.6-high-fast",
            "effort": "high",
            "mode": "ask",
            "prompt": "Review only; do not edit."
        }))
        .expect("the spawn_agents schema must deserialize into the socket request");

        assert_eq!(task.title.as_deref(), Some("Cheap first pass"));
        assert_eq!(task.agent, "cursor");
        assert_eq!(task.model.as_deref(), Some("grok-4.6-high-fast"));
        assert_eq!(task.effort.as_deref(), Some("high"));
        assert_eq!(task.mode.as_deref(), Some("ask"));
        assert_eq!(task.prompt, "Review only; do not edit.");
    }
}
