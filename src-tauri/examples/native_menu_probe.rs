//! A disposable native regression surface, never the normal wtm composition root.
//!
//! Run only with the watchdog in `scripts/native-menu-probe.py`. There is no App,
//! repository/config store, session restore or agent socket. `WebKit` gets a temporary
//! data directory so a deliberate old-route deadlock cannot affect a running wtm.

// The parent terminal must retain evidence even when the probe's UI deadlocks.
#![allow(clippy::print_stdout)]

use tauri::{WebviewUrl, WebviewWindowBuilder};
use wtm_app_lib::native_menu;

#[tauri::command]
async fn probe_heartbeat() -> &'static str {
    "alive"
}

#[tauri::command]
async fn probe_report(message: String) {
    println!("{message}");
}

fn main() {
    let profile = tempfile::tempdir().expect("create an isolated WebKit profile");
    println!("Native menu probe PID {}", std::process::id());
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            native_menu::popup_native_menu,
            probe_heartbeat,
            probe_report,
        ])
        .setup(move |app| {
            WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                .title("Isolated native menu regression probe")
                .inner_size(760.0, 520.0)
                .data_directory(profile.path().to_path_buf())
                .build()?;
            // The directory must outlive WebKit. Manage it in this isolated runtime,
            // rather than letting the setup closure remove it when it returns.
            tauri::Manager::manage(app, profile);
            Ok(())
        })
        .run(tauri::generate_context!(
            "examples/native-menu-probe/tauri.conf.json",
            test = true
        ))
        .expect("run the isolated probe");
}
