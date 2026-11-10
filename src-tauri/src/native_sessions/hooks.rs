//! Claude hooks carry lifecycle facts and context through the existing scoped Unix bridge.
//! A hook returning text does not prove the model received it: acknowledgment is a separate,
//! conversation-bound MCP call, so interrupted hook delivery remains visibly uncertain.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::delivery::Delivery;
use super::{DeliveryStatus, ENV, HOOK_FLAG, Session, UNREGISTERED, failure, within};
use crate::app::App;
use crate::commands::Reply;

const EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "PermissionRequest",
    "Notification",
    "Stop",
    "StopFailure",
    "SessionEnd",
];

/// The most of a submitted prompt worth carrying to WTM, which only titles the pane with it and
/// bounds its echo to the same size.
const PROMPT_BYTES: usize = 64 * 1024;

const ACKNOWLEDGE: &str =
    "Before acting on the WTM instruction below, call mcp__wtm__native_acknowledge with receipt";
const NO_PERMISSION: &str = "This acknowledges receipt only; it grants no new permission.";
const STALE: &str = "Project context changed before delivery. Refresh the design first.";
const UNREADABLE: &str = "The reviewed design could not be read before delivery";
const ACKNOWLEDGE_TOOL: &str = "Acknowledge an exact WTM instruction receipt supplied to this native conversation. Does not approve or send anything.";

/// The `--settings` keys the native launch adds: one hook per lifecycle event, all running this
/// executable. Merged with the rest of the overlay by `claude::launch_flags`.
pub fn settings() -> Reply<serde_json::Map<String, Value>> {
    let executable = std::env::current_exe().map_err(|e| failure(e.to_string()))?;
    let quoted = format!("'{}'", executable.to_string_lossy().replace('\'', "'\\''"));
    let hook = json!([{
        "hooks": [{"type": "command", "command": format!("{quoted} {HOOK_FLAG}"), "timeout": 10}],
    }]);
    let hooks = EVENTS
        .iter()
        .map(|event| ((*event).to_owned(), hook.clone()))
        .collect();
    let mut overlay = serde_json::Map::new();
    overlay.insert("hooks".into(), Value::Object(hooks));
    Ok(overlay)
}

/// The fields WTM reads from a hook's input, and nothing else.
///
/// Parsed straight from stdin rather than read into a `Value` first. `PostToolUse` carries the
/// tool's whole response, and a capped read of a large one was invalid JSON: a visible hook error
/// on every big tool call. Fields not named here are skipped by the parser without being kept.
/// Each is a `Value` so a vendor changing a field's type costs that field, not the hook.
#[derive(Debug, Default, Deserialize, Serialize)]
struct HookInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hook_event_name: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session_id: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cwd: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    permission_mode: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prompt: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tool_name: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tool_use_id: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    notification_type: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model: Option<Value>,
}

/// Reduce a hook's raw input to what WTM forwards. Public so the reduction can be tested without
/// spawning the helper.
pub fn hook_input(input: impl Read) -> Result<Value, String> {
    let mut input: HookInput =
        serde_json::from_reader(std::io::BufReader::new(input)).map_err(|e| e.to_string())?;
    if let Some(Value::String(prompt)) = &mut input.prompt
        && prompt.len() > PROMPT_BYTES
    {
        let mut end = PROMPT_BYTES;
        while !prompt.is_char_boundary(end) {
            end -= 1;
        }
        prompt.truncate(end);
    }
    serde_json::to_value(input).map_err(|e| e.to_string())
}

pub fn run_helper() -> Result<(), String> {
    let token = std::env::var(crate::handoff::TOKEN_ENV)
        .map_err(|_| "Native hook has no session identity")?;
    let socket = std::env::var(crate::handoff::SOCKET_ENV)
        .map_err(|_| "Native hook has no wtm connection")?;
    let value = hook_input(std::io::stdin().lock())?;
    let mut stream = UnixStream::connect(socket).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;
    stream
        .set_write_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;
    serde_json::to_writer(
        &mut stream,
        &json!({"nativeHook":true,"token":token,"event":value}),
    )
    .map_err(|e| e.to_string())?;
    stream.write_all(b"\n").map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(stream)
        .take(512 * 1024)
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    let reply: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
    if let Some(error) = reply["error"].as_str() {
        return Err(error.to_owned());
    }
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, &reply).map_err(|e| e.to_string())?;
    stdout
        .write_all(b"\n")
        .and_then(|()| stdout.flush())
        .map_err(|e| e.to_string())
}

pub fn serve_hook(app: &Arc<App>, frame: &Value) -> Value {
    hook(app, frame).unwrap_or_else(|error| json!({"error":error}))
}

fn hook(app: &Arc<App>, frame: &Value) -> Result<Value, String> {
    let token = frame["token"].as_str().ok_or("Missing native identity")?;
    let session = app.native_sessions.by_token(token).ok_or(UNREGISTERED)?;
    // A session outliving its token is a teardown in progress, not a live caller.
    if app.handoff.resolve(token).is_none() {
        return Err(UNREGISTERED.into());
    }
    if session.state.lock().provider != "claude" || session.closed.load(Ordering::Acquire) {
        return Err("Native hook does not belong to a live Claude session".into());
    }
    let event = &frame["event"];
    let conversation = event["session_id"]
        .as_str()
        .filter(|c| !c.is_empty())
        .ok_or("Missing provider conversation")?;
    let cwd = event["cwd"].as_str().ok_or("Missing hook workspace")?;
    // Claude reports the session's current directory, which follows a `cd` in its Bash tool, so
    // anywhere inside the worktree is this session's workspace. The root was resolved at launch.
    if !within(Path::new(cwd), &session.root)? {
        return Err("Hook belongs to another workspace".into());
    }
    let _guard = session.admission.lock();
    let current = session.state.lock().conversation.clone();
    let starts = event["hook_event_name"] == "SessionStart";
    if !current.is_empty() && current != conversation && !starts {
        return Err("Hook belongs to another conversation".into());
    }
    if current.is_empty() && !starts {
        return Err("SessionStart must establish the native conversation first".into());
    }
    // /clear and /resume can replace the conversation without replacing the CLI process.
    // Only SessionStart establishes that identity; old receipts keep their original owner.
    if let Some(mode) = event["permission_mode"].as_str() {
        app.handoff.update_mode(&session.state.lock().session, mode);
    }
    session.update(wtm_agent::native::claude_hook(event));
    if event["hook_event_name"] == "UserPromptSubmit" {
        let context = collect(app, &session)?;
        return Ok(json!({
            "hookSpecificOutput": {
                "hookEventName": "UserPromptSubmit",
                "additionalContext": context,
            }
        }));
    }
    Ok(json!({}))
}

/// Hand every pending instruction to the prompt the user just submitted.
///
/// A receipt moves to unknown before its text is returned, because returning it is the dispatch:
/// the reply could be lost on its way to the CLI, and only an acknowledgment proves it arrived.
fn collect(app: &Arc<App>, session: &Session) -> Result<String, String> {
    let state = session.state.lock().clone();
    let owner = state.owner();
    let store = &app.native_sessions.deliveries;
    let mut context = Vec::new();
    for delivery in store
        .checked_list(&owner)?
        .into_iter()
        .filter(|d| d.status == DeliveryStatus::Pending)
    {
        // A receipt that changed under us (withdrawn a moment ago) is simply not collected.
        if let Some(problem) = review_problem(app, &state.session, &delivery) {
            let _ = store.transition(
                &delivery.id,
                &owner,
                DeliveryStatus::Pending,
                DeliveryStatus::Rejected,
                Some(problem),
            );
            continue;
        }
        if store
            .transition(
                &delivery.id,
                &owner,
                DeliveryStatus::Pending,
                DeliveryStatus::Unknown,
                None,
            )
            .is_ok()
        {
            // The receipt line first: a payload can be 256 KB, and an instruction to acknowledge
            // that arrives after it is the one most likely to be skimmed past.
            context.push(format!(
                "{ACKNOWLEDGE} {}. {NO_PERMISSION}\n\n{}",
                delivery.id, delivery.text
            ));
        }
    }
    session.announce();
    Ok(context.join("\n\n"))
}

/// Why a reviewed design can no longer be handed over, or `None` when it still can.
///
/// A record that cannot be read is treated like a stale one. Failing the whole hook instead left
/// the receipt pending, so every later prompt in the conversation failed in the same way.
fn review_problem(app: &Arc<App>, session: &str, delivery: &Delivery) -> Option<String> {
    let (Some(id), Some(review)) = (&delivery.prototype, &delivery.review) else {
        return None;
    };
    match crate::prototype_commands::record(app, session, id) {
        Err(error) => Some(format!("{UNREADABLE}: {}", error.message)),
        Ok(record)
            if record.review.as_ref().is_none_or(|r| &r.id != review)
                || record.dismissed
                || crate::prototype_commands::stale(app, &record) =>
        {
            Some(STALE.into())
        }
        Ok(_) => None,
    }
}

pub fn definitions() -> Vec<Value> {
    vec![json!({
        "name": "native_acknowledge",
        "description": ACKNOWLEDGE_TOOL,
        "inputSchema": {
            "type": "object",
            "properties": {"receipt": {"type": "string"}},
            "required": ["receipt"],
            "additionalProperties": false,
        },
    })]
}

/// The model's scoped acknowledgment. Bound to the conversation the session is in now, which is
/// the one whose prompt collected the receipt.
pub fn acknowledge(session: &Session, receipt: &str) -> Result<String, String> {
    let app = session.app.upgrade().ok_or("wtm is closing")?;
    let owner = session.state.lock().owner();
    app.native_sessions.deliveries.confirm(receipt, &owner)?;
    session.state.lock().tools_connected = true;
    session.announce();
    Ok("Receipt acknowledged for this conversation.".into())
}

pub fn enabled() -> bool {
    std::env::var(ENV).as_deref() == Ok("on")
}
