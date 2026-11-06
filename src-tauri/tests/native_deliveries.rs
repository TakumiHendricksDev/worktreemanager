//! A restart or ambiguous provider write must never turn one exact review into two instructions.
#![allow(clippy::unwrap_used)]
use wtm_app_lib::native_sessions::delivery::{DeliveryStatus, Owner, Store};
fn owner() -> Owner {
    Owner {
        project: "project".into(),
        worktree: "worktree".into(),
        provider: "codex".into(),
        conversation: "conversation".into(),
    }
}
#[test]
fn an_uncertain_review_survives_restart_and_is_never_prepared_twice() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::new(root.path().into());
    let first = store
        .prepare(
            owner(),
            "Implement",
            "Exact approved content",
            Some(("prototype", "review")),
        )
        .unwrap();
    store
        .transition(
            &first.id,
            &owner(),
            DeliveryStatus::Pending,
            DeliveryStatus::Unknown,
            None,
        )
        .unwrap();
    drop(store);
    let restored = Store::new(root.path().into());
    let duplicate = restored
        .prepare(
            owner(),
            "Implement again",
            "Changed content",
            Some(("prototype", "review")),
        )
        .unwrap();
    assert_eq!(duplicate.id, first.id);
    assert_eq!(duplicate.status, DeliveryStatus::Unknown);
    assert_eq!(duplicate.text, "Exact approved content");
    assert!(
        restored
            .prepare(owner(), "Another request", "new", None)
            .is_err()
    );
    assert_eq!(restored.list(&owner()).len(), 1);
}
#[test]
fn acknowledgment_is_bound_to_the_provider_conversation_and_expected_state() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::new(root.path().into());
    let receipt = store
        .prepare(owner(), "Implement", "approved", None)
        .unwrap();
    let other = Owner {
        conversation: "other".into(),
        ..owner()
    };
    assert!(
        store
            .transition(
                &receipt.id,
                &other,
                DeliveryStatus::Pending,
                DeliveryStatus::Delivered,
                None
            )
            .is_err()
    );
    assert!(
        store
            .transition(
                &receipt.id,
                &owner(),
                DeliveryStatus::Unknown,
                DeliveryStatus::Delivered,
                None
            )
            .is_err()
    );
    assert!(store.list(&other).is_empty());
    assert_eq!(
        store.read(&receipt.id).unwrap().status,
        DeliveryStatus::Pending
    );
}
#[test]
fn a_confirmed_review_cannot_be_sent_again_and_a_rejected_one_can_be_retried_explicitly() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::new(root.path().into());
    let first = store
        .prepare(
            owner(),
            "Implement",
            "approved",
            Some(("prototype", "review")),
        )
        .unwrap();
    store
        .transition(
            &first.id,
            &owner(),
            DeliveryStatus::Pending,
            DeliveryStatus::Unknown,
            None,
        )
        .unwrap();
    store
        .transition(
            &first.id,
            &owner(),
            DeliveryStatus::Unknown,
            DeliveryStatus::Rejected,
            Some("busy".into()),
        )
        .unwrap();
    let retry = store
        .prepare(
            owner(),
            "Implement",
            "approved",
            Some(("prototype", "review")),
        )
        .unwrap();
    assert_ne!(retry.id, first.id);
    store
        .transition(
            &retry.id,
            &owner(),
            DeliveryStatus::Pending,
            DeliveryStatus::Unknown,
            None,
        )
        .unwrap();
    store
        .transition(
            &retry.id,
            &owner(),
            DeliveryStatus::Unknown,
            DeliveryStatus::Delivered,
            None,
        )
        .unwrap();
    assert_eq!(
        store
            .prepare(
                owner(),
                "Implement",
                "approved",
                Some(("prototype", "review"))
            )
            .unwrap()
            .id,
        retry.id
    );
}
#[test]
fn linked_receipts_and_unregistered_conversations_are_refused() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::new(root.path().into());
    assert!(
        store
            .prepare(
                Owner {
                    conversation: String::new(),
                    ..owner()
                },
                "title",
                "text",
                None
            )
            .is_err()
    );
    let receipt = store.prepare(owner(), "title", "text", None).unwrap();
    let link_id = uuid::Uuid::new_v4().to_string();
    std::os::unix::fs::symlink(
        root.path().join(format!("{}.json", receipt.id)),
        root.path().join(format!("{link_id}.json")),
    )
    .unwrap();
    assert!(store.read(&link_id).is_err());
    assert!(store.read("../elsewhere").is_err());
}

#[test]
fn an_unreadable_journal_cannot_authorize_a_duplicate_dispatch() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::new(root.path().into());
    let receipt = store
        .prepare(
            owner(),
            "Implement",
            "approved",
            Some(("prototype", "review")),
        )
        .unwrap();
    std::fs::write(
        root.path().join(format!("{}.json", receipt.id)),
        "incomplete write",
    )
    .unwrap();
    assert!(
        store
            .prepare(
                owner(),
                "Implement",
                "approved",
                Some(("prototype", "review"))
            )
            .is_err()
    );
}

#[test]
fn public_history_can_confirm_a_lost_reply_after_restart_without_replaying_it() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::new(root.path().into());
    let receipt = store
        .prepare(owner(), "Implement", "exact content", None)
        .unwrap();
    store
        .transition(
            &receipt.id,
            &owner(),
            DeliveryStatus::Pending,
            DeliveryStatus::Unknown,
            None,
        )
        .unwrap();
    drop(store);
    let restored = Store::new(root.path().into());
    let thread = serde_json::json!({"id":"conversation","turns":[{"items":[{
        "type":"userMessage","id":"provider-message","clientId":receipt.id,
        "content":[{"type":"text","text":"exact content","text_elements":[]}]
    }]}]});
    assert!(restored.reconcile_codex(&owner(), &thread).unwrap());
    assert_eq!(
        restored.read(&receipt.id).unwrap().status,
        DeliveryStatus::Delivered
    );
    assert!(!restored.reconcile_codex(&owner(), &thread).unwrap());
    assert_eq!(restored.list(&owner()).len(), 1);
}

#[test]
fn missing_partial_or_mismatched_history_never_resolves_an_uncertain_receipt() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::new(root.path().into());
    let receipt = store
        .prepare(owner(), "Implement", "exact content", None)
        .unwrap();
    store
        .transition(
            &receipt.id,
            &owner(),
            DeliveryStatus::Pending,
            DeliveryStatus::Unknown,
            None,
        )
        .unwrap();
    let thread = serde_json::json!({"id":"conversation","turns":[{"items":[{
        "type":"userMessage","id":"provider-message","clientId":receipt.id,
        "content":[{"type":"text","text":"exact content"}]
    }]}]});
    assert!(
        !store
            .reconcile_codex(&owner(), &serde_json::json!({"id":"conversation"}))
            .unwrap()
    );
    for (pointer, replacement) in [
        ("/turns/0/items/0/type", "agentMessage"),
        ("/turns/0/items/0/clientId", "another-request"),
        ("/turns/0/items/0/content/0/text", "different content"),
    ] {
        let mut other = thread.clone();
        *other.pointer_mut(pointer).unwrap() = serde_json::json!(replacement);
        assert!(!store.reconcile_codex(&owner(), &other).unwrap());
    }
    assert!(
        store
            .reconcile_codex(
                &Owner {
                    conversation: "another-thread".into(),
                    ..owner()
                },
                &thread
            )
            .is_err()
    );
    assert_eq!(
        store.read(&receipt.id).unwrap().status,
        DeliveryStatus::Unknown
    );
}
