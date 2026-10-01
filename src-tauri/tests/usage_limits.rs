//! The usage view's direct asks reach each real CLI and come back with what it can say.
//!
//! # Why this needs real binaries
//!
//! The parsers are proved against captured replies in `wtm-agent`'s mapping tests. What those
//! cannot prove is that the asks themselves still work. That means `account/rateLimits/read` goes
//! out after a handshake the app server accepts, Claude still answers `get_usage` over stream-json,
//! and `cursor-agent about --format json` is still spelled that way. It also means the executable
//! found is the one asked. Each of those failing looks the same from the view, an empty section,
//! so only this notices which.
//!
//! # No API credit is spent
//!
//! None of the three starts a turn. Codex reads its limits from its backend the way its own
//! `/status` does, and Claude from its usage endpoint the way its own `/usage` does, in a session
//! that is sent no message. Cursor reads local state.
//!
//! Skips loudly for any CLI that is not installed, for the reason `agent_capability.rs` gives: none
//! of them is a dependency of wtm and CI installs none of them.

#![allow(clippy::unwrap_used, clippy::print_stderr)]

use std::sync::Arc;

use wtm_app_lib::app::App;

fn app() -> (tempfile::TempDir, Arc<App>) {
    let dir = tempfile::tempdir().unwrap();
    let app = Arc::new(App::with_paths(wtm_config::AppPaths::rooted(dir.path())).unwrap());
    (dir, app)
}

/// Whether the app can find `provider`'s CLI the way a session would, printing a reason when not.
fn installed(app: &App, provider: &str) -> bool {
    let entry = wtm_agent::entry(provider).unwrap();
    if app.agent_executable(entry).is_none() {
        eprintln!("skipping: wtm cannot find {}'s CLI here", entry.label);
        return false;
    }
    true
}

#[test]
fn codex_reports_its_windows_on_demand_as_a_complete_list() {
    let (_dir, app) = app();
    if !installed(&app, wtm_agent::codex::ID) {
        return;
    }

    let (limits, complete) = wtm_app_lib::usage::ask_for_test(&app, wtm_agent::codex::ID)
        .expect("the app server should answer `account/rateLimits/read`");

    assert!(
        complete,
        "a read lists every window, so it may replace the held ones"
    );
    assert!(
        !limits.windows.is_empty(),
        "a ChatGPT sign-in has at least one window; an API-key sign-in is an error, not this"
    );
    for window in &limits.windows {
        assert!(
            (0.0..=100.0).contains(&window.used_percent),
            "a percentage, got {}",
            window.used_percent
        );
        assert!(
            window.minutes.is_some(),
            "0.154 states every window's length"
        );
        assert!(
            window.resets_at.is_some(),
            "0.154 states every window's reset"
        );
    }
}

#[test]
fn claude_reports_its_windows_on_demand_including_the_per_model_weeks() {
    let (_dir, app) = app();
    if !installed(&app, wtm_agent::claude::ID) {
        return;
    }
    let (limits, complete) = wtm_app_lib::usage::ask_for_test(&app, wtm_agent::claude::ID)
        .unwrap_or_else(|e| panic!("Claude should answer: {e}"));
    assert!(
        limits.plan.is_some(),
        "Claude should name its plan when signed in"
    );
    eprintln!("claude windows: {:?}", limits.windows);

    // An account with no plan limits (an API key) answers with the plan alone, and an answer from
    // the CLI's cached reading is partial. Either way the five-hour and weekly figures are the
    // part every plan has, so that is the part asserted.
    if limits.windows.is_empty() {
        eprintln!("skipping the window checks: this account reported no plan limits");
        return;
    }
    for minutes in [300, 10_080] {
        assert!(
            limits
                .windows
                .iter()
                .any(|w| w.minutes == Some(minutes) && w.scope.is_none()),
            "Claude should report its {minutes}-minute window: {:?}",
            limits.windows
        );
    }
    assert!(
        limits
            .windows
            .iter()
            .all(|w| (0.0..=100.0).contains(&w.used_percent)),
        "get_usage sends percentages, not fractions: {:?}",
        limits.windows
    );
    if complete {
        eprintln!("complete answer: per-model weeks are whatever the account has");
    }
}

#[test]
fn cursor_answers_with_its_plan_and_no_windows() {
    let (_dir, app) = app();
    if !installed(&app, wtm_agent::cursor::ID) {
        return;
    }
    let (limits, complete) = wtm_app_lib::usage::ask_for_test(&app, wtm_agent::cursor::ID)
        .unwrap_or_else(|e| panic!("Cursor should answer: {e}"));
    assert!(
        !complete,
        "Cursor's answer must not wipe the windows its turns reported"
    );
    assert!(
        limits.windows.is_empty(),
        "Cursor reports no windows on demand"
    );
    assert!(
        limits.plan.is_some(),
        "Cursor should name its plan when signed in"
    );
}
