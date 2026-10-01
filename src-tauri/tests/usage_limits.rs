//! The usage view's direct asks reach each real CLI and come back with what it can say.
//!
//! # Why this needs real binaries
//!
//! The parsers are proved against captured replies in `wtm-agent`'s mapping tests. What those
//! cannot prove is that the asks themselves still work: that `account/rateLimits/read` goes out
//! after a handshake the app server accepts, that `claude auth status --json` and
//! `cursor-agent about --format json` are still spelled that way, and that the executable found
//! is the one asked. Each of those failing looks the same from the view, an empty section, so only
//! this notices which.
//!
//! # No API credit is spent
//!
//! None of the three starts a turn. Codex reads its limits from its backend the way its own
//! `/status` does; the other two read local state.
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
fn claude_and_cursor_answer_with_their_plan_and_no_windows() {
    let (_dir, app) = app();
    for provider in [wtm_agent::claude::ID, wtm_agent::cursor::ID] {
        if !installed(&app, provider) {
            continue;
        }
        let (limits, complete) = wtm_app_lib::usage::ask_for_test(&app, provider)
            .unwrap_or_else(|e| panic!("{provider} should answer: {e}"));

        assert!(
            !complete,
            "{provider}'s answer must not wipe the windows its turns reported"
        );
        assert!(
            limits.windows.is_empty(),
            "{provider} reports no windows on demand"
        );
        assert!(
            limits.plan.is_some(),
            "{provider} should name its plan when signed in"
        );
    }
}
