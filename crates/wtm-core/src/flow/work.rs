//! Work items: what flows are about.
//!
//! # A worktree is not a work item's identity
//!
//! A work item is an issue, a pull request, or neither. Its branch, its worktree and even its pull
//! request can change over its life, and its history has to survive each change, so those are
//! [`WorkLink`]s recorded against it rather than part of what it is. Deleting a worktree and
//! creating another for the same issue is the same work item.
//!
//! # One work item per issue, and one per pull request
//!
//! A store keeps at most one work item per `(tracker, key)` and at most one per `(repo, number)`.
//! That is what makes starting work idempotent: a second click, or a click and then a drag, finds
//! the item the first one made instead of making another. The pull request rule covers an issue's
//! *current* pull request too, so reviewing a pull request that already belongs to one of your
//! issues opens that issue's work item rather than a second one beside it.

use serde::{Deserialize, Serialize};

use super::UnixMs;
use crate::model::ProjectId;

id! {
    /// One work item, as the store numbers it.
    WorkItemId
}

/// An issue in a tracker.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueRef {
    /// Which tracker, as the project's workflow config names it.
    pub tracker: String,
    /// The tracker's own key for the issue.
    pub key: String,
}

/// A pull request on a forge.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestRef {
    /// `owner/name` on the forge.
    pub repo: String,
    pub number: u64,
}

/// How a work item can be looked up.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkKey {
    Issue(IssueRef),
    PullRequest(PullRequestRef),
}

/// What a work item is backed by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Backing {
    Issue,
    /// A pull request with no issue: typically someone else's, opened for review.
    PullRequest,
    /// Neither, such as reviewing work that has not been pushed anywhere.
    AdHoc,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkItem {
    pub id: WorkItemId,
    pub project: ProjectId,
    pub title: String,
    pub issue: Option<IssueRef>,
    /// The current pull request. Earlier ones are in the item's links.
    pub pull_request: Option<PullRequestRef>,
    pub created_at: UnixMs,
}

impl WorkItem {
    /// An issue backs an item for good: linking its pull request later does not make it a pull
    /// request's item, because the issue is still where its status and its history live.
    #[must_use]
    pub fn backing(&self) -> Backing {
        match (&self.issue, &self.pull_request) {
            (Some(_), _) => Backing::Issue,
            (None, Some(_)) => Backing::PullRequest,
            (None, None) => Backing::AdHoc,
        }
    }
}

/// A work item before the store has numbered it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewWorkItem {
    pub project: ProjectId,
    pub title: String,
    pub issue: Option<IssueRef>,
    pub pull_request: Option<PullRequestRef>,
}

/// Something a work item used at some point in its life.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkLink {
    Branch { name: String },
    Worktree { path: String },
    PullRequest(PullRequestRef),
}

/// A link as the store recorded it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordedLink {
    pub link: WorkLink,
    pub at: UnixMs,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(issue: Option<IssueRef>, pull_request: Option<PullRequestRef>) -> WorkItem {
        WorkItem {
            id: WorkItemId(1),
            project: ProjectId::from_root(std::path::Path::new("/work/webapp")),
            title: "Bulk export".to_owned(),
            issue,
            pull_request,
            created_at: 0,
        }
    }

    fn issue() -> IssueRef {
        IssueRef {
            tracker: "jira".to_owned(),
            key: "ACME-12".to_owned(),
        }
    }

    fn pull_request() -> PullRequestRef {
        PullRequestRef {
            repo: "acme/webapp".to_owned(),
            number: 40,
        }
    }

    #[test]
    fn an_issue_backed_item_stays_issue_backed_once_its_pull_request_is_linked() {
        assert_eq!(item(Some(issue()), None).backing(), Backing::Issue);
        assert_eq!(
            item(Some(issue()), Some(pull_request())).backing(),
            Backing::Issue
        );
    }

    #[test]
    fn an_item_with_only_a_pull_request_is_backed_by_it() {
        assert_eq!(
            item(None, Some(pull_request())).backing(),
            Backing::PullRequest
        );
    }

    #[test]
    fn an_item_with_neither_an_issue_nor_a_pull_request_is_ad_hoc() {
        assert_eq!(item(None, None).backing(), Backing::AdHoc);
    }
}
