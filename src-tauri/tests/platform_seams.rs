//! Platform differences must stay data, not compile-time branches.
//!
//! The governing principle is that **macOS is the unattributed default and anything else is
//! an explicit override**, expressed wherever possible as *data* that every target compiles.
//! A `#[cfg(target_os)]` is the opposite — it deletes one arm before the compiler ever sees
//! it, so that arm is untested on whichever runner is executing and neither arm can be
//! exercised from a unit test.
//!
//! This outlived the Linux build it was written for, and deliberately. wtm ships on macOS
//! only (ARCHITECTURE.md §9 records why the Linux target went away), so the non-mac arms
//! below are unreachable as shipped — but the reason to keep them is the same reason the
//! rule exists: an untaken arm that still *compiles* is one the type checker keeps honest,
//! and the alternative is `#[cfg]`s spreading back through the call sites. The number the
//! lint watches is the number of files that branch at all, and dropping a platform is not a
//! licence to raise it.
//!
//! A seam is warranted only where the other platform's code **cannot compile or cannot be
//! expressed** — a constant with no portable spelling, a syscall that does not exist. `open`
//! vs `xdg-open` qualifies: there is no portable name and no runtime way to pick one.
//! `fs::metadata("/Applications/Zed.app")` does not; it compiles everywhere and answers
//! correctly everywhere, which is why the opener catalogue stays testable as plain data.
//!
//! If you are here because this test went red: adding a seam is allowed, but it is a
//! decision. Add the file to `ALLOWED` **with the reason written next to it**, the way the
//! entries below are.

// Same justification as `repo_hygiene.rs`: asking git for its own file list is not app code
// and does not need a timeout, a sanitized environment or a tracing span.
#![allow(clippy::unwrap_used, clippy::disallowed_methods)]

use std::path::{Path, PathBuf};

/// Production files permitted to contain a `cfg(target_os)`, and why.
const ALLOWED: &[(&str, &str)] = &[
    (
        "crates/wtm-exec/src/path.rs",
        "`mod platform` — the PATH floor, the default shell and the platform name. None has \
         a portable spelling, and the two arms are deliberately written side by side so \
         they can be read against each other.",
    ),
    (
        "src-tauri/src/openers.rs",
        "`OPENER` — the OS's hand-this-to-the-default-handler front end. `open` on macOS, \
         `xdg-open` elsewhere; there is no portable name and no runtime way to choose.",
    ),
    (
        "src-tauri/src/browser.rs",
        "`native_handle` — Tauri exposes WKWebView pointers on macOS but a WebKitGTK object \
         and no controller method on Linux. The conversion cannot compile across targets; \
         the composition root owns it so the native FFI adapter need not depend on Tauri.",
    ),
    (
        "crates/wtm-webview/src/lib.rs",
        "`WKContentWorld` / `WKUserScript` / `WKScriptMessageHandler` — WebKit's content-world \
         API does not exist off macOS, so the other arm cannot compile. Same shape as \
         `wtm-notify`: the facade keeps both arms building and the no-op arm's Handle is \
         uninhabited, which is the type-level proof the branch is total.",
    ),
    (
        "crates/wtm-notify/src/lib.rs",
        "`UNUserNotificationCenter` — the framework does not exist off macOS, so the other \
         arm cannot compile. The facade keeps both arms building on both runners and this is \
         the only file that branches; the no-op arm's Center is uninhabited, which is the \
         type-level proof the branch is total.",
    ),
];

/// Directories whose Rust is not production code.
///
/// Tests may branch on the platform freely: a test that asserts macOS-specific behaviour
/// has nowhere else to say so, and unlike production code it cannot silently ship the wrong
/// arm to a user.
const TEST_DIRS: &[&str] = &["tests/", "/tests/", "benches/", "/benches/"];

#[test]
fn the_production_tree_has_exactly_the_platform_seams_it_declares() {
    let root = workspace_root();
    let mut found: Vec<(String, usize, String)> = Vec::new();

    for relative in tracked_files(&root) {
        let display = relative.to_string_lossy().replace('\\', "/");
        let is_rust = Path::new(&display)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("rs"));
        if !is_rust || TEST_DIRS.iter().any(|dir| display.contains(dir)) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(root.join(&relative)) else {
            continue;
        };

        for (number, line) in text.lines().enumerate() {
            // A line of prose is not a seam. Writing down *why* a file chose a runtime probe
            // over a compile-time branch means naming the thing not being used, and a scanner
            // that reads its own advice back as a violation punishes the explanation this
            // test's failure message asks for. Only `//`-prefixed lines are skipped, so an
            // attribute with a trailing comment is still caught.
            if line.trim_start().starts_with("//") {
                continue;
            }
            // Matches `cfg(target_os = …)` and `cfg(not(target_os = …))` alike. `cfg!(…)`
            // is deliberately *not* matched: it is a runtime boolean, both arms compile,
            // and it is the escape hatch this test wants people to reach for.
            if line.contains("cfg(target_os") || line.contains("cfg(not(target_os") {
                found.push((display.clone(), number + 1, line.trim().to_owned()));
            }
        }
    }

    let mut unexpected: Vec<String> = Vec::new();
    for (file, line, text) in &found {
        if !ALLOWED.iter().any(|(allowed, _)| allowed == file) {
            unexpected.push(format!("{file}:{line}: {text}"));
        }
    }

    assert!(
        unexpected.is_empty(),
        "{} undeclared platform seam(s).\n\n{}\n\nPrefer data the other platform simply \
         answers `None` to, or a runtime `cfg!(target_os = …)` / `std::env::consts::OS` \
         check, so both arms compile and both stay testable on either runner. If a seam is \
         genuinely unavoidable, add the file to ALLOWED in this test with the reason.",
        unexpected.len(),
        unexpected.join("\n")
    );

    // The other direction: an entry that no longer describes anything is a stale licence.
    for (allowed, reason) in ALLOWED {
        assert!(
            found.iter().any(|(file, _, _)| file == allowed),
            "`{allowed}` is listed as a platform seam but no longer contains one. Remove it \
             from ALLOWED rather than leaving a standing permission behind. Stated reason \
             was: {reason}"
        );
    }
}

/// Every path git tracks, relative to the workspace root. See `repo_hygiene.rs` for why
/// this asks git rather than walking the working tree.
fn tracked_files(root: &Path) -> Vec<PathBuf> {
    let output = std::process::Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(root)
        .output()
        .expect("git ls-files");

    assert!(
        output.status.success(),
        "git ls-files failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    String::from_utf8_lossy(&output.stdout)
        .split('\0')
        .filter(|entry| !entry.is_empty())
        .map(PathBuf::from)
        .collect()
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-tauri has a parent")
        .to_path_buf()
}
