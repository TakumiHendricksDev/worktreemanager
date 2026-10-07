// Suppress the console window on Windows in a release build. Harmless on macOS, and
// keeping it means the crate does not need a cfg dance if it is ever built elsewhere.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

/// Start the GUI, unless this process was asked to be an MCP server instead.
///
/// # Why one binary does both
///
/// An agent CLI reaches wtm's handoff tool through an MCP server it spawns itself, and that server
/// has to be an executable on disk. Shipping a second one means declaring a Tauri `externalBin`,
/// bundling it, and resolving its path at runtime from inside a `.app` — where it differs from the
/// `cargo` layout a dev build uses. `current_exe()` is already right in both, so the server is this
/// same binary behind a flag.
///
/// The branch is here, in `main`, rather than inside `run()`: nothing about Tauri is constructed on
/// this path, so no window appears, no menu is installed, and stdout stays clean for the protocol.
fn main() {
    if std::env::args().nth(1).as_deref() == Some(wtm_app_lib::shell_control::ARGV_FLAG) {
        let args: Vec<_> = std::env::args().collect();
        let result = match args.as_slice() {
            [_, _, socket, capability, fd] => fd
                .parse()
                .map_err(|_| "Invalid shell descriptor.".to_owned())
                .and_then(|fd| {
                    wtm_app_lib::shell_control::run_helper(
                        std::path::Path::new(socket),
                        capability,
                        fd,
                    )
                }),
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
    if std::env::args().nth(1).as_deref() == Some(wtm_app_lib::bridge::ARGV_FLAG) {
        wtm_app_lib::bridge::serve_stdio();
        return;
    }
    wtm_app_lib::run();
}
