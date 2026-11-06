//! Editing needs provider identities and a completed exact boundary, never a visible row index.
use wtm_app_lib::{app::SeqEvent, chat_edit::messages};
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
    let history = log(vec![
        AgentEvent::HistoryBoundary { complete: true },
        AgentEvent::TurnStarted { turn: "t1".into() },
        identity("m1", "t1", false),
        finish("t1"),
        AgentEvent::TurnStarted { turn: "t2".into() },
        identity("m2", "t2", false),
        finish("t2"),
    ]);
    let before = serde_json::to_value(&history).ok();
    let rows = messages(&history);
    assert_eq!(rows[0].id, "m1");
    assert_eq!(rows[0].previous, None);
    assert!(rows[0].problem.is_none());
    assert_eq!(rows[1].id, "m2");
    assert_eq!(rows[1].previous.as_deref(), Some("t1"));
    assert!(rows[1].problem.is_none());
    assert_eq!(serde_json::to_value(&history).ok(), before);
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
