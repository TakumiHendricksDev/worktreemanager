// Suppress the console window on Windows in a release build. Harmless on macOS, and
// keeping it means the crate does not need a cfg dance if it is ever built elsewhere.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    wtm_app_lib::run();
}
