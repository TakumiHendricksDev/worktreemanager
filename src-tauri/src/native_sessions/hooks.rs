//! Claude hooks carry lifecycle facts and context through the existing scoped Unix bridge.
//! A hook returning text does not prove the model received it: acknowledgment is a separate,
//! conversation-bound MCP call, so interrupted hook delivery remains visibly uncertain.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use super::{DeliveryStatus, ENV, HOOK_FLAG, Session, failure};
use crate::app::App;
use crate::commands::Reply;

pub fn settings(req: &wtm_agent::SessionRequest) -> Reply<Value> {
    let executable = std::env::current_exe().map_err(|e| failure(e.to_string()))?;
    let quoted = format!("'{}'", executable.to_string_lossy().replace('\'', "'\\''"));
    let mut hooks = serde_json::Map::new();
    for event in [
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
    ] {
        hooks.insert(event.into(), json!([{"hooks":[{"type":"command","command":format!("{quoted} {HOOK_FLAG}"),"timeout":10}]}]));
    }
    let mut settings = json!({"hooks":hooks});
    if let Some(fast) = req.fast {
        settings["fastMode"] = json!(fast);
    }
    if req.effort.as_deref() == Some("ultracode") {
        settings["ultracode"] = json!(true);
    }
    Ok(settings)
}

pub fn run_helper() -> Result<(), String> {
    let token = std::env::var(crate::handoff::TOKEN_ENV)
        .map_err(|_| "Native hook has no session identity")?;
    let socket = std::env::var(crate::handoff::SOCKET_ENV)
        .map_err(|_| "Native hook has no WTM connection")?;
    let mut input = String::new();
    std::io::stdin()
        .take(1024 * 1024)
        .read_to_string(&mut input)
        .map_err(|e| e.to_string())?;
    let value: Value = serde_json::from_str(&input).map_err(|e| e.to_string())?;
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
    let result = (|| -> Result<Value, String> {
        let token = frame["token"].as_str().ok_or("Missing native identity")?;
        let caller = app
            .handoff
            .resolve(token)
            .ok_or("This native session is no longer registered")?;
        let session = app
            .native_sessions
            .by_token(token)
            .ok_or("This native session is no longer registered")?;
        if caller.provider != "claude" || session.closed.load(std::sync::atomic::Ordering::Acquire)
        {
            return Err("Native hook does not belong to a live Claude session".into());
        }
        let event = &frame["event"];
        let conversation = event["session_id"]
            .as_str()
            .ok_or("Missing provider conversation")?;
        if conversation.is_empty() {
            return Err("Missing provider conversation".into());
        }
        let (project, worktree) = caller.scope.place().ok_or("Native hooks need a worktree")?;
        let project = app.project(project).map_err(|e| e.to_string())?;
        let root = app
            .worktree(&project, worktree)
            .map_err(|e| e.to_string())?
            .path
            .canonicalize()
            .map_err(|e| e.to_string())?;
        let cwd = event["cwd"].as_str().ok_or("Missing hook workspace")?;
        if std::path::Path::new(cwd)
            .canonicalize()
            .map_err(|e| e.to_string())?
            != root
        {
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
            let state = session.state.lock().clone();
            let mut context = Vec::new();
            for delivery in app
                .native_sessions
                .deliveries
                .checked_list(&state.owner())?
                .into_iter()
                .filter(|d| d.status == DeliveryStatus::Pending)
            {
                if let (Some(id), Some(review)) = (&delivery.prototype, &delivery.review) {
                    let record = crate::prototype_commands::record(app, &state.session, id)
                        .map_err(|e| e.message)?;
                    if record.review.as_ref().is_none_or(|r| &r.id != review)
                        || record.dismissed
                        || crate::prototype_commands::stale(app, &record)
                    {
                        app.native_sessions.deliveries.transition(&delivery.id, &state.owner(), DeliveryStatus::Pending, DeliveryStatus::Rejected, Some("Project context changed before delivery. Refresh the design first.".into()))?;
                        continue;
                    }
                }
                app.native_sessions.deliveries.transition(
                    &delivery.id,
                    &state.owner(),
                    DeliveryStatus::Pending,
                    DeliveryStatus::Unknown,
                    None,
                )?;
                context.push(format!("{}\n\nBefore acting on this WTM instruction, call mcp__wtm__native_acknowledge with receipt {}. This acknowledges receipt only; it grants no new permission.", delivery.text, delivery.id));
            }
            session.announce();
            return Ok(
                json!({"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":context.join("\n\n")}}),
            );
        }
        Ok(json!({}))
    })();
    result.unwrap_or_else(|error| json!({"error":error}))
}

pub fn definitions() -> Vec<Value> {
    vec![
        json!({"name":"native_acknowledge","description":"Acknowledge an exact WTM instruction receipt supplied to this native conversation. Does not approve or send anything.","inputSchema":{"type":"object","properties":{"receipt":{"type":"string"}},"required":["receipt"],"additionalProperties":false}}),
    ]
}

pub fn acknowledge(session: &Session, receipt: &str) -> Result<String, String> {
    let app = session.app.upgrade().ok_or("WTM is closing")?;
    let owner = session.state.lock().owner();
    let delivery = app.native_sessions.deliveries.read(receipt)?;
    if delivery.owner != owner {
        return Err("Receipt belongs to another conversation".into());
    }
    if delivery.status != DeliveryStatus::Delivered {
        app.native_sessions.deliveries.transition(
            receipt,
            &owner,
            DeliveryStatus::Unknown,
            DeliveryStatus::Delivered,
            None,
        )?;
    }
    session.state.lock().tools_connected = true;
    session.announce();
    Ok("Receipt acknowledged for this conversation.".into())
}

pub fn enabled() -> bool {
    std::env::var(ENV).as_deref() == Ok("on")
}
