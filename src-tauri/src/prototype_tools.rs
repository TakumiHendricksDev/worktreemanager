//! Open Design's design workflow exposed to the already-running chat agent. Tools never start
//! another session, and deliberately offer no implementation-approval operation to the model.

use crate::app::App;
use crate::handoff::{CodeCall, Response};
use crate::prototype_commands as lifecycle;
use serde_json::{Value, json};
use std::sync::Arc;

pub const PREFIX: &str = "prototype_";
pub const ENV: &str = "WTM_DESIGN_TOOLS";
pub const INSTRUCTIONS: &str = "Open Design prototyping is built into this WTM chat. For an explicit prototype/design/mockup request, or a substantial change to UI layout, screens, components or user flows, call mcp__wtm__prototype_prepare before editing application source. Use the returned project evidence and Open Design charter to create the design, then call mcp__wtm__prototype_publish with the complete self-contained HTML and honest component/token reuse notes. The rendered review opens beside this conversation. For feedback, call prototype_prepare with the existing id and the user's requested revision, then publish the new revision. Stop after publishing and wait for the user's Implement this design action; a prototype, feedback, or successful generation is not implementation approval. Only a WTM implementation approval turn for the exact reviewed revision authorizes application implementation. Keep design artifacts out of the source tree: submit them through the tools. This uses your current session, model and permission mode; do not launch Open Design's CLI/daemon, another provider, or ask for another login. Backend work, investigations, trivial text fixes and nonvisual tasks continue normally without a prototype. The wait-before-editing rule is an instruction, not an additional filesystem sandbox.";

pub fn definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "prototype_prepare",
            "description": "Start or revise an Open Design prototype in THIS conversation using this worktree's current components, tokens and design guidance. Use for requests like 'prototype this registration export using our components', and before substantial UI implementation. Automatically prepares context; no setup, login, CLI or service. Returns the bundled Open Design design charter, source/token evidence and a generation id. Submit the result with prototype_publish, then wait for the user's explicit Implement action before editing app source. For feedback pass the existing id. Preparing a revision invalidates any prior approval. Does not write app source.",
            "inputSchema": {"type":"object","properties":{
                "brief":{"type":"string","description":"The requested UI or revision, including relevant conversation constraints."},
                "id":{"type":"string","description":"Existing prototype id when revising."},
                "sources":{"type":"array","items":{"type":"string"},"description":"Optional relative source paths to select from discovered design context. Omit for automatic context."}
            },"required":["brief"],"additionalProperties":false}
        }),
        json!({
            "name":"prototype_publish",
            "description":"Save a complete self-contained prototype and open its immutable rendered review beside THIS chat. Supply the exact id and revision from prototype_prepare. HTML may use inline CSS/scripts and data images only; no external resources. Include real source component references and distinguish copied tokens/markup from framework approximations in reuse. This tool cannot approve implementation. After publication stop and wait for the user to review, revise or click Implement this design.",
            "inputSchema":{"type":"object","properties":{
                "id":{"type":"string"},"revision":{"type":"string"},
                "html":{"type":"string","description":"Complete standalone HTML document. Add stable data-od-id attributes to interactive regions for feedback."},
                "reuse":{"type":"string","description":"Component/token references, actual reuse versus HTML approximation, interaction/accessibility checks and unresolved limitations."}
            },"required":["id","revision","html","reuse"],"additionalProperties":false}
        }),
    ]
}

pub fn run(handle: &tauri::AppHandle, app: &Arc<App>, token: &str, call: &CodeCall) -> Response {
    let result = (|| -> Result<String, crate::view::ErrorView> {
        let caller = app.handoff.resolve(token).ok_or_else(|| {
            crate::prototype_context::failure("This chat is no longer registered")
        })?;
        if caller.scope.place().is_none() {
            return Err(crate::prototype_context::failure(
                "Open a worktree chat to prototype its UI",
            ));
        }
        let session = caller.session.as_deref().ok_or_else(|| {
            crate::prototype_context::failure("This chat is still starting; retry in a moment")
        })?;
        let required = |key: &str| {
            call.args[key]
                .as_str()
                .ok_or_else(|| crate::prototype_context::failure(format!("{key} is required")))
        };
        match call.tool.as_str() {
            "prototype_prepare" => {
                let paths = call
                    .args
                    .get("sources")
                    .map(|v| serde_json::from_value::<Vec<String>>(v.clone()))
                    .transpose()
                    .map_err(|_| {
                        crate::prototype_context::failure(
                            "sources must be a list of relative paths",
                        )
                    })?;
                let old = call.args["id"]
                    .as_str()
                    .and_then(|id| lifecycle::record(app, session, id).ok())
                    .and_then(|p| {
                        p.review
                            .as_ref()
                            .and_then(|r| crate::prototypes::artifact(&p, &r.id).ok())
                    });
                let prototype = lifecycle::prepare(
                    app,
                    session,
                    required("brief")?,
                    call.args["id"].as_str(),
                    paths.as_deref(),
                )?;
                lifecycle::announce(handle, app, session, &prototype);
                let tokens = crate::design_evidence::collect(&prototype.sources);
                Ok(json!({"id":prototype.id,"revision":prototype.revision,"brief":prototype.brief,"feedback":prototype.feedback,"upstream":prototype.upstream,
                    "charter":crate::prototypes::CHARTER,
                    "hostContract":"Use supplied project sources as the visual authority. Reuse CSS tokens and markup directly where compatible. Framework components are HTML visual approximations; name their actual APIs in reuse notes for implementation. Preserve untouched areas in revisions. Produce complete self-contained HTML with inline styles/scripts, system or embedded fonts and inline/data images; no CDN, imports, network or app-source writes. Submit via prototype_publish. Do not create files in the application. Stop for explicit Implement this design approval.",
                    "tokens":tokens,"sources":prototype.sources,"previousArtifact":old.map(|(html,reuse)|json!({"html":html,"reuse":reuse}))}).to_string())
            }
            "prototype_publish" => {
                let prototype = lifecycle::publish(
                    app,
                    session,
                    required("id")?,
                    required("revision")?,
                    required("html")?,
                    required("reuse")?,
                )?;
                lifecycle::announce(handle, app, session, &prototype);
                let preview =
                    lifecycle::preview(handle, app, session, &prototype.id, &prototype.revision);
                Ok(match preview {
                    Ok(view) => format!(
                        "Prototype saved and rendered in review pane {}. The user can revise it in this chat or click Implement this design. Stop here; application implementation is not approved.",
                        view.id
                    ),
                    Err(error) => format!(
                        "Prototype saved. The review pane could not open: {}. The user can retry with Show preview in this chat. Stop here; application implementation is not approved.",
                        error.message
                    ),
                })
            }
            _ => Err(crate::prototype_context::failure("Unknown prototype tool")),
        }
    })();
    match result {
        Ok(text) => Response::ok(text),
        Err(e) => Response::failed(e.message),
    }
}
