// Suppress the console window on Windows in a release build. Harmless on macOS, and
// keeping it means the crate does not need a cfg dance if it is ever built elsewhere.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::ffi::OsStr;

/// The argument that starts the GUI with every user plugin package off.
///
/// For the launch after a package broke the workbench badly enough that Settings cannot be
/// reached to disable it. Matched anywhere on the command line rather than only first, because
/// it is typed by a person (`open -na "Worktree Manager" --args --safe-mode`), not composed by
/// a program.
const SAFE_MODE_FLAG: &str = "--safe-mode";

/// Start the GUI, unless this process was asked to be one of the app's three helpers instead:
/// the MCP server an agent CLI spawns (`bridge::ARGV_FLAG`), the private helper a Run shell's
/// integration calls (`shell_control::ARGV_FLAG`), or a Native CLI session's lifecycle hook
/// (`native_sessions::HOOK_FLAG`).
///
/// # Why one binary does all four
///
/// Each helper is started by something else — an agent CLI, a shell, a vendor's hook runner — and
/// has to be an executable on disk. Shipping more of them means declaring Tauri `externalBin`s,
/// bundling them, and resolving their paths at runtime from inside a `.app`, where they differ
/// from the `cargo` layout a dev build uses. `current_exe()` is already right in both, so every
/// helper is this same binary behind a flag.
///
/// The branch is here, in `main`, rather than inside `run()`: nothing about Tauri is constructed on
/// these paths, so no window appears, no menu is installed, and stdout stays clean for a protocol.
///
/// Arguments are read with `args_os`, never `args`: the latter panics on an argument that is not
/// UTF-8, and a path handed to the GUI by Finder or a shell can be exactly that. No flag here is
/// anything but ASCII, so a non-UTF-8 argument simply matches none of them.
fn main() {
    let first = std::env::args_os().nth(1);
    let first = first.as_deref();
    if first == Some(OsStr::new(wtm_app_lib::native_sessions::HOOK_FLAG)) {
        if let Err(error) = wtm_app_lib::native_sessions::run_helper() {
            let _ = std::io::Write::write_all(
                &mut std::io::stderr(),
                format!("wtm integration: {error}\n").as_bytes(),
            );
            std::process::exit(1);
        }
        return;
    }
    if first == Some(OsStr::new(wtm_app_lib::shell_control::ARGV_FLAG)) {
        let args: Vec<_> = std::env::args_os().collect();
        let result = match args.as_slice() {
            [_, _, socket, capability, fd] => {
                let capability = capability.to_str();
                let fd = fd.to_str().and_then(|fd| fd.parse().ok());
                match (capability, fd) {
                    (Some(capability), Some(fd)) => wtm_app_lib::shell_control::run_helper(
                        std::path::Path::new(socket),
                        capability,
                        fd,
                    ),
                    (None, _) => Err("Invalid shell capability.".to_owned()),
                    (_, None) => Err("Invalid shell descriptor.".to_owned()),
                }
            }
            _ => {
                Err("The private shell helper needs its socket, capability and descriptor.".into())
            }
        };
        if let Err(error) = result {
            // This mode has no GUI logger; the visible terminal owns the error.
            let _ = std::io::Write::write_all(
                &mut std::io::stderr(),
                format!("wtm Run: {error}\n").as_bytes(),
            );
        }
        return;
    }
    if first == Some(OsStr::new(wtm_app_lib::bridge::ARGV_FLAG)) {
        wtm_app_lib::bridge::serve_stdio();
        return;
    }
    let safe_mode = std::env::args_os().any(|arg| arg == SAFE_MODE_FLAG);
    wtm_app_lib::run(safe_mode);
}
