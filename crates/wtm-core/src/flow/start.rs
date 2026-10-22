//! Start Work: an issue becomes one run with one worktree, however often it is started.
//!
//! # Once, by the store
//!
//! Start is a button, ⌘↵ in the queue and a drop on the board's start column, and any two of them
//! can arrive together. The store keeps one work item per issue and one open flow of each kind per
//! work item ([`FlowStore`](crate::ports::FlowStore)), and the job that makes the worktree is keyed
//! by the work item ([`StartStore`](crate::ports::StartStore)), so a second Start finds what the
//! first made and focuses it. Nothing checks first and writes second.
//!
//! # Where the work happens, in the plan's order (§4.4)
//!
//! 1. **The worktree an earlier Start bound the work item to**, while it still exists. That is a
//!    decision already made, so it is used without asking.
//! 2. **A worktree whose branch or directory carries the key.**
//! 3. **The work item's pull request's head branch**, where it is checked out, or adopted.
//! 4. **A branch the project's `existing_branch_match` offers.**
//! 5. **A new worktree from the base**, planned by the New Worktree form's own pipeline.
//!
//! [`resolve`] decides from what the caller read and reads nothing itself, so the order is tested
//! here at a fixed state rather than against a repository.
//!
//! # A preview when there is a choice
//!
//! One existing candidate is the answer, and the window says which and why. More than one is a
//! choice nobody should make by rule — two worktrees can carry the same key for different reasons
//! — so the preview lists them in order, with a new worktree as the last option, and nothing is
//! recorded until one is picked. A lone branch from `existing_branch_match` is offered rather than
//! taken unless every matcher says `prefer`, because that is what the project's own form does with
//! it.
//!
//! # The job, and a crash in the middle of it
//!
//! Making a worktree runs `git worktree add` and then the project's setup, which can take minutes
//! and can be cut off by a quit or a crash. The job ([`StartJob`]) is written before either starts,
//! with the place it is making, so a relaunch knows what was under way: the worktree is there or it
//! is not. Resuming uses the existing pipeline's own two entry points — Retry setup on a worktree
//! that exists, which is also how the New Worktree form adopts one, or the create again when it
//! does not — and never a second worktree beside the first.
//!
//! # The board
//!
//! A card dropped on the configured start column is Start Work; on any other column it is a change
//! of team status and nothing else ([`drop_meaning`]). Dragging an issue backward never cancels its
//! run or touches its worktree: the run is the record of work done, and a status is the team's
//! opinion of where the issue is.

use serde::{Deserialize, Serialize};

use super::UnixMs;
use super::run::Field;
use super::state::FlowId;
use super::tracker::{StatusMap, StatusRole, same_status};
use super::work::WorkItemId;
use crate::model::ProjectId;

/// `[workflow.start]`: what Start Work does beyond starting the run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartSpec {
    /// Move the issue to the first `started` status when you start it. On unless turned off,
    /// because that is what starting means on most teams' boards, and it is shown on the Start
    /// button before you press it. Nothing else moves an issue on its own.
    #[serde(default = "yes")]
    pub move_issue: bool,
    /// The New Worktree form field Start Work fills with the issue's key when it makes a worktree.
    /// Absent: the form's first required text field, which is where a key goes on a form that has
    /// one and the only free text a form without one asks for.
    #[serde(default)]
    pub issue_field: Option<String>,
}

const fn yes() -> bool {
    true
}

impl Default for StartSpec {
    fn default() -> Self {
        Self {
            move_issue: true,
            issue_field: None,
        }
    }
}

impl StartSpec {
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// An existing worktree, as resolution sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Place {
    /// The worktree's id, which is its path.
    pub id: String,
    pub branch: Option<String>,
    /// The issue key its branch or directory carries, if any.
    pub key: Option<String>,
}

/// One place an issue's work could happen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum Candidate {
    /// The worktree an earlier Start bound the work item to.
    Bound { worktree: Place },
    /// A worktree whose branch or directory carries the key.
    KeyInName { worktree: Place },
    /// The work item's pull request's head branch: the worktree it is checked out in, or the branch
    /// to adopt into a new one.
    PullRequestHead {
        number: u64,
        branch: String,
        worktree: Option<Place>,
    },
    /// A branch the project's `existing_branch_match` offers: the worktree it is checked out in, or
    /// the branch to adopt into the directory the matcher names.
    BranchMatch {
        branch: String,
        worktree: Option<Place>,
        remote_only: bool,
        directory: String,
    },
    /// A new worktree from the base, as the New Worktree form would make it.
    New {
        branch: Option<String>,
        directory: String,
        base: String,
    },
}

impl Candidate {
    /// How the window names a candidate when it hands a choice back. Unique within a resolution:
    /// a worktree is listed once, and so is a branch.
    #[must_use]
    pub fn id(&self) -> String {
        match self.worktree() {
            Some(place) => format!("worktree:{}", place.id),
            None => match self {
                Self::PullRequestHead { branch, .. } | Self::BranchMatch { branch, .. } => {
                    format!("branch:{branch}")
                }
                _ => "new".to_owned(),
            },
        }
    }

    /// The existing worktree this candidate is, when it is one.
    #[must_use]
    pub fn worktree(&self) -> Option<&Place> {
        match self {
            Self::Bound { worktree } | Self::KeyInName { worktree } => Some(worktree),
            Self::PullRequestHead { worktree, .. } | Self::BranchMatch { worktree, .. } => {
                worktree.as_ref()
            }
            Self::New { .. } => None,
        }
    }

    /// The branch this candidate puts the work on, when it names one.
    #[must_use]
    pub fn branch(&self) -> Option<&str> {
        match self {
            Self::Bound { worktree } | Self::KeyInName { worktree } => worktree.branch.as_deref(),
            Self::PullRequestHead { branch, .. } | Self::BranchMatch { branch, .. } => Some(branch),
            Self::New { branch, .. } => branch.as_deref(),
        }
    }

    /// The branch to adopt into a new worktree, for a candidate that is a branch and not yet a
    /// worktree.
    #[must_use]
    pub fn adopts(&self) -> Option<&str> {
        match self {
            Self::PullRequestHead {
                branch,
                worktree: None,
                ..
            }
            | Self::BranchMatch {
                branch,
                worktree: None,
                ..
            } => Some(branch),
            _ => None,
        }
    }

    /// Where the work will be: the existing worktree's path, the matcher's directory, or the new
    /// worktree's. `None` only for a pull request's branch, whose directory is the form's to plan.
    #[must_use]
    pub fn directory(&self) -> Option<&str> {
        match self {
            Self::BranchMatch {
                worktree: None,
                directory,
                ..
            }
            | Self::New { directory, .. } => Some(directory),
            Self::PullRequestHead { worktree: None, .. } => None,
            _ => self.worktree().map(|place| place.id.as_str()),
        }
    }
}

/// The new worktree the form's pipeline would make for the issue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewWorktree {
    pub branch: Option<String>,
    pub directory: String,
    pub base: String,
}

/// A branch the project's `existing_branch_match` offers, as the form's pipeline listed it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchOffer {
    pub branch: String,
    pub remote_only: bool,
    pub directory: String,
}

/// The work item's pull request, by the branch it is from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestHeadRef {
    pub number: u64,
    pub branch: String,
}

/// What resolution goes on, read by the caller.
#[derive(Debug, Clone, Default)]
pub struct ResolutionInput {
    pub key: String,
    /// The worktree the work item was last bound to, by path, whether or not it still exists.
    pub bound: Option<String>,
    /// The project's worktrees, bare and prunable ones left out.
    pub worktrees: Vec<Place>,
    pub pull_request: Option<PullRequestHeadRef>,
    /// What `existing_branch_match` offers, in the form's order.
    pub offers: Vec<BranchOffer>,
    /// Every matcher that is not `ignore` says `prefer`: a lone match may be taken without asking.
    pub prefer_offers: bool,
    /// The new worktree, when the form could plan one from the issue alone.
    pub new: Option<NewWorktree>,
}

/// Where Start Work would put the issue's work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Resolved {
    /// This one: the bound worktree, the only existing candidate, or a new worktree when there is
    /// none.
    One { candidate: Candidate },
    /// More than one, in the plan's order: a preview, nothing recorded until one is picked.
    Choose { candidates: Vec<Candidate> },
    /// Nothing exists, and the form cannot plan a new worktree from the issue alone.
    Nothing,
}

impl Resolved {
    /// The candidate with `id`, from any shape of resolution.
    #[must_use]
    pub fn find(&self, id: &str) -> Option<&Candidate> {
        match self {
            Self::One { candidate } => (candidate.id() == id).then_some(candidate),
            Self::Choose { candidates } => candidates.iter().find(|c| c.id() == id),
            Self::Nothing => None,
        }
    }
}

/// Decide where an issue's work happens. See the module docs for the order and when it asks.
#[must_use]
pub fn resolve(input: &ResolutionInput) -> Resolved {
    let at = |id: &str| input.worktrees.iter().find(|w| w.id == id);
    let checked_out = |branch: &str| {
        input
            .worktrees
            .iter()
            .find(|w| w.branch.as_deref() == Some(branch))
    };

    if let Some(place) = input.bound.as_deref().and_then(at) {
        return Resolved::One {
            candidate: Candidate::Bound {
                worktree: place.clone(),
            },
        };
    }

    let mut found: Vec<Candidate> = input
        .worktrees
        .iter()
        .filter(|w| w.key.as_deref() == Some(input.key.as_str()))
        .map(|w| Candidate::KeyInName {
            worktree: w.clone(),
        })
        .collect();
    let listed = |found: &[Candidate], place: &Place| {
        found
            .iter()
            .any(|c| c.worktree().is_some_and(|w| w.id == place.id))
    };

    if let Some(pr) = &input.pull_request {
        let worktree = checked_out(&pr.branch).cloned();
        if worktree.as_ref().is_none_or(|w| !listed(&found, w)) {
            found.push(Candidate::PullRequestHead {
                number: pr.number,
                branch: pr.branch.clone(),
                worktree,
            });
        }
    }

    let mut offered = 0;
    for offer in &input.offers {
        let worktree = checked_out(&offer.branch).cloned();
        let already = found
            .iter()
            .any(|c| c.branch() == Some(offer.branch.as_str()))
            || worktree.as_ref().is_some_and(|w| listed(&found, w));
        if already {
            continue;
        }
        if worktree.is_none() {
            offered += 1;
        }
        found.push(Candidate::BranchMatch {
            branch: offer.branch.clone(),
            worktree,
            remote_only: offer.remote_only,
            directory: offer.directory.clone(),
        });
    }

    let new = input.new.as_ref().map(|plan| Candidate::New {
        branch: plan.branch.clone(),
        directory: plan.directory.clone(),
        base: plan.base.clone(),
    });
    // A lone branch the form would offer rather than take is a choice between it and a new one.
    let offer_only = found.len() == 1 && offered == 1 && !input.prefer_offers;
    match (found.len(), new) {
        (0, Some(new)) => Resolved::One { candidate: new },
        (0, None) => Resolved::Nothing,
        (1, new) if !offer_only || new.is_none() => Resolved::One {
            candidate: found.remove(0),
        },
        (_, new) => {
            found.extend(new);
            Resolved::Choose { candidates: found }
        }
    }
}

/// Where a start job is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartJobState {
    /// `git worktree add` is about to run, or running.
    Creating,
    /// The worktree exists and the project's setup is running in it.
    SettingUp,
    /// The worktree is there and set up, or was there already: the run is ready to plan.
    Ready,
    /// The worktree exists and its setup failed. Kept, for the reason ARCHITECTURE §5 gives.
    SetupFailed,
    /// Nothing was made: the pipeline refused, or git did.
    Failed,
    /// wtm stopped while it was creating or setting up, and the job waits for Resume.
    Interrupted,
}

impl StartJobState {
    /// Whether a job in this state may move to `next`.
    #[must_use]
    pub fn can_move_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (
                Self::Creating,
                Self::SettingUp
                    | Self::Ready
                    | Self::SetupFailed
                    | Self::Failed
                    | Self::Interrupted
            ) | (
                Self::SettingUp,
                Self::Ready | Self::SetupFailed | Self::Interrupted
            ) | (
                // Retry setup, or use the worktree as it is.
                Self::SetupFailed | Self::Interrupted,
                Self::SettingUp | Self::Ready
            ) | (Self::Interrupted, Self::Creating)
        )
    }

    /// Whether a thread is working on it, when the process that started it is still alive.
    #[must_use]
    pub fn is_busy(self) -> bool {
        matches!(self, Self::Creating | Self::SettingUp)
    }
}

/// The job that gives a started issue its worktree. One per work item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartJob {
    pub work_item: WorkItemId,
    /// The Implement flow it prepares.
    pub flow: FlowId,
    pub project: ProjectId,
    pub key: String,
    /// What was picked, as resolution offered it.
    pub candidate: Candidate,
    /// The worktree's path: where it is, or where it is being made.
    pub worktree: String,
    pub branch: Option<String>,
    pub state: StartJobState,
    /// Why it failed, or what its setup said when it failed.
    pub detail: Option<String>,
    /// The terminal the setup runs in, while this process has it.
    pub setup_session: Option<String>,
    pub created_at: UnixMs,
    pub updated_at: UnixMs,
}

/// A start job to record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewStartJob {
    pub work_item: WorkItemId,
    pub flow: FlowId,
    pub project: ProjectId,
    pub key: String,
    pub candidate: Candidate,
    pub worktree: String,
    pub branch: Option<String>,
    /// `Creating` for a worktree to make, `Ready` for one that exists.
    pub state: StartJobState,
}

/// A change to a start job, checked against [`StartJobState::can_move_to`] when the state moves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartJobChange {
    pub state: StartJobState,
    pub detail: Field<String>,
    pub setup_session: Field<String>,
    /// The worktree's path as git reports it, once it exists. The planned path is the form's,
    /// absolutized; git's can differ by a symlink in a parent, and git's is the worktree's id.
    pub worktree: Option<String>,
}

impl StartJobChange {
    #[must_use]
    pub fn to(state: StartJobState) -> Self {
        Self {
            state,
            detail: Field::Keep,
            setup_session: Field::Keep,
            worktree: None,
        }
    }

    #[must_use]
    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Field::Set(detail.into());
        self
    }
}

/// Why a start job refused a change.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StartJobError {
    #[error("a start job cannot move from {from:?} to {to:?}")]
    NoSuchStep {
        from: StartJobState,
        to: StartJobState,
    },
}

/// The status Start Work moves an issue to: the first name mapped to `started`, which is also the
/// board's start column.
#[must_use]
pub fn start_status(map: &StatusMap) -> Option<&str> {
    map.target(StatusRole::Started)
}

/// What starting would do to the issue's team status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum StartMove {
    /// Move it from `from` to `to`, as part of the start.
    Move {
        from: String,
        to: String,
        /// It is in a `needs_review` status, which a drop asks you to confirm leaving.
        leaving_review: bool,
    },
    /// Leave it where it is, for this reason.
    Stay { reason: StayReason },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StayReason {
    /// `[workflow.start] move_issue = false`.
    TurnedOff,
    /// No status is mapped to `started`, so there is nowhere to move it.
    NoStartStatus,
    /// It is in the start status already.
    AlreadyThere,
    /// It is past the start of the work, or in a status nobody mapped. The Start button does not
    /// move an issue backward or from a status it cannot place; dropping it on the start column
    /// does, because that is what the drop asks for.
    NotToDo,
}

/// What a Start of an issue in `current` would do to its status. `by_drop` is a card dropped on
/// the start column, which moves it from wherever it is.
#[must_use]
pub fn start_move(map: &StatusMap, enabled: bool, current: &str, by_drop: bool) -> StartMove {
    let stay = |reason| StartMove::Stay { reason };
    if !enabled {
        return stay(StayReason::TurnedOff);
    }
    let Some(to) = start_status(map) else {
        return stay(StayReason::NoStartStatus);
    };
    if same_status(current, to) {
        return stay(StayReason::AlreadyThere);
    }
    let role = map.role_of(current);
    if !by_drop && role != Some(StatusRole::Todo) {
        return stay(StayReason::NotToDo);
    }
    StartMove::Move {
        from: current.to_owned(),
        to: to.to_owned(),
        leaving_review: role == Some(StatusRole::NeedsReview),
    }
}

/// What dropping an issue's card on a board column means.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum DropMeaning {
    /// The configured start column: Start Work.
    Start,
    /// Another column: a change of team status, and nothing else.
    Move {
        to: String,
        /// Toward the start of the board: never touches a run or a worktree, and offers to pause a
        /// run the issue has.
        backward: bool,
        /// Out of a `needs_review` status, which asks you to confirm first: someone may be
        /// reviewing it.
        leaving_review: bool,
    },
    /// The column it is in already.
    Stay,
}

/// What dropping an issue in `from` on the column of `to` means, by the project's mapping.
///
/// Backward is by column, which is role order and then the order the config lists a role's names
/// in. An issue in a status nobody mapped has no place on that line, so no drop of it is backward.
#[must_use]
pub fn drop_meaning(map: &StatusMap, from: &str, to: &str) -> DropMeaning {
    if same_status(from, to) {
        return DropMeaning::Stay;
    }
    if start_status(map).is_some_and(|start| same_status(start, to)) {
        return DropMeaning::Start;
    }
    let backward = match (map.column_of(from), map.column_of(to)) {
        (Some(from), Some(to)) => to < from,
        _ => false,
    };
    DropMeaning::Move {
        to: to.to_owned(),
        backward,
        leaving_review: map.role_of(from) == Some(StatusRole::NeedsReview)
            && map.role_of(to) != Some(StatusRole::NeedsReview),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn place(id: &str, branch: &str, key: Option<&str>) -> Place {
        Place {
            id: id.to_owned(),
            branch: Some(branch.to_owned()),
            key: key.map(str::to_owned),
        }
    }

    fn new() -> NewWorktree {
        NewWorktree {
            branch: Some("ACME-12".to_owned()),
            directory: "/work/ACME-12".to_owned(),
            base: "origin/main".to_owned(),
        }
    }

    fn input() -> ResolutionInput {
        ResolutionInput {
            key: "ACME-12".to_owned(),
            new: Some(new()),
            ..ResolutionInput::default()
        }
    }

    fn kinds(resolved: &Resolved) -> Vec<&'static str> {
        let one;
        let candidates: &[Candidate] = match resolved {
            Resolved::One { candidate } => {
                one = [candidate.clone()];
                &one
            }
            Resolved::Choose { candidates } => candidates,
            Resolved::Nothing => &[],
        };
        candidates
            .iter()
            .map(|c| match c {
                Candidate::Bound { .. } => "bound",
                Candidate::KeyInName { .. } => "key",
                Candidate::PullRequestHead { .. } => "pull_request",
                Candidate::BranchMatch { .. } => "branch",
                Candidate::New { .. } => "new",
            })
            .collect()
    }

    fn map() -> StatusMap {
        StatusMap {
            todo: vec!["Backlog".to_owned(), "To Do".to_owned()],
            started: vec!["In Progress".to_owned(), "Blocked".to_owned()],
            needs_review: vec!["In Review".to_owned()],
            done: vec!["Done".to_owned()],
        }
    }

    #[test]
    fn with_nothing_existing_the_issue_gets_a_new_worktree_from_the_base_without_a_preview() {
        let resolved = resolve(&input());
        assert_eq!(
            resolved,
            Resolved::One {
                candidate: Candidate::New {
                    branch: Some("ACME-12".to_owned()),
                    directory: "/work/ACME-12".to_owned(),
                    base: "origin/main".to_owned(),
                }
            }
        );
    }

    #[test]
    fn the_bound_worktree_wins_over_every_other_candidate_without_asking() {
        let mut input = input();
        input.bound = Some("/work/bound".to_owned());
        input.worktrees = vec![
            place("/work/carries", "feature/ACME-12-export", Some("ACME-12")),
            place("/work/bound", "my-branch", None),
        ];
        input.offers = vec![BranchOffer {
            branch: "story/ACME-12-x".to_owned(),
            remote_only: false,
            directory: "/work/x".to_owned(),
        }];
        assert_eq!(kinds(&resolve(&input)), ["bound"]);
    }

    #[test]
    fn a_bound_worktree_that_was_removed_falls_through_to_the_rest_of_the_order() {
        let mut input = input();
        input.bound = Some("/work/gone".to_owned());
        input.worktrees = vec![place("/work/carries", "feature/ACME-12-x", Some("ACME-12"))];
        assert_eq!(kinds(&resolve(&input)), ["key"]);
    }

    #[test]
    fn one_worktree_carrying_the_key_is_adopted_without_a_preview() {
        let mut input = input();
        input.worktrees = vec![
            place("/work/other", "feature/ACME-99-other", Some("ACME-99")),
            place("/work/carries", "feature/ACME-12-export", Some("ACME-12")),
        ];
        match resolve(&input) {
            Resolved::One {
                candidate: Candidate::KeyInName { worktree },
            } => assert_eq!(worktree.id, "/work/carries"),
            other => panic!("expected the worktree carrying the key, got {other:?}"),
        }
    }

    #[test]
    fn more_than_one_candidate_is_a_preview_in_the_plans_order_ending_with_a_new_worktree() {
        let mut input = input();
        input.worktrees = vec![
            place("/work/a", "feature/ACME-12-a", Some("ACME-12")),
            place("/work/b", "bugfix/ACME-12-b", Some("ACME-12")),
        ];
        input.pull_request = Some(PullRequestHeadRef {
            number: 40,
            branch: "takumi/export".to_owned(),
        });
        input.offers = vec![BranchOffer {
            branch: "story/ACME-12-old".to_owned(),
            remote_only: true,
            directory: "/work/ACME-12-old".to_owned(),
        }];
        assert_eq!(
            kinds(&resolve(&input)),
            ["key", "key", "pull_request", "branch", "new"]
        );
    }

    #[test]
    fn a_pull_requests_branch_checked_out_in_a_worktree_carrying_the_key_is_listed_once() {
        let mut input = input();
        input.worktrees = vec![place("/work/a", "feature/ACME-12-a", Some("ACME-12"))];
        input.pull_request = Some(PullRequestHeadRef {
            number: 40,
            branch: "feature/ACME-12-a".to_owned(),
        });
        input.offers = vec![BranchOffer {
            branch: "feature/ACME-12-a".to_owned(),
            remote_only: false,
            directory: "/work/a".to_owned(),
        }];
        assert_eq!(kinds(&resolve(&input)), ["key"]);
    }

    #[test]
    fn a_pull_requests_branch_checked_out_elsewhere_is_that_worktree() {
        let mut input = input();
        input.worktrees = vec![place("/work/pr", "takumi/export", None)];
        input.pull_request = Some(PullRequestHeadRef {
            number: 40,
            branch: "takumi/export".to_owned(),
        });
        match resolve(&input) {
            Resolved::One {
                candidate: Candidate::PullRequestHead { worktree, .. },
            } => assert_eq!(worktree.unwrap().id, "/work/pr"),
            other => panic!("expected the pull request's worktree, got {other:?}"),
        }
    }

    #[test]
    fn a_lone_offered_branch_is_a_choice_unless_every_matcher_prefers_it() {
        let mut input = input();
        input.offers = vec![BranchOffer {
            branch: "story/ACME-12-old".to_owned(),
            remote_only: false,
            directory: "/work/ACME-12-old".to_owned(),
        }];
        assert_eq!(kinds(&resolve(&input)), ["branch", "new"]);

        input.prefer_offers = true;
        let resolved = resolve(&input);
        assert_eq!(kinds(&resolved), ["branch"]);
        if let Resolved::One { candidate } = resolved {
            assert_eq!(candidate.adopts(), Some("story/ACME-12-old"));
        }
    }

    #[test]
    fn nothing_existing_and_no_plan_for_a_new_worktree_is_nothing() {
        let mut input = input();
        input.new = None;
        assert_eq!(resolve(&input), Resolved::Nothing);
    }

    #[test]
    fn every_candidate_in_a_preview_has_its_own_id_and_can_be_found_by_it() {
        let mut input = input();
        input.worktrees = vec![place("/work/a", "feature/ACME-12-a", Some("ACME-12"))];
        input.offers = vec![BranchOffer {
            branch: "story/ACME-12-old".to_owned(),
            remote_only: false,
            directory: "/work/ACME-12-old".to_owned(),
        }];
        let resolved = resolve(&input);
        let Resolved::Choose { candidates } = &resolved else {
            panic!("expected a preview, got {resolved:?}");
        };
        let ids: Vec<String> = candidates.iter().map(Candidate::id).collect();
        assert_eq!(ids, ["worktree:/work/a", "branch:story/ACME-12-old", "new"]);
        for id in &ids {
            assert_eq!(
                resolved.find(id).map(Candidate::id).as_deref(),
                Some(id.as_str())
            );
        }
        assert!(resolved.find("worktree:/work/elsewhere").is_none());
    }

    #[test]
    fn a_start_job_never_goes_back_to_creating_once_its_worktree_exists() {
        use StartJobState::*;
        assert!(Creating.can_move_to(SettingUp));
        assert!(SettingUp.can_move_to(Interrupted));
        assert!(Interrupted.can_move_to(SettingUp));
        assert!(Interrupted.can_move_to(Creating));
        assert!(SetupFailed.can_move_to(SettingUp));
        assert!(!SettingUp.can_move_to(Creating));
        assert!(!SetupFailed.can_move_to(Creating));
        assert!(!Ready.can_move_to(Creating));
        assert!(!Failed.can_move_to(Ready));
    }

    #[test]
    fn starting_moves_an_issue_from_to_do_to_the_first_started_status() {
        assert_eq!(
            start_move(&map(), true, "To Do", false),
            StartMove::Move {
                from: "To Do".to_owned(),
                to: "In Progress".to_owned(),
                leaving_review: false,
            }
        );
    }

    #[test]
    fn the_start_button_leaves_an_issue_past_to_do_or_unmapped_where_it_is() {
        for status in ["In Review", "Done", "Blocked", "Declined"] {
            assert_eq!(
                start_move(&map(), true, status, false),
                StartMove::Stay {
                    reason: StayReason::NotToDo
                },
                "{status}"
            );
        }
        assert_eq!(
            start_move(&map(), true, "in progress", false),
            StartMove::Stay {
                reason: StayReason::AlreadyThere
            }
        );
        assert_eq!(
            start_move(&map(), false, "To Do", false),
            StartMove::Stay {
                reason: StayReason::TurnedOff
            }
        );
        assert_eq!(
            start_move(&StatusMap::default(), true, "To Do", false),
            StartMove::Stay {
                reason: StayReason::NoStartStatus
            }
        );
    }

    #[test]
    fn a_drop_on_the_start_column_moves_the_issue_from_wherever_it_is_and_says_when_it_leaves_review()
     {
        assert_eq!(
            start_move(&map(), true, "In Review", true),
            StartMove::Move {
                from: "In Review".to_owned(),
                to: "In Progress".to_owned(),
                leaving_review: true,
            }
        );
    }

    #[test]
    fn the_window_reads_every_field_of_a_choice_and_a_move_in_camel_case() {
        // `rename_all` on an enum names its variants and leaves their fields alone, so a field of
        // two words reached the window in snake case and read as missing there. Found clicking
        // through: an offered branch's `remoteOnly` and a move's `leavingReview`.
        let candidate = serde_json::to_value(Candidate::BranchMatch {
            branch: "story/ACME-12".to_owned(),
            worktree: None,
            remote_only: true,
            directory: "/work/ACME-12".to_owned(),
        })
        .unwrap();
        assert_eq!(candidate["kind"], "branch_match");
        assert_eq!(candidate["remoteOnly"], true);
        let moved = serde_json::to_value(start_move(&map(), true, "In Review", true)).unwrap();
        assert_eq!(moved["leavingReview"], true);
        let dropped = serde_json::to_value(drop_meaning(&map(), "In Review", "To Do")).unwrap();
        assert_eq!(dropped["leavingReview"], true);
    }

    #[test]
    fn only_the_first_started_status_is_the_start_column() {
        assert_eq!(
            drop_meaning(&map(), "To Do", "In Progress"),
            DropMeaning::Start
        );
        assert_eq!(
            drop_meaning(&map(), "To Do", "Blocked"),
            DropMeaning::Move {
                to: "Blocked".to_owned(),
                backward: false,
                leaving_review: false,
            }
        );
    }

    #[test]
    fn a_drop_toward_the_start_of_the_board_is_backward_and_out_of_review_is_flagged() {
        assert_eq!(
            drop_meaning(&map(), "In Review", "Blocked"),
            DropMeaning::Move {
                to: "Blocked".to_owned(),
                backward: true,
                leaving_review: true,
            }
        );
        assert_eq!(
            drop_meaning(&map(), "In Progress", "To Do"),
            DropMeaning::Move {
                to: "To Do".to_owned(),
                backward: true,
                leaving_review: false,
            }
        );
        assert_eq!(
            drop_meaning(&map(), "In Progress", "In Review"),
            DropMeaning::Move {
                to: "In Review".to_owned(),
                backward: false,
                leaving_review: false,
            }
        );
    }

    #[test]
    fn dropping_an_issue_on_its_own_column_means_nothing_and_an_unmapped_one_is_never_backward() {
        assert_eq!(drop_meaning(&map(), "To Do", "to do"), DropMeaning::Stay);
        assert_eq!(
            drop_meaning(&map(), "Declined", "Backlog"),
            DropMeaning::Move {
                to: "Backlog".to_owned(),
                backward: false,
                leaving_review: false,
            }
        );
    }
}
