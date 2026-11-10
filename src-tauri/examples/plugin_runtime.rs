//! Opt-in native plugin regression. Uses an isolated config and a reviewed fixture; no provider,
//! user repository, network connection or installed package is touched. Exits after its report.
//!
//! # Why this is an example and not a test
//!
//! What it checks needs a Tauri runtime with a real window: the plugin SDK inside a WebKit
//! frame, the theme reaching that frame, a native view closing, an API 2 runtime answering host
//! calls through window events, and the production agent sink persisting a draft's conversation
//! id over a real `AppHandle`. A test binary cannot build any of that, so `just check` and CI do
//! not run this; run it with `WTM_PLUGIN_CHECK_OUTPUT=<dir> cargo run --example plugin_runtime`.
//!
//! Everything about authoring and chat editing that does not need a window — the brief sent once,
//! review refused while the agent works, the edit journal's retry, mismatch and unknown-outcome
//! rules, the stale-revision fence, the first-message fresh conversation and the fork probe — is
//! in `tests/plugin_authoring.rs` and `tests/chat_edit.rs`, which do run in CI. Keep it there.
#![allow(clippy::disallowed_methods)]
use std::{
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use wtm_app_lib::{app::App, browser, plugin_views};
use wtm_plugins::contributions::{Binding, Slot};

const HTML: &str = r#"<!doctype html><html><head><style>body{margin:0;padding:40px;font:15px system-ui;background:var(--wtm-bg);color:var(--wtm-fg)}h1{font-size:22px}pre{white-space:pre-wrap}button{padding:10px}</style></head><body><h1>WTM native plugin runtime</h1><p>This is an isolated test package using the public SDK.</p><pre id="result">Connecting to host…</pre><script>
(async()=>{
 const results={context:await wtm.request('context'),projects:await wtm.request('workspace.projects')};
 try{parent.document;results.opaque=false}catch{results.opaque=true}
 try{await wtm.request('command',{id:'undeclared'});results.scoped=false}catch{results.scoped=true}
 document.getElementById('result').textContent=JSON.stringify(results,null,2);
 addEventListener('wtm-theme',()=>{wtm.request('command',{id:'theme',input:getComputedStyle(document.documentElement).getPropertyValue('--wtm-bg')})});
 await wtm.request('command',{id:'report',input:results});
})().catch(e=>document.getElementById('result').textContent=e.message);
</script></body></html>"#;

fn exercise(
    handle: &tauri::AppHandle,
    app: &Arc<App>,
    root: &std::path::Path,
) -> Result<(), String> {
    std::fs::write(
        root.join("phase.txt"),
        "Checking the native SDK and recovery",
    )
    .map_err(|e| e.to_string())?;
    let source = root.join("package");
    std::fs::create_dir_all(&source).map_err(|e| e.to_string())?;
    std::fs::write(source.join("wtm-plugin.toml"), "schema=1\napi=1\nid='example.native-check'\nname='Native check'\nversion='0.1.0'\n[[views]]\nid='view'\ntitle='Native check'\nfile='view.html'\nslots=['workbench']\n[[commands]]\nid='report'\ntitle='Report'\ncommand=['./report.py','report.json']\n[[commands]]\nid='theme'\ntitle='Theme report'\ncommand=['./report.py','theme.json']\n").map_err(|e| e.to_string())?;
    std::fs::write(source.join("view.html"), HTML).map_err(|e| e.to_string())?;
    let script = source.join("report.py");
    std::fs::write(&script, "#!/usr/bin/env python3\nimport json,os,pathlib,sys\ndata=json.load(sys.stdin)\npathlib.Path(os.environ['WTM_PLUGIN_DATA'],sys.argv[1]).write_text(json.dumps(data['params']))\nprint('ok')\n").map_err(|e| e.to_string())?;
    std::fs::set_permissions(script, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| e.to_string())?;
    let review = app
        .plugins
        .prepare(source.to_str().ok_or("Invalid source path")?, "local", "")?;
    app.plugins.install(&review.token)?;
    app.plugins.bind(
        Slot::Workbench,
        Binding::Plugin {
            plugin: "example.native-check".into(),
            view: "view".into(),
        },
    )?;
    let owner = "plugin-slot:example.native-check:view:workbench";
    let page = browser::open_prototype(
        handle,
        app,
        "",
        "",
        &plugin_views::document(HTML),
        owner,
        &format!("{owner}/native-check"),
    )
    .map_err(|e| e.message)?;
    browser::set_bounds(
        handle,
        app,
        &page.id,
        Some(browser::Bounds {
            x: 0.0,
            y: 0.0,
            w: 1000.0,
            h: 700.0,
        }),
        "main",
    )
    .map_err(|e| e.message)?;
    let data = app
        .config
        .paths()
        .config_dir
        .join("plugins/data/example.native-check");
    let report = data.join("report.json");
    let deadline = Instant::now() + Duration::from_secs(40);
    while !report.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    if !report.exists() {
        let diagnostics = serde_json::json!({
            "view": app.browsers.view_of(&page.id),
            "console": browser::console(app, &page.id, false),
            "snapshot": browser::call(handle, app, &page.id, "snapshot", &serde_json::json!({}), Duration::from_secs(5)),
        });
        let _ = std::fs::write(root.join("failure.json"), diagnostics.to_string());
        if let Ok(png) = browser::snapshot_png(handle, app, &page.id, None) {
            let _ = std::fs::write(root.join("failure.png"), png);
        }
    }
    let result: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&report).map_err(|e| format!("Native SDK did not finish: {e}"))?,
    )
    .map_err(|e| e.to_string())?;
    if result["opaque"] != true
        || result["scoped"] != true
        || result["context"]["plugin"] != "example.native-check"
    {
        return Err(format!("Unexpected SDK result: {result}"));
    }
    browser::call(
        handle,
        app,
        &page.id,
        "setTheme",
        &serde_json::json!({"bg":"rgb(30, 37, 32)","fg":"rgb(235, 240, 236)"}),
        Duration::from_secs(5),
    )?;
    let theme = data.join("theme.json");
    let deadline = Instant::now() + Duration::from_secs(40);
    while !theme.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    if !std::fs::read_to_string(theme)
        .map_err(|e| format!("Theme report is missing: {e}"))?
        .contains("30, 37, 32")
    {
        return Err("Theme did not reach the plugin frame".into());
    }
    std::fs::write(
        root.join("native-plugin.png"),
        browser::snapshot_png(handle, app, &page.id, None).map_err(|e| e.message)?,
    )
    .map_err(|e| e.to_string())?;
    browser::close(handle, app, &page.id);
    if app.browsers.view_of(&page.id).is_some() {
        return Err("Closing left a native view registered".into());
    }
    app.plugins.recover()?;
    if app.plugins.binding(Slot::Workbench) != Binding::Builtin {
        return Err("Recovery did not restore the workbench".into());
    }
    std::fs::write(root.join("phase.txt"), "Checking authoring resume")
        .map_err(|e| e.to_string())?;
    check_authoring(handle, app, root).map_err(|e| format!("Authoring: {e}"))?;
    std::fs::write(
        root.join("phase.txt"),
        "Checking activation, host calls and runtime recovery",
    )
    .map_err(|e| e.to_string())?;
    check_extensions(handle, app, root).map_err(|e| format!("Extensions: {e}"))?;
    Ok(())
}

fn check_extensions(
    handle: &tauri::AppHandle,
    app: &Arc<App>,
    root: &std::path::Path,
) -> Result<(), String> {
    use serde_json::{Value, json};
    use tauri::{Listener, Manager};
    use wtm_app_lib::extensions::Runtime;
    let source = root.join("extensions");
    std::fs::create_dir_all(&source).map_err(|e| e.to_string())?;
    std::fs::write(source.join("wtm-plugin.toml"),"schema=1\napi=2\nid='example.extensions-check'\nname='Extension check'\nversion='1.0.0'\n[extensions]\nruntime=['python3','runtime.py']\naccess=['list_projects']\nevents=['workspace.changed']\n").map_err(|e|e.to_string())?;
    std::fs::write(source.join("runtime.py"),r"
import json,sys
pending=None
events=[]
for line in sys.stdin:
    frame=json.loads(line)
    method=frame.get('method')
    if method=='event': events.append(frame['params']['name'])
    if method=='plugin.crash': sys.exit(4)
    if method=='plugin.echo': print(json.dumps({'id':frame['id'],'result':{'echo':frame['params'],'events':events}}),flush=True)
    if method=='plugin.host':
        pending=frame['id']
        print(json.dumps({'id':'host-query','method':'host','params':{'operation':'list_projects','arguments':{},'context':{}}}),flush=True)
    if frame.get('id')=='host-query' and 'method' not in frame:
        print(json.dumps({'id':pending,'result':frame}),flush=True)
").map_err(|e|e.to_string())?;
    let review = app
        .plugins
        .prepare(source.to_str().ok_or("Invalid source")?, "local", "")?;
    app.plugins.install(&review.token)?;
    let window = handle.get_window("main").ok_or("No host window")?;
    let host = Arc::clone(app);
    let listener = window.listen("extensions:host", move |event| {
        if let Ok(request) = serde_json::from_str::<Value>(event.payload())
            && request["plugin"] == "example.extensions-check"
            && request["operation"] == "list_projects"
            && let Some(id) = request["id"].as_str()
        {
            host.extensions.answer(id, json!([]), None);
        }
    });
    let first = Runtime::call(
        handle,
        app,
        "example.extensions-check",
        "plugin.echo",
        &json!({"value":"one"}),
    )?;
    if first["echo"]["value"] != "one" {
        return Err("Activation runtime did not answer its action".into());
    }
    Runtime::event(
        handle,
        app,
        "workspace.changed",
        &json!({"worktreeId":"demo"}),
        "demo",
    );
    let second = Runtime::call(
        handle,
        app,
        "example.extensions-check",
        "plugin.echo",
        &json!({}),
    )?;
    if second["events"] != json!(["workspace.changed"]) {
        return Err("Runtime subscription did not receive its event".into());
    }
    let host = Runtime::call(
        handle,
        app,
        "example.extensions-check",
        "plugin.host",
        &json!({}),
    )?;
    if host["result"] != json!([]) {
        return Err(format!("Owner-scoped host call failed: {host}"));
    }
    if Runtime::host(
        handle,
        app,
        "example.extensions-check",
        "answer_approval",
        &json!({}),
        &json!({}),
    )
    .is_ok()
    {
        return Err("Protected operation reached the UI".into());
    }
    if Runtime::call(
        handle,
        app,
        "example.extensions-check",
        "plugin.crash",
        &json!({}),
    )
    .is_ok()
    {
        return Err("Runtime crash was reported as a successful action".into());
    }
    if Runtime::call(
        handle,
        app,
        "example.extensions-check",
        "plugin.echo",
        &json!({}),
    )
    .is_ok()
    {
        return Err("A failed runtime restarted without an explicit user action".into());
    }
    app.extensions.reset(app, "example.extensions-check");
    Runtime::call(
        handle,
        app,
        "example.extensions-check",
        "plugin.echo",
        &json!({}),
    )?;
    app.extensions.stop(app, "example.extensions-check");
    app.plugins.set_enabled("example.extensions-check", false)?;
    if Runtime::call(
        handle,
        app,
        "example.extensions-check",
        "plugin.echo",
        &json!({}),
    )
    .is_ok()
    {
        return Err("Disabled runtime accepted an action".into());
    }
    window.unlisten(listener);
    Ok(())
}
fn check_authoring(
    handle: &tauri::AppHandle,
    app: &Arc<App>,
    root: &std::path::Path,
) -> Result<(), String> {
    let folder = root.join("generated");
    let job = wtm_app_lib::plugin_authoring::prepare(
        app,
        &uuid::Uuid::new_v4().to_string(),
        "codex",
        "Create a panel",
        folder.to_str().ok_or("Invalid folder")?,
    )
    .map_err(|e| e.message)?;
    let first =
        wtm_app_lib::plugin_authoring::open(handle, app, &job.id, false).map_err(|e| e.message)?;
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        let saved = app.plugin_jobs.get(&job.id).map_err(|e| e.message)?;
        if saved.conversation.is_some() && folder.join("wtm-plugin.toml").exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let saved = app.plugin_jobs.get(&job.id).map_err(|e| e.message)?;
    if saved.conversation.as_deref() != Some("native-author-thread") {
        return Err("Provider conversation identity was not persisted".into());
    }
    let again =
        wtm_app_lib::plugin_authoring::open(handle, app, &job.id, false).map_err(|e| e.message)?;
    if again.session != first.session {
        return Err("Reopening created a duplicate process".into());
    }
    app.close_agent(first.session.as_deref().ok_or("No author session")?);
    let resumed =
        wtm_app_lib::plugin_authoring::open(handle, app, &job.id, false).map_err(|e| e.message)?;
    std::thread::sleep(Duration::from_millis(500));
    app.close_agent(
        resumed
            .session
            .as_deref()
            .ok_or("No resumed author session")?,
    );
    let turns = std::fs::read_to_string(folder.join(".git/author-turns.jsonl"))
        .map_err(|e| e.to_string())?;
    if turns.lines().count() != 1 {
        return Err("Reopening replayed the original prompt".into());
    }
    let review = app
        .plugins
        .prepare(folder.to_str().ok_or("Invalid folder")?, "local", "")?;
    if app
        .plugins
        .snapshot()
        .registry
        .installed
        .contains_key("example.generated-check")
    {
        return Err("Authoring installed its output without review".into());
    }
    app.plugins.cancel(&review.token)?;
    Ok(())
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(std::env::var("WTM_PLUGIN_CHECK_OUTPUT")?);
    std::fs::create_dir_all(&root)?;
    let root = root.canonicalize()?;
    let bin = root.join("bin");
    std::fs::create_dir_all(&bin)?;
    let provider = bin.join("codex");
    std::fs::write(&provider, include_str!("fixtures/plugin_codex.py"))?;
    std::fs::set_permissions(provider, std::fs::Permissions::from_mode(0o755))?;
    std::fs::create_dir_all(root.join("config"))?;
    std::fs::write(
        root.join("config/config.toml"),
        format!(
            "[exec]\npath = {:?}\n",
            format!(
                "{}:{}",
                bin.display(),
                wtm_exec::ResolvedPath::resolve(None).value
            )
        ),
    )?;
    let app = Arc::new(App::with_paths(wtm_config::AppPaths::rooted(
        &root.join("config"),
    ))?);
    let mut context = wtm_app_lib::application_context();
    context.config_mut().app.windows.clear();
    tauri::Builder::default()
        .manage(Arc::clone(&app))
        .setup(move |runtime| {
            tauri::WindowBuilder::new(runtime, "main")
                .title("WTM Plugin Runtime Check")
                .inner_size(1000.0, 700.0)
                .build()?;
            let handle = runtime.handle().clone();
            std::thread::spawn(move || {
                let (report, code) = match exercise(&handle, &app, &root) {
                    Ok(()) => (PASS.to_owned(), 0),
                    Err(e) => (format!("FAIL: {e}"), 1),
                };
                let _ = std::fs::write(root.join("result.txt"), report);
                handle.exit(code);
            });
            Ok(())
        })
        .run(context)?;
    Ok(())
}

const PASS: &str = "PASS: native SDK, context, command ownership, opaque frame, theme, close, \
    recovery, authoring resume through the production sink, and API 2 runtime/events/host calls";
