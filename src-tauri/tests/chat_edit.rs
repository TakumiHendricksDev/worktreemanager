//! Editing needs provider identities and a completed exact boundary, never a visible row index,
//! and one edit request is submitted at most once.
//!
//! The first tests read event logs directly: which messages can be edited is a pure function of
//! what the provider said. The rest drive a fake `codex app-server` (see `support/fake_codex.rs`)
//! through the real fork, the real journal on disk and a real second process, because "a retry
//! returns the first outcome" and "an unknown outcome is never resubmitted" are claims about
//! what a provider process received, and only counting what it received can fail them.
#![allow(clippy::unwrap_used)]

#[path = "support/fake_codex.rs"]
mod fake_codex;

use std::{os::unix::fs::PermissionsExt, path::Path};

use wtm_app_lib::{
    app::{CHAT_EDITS_DIR, SeqEvent},
    chat_edit::{self, Edit, Snapshot, messages},
    plugin_authoring,
};
use wtm_core::model::{AgentEvent, Usage};

fn log(events: Vec<AgentEvent>) -> Vec<SeqEvent> {
    events
        .into_iter()
        .zip(1..)
        .map(|(event, seq)| SeqEvent { seq, event })
        .collect()
}
fn identity(id: &str, turn: &str, attachments: bool) -> AgentEvent {
    AgentEvent::MessageIdentity {
        message: id.into(),
        turn: turn.into(),
        text: "Repeated text".into(),
        attachments,
        truncated: false,
    }
}
fn finish(turn: &str) -> AgentEvent {
    AgentEvent::TurnFinished {
        turn: turn.into(),
        usage: Usage::default(),
        cost_usd: None,
    }
}

#[test]
fn repeated_text_keeps_distinct_provider_ids_and_branches_before_the_selected_turn() {
    let rows = messages(&log(vec![
        AgentEvent::HistoryBoundary { complete: true },
        AgentEvent::TurnStarted { turn: "t1".into() },
        identity("m1", "t1", false),
        finish("t1"),
        AgentEvent::TurnStarted { turn: "t2".into() },
        identity("m2", "t2", false),
        finish("t2"),
    ]));
    assert_eq!(rows[0].id, "m1");
    assert_eq!(rows[0].previous, None);
    assert!(rows[0].problem.is_none());
    assert_eq!(rows[1].id, "m2");
    assert_eq!(rows[1].previous.as_deref(), Some("t1"));
    assert!(rows[1].problem.is_none());
}

#[test]
fn truncated_history_busy_turns_attachments_and_mid_turn_messages_are_not_fake_edits() {
    let rows = messages(&log(vec![
        AgentEvent::TurnStarted { turn: "t1".into() },
        identity("m1", "t1", false),
        finish("t1"),
    ]));
    assert!(rows[0].problem.is_some());
    let rows = messages(&log(vec![
        AgentEvent::HistoryBoundary { complete: true },
        AgentEvent::TurnStarted { turn: "t1".into() },
        identity("m1", "t1", false),
    ]));
    assert!(rows[0].problem.is_some());
    let rows = messages(&log(vec![
        AgentEvent::HistoryBoundary { complete: true },
        AgentEvent::TurnStarted { turn: "t1".into() },
        identity("m1", "t1", true),
        identity("m2", "t1", false),
        finish("t1"),
    ]));
    assert!(rows.iter().all(|r| r.problem.is_some()));
    let mut shortened = identity("shortened", "t1", false);
    if let AgentEvent::MessageIdentity { truncated, .. } = &mut shortened {
        *truncated = true;
    }
    let rows = messages(&log(vec![
        AgentEvent::HistoryBoundary { complete: true },
        AgentEvent::TurnStarted { turn: "t1".into() },
        shortened,
        finish("t1"),
    ]));
    assert!(
        rows[0]
            .problem
            .as_deref()
            .is_some_and(|p| p.contains("truncated"))
    );
}

#[test]
fn a_turn_that_failed_is_over_so_its_message_and_the_next_can_be_edited() {
    // Codex reports an errored turn as `Failed`, with no turn id and no `TurnFinished`.
    for stopped in [
        AgentEvent::Failed {
            message: "model error".into(),
        },
        AgentEvent::LimitReached {
            message: "usage limit".into(),
            resets_at: None,
        },
    ] {
        let rows = messages(&log(vec![
            AgentEvent::HistoryBoundary { complete: true },
            AgentEvent::TurnStarted { turn: "t1".into() },
            identity("m1", "t1", false),
            stopped,
            AgentEvent::TurnStarted { turn: "t2".into() },
            identity("m2", "t2", false),
            finish("t2"),
        ]));
        assert!(rows[0].problem.is_none(), "{:?}", rows[0].problem);
        assert_eq!(rows[1].previous.as_deref(), Some("t1"));
        assert!(rows[1].problem.is_none(), "{:?}", rows[1].problem);
    }
}

// ── With a provider process ───────────────────────────────────────────────────────────────────

/// A live Codex conversation in its own registered folder, its first message sent.
fn conversation(h: &fake_codex::Harness, name: &str) -> (std::path::PathBuf, String) {
    let folder = h.folder(name);
    let job = plugin_authoring::prepare(
        &h.app,
        &uuid::Uuid::new_v4().to_string(),
        "codex",
        "First original",
        folder.to_str().unwrap(),
    )
    .unwrap();
    let session = plugin_authoring::open(&h.window, &h.app, &job.id, false)
        .unwrap()
        .session
        .unwrap();
    (folder, session)
}

/// Wait until `session` is editable with exactly `count` editable messages.
fn editable(h: &fake_codex::Harness, session: &str, count: usize) -> Snapshot {
    let ready = || {
        chat_edit::snapshot(&h.app, session).ok().filter(|s| {
            s.problem.is_none()
                && s.messages.len() == count
                && s.messages.iter().all(|m| m.problem.is_none())
        })
    };
    fake_codex::wait(&format!("{count} editable messages"), || ready().is_some());
    ready().unwrap()
}

fn edit(snapshot: &Snapshot, message: usize, text: &str) -> Edit {
    Edit {
        id: uuid::Uuid::new_v4().to_string(),
        session: snapshot.session.clone(),
        message: snapshot.messages[message].id.clone(),
        revision: snapshot.revision,
        text: text.into(),
    }
}

fn threads(folder: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(folder.join(".git/provider.json")).unwrap()).unwrap()
}

#[test]
fn an_edit_branches_once_and_a_retry_returns_the_first_outcome() {
    let h = fake_codex::Harness::new();
    let (folder, session) = conversation(&h, "edit-source");
    editable(&h, &session, 1);
    h.app
        .send_agent_turn(&session, "Second original", &[])
        .unwrap();
    let snapshot = editable(&h, &session, 2);
    let original = h.app.agent_fork_source(&session).unwrap().provider_session;
    let before = threads(&folder)[&original].clone();

    let request = edit(&snapshot, 1, "Second edited");
    let branch = chat_edit::apply(&h.window, &h.app, request.clone()).unwrap();
    assert!(branch.submitted);
    assert_eq!(branch.original, session);
    assert_ne!(branch.session, session);
    let history = editable(&h, &branch.session, 2);
    assert_eq!(history.messages[0].id, snapshot.messages[0].id);
    assert_eq!(history.messages[1].text, "Second edited");
    let fork: serde_json::Value =
        serde_json::from_slice(&std::fs::read(folder.join(".git/fork.json")).unwrap()).unwrap();
    assert_eq!(fork["lastTurnId"], snapshot.messages[0].turn);
    assert_eq!(fork["ephemeral"], false);

    // The same request again, in another spelling of its id: the first outcome, nothing sent.
    let braced = Edit {
        id: format!("{{{}}}", request.id),
        ..request.clone()
    };
    let retried = chat_edit::apply(&h.window, &h.app, braced).unwrap();
    assert_eq!(retried.session, branch.session);
    assert!(retried.submitted);
    let data = threads(&folder);
    assert_eq!(data[&original], before, "the original conversation changed");
    assert_eq!(
        data.as_object().unwrap().len(),
        2,
        "a retry made another branch"
    );
    assert_eq!(fake_codex::Harness::turns_sent(&folder), 3);

    // One id is one request.
    let mismatch = Edit {
        text: "Different retry".into(),
        ..request.clone()
    };
    let refusal = chat_edit::apply(&h.window, &h.app, mismatch).unwrap_err();
    assert!(
        refusal.message.contains("different request"),
        "{}",
        refusal.message
    );

    // The journal is private, and its folder too.
    let journals = h.config.path().join(CHAT_EDITS_DIR);
    let journal = journals.join(format!("{}.json", request.id));
    assert_eq!(
        std::fs::metadata(&journals).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(&journal).unwrap().permissions().mode() & 0o777,
        0o600
    );
    h.app.close_agent(&branch.session);
    h.app.close_agent(&session);
}

#[test]
fn an_edit_made_against_older_history_is_refused() {
    let h = fake_codex::Harness::new();
    let (_, session) = conversation(&h, "edit-source");
    editable(&h, &session, 1);
    h.app
        .send_agent_turn(&session, "Second original", &[])
        .unwrap();
    let stale = editable(&h, &session, 2);
    h.app
        .send_agent_turn(&session, "Third original", &[])
        .unwrap();
    editable(&h, &session, 3);

    let refusal = chat_edit::apply(&h.window, &h.app, edit(&stale, 1, "Late")).unwrap_err();
    assert!(
        refusal.message.contains("Conversation changed"),
        "{}",
        refusal.message
    );
    assert_eq!(h.app.live_agents().len(), 1, "a stale edit opened a branch");
    h.app.close_agent(&session);
}

#[test]
fn editing_the_first_message_starts_a_fresh_conversation_without_asking_codex_about_forks() {
    let h = fake_codex::Harness::new();
    let (_, session) = conversation(&h, "edit-source");
    editable(&h, &session, 1);
    h.app
        .send_agent_turn(&session, "Second original", &[])
        .unwrap();
    let snapshot = editable(&h, &session, 2);

    let first = chat_edit::apply(&h.window, &h.app, edit(&snapshot, 0, "First edited")).unwrap();
    let history = editable(&h, &first.session, 1);
    assert_eq!(history.messages[0].text, "First edited");
    assert_eq!(history.messages[0].previous, None);
    // Nothing to fork, so nothing to probe: the 15-second schema run is for forks only.
    assert_eq!(h.probes(), 0);

    // A fork asks once, and the answer is kept for the same Codex.
    let snapshot = editable(&h, &session, 2);
    let second = chat_edit::apply(&h.window, &h.app, edit(&snapshot, 1, "Second edited")).unwrap();
    let snapshot = editable(&h, &session, 2);
    let third = chat_edit::apply(&h.window, &h.app, edit(&snapshot, 1, "Second again")).unwrap();
    assert_eq!(h.probes(), 1);
    for session in [&first.session, &second.session, &third.session, &session] {
        h.app.close_agent(session);
    }
}

#[test]
fn a_codex_without_exact_forks_is_refused_in_plain_words() {
    let h = fake_codex::Harness::new();
    std::fs::write(h.bin.join("no-fork-schema"), "").unwrap();
    let (_, session) = conversation(&h, "edit-source");
    editable(&h, &session, 1);
    h.app
        .send_agent_turn(&session, "Second original", &[])
        .unwrap();
    let snapshot = editable(&h, &session, 2);

    let refusal = chat_edit::apply(&h.window, &h.app, edit(&snapshot, 1, "Edited")).unwrap_err();
    assert!(
        refusal.message.contains("Update Codex"),
        "{}",
        refusal.message
    );
    assert_eq!(h.app.live_agents().len(), 1);
    h.app.close_agent(&session);
}

#[test]
fn a_failed_turn_leaves_the_conversation_ready_to_edit() {
    let h = fake_codex::Harness::new();
    let folder = h.folder("edit-source");
    let job = plugin_authoring::prepare(
        &h.app,
        &uuid::Uuid::new_v4().to_string(),
        "codex",
        "First original",
        folder.to_str().unwrap(),
    )
    .unwrap();
    std::fs::write(folder.join(".git/fixture-fail"), "").unwrap();
    let session = plugin_authoring::open(&h.window, &h.app, &job.id, false)
        .unwrap()
        .session
        .unwrap();
    fake_codex::wait("the turn to fail", || {
        h.app
            .overview_of(&session)
            .is_some_and(|s| s.failed.is_some())
    });
    std::fs::remove_file(folder.join(".git/fixture-fail")).unwrap();

    // Before, a failure read as "busy" until the next turn, and the failed turn stayed open.
    let snapshot = editable(&h, &session, 1);
    assert_eq!(snapshot.status, "ready");
    h.app
        .send_agent_turn(&session, "Second original", &[])
        .unwrap();
    let snapshot = editable(&h, &session, 2);
    let branch = chat_edit::apply(&h.window, &h.app, edit(&snapshot, 1, "Second edited")).unwrap();
    assert!(branch.submitted);
    h.app.close_agent(&branch.session);
    h.app.close_agent(&session);
}

#[test]
fn an_edit_that_could_not_open_a_branch_reports_the_same_failure_when_retried() {
    let h = fake_codex::Harness::new();
    let (folder, session) = conversation(&h, "edit-source");
    let snapshot = editable(&h, &session, 1);
    // The workspace moved away after the source opened, so the branch cannot be placed in it:
    // the failure comes after the edit was journalled and before anything could be submitted.
    // (Moving it rather than taking `codex` off PATH, which could find a real Codex instead.)
    let moved = folder.with_extension("moved");
    std::fs::rename(&folder, &moved).unwrap();

    let request = edit(&snapshot, 0, "First edited");
    let failure = chat_edit::apply(&h.window, &h.app, request.clone()).unwrap_err();
    assert!(
        !failure.message.contains("unknown outcome"),
        "{}",
        failure.message
    );
    // The real reason again, not "unknown outcome": nothing was sent, and the journal says so.
    let again = chat_edit::apply(&h.window, &h.app, request).unwrap_err();
    assert_eq!(again.message, failure.message);
    assert_eq!(fake_codex::Harness::turns_sent(&moved), 1);
    assert_eq!(h.app.live_agents().len(), 1, "no branch should be running");
    h.app.close_agent(&session);
}

#[test]
fn an_edit_whose_outcome_is_unknown_is_never_submitted_again() {
    let h = fake_codex::Harness::new();
    let (folder, session) = conversation(&h, "edit-source");
    let snapshot = editable(&h, &session, 1);
    let journals = h.config.path().join(CHAT_EDITS_DIR);
    std::fs::create_dir_all(&journals).unwrap();

    // What a crash after journalling and before the outcome leaves behind.
    let pending = edit(&snapshot, 0, "First edited");
    std::fs::write(
        journals.join(format!("{}.json", pending.id)),
        serde_json::json!({"request": pending, "outcome": null}).to_string(),
    )
    .unwrap();
    // And what a crash between creating the file and writing it left, before writes were whole.
    let empty = edit(&snapshot, 0, "First edited");
    std::fs::write(journals.join(format!("{}.json", empty.id)), "").unwrap();

    for request in [pending, empty] {
        let refusal = chat_edit::apply(&h.window, &h.app, request).unwrap_err();
        assert!(
            refusal.message.contains("unknown outcome"),
            "{}",
            refusal.message
        );
    }
    assert_eq!(fake_codex::Harness::turns_sent(&folder), 1);
    assert_eq!(h.app.live_agents().len(), 1);
    h.app.close_agent(&session);
}
