//! Opt-in native regression probe. It calls the real preview loader and Reload path, verifies
//! the isolated runtime sees the loaded sandbox frame, and saves PNGs for visual inspection.
//! No provider, bridge socket, global config or repository writes are involved. An optional local
//! HTML path tests an existing artifact without changing its record or reviewed bytes.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use wtm_app_lib::{app::App, browser, prototypes};

const SYNTHETIC: &str = r#"<!doctype html><html lang="en"><head><meta charset="utf-8"><style>
:root{--accent:#734399;--bg:#f6f3ed;--fg:#282333;--radius:6px}body{font:16px system-ui;background:var(--bg);color:var(--fg);margin:0;padding:48px}main{max-width:680px;margin:auto}label{display:block;margin:20px 0}button{background:var(--accent);color:white;padding:12px 24px;border:0;border-radius:var(--radius)}select{padding:8px}h1{font-size:32px}fieldset{padding:20px;border:1px solid #aaa}output{display:block;margin-top:20px}
</style></head><body><main><p>Native WTM preview regression</p><h1>Registration export</h1><label>Format <select><option>CSV spreadsheet</option><option>JSON</option></select></label><fieldset><legend>Fields to include</legend><label><input type="checkbox" checked> Registration name</label><label><input type="checkbox" checked> Registration date</label></fieldset><p><button class="btn btn-primary project-action local-button" onclick="document.querySelector('output').textContent='Export ready — interaction works'">Export registrations</button></p><output>Ready to review</output><p id="isolation"></p><script>
try{parent.document.body;document.querySelector('#isolation').textContent='ERROR: parent accessible'}catch{document.querySelector('#isolation').textContent='Opaque sandbox: parent access blocked'}
document.querySelector('button').click();
fetch('https://example.invalid/wtm-preview-probe').then(()=>document.querySelector('#isolation').textContent+='; ERROR: network allowed').catch(()=>document.querySelector('#isolation').textContent+='; network blocked');
</script></main></body></html>"#;

fn exercise(
    handle: &tauri::AppHandle,
    app: &Arc<App>,
    html: &str,
    output: &std::path::Path,
) -> Result<(), String> {
    let view = browser::open_prototype(
        handle,
        app,
        "fixture",
        "fixture",
        &prototypes::review_document(html),
        Some("preview-check"),
        "fixture/review",
    )
    .map_err(|e| e.message)?;
    let inspect = || -> Result<(), String> {
        let dom = browser::call(
            handle,
            app,
            &view.id,
            "getContent",
            &serde_json::json!({"format":"html","maxChars":60000}),
            Duration::from_secs(5),
        )?;
        let text = dom.as_str().ok_or("No preview DOM")?;
        if !text.contains("data-wtm-prototype-preview") || !text.contains("data-loaded=\"true\"") {
            return Err("The artifact frame did not load".to_owned());
        }
        if browser::navigate(handle, app, &view.id, "https://example.invalid").is_ok() {
            return Err("A fixed preview accepted navigation".to_owned());
        }
        Ok(())
    };
    inspect()?;
    browser::set_bounds(
        handle,
        app,
        &view.id,
        Some(browser::Bounds {
            x: 0.0,
            y: 0.0,
            w: 1100.0,
            h: 760.0,
        }),
        "main",
    )
    .map_err(|e| e.message)?;
    let image = browser::snapshot_png(handle, app, &view.id, None).map_err(|e| e.message)?;
    std::fs::write(output.join("preview.png"), image).map_err(|e| e.to_string())?;
    browser::history(handle, app, &view.id, browser::HistoryAction::Reload)
        .map_err(|e| e.message)?;
    inspect()?;
    let image = browser::snapshot_png(handle, app, &view.id, None).map_err(|e| e.message)?;
    std::fs::write(output.join("preview-reloaded.png"), image).map_err(|e| e.to_string())?;
    // A missing host readiness marker used to look like a successful empty pane forever.
    let failed = browser::open_prototype(
        handle,
        app,
        "fixture",
        "failure",
        "<html><body>Missing frame</body></html>",
        Some("preview-check"),
        "failure/review",
    );
    if failed.is_ok()
        || !app.browsers.list(Some("failure")).iter().any(|v| {
            v.load_error
                .as_deref()
                .is_some_and(|m| m.contains("Reload"))
        })
    {
        return Err("A failed preview did not expose a retryable error".to_owned());
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = PathBuf::from(std::env::var("WTM_PREVIEW_CHECK_OUTPUT")?);
    std::fs::create_dir_all(&output)?;
    let html = match std::env::args().nth(1) {
        Some(path) => std::fs::read_to_string(path)?,
        None => SYNTHETIC.to_owned(),
    };
    let app = Arc::new(App::with_paths(wtm_config::AppPaths::rooted(
        &output.join("config"),
    ))?);
    let mut context = wtm_app_lib::application_context();
    context.config_mut().app.windows.clear();
    tauri::Builder::default().manage(Arc::clone(&app)).setup(move |runtime| {
        tauri::WindowBuilder::new(runtime, "main").title("WTM Preview Regression").inner_size(1100.0,760.0).build()?;
        let handle = runtime.handle().clone();
        std::thread::spawn(move || {
            let result = exercise(&handle, &app, &html, &output);
            let report = match result { Ok(()) => "PASS: native load, sandbox frame readiness, navigation refusal, reload, screenshot and retryable failure".to_owned(), Err(error) => format!("FAIL: {error}") };
            let _ = std::fs::write(output.join("result.txt"), report);
        });
        Ok(())
    }).run(context)?;
    Ok(())
}
