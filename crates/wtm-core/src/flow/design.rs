//! Designing an issue: the revisions a designer publishes for a run, your comments and decisions
//! about each, when an approval stops covering what it approved, and the gate in front of
//! Implementing.
//!
//! # Every revision is kept, and stays addressable
//!
//! A design is argued over: revision 2 answers the comments on revision 1, and "the header was
//! better before" is a comment about revision 1 made while looking at revision 3. So a run's
//! revisions are numbered and never changed, each with the artifact it published, that artifact's
//! fingerprint, the brief it was made from, the feedback it answered, its reuse notes and the
//! sources it drew on. An approval pins one of them, and a revision that could be rewritten would
//! make the pin mean nothing.
//!
//! # An approval covers a revision, its artifact, and what it was drawn from
//!
//! "Implement this design" means this revision, as you saw it, for the project as it was when it
//! was drawn. Open Design's existing rules (ARCHITECTURE §6g) say a new revision and a changed
//! source both invalidate an approval, and this reuses them as pins ([`design_binding`]): the
//! revision's number, the fingerprint of its HTML and reuse notes, and the hash of the fingerprints
//! of the project files its evidence came from. A newer revision, an artifact that no longer reads
//! as approved, or a component file edited since all make the approval stale, and staleness is the
//! same [`Approval::check`] every other approval uses.
//!
//! # Implementing waits for the design the plan asked for
//!
//! [`implementing_gate`] is the rule the engine applies before a run may enter Implementing: an
//! approved plan that still covers the issue, and, when the plan asked for a design (and you did not
//! approve it with the design skipped) or a design was started for the run anyway, an approved
//! design revision that still covers what would be built. It is a rule about standings, so it can
//! be tested without a store, a worktree or an engine, and the engine asks it on every route into
//! Implementing rather than trusting each caller to.

use serde::{Deserialize, Serialize};

use super::UnixMs;
use super::approval::{Approval, ApprovalId, Binding, PinChange, Pinned};
use super::planning::PlanStanding;
use super::publish::hash_of;
use super::run::{InvocationId, StepId};
use super::state::FlowId;
use crate::model::ProjectId;

id! {
    /// One design revision, as the store numbers it.
    DesignRevisionId
}

id! {
    /// One comment on a design revision, as the store numbers it.
    DesignCommentId
}

id! {
    /// One decision about a design revision, as the store numbers it.
    DesignDecisionId
}

/// Why a run designs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DesignOrigin {
    /// The plan you approved said the work needs a design.
    Plan,
    /// You asked for one, on a plan that did not, or one approved with the design skipped.
    You,
}

/// An Implement run's design: the role that draws it, where, and the prototype it publishes into.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Design {
    pub flow: FlowId,
    pub project: ProjectId,
    /// The worktree's id, which is its path.
    pub worktree: String,
    pub key: String,
    pub role: String,
    pub origin: DesignOrigin,
    /// What you asked for, in your words, when you asked for the design yourself.
    pub request: Option<String>,
    /// The prototype under WTM's data root its revisions are artifacts of, once the designer has
    /// prepared one.
    pub prototype: Option<String>,
    pub created_at: UnixMs,
    pub updated_at: UnixMs,
}

/// A run's design to record, the first time it designs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewDesign {
    pub flow: FlowId,
    pub project: ProjectId,
    pub worktree: String,
    pub key: String,
    pub role: String,
    pub origin: DesignOrigin,
    pub request: Option<String>,
}

/// One project file a revision's evidence came from, and the fingerprint of its content then.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourcePin {
    /// Relative to the worktree.
    pub path: String,
    pub fingerprint: String,
}

/// One revision the designer published, numbered within its run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesignRevision {
    pub id: DesignRevisionId,
    pub flow: FlowId,
    /// 1 for the first, one more for each after it.
    pub revision: u32,
    pub prototype: String,
    /// The artifact's id within its prototype: the directory its HTML and reuse notes are in.
    pub review: String,
    /// The fingerprint of the artifact's HTML and reuse notes together.
    pub fingerprint: String,
    /// What the designer was asked to draw, as it told the prototype tool.
    pub brief: String,
    /// What this revision answers: your note and comments on the one before, as they were sent.
    pub feedback: Option<String>,
    /// The designer's account of which components and tokens it reused and what it approximated.
    pub reuse: String,
    pub sources: Vec<SourcePin>,
    /// The plan revision it designs.
    pub plan_revision: u32,
    pub step: Option<StepId>,
    pub invocation: Option<InvocationId>,
    pub submitted_at: UnixMs,
}

/// A revision to record, before the store numbers it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewDesignRevision {
    pub flow: FlowId,
    pub prototype: String,
    pub review: String,
    pub fingerprint: String,
    pub brief: String,
    pub feedback: Option<String>,
    pub reuse: String,
    pub sources: Vec<SourcePin>,
    pub plan_revision: u32,
    pub step: Option<StepId>,
    pub invocation: Option<InvocationId>,
}

/// A comment on one revision, such as "Header: use the page header". Kept with the revision it was
/// made on, and sent to the designer with the next request for changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesignComment {
    pub id: DesignCommentId,
    pub flow: FlowId,
    pub revision: u32,
    pub text: String,
    pub at: UnixMs,
}

/// What you decided about one revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DesignVerdict {
    /// Implement against it.
    Approved { approval: ApprovalId },
    /// Not this one: the designer is told in the same conversation, with the revision's comments.
    ChangesRequested { note: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesignDecision {
    pub id: DesignDecisionId,
    pub flow: FlowId,
    pub revision: u32,
    pub verdict: DesignVerdict,
    pub at: UnixMs,
}

/// What an approval of a design was of, kept with it: the revision and exactly what it pinned, so
/// the window can show what you approved and the implementer is handed that and nothing newer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DesignApproval {
    pub revision: u32,
    pub prototype: String,
    pub review: String,
    pub fingerprint: String,
    pub sources: Vec<SourcePin>,
    pub plan_revision: u32,
}

impl DesignApproval {
    /// An approval of exactly `revision`.
    #[must_use]
    pub fn of(revision: &DesignRevision) -> Self {
        Self {
            revision: revision.revision,
            prototype: revision.prototype.clone(),
            review: revision.review.clone(),
            fingerprint: revision.fingerprint.clone(),
            sources: revision.sources.clone(),
            plan_revision: revision.plan_revision,
        }
    }

    /// The pins this approval covers.
    #[must_use]
    pub fn binding(&self) -> Binding {
        design_binding(
            self.revision,
            &self.fingerprint,
            &sources_pin(&self.sources),
        )
    }
}

/// The value of [`Pinned::Sources`] for these sources: a hash of their paths and fingerprints, so a
/// change to any one of them, or one more or one fewer, is a change.
#[must_use]
pub fn sources_pin(sources: &[SourcePin]) -> String {
    hash_of(&sources)
}

/// The pins a design approval covers: the revision, its artifact's fingerprint, and its sources.
#[must_use]
pub fn design_binding(revision: u32, fingerprint: &str, sources: &str) -> Binding {
    Binding::default()
        .with(Pinned::DesignRevision, revision.to_string())
        .with(Pinned::Payload, fingerprint)
        .with(Pinned::Sources, sources)
}

/// What an approved design is measured against now, as the engine reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesignNow {
    /// The newest revision's number.
    pub latest: u32,
    /// The newest revision's artifact as it reads from disk now, or `None` when it cannot be read
    /// or no longer matches the fingerprint it was published with.
    pub fingerprint: Option<String>,
    /// The newest revision's sources, each with its content's fingerprint in the worktree now. A
    /// file that is gone carries a fingerprint that says so.
    pub sources: Vec<SourcePin>,
}

/// The fingerprint that stands for an artifact that could not be read back as it was published.
pub const UNREADABLE: &str = "unreadable";

impl DesignNow {
    #[must_use]
    pub fn binding(&self) -> Binding {
        design_binding(
            self.latest,
            self.fingerprint.as_deref().unwrap_or(UNREADABLE),
            &sources_pin(&self.sources),
        )
    }
}

/// An approval of an earlier revision that a newer one has replaced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Replaced {
    pub approval: ApprovalId,
    pub revision: u32,
}

/// Where a run's design stands, for the Design tab, the queue and the gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum DesignStanding {
    /// No revision yet.
    None,
    /// The newest revision waits for your decision. `replaced` is your approval of an earlier
    /// revision, which this one no longer lets stand.
    Waiting {
        revision: u32,
        replaced: Option<Replaced>,
    },
    /// You asked for changes to the newest revision; the designer is drawing the next.
    Revising { revision: u32, note: String },
    /// Approved, and the approval still covers the newest revision, its artifact and its sources.
    Approved { approval: ApprovalId, revision: u32 },
    /// Approved, but the artifact or the files it was drawn from have changed since.
    Stale {
        approval: ApprovalId,
        revision: u32,
        changes: Vec<PinChange>,
    },
}

/// Where a design stands, given the newest revision, every decision oldest first, the approval the
/// newest approving decision made with what it was of, and what that approval is measured against
/// now.
#[must_use]
pub fn standing(
    latest: Option<&DesignRevision>,
    decisions: &[DesignDecision],
    approval: Option<(&Approval, &DesignApproval)>,
    now: &DesignNow,
) -> DesignStanding {
    let Some(latest) = latest else {
        return DesignStanding::None;
    };
    // Asking for changes to the newest revision outranks having approved it before: you approved
    // it, looked again, and asked for something else, so it is not what is to be built.
    if let Some(DesignDecision {
        revision,
        verdict: DesignVerdict::ChangesRequested { note },
        ..
    }) = decisions.last()
        && *revision == latest.revision
    {
        return DesignStanding::Revising {
            revision: *revision,
            note: note.clone(),
        };
    }
    let Some((approval, subject)) = approval else {
        return DesignStanding::Waiting {
            revision: latest.revision,
            replaced: None,
        };
    };
    match approval.check(&now.binding()) {
        Ok(()) => DesignStanding::Approved {
            approval: approval.id,
            revision: subject.revision,
        },
        // A newer revision is not a stale approval to look into but a new one to decide on: the
        // approval of the old one is shown as replaced beside it.
        Err(_) if subject.revision != latest.revision => DesignStanding::Waiting {
            revision: latest.revision,
            replaced: Some(Replaced {
                approval: approval.id,
                revision: subject.revision,
            }),
        },
        Err(stale) => DesignStanding::Stale {
            approval: approval.id,
            revision: subject.revision,
            changes: stale.changes,
        },
    }
}

/// Why a run may not enter Implementing yet. The message is a sentence the window shows as it is.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum Blocked {
    #[error("there is no plan to implement yet")]
    NoPlan,
    #[error("plan revision {revision} waits for your approval")]
    PlanNotApproved { revision: u32 },
    #[error("you asked for changes to plan revision {revision}, and the planner is revising it")]
    PlanRevising { revision: u32 },
    #[error(
        "your approval of plan revision {revision} no longer covers the issue as it reads now; \
         approve it again in the Plan tab"
    )]
    PlanStale { revision: u32 },
    #[error("the plan needs a design, and no design revision has been published yet")]
    NoDesign,
    #[error("the plan needs a design, and design revision {revision} waits for your approval")]
    DesignNotApproved { revision: u32 },
    #[error("you asked for changes to design revision {revision}, and the designer is revising it")]
    DesignRevising { revision: u32 },
    #[error(
        "your approval of design revision {revision} no longer covers it: the artifact or the \
         project files it was drawn from changed; ask for a fresh revision and approve that"
    )]
    DesignStale { revision: u32 },
}

/// What the gate in front of Implementing reads.
#[derive(Debug, Clone, Copy)]
pub struct GateFacts<'a> {
    pub plan: &'a PlanStanding,
    /// Whether the approved plan said the work needs a design.
    pub design_needed: bool,
    /// Whether a design was started for this run, because the plan asked or because you did.
    pub designing: bool,
    pub design: &'a DesignStanding,
}

/// Whether a run may enter Implementing now.
///
/// An approved plan that still covers the issue, always (§4.2: nothing writes without one). A
/// design only when one is required: the plan asked for it and you did not approve the plan with
/// the design skipped, or a design was started for the run anyway — once you are looking at
/// revisions, implementing before you approve one would build something you have not agreed to.
///
/// # Errors
///
/// [`Blocked`], with the first thing that stands in the way.
pub fn implementing_gate(facts: &GateFacts<'_>) -> Result<(), Blocked> {
    let skip_design = match facts.plan {
        PlanStanding::None => return Err(Blocked::NoPlan),
        PlanStanding::Waiting { revision } => {
            return Err(Blocked::PlanNotApproved {
                revision: *revision,
            });
        }
        PlanStanding::Revising { revision, .. } => {
            return Err(Blocked::PlanRevising {
                revision: *revision,
            });
        }
        PlanStanding::Stale { revision, .. } => {
            return Err(Blocked::PlanStale {
                revision: *revision,
            });
        }
        PlanStanding::Approved { skip_design, .. } => *skip_design,
    };
    let required = facts.designing || (facts.design_needed && !skip_design);
    if !required {
        return Ok(());
    }
    match facts.design {
        DesignStanding::Approved { .. } => Ok(()),
        DesignStanding::None => Err(Blocked::NoDesign),
        DesignStanding::Waiting { revision, .. } => Err(Blocked::DesignNotApproved {
            revision: *revision,
        }),
        DesignStanding::Revising { revision, .. } => Err(Blocked::DesignRevising {
            revision: *revision,
        }),
        DesignStanding::Stale { revision, .. } => Err(Blocked::DesignStale {
            revision: *revision,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::approval::ApprovalIntent;

    fn source(path: &str, fingerprint: &str) -> SourcePin {
        SourcePin {
            path: path.to_owned(),
            fingerprint: fingerprint.to_owned(),
        }
    }

    fn revision(number: u32, fingerprint: &str) -> DesignRevision {
        DesignRevision {
            id: DesignRevisionId(i64::from(number)),
            flow: FlowId(1),
            revision: number,
            prototype: "p".to_owned(),
            review: format!("r{number}"),
            fingerprint: fingerprint.to_owned(),
            brief: "A unit export screen".to_owned(),
            feedback: None,
            reuse: "PageHeader, DataTable".to_owned(),
            sources: vec![source("src/components/PageHeader.svelte", "f1")],
            plan_revision: 1,
            step: None,
            invocation: None,
            submitted_at: 0,
        }
    }

    fn approve(of: &DesignRevision, id: i64) -> (Approval, DesignApproval) {
        let subject = DesignApproval::of(of);
        let approval = Approval::grant(
            ApprovalId(id),
            FlowId(1),
            ApprovalIntent::Design,
            subject.binding(),
            0,
        )
        .unwrap();
        (approval, subject)
    }

    fn approved(revision: u32, approval: i64) -> DesignDecision {
        DesignDecision {
            id: DesignDecisionId(approval),
            flow: FlowId(1),
            revision,
            verdict: DesignVerdict::Approved {
                approval: ApprovalId(approval),
            },
            at: 0,
        }
    }

    fn changes(revision: u32) -> DesignDecision {
        DesignDecision {
            id: DesignDecisionId(100),
            flow: FlowId(1),
            revision,
            verdict: DesignVerdict::ChangesRequested {
                note: "Header: use PageHeader".to_owned(),
            },
            at: 0,
        }
    }

    /// The design as it reads now when nothing has moved since `latest` was published.
    fn unchanged(latest: &DesignRevision) -> DesignNow {
        DesignNow {
            latest: latest.revision,
            fingerprint: Some(latest.fingerprint.clone()),
            sources: latest.sources.clone(),
        }
    }

    #[test]
    fn a_design_approval_holds_while_its_revision_artifact_and_sources_are_unchanged() {
        let first = revision(1, "sha256:aa");
        let (approval, subject) = approve(&first, 7);
        assert_eq!(
            standing(
                Some(&first),
                &[approved(1, 7)],
                Some((&approval, &subject)),
                &unchanged(&first)
            ),
            DesignStanding::Approved {
                approval: ApprovalId(7),
                revision: 1
            }
        );
    }

    #[test]
    fn a_new_revision_replaces_the_approval_of_an_earlier_one_and_waits_for_its_own() {
        let first = revision(1, "sha256:aa");
        let second = revision(2, "sha256:bb");
        let (approval, subject) = approve(&first, 7);
        let standing = standing(
            Some(&second),
            &[approved(1, 7), changes(1)],
            Some((&approval, &subject)),
            &unchanged(&second),
        );
        assert_eq!(
            standing,
            DesignStanding::Waiting {
                revision: 2,
                replaced: Some(Replaced {
                    approval: ApprovalId(7),
                    revision: 1
                }),
            }
        );
        // And the approval itself says what moved, which is how the window shows it as replaced.
        let moved = approval.check(&unchanged(&second).binding()).unwrap_err();
        assert!(
            moved
                .changes
                .iter()
                .any(|change| change.pin == Pinned::DesignRevision)
        );
    }

    #[test]
    fn a_new_revision_the_designer_published_without_being_asked_still_replaces_the_approval() {
        // A resumed designer that publishes again: nobody asked for changes, but what would be
        // built is no longer what you approved.
        let first = revision(1, "sha256:aa");
        let second = revision(2, "sha256:bb");
        let (approval, subject) = approve(&first, 7);
        assert!(matches!(
            standing(
                Some(&second),
                &[approved(1, 7)],
                Some((&approval, &subject)),
                &unchanged(&second)
            ),
            DesignStanding::Waiting {
                revision: 2,
                replaced: Some(_)
            }
        ));
    }

    #[test]
    fn a_changed_source_file_makes_a_design_approval_stale() {
        let first = revision(1, "sha256:aa");
        let (approval, subject) = approve(&first, 7);
        let mut now = unchanged(&first);
        now.sources = vec![source("src/components/PageHeader.svelte", "f2")];
        let DesignStanding::Stale {
            revision, changes, ..
        } = standing(
            Some(&first),
            &[approved(1, 7)],
            Some((&approval, &subject)),
            &now,
        )
        else {
            panic!("an edited source must make the approval stale");
        };
        assert_eq!(revision, 1);
        assert_eq!(
            changes.iter().map(|c| c.pin).collect::<Vec<_>>(),
            [Pinned::Sources]
        );
    }

    #[test]
    fn a_source_file_that_was_removed_makes_a_design_approval_stale() {
        let first = revision(1, "sha256:aa");
        let (approval, subject) = approve(&first, 7);
        let mut now = unchanged(&first);
        now.sources.clear();
        assert!(matches!(
            standing(
                Some(&first),
                &[approved(1, 7)],
                Some((&approval, &subject)),
                &now
            ),
            DesignStanding::Stale { .. }
        ));
    }

    #[test]
    fn an_artifact_that_no_longer_reads_as_published_makes_its_approval_stale() {
        let first = revision(1, "sha256:aa");
        let (approval, subject) = approve(&first, 7);
        let mut now = unchanged(&first);
        now.fingerprint = None;
        let DesignStanding::Stale { changes, .. } = standing(
            Some(&first),
            &[approved(1, 7)],
            Some((&approval, &subject)),
            &now,
        ) else {
            panic!("an unreadable artifact must make the approval stale");
        };
        assert_eq!(changes[0].pin, Pinned::Payload);
        assert_eq!(changes[0].current.as_deref(), Some(UNREADABLE));
    }

    #[test]
    fn asking_for_changes_to_the_newest_revision_outranks_having_approved_it() {
        let first = revision(1, "sha256:aa");
        let (approval, subject) = approve(&first, 7);
        assert!(matches!(
            standing(
                Some(&first),
                &[approved(1, 7), changes(1)],
                Some((&approval, &subject)),
                &unchanged(&first)
            ),
            DesignStanding::Revising { revision: 1, .. }
        ));
    }

    #[test]
    fn a_revision_nobody_has_decided_on_waits() {
        let first = revision(1, "sha256:aa");
        assert_eq!(
            standing(Some(&first), &[], None, &unchanged(&first)),
            DesignStanding::Waiting {
                revision: 1,
                replaced: None
            }
        );
        assert_eq!(
            standing(None, &[], None, &unchanged(&first)),
            DesignStanding::None
        );
    }

    #[test]
    fn a_design_approval_cannot_be_granted_without_its_revision_pinned() {
        assert!(
            Approval::grant(
                ApprovalId(1),
                FlowId(1),
                ApprovalIntent::Design,
                Binding::default().with(Pinned::Payload, "sha256:aa"),
                0,
            )
            .is_err()
        );
    }

    // ── the gate ──

    fn plan_approved(skip_design: bool) -> PlanStanding {
        PlanStanding::Approved {
            approval: ApprovalId(1),
            revision: 1,
            skip_design,
        }
    }

    fn gate(
        plan: &PlanStanding,
        design_needed: bool,
        designing: bool,
        design: &DesignStanding,
    ) -> Result<(), Blocked> {
        implementing_gate(&GateFacts {
            plan,
            design_needed,
            designing,
            design,
        })
    }

    #[test]
    fn nothing_is_implemented_without_an_approved_plan_that_still_covers_the_issue() {
        let none = DesignStanding::None;
        assert_eq!(
            gate(&PlanStanding::None, false, false, &none),
            Err(Blocked::NoPlan)
        );
        assert_eq!(
            gate(&PlanStanding::Waiting { revision: 2 }, false, false, &none),
            Err(Blocked::PlanNotApproved { revision: 2 })
        );
        assert_eq!(
            gate(
                &PlanStanding::Stale {
                    approval: ApprovalId(1),
                    revision: 2,
                    changes: Vec::new(),
                    diff: Vec::new(),
                },
                false,
                false,
                &none
            ),
            Err(Blocked::PlanStale { revision: 2 })
        );
    }

    #[test]
    fn a_plan_that_needs_no_design_goes_straight_to_implementing() {
        assert_eq!(
            gate(&plan_approved(false), false, false, &DesignStanding::None),
            Ok(())
        );
    }

    #[test]
    fn a_plan_that_needs_a_design_cannot_be_implemented_until_a_revision_is_approved() {
        let plan = plan_approved(false);
        assert_eq!(
            gate(&plan, true, true, &DesignStanding::None),
            Err(Blocked::NoDesign)
        );
        // Even when nothing was started: the plan's word is enough.
        assert_eq!(
            gate(&plan, true, false, &DesignStanding::None),
            Err(Blocked::NoDesign)
        );
        assert_eq!(
            gate(
                &plan,
                true,
                true,
                &DesignStanding::Waiting {
                    revision: 1,
                    replaced: None
                }
            ),
            Err(Blocked::DesignNotApproved { revision: 1 })
        );
        assert_eq!(
            gate(
                &plan,
                true,
                true,
                &DesignStanding::Revising {
                    revision: 1,
                    note: "Header".to_owned()
                }
            ),
            Err(Blocked::DesignRevising { revision: 1 })
        );
        assert_eq!(
            gate(
                &plan,
                true,
                true,
                &DesignStanding::Approved {
                    approval: ApprovalId(4),
                    revision: 1
                }
            ),
            Ok(())
        );
    }

    #[test]
    fn a_new_revision_after_an_approval_blocks_implementing_until_it_is_approved_too() {
        let plan = plan_approved(false);
        let replaced = DesignStanding::Waiting {
            revision: 2,
            replaced: Some(Replaced {
                approval: ApprovalId(4),
                revision: 1,
            }),
        };
        assert_eq!(
            gate(&plan, true, true, &replaced),
            Err(Blocked::DesignNotApproved { revision: 2 })
        );
    }

    #[test]
    fn a_stale_design_approval_blocks_implementing() {
        let stale = DesignStanding::Stale {
            approval: ApprovalId(4),
            revision: 1,
            changes: Vec::new(),
        };
        assert_eq!(
            gate(&plan_approved(false), true, true, &stale),
            Err(Blocked::DesignStale { revision: 1 })
        );
    }

    #[test]
    fn a_plan_approved_with_the_design_skipped_goes_straight_to_implementing() {
        assert_eq!(
            gate(&plan_approved(true), true, false, &DesignStanding::None),
            Ok(())
        );
    }

    #[test]
    fn a_design_you_asked_for_is_required_even_when_the_plan_did_not_ask_for_one() {
        assert_eq!(
            gate(
                &plan_approved(true),
                false,
                true,
                &DesignStanding::Waiting {
                    revision: 1,
                    replaced: None
                }
            ),
            Err(Blocked::DesignNotApproved { revision: 1 })
        );
    }
}
