//! A restart or ambiguous provider write must never turn one exact review into two instructions.
//!
//! And an uncertain one must never block a conversation for good: the user can settle what no
//! provider evidence will, without anything being replayed. The journal is read once and kept in
//! memory, so these tests also pin what a restart, and only a restart, reads back from disk.
#![allow(clippy::unwrap_used)]
use wtm_app_lib::native_sessions::delivery::{
    DeliveryStatus, Owner, Resolution, SETTLED_RETENTION, Store,
};
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
    // A linked record in the journal is one nothing can vouch for, so the next process sends nothing.
    drop(store);
    assert!(
        Store::new(root.path().into())
            .prepare(owner(), "next", "next", None)
            .is_err()
    );
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
    // The live store remembers what it wrote; the damage is what the next process reads.
    drop(store);
    let restarted = Store::new(root.path().into());
    assert!(
        restarted
            .prepare(
                owner(),
                "Implement",
                "approved",
                Some(("prototype", "review"))
            )
            .is_err()
    );
    assert!(
        restarted
            .prepare(owner(), "Something else", "other", None)
            .is_err()
    );
    assert!(restarted.receipts(&owner(), &[]).is_err());
}

fn uncertain(store: &Store, text: &str) -> wtm_app_lib::native_sessions::delivery::Delivery {
    let receipt = store
        .prepare(owner(), "Implement", text, Some(("prototype", text)))
        .unwrap();
    store
        .transition(
            &receipt.id,
            &owner(),
            DeliveryStatus::Pending,
            DeliveryStatus::Unknown,
            None,
        )
        .unwrap()
}

#[test]
fn the_user_can_settle_an_unknown_delivery_as_not_delivered_and_only_an_explicit_retry_follows() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::new(root.path().into());
    let lost = uncertain(&store, "approved");
    // While it is uncertain, nothing else may be sent into this conversation.
    assert!(store.prepare(owner(), "Next", "next", None).is_err());
    assert!(
        store
            .resolve(&lost.id, |o| *o == owner(), Resolution::Withdrawn)
            .is_err(),
        "an uncertain instruction may have arrived, so it cannot be withdrawn"
    );
    let resolved = store
        .resolve(&lost.id, |o| *o == owner(), Resolution::NotDelivered)
        .unwrap();
    assert_eq!(resolved.status, DeliveryStatus::Rejected);
    assert_eq!(resolved.resolution, Some(Resolution::NotDelivered));
    assert!(resolved.error.unwrap().contains("did not resend"));
    drop(store);
    let restarted = Store::new(root.path().into());
    assert_eq!(
        restarted.read(&lost.id).unwrap().resolution,
        Some(Resolution::NotDelivered)
    );
    // The same review may now be approved again, as a new receipt, and only on request.
    let retry = restarted
        .prepare(
            owner(),
            "Implement",
            "approved",
            Some(("prototype", "approved")),
        )
        .unwrap();
    assert_ne!(retry.id, lost.id);
    assert_eq!(retry.status, DeliveryStatus::Pending);
}

#[test]
fn an_unwanted_pending_instruction_can_be_withdrawn_only_while_nothing_has_collected_it() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::new(root.path().into());
    let pending = store
        .prepare(owner(), "Implement", "approved", None)
        .unwrap();
    assert!(
        store
            .resolve(&pending.id, |o| *o == owner(), Resolution::NotDelivered)
            .is_err(),
        "a pending instruction was never dispatched; there is nothing to look for"
    );
    let elsewhere = store.resolve(&pending.id, |_| false, Resolution::Withdrawn);
    assert!(elsewhere.is_err(), "another pane cannot answer for it");
    let withdrawn = store
        .resolve(&pending.id, |o| *o == owner(), Resolution::Withdrawn)
        .unwrap();
    assert_eq!(withdrawn.status, DeliveryStatus::Rejected);
    assert_eq!(withdrawn.resolution, Some(Resolution::Withdrawn));
    assert!(
        store
            .resolve(&pending.id, |o| *o == owner(), Resolution::Withdrawn)
            .is_err()
    );
    assert!(store.prepare(owner(), "Next", "next", None).is_ok());
}

#[test]
fn provider_evidence_overturns_a_users_not_delivered_but_never_a_withdrawal() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::new(root.path().into());
    let lost = uncertain(&store, "approved");
    store
        .resolve(&lost.id, |o| *o == owner(), Resolution::NotDelivered)
        .unwrap();
    let confirmed = store.confirm(&lost.id, &owner()).unwrap();
    assert_eq!(confirmed.status, DeliveryStatus::Delivered);
    assert_eq!(confirmed.resolution, None);
    assert_eq!(confirmed.error, None);

    let withdrawn = store.prepare(owner(), "Implement", "other", None).unwrap();
    store
        .resolve(&withdrawn.id, |o| *o == owner(), Resolution::Withdrawn)
        .unwrap();
    assert!(store.confirm(&withdrawn.id, &owner()).is_err());
    let other = Owner {
        conversation: "other".into(),
        ..owner()
    };
    let next = uncertain(&store, "third");
    assert!(store.confirm(&next.id, &other).is_err());
    assert_eq!(
        store.read(&next.id).unwrap().status,
        DeliveryStatus::Unknown
    );
}

#[test]
fn an_earlier_conversations_receipts_stay_listed_only_while_they_are_unresolved() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::new(root.path().into());
    let earlier = Owner {
        conversation: "before-clear".into(),
        ..owner()
    };
    let open = store
        .prepare(earlier.clone(), "Open", "open", None)
        .unwrap();
    let elsewhere = Owner {
        worktree: "another".into(),
        conversation: "before-clear".into(),
        ..owner()
    };
    store.prepare(elsewhere, "Other", "other", None).unwrap();
    let listed = store
        .receipts(&owner(), &["before-clear".to_owned()])
        .unwrap();
    assert_eq!(
        listed.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(),
        [open.id.as_str()]
    );
    assert!(store.receipts(&owner(), &[]).unwrap().is_empty());
    store
        .resolve(&open.id, |o| *o == earlier, Resolution::Withdrawn)
        .unwrap();
    assert!(
        store
            .receipts(&owner(), &["before-clear".to_owned()])
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store.list(&earlier).len(),
        1,
        "its own conversation keeps it"
    );
}

#[test]
fn the_journal_is_answered_from_memory_and_reread_only_by_the_next_process() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::new(root.path().into());
    let receipt = store
        .prepare(owner(), "Implement", "approved", None)
        .unwrap();
    let path = root.path().join(format!("{}.json", receipt.id));
    let mut edited = receipt.clone();
    edited.status = DeliveryStatus::Delivered;
    std::fs::write(&path, serde_json::to_vec(&edited).unwrap()).unwrap();
    assert_eq!(
        store.list(&owner())[0].status,
        DeliveryStatus::Pending,
        "a listing read the disk"
    );
    assert_eq!(
        Store::new(root.path().into()).list(&owner())[0].status,
        DeliveryStatus::Delivered
    );
}

#[test]
fn settled_receipts_are_pruned_after_the_retention_and_unresolved_ones_never_by_age() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::new(root.path().into());
    let old = uncertain(&store, "old");
    store.confirm(&old.id, &owner()).unwrap();
    let recent = uncertain(&store, "recent");
    store.confirm(&recent.id, &owner()).unwrap();
    let waiting = store.prepare(owner(), "Waiting", "waiting", None).unwrap();
    let written = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
    for id in [&old.id, &waiting.id] {
        std::fs::File::options()
            .write(true)
            .open(root.path().join(format!("{id}.json")))
            .unwrap()
            .set_modified(written)
            .unwrap();
    }
    let recently = written + SETTLED_RETENTION + std::time::Duration::from_secs(60);
    std::fs::File::options()
        .write(true)
        .open(root.path().join(format!("{}.json", recent.id)))
        .unwrap()
        .set_modified(recently - std::time::Duration::from_secs(120))
        .unwrap();
    let now = u64::try_from(
        recently
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    assert_eq!(store.prune(now), 1);
    assert!(store.read(&old.id).is_err());
    assert!(!root.path().join(format!("{}.json", old.id)).exists());
    assert!(store.read(&recent.id).is_ok());
    assert!(store.read(&waiting.id).is_ok());
    assert_eq!(Store::new(root.path().into()).list(&owner()).len(), 2);
}

#[test]
fn a_removed_worktree_takes_every_one_of_its_receipts_with_it() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::new(root.path().into());
    let gone = uncertain(&store, "gone");
    let kept = Owner {
        worktree: "kept".into(),
        ..owner()
    };
    store.prepare(kept.clone(), "Kept", "kept", None).unwrap();
    assert_eq!(store.forget_worktree("worktree"), 1);
    assert!(store.read(&gone.id).is_err());
    assert_eq!(Store::new(root.path().into()).list(&kept).len(), 1);
}

#[test]
fn a_design_answers_with_its_live_receipt_rather_than_an_earlier_rejected_attempt() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::new(root.path().into());
    let first = uncertain(&store, "approved");
    store
        .resolve(&first.id, |o| *o == owner(), Resolution::NotDelivered)
        .unwrap();
    assert_eq!(
        store.for_review("prototype", "approved").unwrap().id,
        first.id
    );
    let retry = store
        .prepare(
            owner(),
            "Implement",
            "approved",
            Some(("prototype", "approved")),
        )
        .unwrap();
    assert_eq!(
        store.for_review("prototype", "approved").unwrap().id,
        retry.id
    );
    assert!(store.for_review("prototype", "another").is_none());
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
