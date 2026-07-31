//! Worktree Manager — the Tauri application.
//!
//! The only crate in the workspace that knows Tauri exists. It wires concrete adapters
//! into the domain's ports ([`app`]), renders domain facts into a UI contract
//! ([`view`], [`display`]), and exposes that over IPC ([`commands`]).

#![cfg_attr(test, allow(clippy::unwrap_used))]

pub mod app;
pub mod commands;
pub mod display;
pub mod openers;
pub mod pty_bridge;
pub mod view;

use std::sync::Arc;

use app::App;

/// Tell the webview which platform it is running on, before it paints.
///
/// The frontend needs this for one reason: window chrome. On macOS the traffic lights
/// are drawn *inside* the webview's rect, so the title strip must reserve a gutter for
/// them; on Linux the window manager draws its controls on the right, outside the
/// webview entirely, and that gutter would be 76px of dead space.
///
/// That is a first-paint fact, so it cannot arrive over IPC — a `#[tauri::command]`
/// resolves a frame or two late and the window would visibly reflow on every launch,
/// which is the exact failure the pre-paint script in `index.html` exists to prevent.
/// `js_init_script` runs after the global object exists but before the document is
/// parsed and before any script in the page runs, which is the only slot early enough.
///
/// A plugin declaring no commands needs no entry in `capabilities/default.json`.
///
/// No `#[cfg]` here: `std::env::consts::OS` is already the right runtime constant, and
/// it is the same vocabulary `os.platform` hands to config templates — one set of
/// names, two consumers.
fn platform_plugin<R: tauri::Runtime>() -> tauri::plugin::TauriPlugin<R> {
    tauri::plugin::Builder::new("wtm-platform")
        // `{:?}` on a `&str` produces a valid JS string literal, and the value is one of
        // a fixed set from libstd — never user input.
        .js_init_script(format!(
            "window.__WTM_PLATFORM__ = {:?};",
            std::env::consts::OS
        ))
        .build()
}

/// The event the Settings menu item fires. `App.svelte` listens for it.
///
/// A menu accelerator is handled by `AppKit` and never reaches the webview, so a keydown
/// handler in the frontend cannot see ⌘, at all. This is the same Rust-to-webview route
/// [`pty_bridge`] already uses.
pub const SETTINGS_EVENT: &str = "wtm:settings";

/// The application menu.
///
/// # This exists to add one item, and most of it is not that item
///
/// Until now the app never called `set_menu`, so Tauri installed its default. Setting one
/// replaces that default **wholesale** — every submenu, including Edit. An app that builds
/// only an app menu therefore loses cut, copy, paste and select-all everywhere: in the Add a
/// repository field, in the New Worktree form, and in the live terminal, where copying a
/// stack trace out of a failed setup run is much of the point of the pane.
///
/// So Edit and Window are rebuilt here explicitly. They are not decoration and they are not
/// optional; deleting either is the bug this comment exists to prevent.
///
/// # No `#[cfg]`, per `tests/platform_seams.rs`
///
/// Every item here compiles on both platforms — the four macOS-specific ones (`services`,
/// `hide`, `hide_others`, `show_all`) are documented as unsupported rather than gated, so
/// they no-op elsewhere. What is genuinely macOS-only is *installing* the result, because
/// GTK renders a menu as a bar inside the window and the Linux build deliberately has none.
/// That decision is a runtime check in [`run`], which keeps this function under test on
/// either runner.
fn build_menu<R: tauri::Runtime>(
    handle: &tauri::AppHandle<R>,
) -> tauri::Result<tauri::menu::Menu<R>> {
    use tauri::menu::{AboutMetadata, MenuBuilder, MenuItemBuilder, SubmenuBuilder};

    // `Settings…` with an ellipsis, and `CmdOrCtrl+,` because that is the platform
    // convention. A preferences item anywhere else, or bound to anything else, is one
    // people fail to find.
    let settings = MenuItemBuilder::new("Settings…")
        .id(SETTINGS_EVENT)
        .accelerator("CmdOrCtrl+,")
        .build(handle)?;

    let app_menu = SubmenuBuilder::new(handle, "Worktree Manager")
        .about(Some(AboutMetadata::default()))
        .separator()
        .item(&settings)
        .separator()
        .services()
        .separator()
        .hide()
        .hide_others()
        .show_all()
        .separator()
        .quit()
        .build()?;

    // Load-bearing. See the header.
    let edit_menu = SubmenuBuilder::new(handle, "Edit")
        .undo()
        .redo()
        .separator()
        .cut()
        .copy()
        .paste()
        .select_all()
        .build()?;

    let window_menu = SubmenuBuilder::new(handle, "Window")
        .minimize()
        .separator()
        .close_window()
        .build()?;

    MenuBuilder::new(handle)
        .items(&[&app_menu, &edit_menu, &window_menu])
        .build()
}

/// Start the application.
///
/// # Panics
///
/// If the app cannot be constructed (no resolvable home directory) or Tauri fails to
/// start. Both are unrecoverable at launch, and a window that appears but cannot do
/// anything would be worse than a clear crash.
pub fn run() {
    init_tracing();

    let app = Arc::new(App::new().expect("initialize the application"));

    tauri::Builder::default()
        .manage(app)
        .plugin(platform_plugin())
        // Unlike `platform_plugin` above, this one declares commands, so it *does* need an
        // entry in `capabilities/default.json` — `dialog:allow-open`, and nothing else.
        .plugin(tauri_plugin_dialog::init())
        .setup(|handle| {
            // A runtime check rather than `#[cfg]`, the same way `platform_plugin` reads
            // `std::env::consts::OS`: both arms compile, so `build_menu` stays under test on
            // a Linux runner. See `tests/platform_seams.rs` for why that is a rule here.
            //
            // Only the *install* is conditional. On GTK a menu becomes a bar inside the
            // window, and this app's Linux build is an ordinary decorated window with none —
            // there the title bar's gear and Ctrl-, are the whole story.
            if std::env::consts::OS == "macos" {
                handle.set_menu(build_menu(handle.handle())?)?;
            }
            Ok(())
        })
        .on_menu_event(|handle, event| {
            use tauri::Emitter;

            if event.id() == SETTINGS_EVENT {
                // A failed emit means the webview is gone, which is not something a menu
                // handler can do anything about.
                let _ = handle.emit(SETTINGS_EVENT, ());
            }
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_projects,
            commands::register_project,
            commands::unregister_project,
            commands::list_worktrees,
            commands::set_worktree_favorite,
            commands::worktree_form,
            commands::field_options,
            commands::list_actions,
            commands::set_config_trust,
            commands::get_pref,
            commands::set_pref,
            commands::doctor,
            commands::list_palettes,
            commands::reveal_env_value,
            commands::preview_worktree,
            commands::create_worktree,
            commands::remove_preflight,
            commands::remove_worktree,
            commands::run_setup,
            commands::run_action,
            commands::pty_write,
            commands::pty_resize,
            commands::pty_kill,
            commands::open_url,
            commands::list_openers,
            commands::open_in,
        ])
        .run(tauri::generate_context!())
        .expect("run the application");
}

/// Configure logging.
///
/// Defaults to `info` for our crates and `warn` for everything else, so the noise from
/// ~800 dependencies stays out of the way. `WTM_LOG` overrides it, using the standard
/// `RUST_LOG` grammar.
fn init_tracing() {
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_env("WTM_LOG").unwrap_or_else(|_| {
        EnvFilter::new(
            "warn,wtm_app_lib=info,wtm_core=info,wtm_git=info,wtm_exec=info,wtm_config=info",
        )
    });

    // Log to a file as well as stderr.
    //
    // A GUI app launched from Finder has no stderr anyone will ever read, so without this a
    // failure leaves no evidence at all — which is what made a silent crash impossible to
    // diagnose. `just logs` tails this.
    //
    // `try_init` rather than `init`: a test binary may already have a subscriber, and failing to
    // start over logging would be absurd.
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_ansi(false);

    let _ = match Tee::open() {
        Some(tee) => builder.with_writer(tee).try_init(),
        None => builder.with_writer(std::io::stderr).try_init(),
    };

    // A panic that reaches here is a bug; make sure it is written down before the unwind
    // takes it somewhere less visible.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!("panic: {info}");
        previous(info);
    }));
}

/// Writes every log line to `~/.config/wtm/wtm.log` **and** stderr.
///
/// Implements `MakeWriter` itself rather than being wrapped in a mutex. `tracing_subscriber`
/// provides that impl only for `std::sync::Mutex`, which the workspace's `disallowed-types` rule
/// rejects — poisoning would mean one panic while logging turns every later log call into a
/// panic too. Owning the impl is a dozen lines and keeps `parking_lot` throughout.
#[derive(Clone)]
struct Tee {
    file: std::sync::Arc<parking_lot::Mutex<std::fs::File>>,
}

impl Tee {
    /// Open the log file, or `None` if the config directory is unavailable.
    fn open() -> Option<Self> {
        let paths = wtm_config::AppPaths::discover().ok()?;
        paths.ensure_dir().ok()?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(paths.config_dir.join("wtm.log"))
            .ok()?;
        Some(Self {
            file: std::sync::Arc::new(parking_lot::Mutex::new(file)),
        })
    }
}

impl std::io::Write for Tee {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        // The file is the one that must succeed; a closed stderr is normal in a bundled app,
        // and a failure to write to it must not lose the log line.
        let written = self.file.lock().write(buf)?;
        let _ = std::io::stderr().write_all(buf);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let _ = std::io::stderr().flush();
        self.file.lock().flush()
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Tee {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        // Cheap: the clone shares one `Arc`, and each write takes the lock only for as long as
        // the write itself.
        self.clone()
    }
}
