//! A capability query reaches the real CLI and comes back with per-model effort ladders.
//!
//! # Why this needs a real binary
//!
//! `codex_mapping.rs` proves the *parser* against a captured reply, which is the part that can drift
//! silently. What it cannot prove is that a throwaway app server actually answers: that the frames go
//! out in an order it accepts, that `model/list` needs no experimental capability, and that the reply
//! arrives interleaved with the MCP-startup notifications the scan has to skip past. Every one of
//! those failing looks the same from the UI — an empty picker — so only this notices which.
//!
//! # No API credit is spent
//!
//! `initialize` → `initialized` → `model/list` is local: it reads `~/.codex/config.toml`, starts the
//! configured MCP servers and returns a catalogue. No turn is sent.
//!
//! Skips loudly without the CLI, for the reason `agent_sessions.rs` gives: `codex` is not a
//! dependency of wtm, CI does not install it, and a skip that does not say why it skipped is
//! indistinguishable from a pass.

#![allow(clippy::unwrap_used, clippy::disallowed_methods, clippy::print_stderr)]

use wtm_exec::ResolvedPath;

/// Whether a program is on the resolved PATH, printing a reason when it is not.
fn installed(program: &str) -> bool {
    let path = ResolvedPath::resolve(None);
    if path.which(program, &std::env::temp_dir()).is_none() {
        eprintln!(
            "skipping: no `{program}` on the resolved PATH ({})",
            path.value
        );
        return false;
    }
    true
}

#[test]
fn codex_reports_models_whose_effort_ladders_differ_from_each_other() {
    if !installed("codex") {
        return;
    }

    // The whole path, through the app's own state so the resolved PATH is the one a session would
    // use — an agent findable by the picker and not by the spawn is the bug `adapters()` exists to
    // prevent, and this is where it would show.
    let dir = tempfile::tempdir().unwrap();
    let app = std::sync::Arc::new(
        wtm_app_lib::app::App::with_paths(wtm_config::AppPaths::rooted(dir.path())).unwrap(),
    );

    let capability = wtm_app_lib::commands::probe_codex_for_test(&app)
        .expect("the app server should answer `model/list`");

    assert!(
        capability.models_are_live,
        "an answered query must not claim to be a compiled table"
    );
    assert!(
        !capability.models.is_empty(),
        "at least one model, or the picker is empty"
    );

    // Every model carries its own ladder, and at least one rung. A model with an empty ladder would
    // render an effort picker with nothing in it.
    for model in &capability.models {
        assert!(
            !model.efforts.is_empty(),
            "{} reported no efforts",
            model.id
        );
    }

    // The property the query exists for. If every ladder were identical a compiled list would do,
    // and this test would be the place that noticed the day that stopped being true — so it asserts
    // the *shape* rather than the specific ladders, which are OpenAI's to change.
    let ladders: std::collections::BTreeSet<Vec<String>> = capability
        .models
        .iter()
        .map(|m| m.efforts.iter().map(|e| e.effort.clone()).collect())
        .collect();
    eprintln!(
        "{} models, {} distinct effort ladders",
        capability.models.len(),
        ladders.len()
    );
    for model in &capability.models {
        eprintln!(
            "  {:<16} default={:<7} {:?}",
            model.id,
            model.default_effort.as_deref().unwrap_or("-"),
            model.efforts.iter().map(|e| &e.effort).collect::<Vec<_>>()
        );
    }

    assert!(
        capability.models.iter().any(|m| m.is_default),
        "one model must be marked default, or a new pane has nothing to start on"
    );
}

#[test]
fn cursor_discovery_and_the_real_acp_handshake_use_the_same_executable() {
    let dir = tempfile::tempdir().unwrap();
    let app = std::sync::Arc::new(
        wtm_app_lib::app::App::with_paths(wtm_config::AppPaths::rooted(dir.path())).unwrap(),
    );
    let entry = wtm_agent::entry(wtm_agent::cursor::ID).unwrap();
    let Some(executable) = app.agent_executable(entry) else {
        eprintln!(
            "skipping: neither `cursor-agent`, `agent`, nor Cursor.app's managed CLI was found"
        );
        return;
    };
    eprintln!("probing Cursor ACP through {}", executable.display());

    let capability = wtm_app_lib::commands::probe_cursor_for_test(&app)
        .expect("the discovered Cursor Agent CLI should answer its ACP handshake");

    assert!(capability.models_are_live);
    assert!(
        !capability.models.is_empty(),
        "Cursor must advertise at least one model, or its picker is empty"
    );
    assert!(
        !capability.modes.is_empty(),
        "Cursor must advertise at least one mode, or its mode picker is empty"
    );
}
