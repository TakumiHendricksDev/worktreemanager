//! Approvals, and how they go stale.
//!
//! # An approval covers exact revisions, not an intention
//!
//! "Publish this" means publish *this head*. If the head moves after you approve, what would be
//! published is not what you looked at, so the approval no longer covers it and has to be asked for
//! again. The same holds for a plan when the issue is edited, a comment when its text changes, and a
//! merge when the pull request gets a new commit. Each approval therefore pins the revisions and
//! hashes it was given, and is checked against the current ones immediately before it is used.
//!
//! # Staleness compares both ways
//!
//! A pin that changed makes an approval stale, and so does a pin that appeared or disappeared. A
//! plan approved while the work item had no issue says nothing about an issue linked since, so the
//! current binding gaining an issue revision is a change like any other. That is why an intent
//! requires only the pins it can never do without: the rest are compared whenever they exist.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::UnixMs;
use super::state::FlowId;

id! {
    /// One approval, as the store numbers it.
    ApprovalId
}

/// What an approval allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalIntent {
    /// Implement this plan.
    Plan,
    /// Implement against this design revision.
    Design,
    /// Mark the pull request ready and move the issue to review, as one approval.
    Publish,
    /// Post exactly this text: a comment, a review, a reviewer request, a tracker comment.
    Post,
    /// Send local fixes to someone else's branch by this route.
    Route,
    /// Merge this head. Only the merge sheet asks for this, and no agent tool can.
    Merge,
}

impl ApprovalIntent {
    /// The pins an approval of this intent cannot be granted without.
    #[must_use]
    pub fn requires(self) -> &'static [Pinned] {
        match self {
            Self::Plan => &[Pinned::PlanRevision],
            Self::Design => &[Pinned::DesignRevision],
            Self::Publish | Self::Merge => &[Pinned::Head],
            Self::Post => &[Pinned::Payload],
            Self::Route => &[Pinned::Remote, Pinned::Ref, Pinned::Diff],
        }
    }
}

/// One thing an approval can be bound to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pinned {
    PlanRevision,
    /// The tracker's revision of the issue, so an edited issue is noticed.
    IssueRevision,
    DesignRevision,
    /// The commit a branch or pull request head pointed at.
    Head,
    /// The commit a review or comment is attached to.
    Commit,
    /// The hash of the exact text or request body that will be sent.
    Payload,
    /// The hash of the diff being sent somewhere.
    Diff,
    Remote,
    Ref,
}

/// The revisions an approval covers, or the ones current now.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Binding(pub BTreeMap<Pinned, String>);

impl Binding {
    #[must_use]
    pub fn with(mut self, pin: Pinned, value: impl Into<String>) -> Self {
        self.0.insert(pin, value.into());
        self
    }
}

/// One difference between what was approved and what is current. `None` means the pin was absent
/// on that side.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PinChange {
    pub pin: Pinned,
    pub approved: Option<String>,
    pub current: Option<String>,
}

/// An approval that no longer covers what would be done, with what changed so the UI can show it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize)]
#[error("the approval is stale: {} pin(s) changed since it was given", .changes.len())]
pub struct Stale {
    pub changes: Vec<PinChange>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ApprovalError {
    #[error("a {intent:?} approval has to pin {missing:?}")]
    Unpinned {
        intent: ApprovalIntent,
        missing: Vec<Pinned>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Approval {
    pub id: ApprovalId,
    pub flow: FlowId,
    pub intent: ApprovalIntent,
    pub binding: Binding,
    pub granted_at: UnixMs,
}

impl Approval {
    /// An approval of `intent` covering exactly `binding`.
    pub fn grant(
        id: ApprovalId,
        flow: FlowId,
        intent: ApprovalIntent,
        binding: Binding,
        granted_at: UnixMs,
    ) -> Result<Self, ApprovalError> {
        let missing: Vec<Pinned> = intent
            .requires()
            .iter()
            .filter(|pin| !binding.0.contains_key(pin))
            .copied()
            .collect();
        if !missing.is_empty() {
            return Err(ApprovalError::Unpinned { intent, missing });
        }
        Ok(Self {
            id,
            flow,
            intent,
            binding,
            granted_at,
        })
    }

    /// Whether this approval still covers `current`: every pin on either side must be on both,
    /// with the same value.
    pub fn check(&self, current: &Binding) -> Result<(), Stale> {
        let approved = &self.binding.0;
        let now = &current.0;
        let mut pins: Vec<Pinned> = approved.keys().chain(now.keys()).copied().collect();
        pins.sort_unstable();
        pins.dedup();

        let changes: Vec<PinChange> = pins
            .into_iter()
            .filter(|pin| approved.get(pin) != now.get(pin))
            .map(|pin| PinChange {
                pin,
                approved: approved.get(&pin).cloned(),
                current: now.get(&pin).cloned(),
            })
            .collect();
        if changes.is_empty() {
            Ok(())
        } else {
            Err(Stale { changes })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(intent: ApprovalIntent, binding: Binding) -> Result<Approval, ApprovalError> {
        Approval::grant(ApprovalId(1), FlowId(1), intent, binding, 0)
    }

    #[test]
    fn an_approval_cannot_be_granted_without_the_pins_its_intent_needs() {
        assert_eq!(
            grant(ApprovalIntent::Publish, Binding::default()),
            Err(ApprovalError::Unpinned {
                intent: ApprovalIntent::Publish,
                missing: vec![Pinned::Head],
            })
        );
        assert!(matches!(
            grant(
                ApprovalIntent::Route,
                Binding::default().with(Pinned::Remote, "fork")
            ),
            Err(ApprovalError::Unpinned { missing, .. }) if missing == [Pinned::Ref, Pinned::Diff]
        ));
    }

    #[test]
    fn an_approval_holds_while_everything_it_pinned_is_unchanged() {
        let binding = Binding::default().with(Pinned::Head, "3f2a9c");
        let approval = grant(ApprovalIntent::Merge, binding.clone()).unwrap();
        assert_eq!(approval.check(&binding), Ok(()));
    }

    #[test]
    fn a_moved_head_makes_a_publish_approval_stale_and_says_what_moved() {
        let approval = grant(
            ApprovalIntent::Publish,
            Binding::default().with(Pinned::Head, "3f2a9c"),
        )
        .unwrap();
        assert_eq!(
            approval.check(&Binding::default().with(Pinned::Head, "7c9e1d")),
            Err(Stale {
                changes: vec![PinChange {
                    pin: Pinned::Head,
                    approved: Some("3f2a9c".to_owned()),
                    current: Some("7c9e1d".to_owned()),
                }],
            })
        );
    }

    #[test]
    fn editing_the_issue_after_the_plan_was_approved_makes_the_approval_stale() {
        let approved = Binding::default()
            .with(Pinned::PlanRevision, "2")
            .with(Pinned::IssueRevision, "17");
        let approval = grant(ApprovalIntent::Plan, approved).unwrap();

        let edited = Binding::default()
            .with(Pinned::PlanRevision, "2")
            .with(Pinned::IssueRevision, "18");
        let stale = approval.check(&edited).unwrap_err();
        assert_eq!(
            stale
                .changes
                .iter()
                .map(|change| change.pin)
                .collect::<Vec<_>>(),
            [Pinned::IssueRevision]
        );
    }

    #[test]
    fn a_plan_approved_before_an_issue_was_linked_does_not_cover_that_issue() {
        let approval = grant(
            ApprovalIntent::Plan,
            Binding::default().with(Pinned::PlanRevision, "1"),
        )
        .unwrap();
        let linked = Binding::default()
            .with(Pinned::PlanRevision, "1")
            .with(Pinned::IssueRevision, "3");
        assert_eq!(
            approval.check(&linked).unwrap_err().changes,
            [PinChange {
                pin: Pinned::IssueRevision,
                approved: None,
                current: Some("3".to_owned()),
            }]
        );
    }

    #[test]
    fn a_comment_approval_goes_stale_when_its_text_is_edited() {
        let approval = grant(
            ApprovalIntent::Post,
            Binding::default()
                .with(Pinned::Payload, "sha256:aa")
                .with(Pinned::Commit, "7c9e1d"),
        )
        .unwrap();
        let edited = Binding::default()
            .with(Pinned::Payload, "sha256:bb")
            .with(Pinned::Commit, "7c9e1d");
        assert!(approval.check(&edited).is_err());
    }
}
