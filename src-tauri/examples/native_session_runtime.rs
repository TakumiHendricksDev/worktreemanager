//! Opt-in smoke probe of the installed, unmodified native CLI through the actual WTM host.
//! Uses an isolated WTM config/workspace; submits no prompt and never answers native permissions.
//! Set `XDG_CONFIG_HOME`, `XDG_DATA_HOME` and `WTM_NATIVE_CHECK_OUTPUT` to an isolated temporary root.
use std::{path::PathBuf, sync::Arc, time::Duration};
use wtm_app_lib::{app::App, bridge, native_sessions};
use wtm_core::ports::{
    exec::{CancelToken, Invocation},
    pty::PtyHost,
};

fn exercise(
    handle: &tauri::AppHandle,
    app: &Arc<App>,
    root: &std::path::Path,
    provider: &str,
) -> Result<(), String> {
    let folder = root.join("workspace");
    std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    for args in [
        vec!["git", "init", "--initial-branch", "main"],
        vec![
            "git",
            "-c",
            "user.name=Native probe",
            "-c",
            "user.email=probe@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            "Initialize isolated probe",
        ],
    ] {
        app.runner
            .run(
                &Invocation::new(
                    args.into_iter().map(str::to_owned).collect(),
                    &folder,
                    10_000,
                ),
                &CancelToken::new(),
            )
            .map_err(|e| e.to_string())?;
    }
    let project = app.register(&folder).map_err(|e| e.to_string())?;
    let project = app
        .project(&project.to_string_lossy())
        .map_err(|e| e.to_string())?;
    let worktree = app
        .worktrees(&project)
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|w| w.is_main)
        .ok_or("No primary worktree")?;
    let id = native_sessions::open(
        handle.clone(),
        app,
        &project.root.to_string_lossy(),
        &worktree.id,
        provider,
        None,
    )
    .map_err(|e| e.message)?;
    for _ in 0..100 {
        let snapshot = app
            .native_sessions
            .get(&id)
            .ok_or("Missing native session")?
            .snapshot();
        std::fs::write(
            root.join("snapshot.json"),
            serde_json::to_vec_pretty(&snapshot).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        if let Some((_, output)) = app.shell_replay(&id) {
            std::fs::write(root.join("terminal.txt"), output).map_err(|e| e.to_string())?;
        }
        if matches!(
            snapshot.status,
            wtm_agent::native::Status::Ended
                | wtm_agent::native::Status::Disconnected
                | wtm_agent::native::Status::Unavailable
        ) {
            app.close_shell(&id);
            return Err(format!(
                "Native connection did not stay live: {:?}, {:?}",
                snapshot.status, snapshot.error
            ));
        }
        if snapshot.context_supplied {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let snapshot = app
        .native_sessions
        .get(&id)
        .ok_or("Missing native session")?
        .snapshot();
    if !app.pty.sessions().iter().any(|s| s.session.as_str() == id) {
        return Err("Native TUI exited".into());
    }
    if std::env::var_os("WTM_NATIVE_CHECK_FIXTURE").is_some() && provider == "codex" {
        if !snapshot.context_supplied {
            return Err("Fixture did not register its conversation".into());
        }
        let session = app
            .native_sessions
            .get(&id)
            .ok_or("Missing native session")?;
        let lost_reply = std::env::var_os("WTM_NATIVE_CHECK_LOST_REPLY").is_some();
        if lost_reply {
            std::fs::write(folder.join(".git/drop-native-reply"), "lose the reply only")
                .map_err(|e| e.to_string())?;
        }
        let receipt = session.send(
            "Fixture review",
            "Exact fixture revision",
            Some(("fixture-design", "fixture-review")),
        )?;
        if lost_reply {
            if receipt.status != native_sessions::delivery::DeliveryStatus::Unknown {
                return Err("A lost acknowledgment was reported as confirmed".into());
            }
            let check = session.check_delivery()?;
            if !check.checked || !check.changed {
                return Err(format!(
                    "Check delivery did not confirm: {:?}",
                    check.reason
                ));
            }
        }
        if app.native_sessions.deliveries.read(&receipt.id)?.status
            != native_sessions::delivery::DeliveryStatus::Delivered
        {
            return Err("Provider acknowledgment did not settle the receipt".into());
        }
        for _ in 0..100 {
            if session.snapshot().status == wtm_agent::native::Status::Idle {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let duplicate = session.send(
            "Duplicate fixture review",
            "Should not be sent",
            Some(("fixture-design", "fixture-review")),
        )?;
        if duplicate.id != receipt.id {
            return Err("Duplicate approval minted another receipt".into());
        }
        let frames: Vec<serde_json::Value> =
            std::fs::read_to_string(folder.join(".git/native-wire.jsonl"))
                .map_err(|e| e.to_string())?
                .lines()
                .map(serde_json::from_str)
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?;
        let count = frames
            .iter()
            .filter(|f| f["method"] == "turn/start")
            .count();
        if count != 1
            || !frames
                .iter()
                .any(|f| f["id"] == 99 && f["result"]["decision"] == "decline")
            || !frames.iter().any(|f| f["nativePermissionPrompt"] == 99)
        {
            return Err(
                "Protocol did not preserve one submission and native permission ownership".into(),
            );
        }
        let opened = frames
            .iter()
            .find(|f| f["method"] == "thread/start")
            .ok_or("Missing native open")?;
        let context = opened["params"]["developerInstructions"]
            .as_str()
            .ok_or("Missing context")?;
        if !context.starts_with("User context") || !context.contains("WTM") {
            return Err("Context was replaced or omitted".into());
        }
    }
    if std::env::var_os("WTM_NATIVE_CHECK_FIXTURE").is_some() && provider == "claude" {
        if !snapshot.context_supplied {
            return Err("Fixture hooks did not register the conversation".into());
        }
        let session = app
            .native_sessions
            .get(&id)
            .ok_or("Missing native session")?;
        let receipt = session.send(
            "Fixture instruction",
            "Exact pending fixture instruction",
            None,
        )?;
        if receipt.status != native_sessions::delivery::DeliveryStatus::Pending {
            return Err("Claude instruction was delivered without continuation".into());
        }
        std::fs::write(
            folder.join(".git/continue-native"),
            "explicit fixture submission",
        )
        .map_err(|e| e.to_string())?;
        for _ in 0..150 {
            let delivered = app.native_sessions.deliveries.read(&receipt.id)?;
            if delivered.status == native_sessions::delivery::DeliveryStatus::Delivered {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        if app.native_sessions.deliveries.read(&receipt.id)?.status
            != native_sessions::delivery::DeliveryStatus::Delivered
        {
            return Err(
                "Scoped MCP acknowledgment did not settle the collected instruction".into(),
            );
        }
        let collected = std::fs::read_to_string(folder.join(".git/collected-native.txt"))
            .map_err(|e| e.to_string())?;
        if !collected.contains("Exact pending fixture instruction") {
            return Err("Hook did not preserve the exact instruction".into());
        }
        for _ in 0..150 {
            if session.snapshot().conversation == "fixture-replacement-conversation" {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let switched = session.snapshot();
        if switched.conversation != "fixture-replacement-conversation"
            || !switched.deliveries.is_empty()
            || switched.status != wtm_agent::native::Status::Idle
        {
            return Err("Native conversation switch did not retain receipt ownership".into());
        }
    }
    let note = if std::env::var_os("WTM_NATIVE_CHECK_FIXTURE").is_some() {
        "PASS: isolated protocol fixture verified context, canonical identity, receipt routing and native permission ownership. No model was invoked."
    } else if snapshot.context_supplied {
        "PASS: native TUI is live; canonical provider conversation and WTM context observed. No model turn submitted."
    } else {
        "PARTIAL: native TUI is live; initial setup or permission remains in the CLI. No context acknowledgment or model turn was observed."
    };
    app.close_shell(&id);
    std::fs::write(root.join("result.txt"), note).map_err(|e| e.to_string())?;
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    match std::env::args().nth(1).as_deref() {
        Some(bridge::ARGV_FLAG) => {
            bridge::serve_stdio();
            return Ok(());
        }
        Some(native_sessions::HOOK_FLAG) => {
            native_sessions::run_helper()?;
            return Ok(());
        }
        _ => {}
    }
    let root = PathBuf::from(std::env::var("WTM_NATIVE_CHECK_OUTPUT")?).canonicalize()?;
    let config = wtm_config::AppPaths::discover()?;
    if !config.config_dir.starts_with(&root) || !config.home_dir.starts_with(&root) {
        return Err("Use isolated XDG_CONFIG_HOME and XDG_DATA_HOME under the probe root".into());
    }
    if std::env::var_os("WTM_NATIVE_CHECK_FIXTURE").is_some() {
        use std::os::unix::fs::PermissionsExt;
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin)?;
        let provider =
            std::env::var("WTM_NATIVE_CHECK_PROVIDER").unwrap_or_else(|_| "codex".into());
        let executable = bin.join(&provider);
        std::fs::write(
            &executable,
            if provider == "claude" {
                include_str!("fixtures/native_claude.py")
            } else {
                include_str!("fixtures/native_codex.py")
            },
        )?;
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))?;
        config.ensure_dir()?;
        std::fs::write(
            &config.config_file,
            format!(
                "[exec]\npath = {:?}\n",
                format!(
                    "{}:{}",
                    bin.display(),
                    wtm_exec::ResolvedPath::resolve(None).value
                )
            ),
        )?;
    }
    let provider = std::env::var("WTM_NATIVE_CHECK_PROVIDER").unwrap_or_else(|_| "codex".into());
    let app = Arc::new(App::with_paths(config)?);
    let mut context = wtm_app_lib::application_context();
    context.config_mut().app.windows.clear();
    tauri::Builder::default()
        .manage(Arc::clone(&app))
        .setup(move |runtime| {
            let handle = runtime.handle().clone();
            bridge::listen(handle.clone(), Arc::clone(&app));
            std::thread::spawn(move || {
                let result = exercise(&handle, &app, &root, &provider);
                let code = if let Err(error) = result {
                    let _ = std::fs::write(root.join("result.txt"), format!("FAIL: {error}"));
                    1
                } else {
                    0
                };
                app.close_shell_runs();
                handle.exit(code);
            });
            Ok(())
        })
        .run(context)?;
    Ok(())
}
