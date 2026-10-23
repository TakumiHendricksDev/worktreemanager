//! What a forge says about a pull request, and how a pasted one is named.
//!
//! # Read side only
//!
//! These are the facts a review needs before it starts: whose branch the pull request is, which
//! commit it is at, whether it comes from a fork and whether its author lets maintainers push to
//! it, what you may do on the repository, who has been asked to review, how its checks stand, and
//! whether its base is protected. Writing back to a forge (publishing a review, pushing a fix) is a
//! later item with approvals of its own, and nothing here can express it.
//!
//! # Tolerant by construction
//!
//! A forge adds fields and states over time, and one it does not know about must not stop a review
//! of everything else. So a state this build has no name for is [`PrState::Unknown`] rather than an
//! error, a reviewer of an unfamiliar kind keeps whatever name it came with, and a check whose
//! outcome is unfamiliar counts as pending, which is the one bucket that never claims a pass.
//!
//! # A repository is `owner/name`, and the host only breaks ties
//!
//! [`RepoSlug::parse_url`] reads the remote URLs people actually configure, and a host there is
//! often an SSH alias (`git@work-github:acme/webapp.git`) rather than the forge's own name. So two
//! slugs name the same repository when their owner and name agree, ignoring case as the forge does,
//! and a matching host is preferred only when more than one remote qualifies.

use serde::{Deserialize, Serialize};

use super::PullRequestRef;
use crate::model::CommitId;
use crate::ports::git::RemoteUrl;

/// Which forge a project's workflow reads, as `[workflow] forge` names it.
///
/// One variant. The table names the forge rather than assuming one so a project that has not said
/// it uses GitHub causes no GitHub call, which is ARCHITECTURE §1's promise for a project without a
/// workflow, kept for one that has a workflow but no forge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForgeKind {
    Github,
}

/// A repository on a forge.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepoSlug {
    pub host: String,
    pub owner: String,
    pub name: String,
}

impl RepoSlug {
    #[must_use]
    pub fn new(host: impl Into<String>, owner: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            owner: owner.into(),
            name: name.into(),
        }
    }

    /// `owner/name`, from `owner/name` as a forge prints it. `None` for anything else.
    #[must_use]
    pub fn from_full_name(host: &str, full: &str) -> Option<Self> {
        let (owner, name) = full.split_once('/')?;
        (is_segment(owner) && is_segment(name)).then(|| Self::new(host, owner, name))
    }

    /// The repository a remote URL points at.
    ///
    /// Handles the forms git accepts for a hosted repository: `https://host/owner/name(.git)`,
    /// `ssh://user@host[:port]/owner/name(.git)`, `git://host/owner/name(.git)` and the scp-like
    /// `user@host:owner/name(.git)`. A local path or a `file://` URL names no forge repository and
    /// is `None`, as is a URL with more or fewer than two path segments.
    #[must_use]
    pub fn parse_url(url: &str) -> Option<Self> {
        let url = url.trim();
        let (host, path) = if let Some((scheme, rest)) = url.split_once("://") {
            if !matches!(
                scheme.to_ascii_lowercase().as_str(),
                "https" | "http" | "ssh" | "git" | "git+ssh" | "ssh+git"
            ) {
                return None;
            }
            let (authority, path) = rest.split_once('/')?;
            let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
            // A port is the authority's business, not the repository's.
            let host = host.split_once(':').map_or(host, |(h, _)| h);
            (host, path)
        } else {
            // scp-like: `[user@]host:path`. A colon after a slash is a local path with a colon
            // in it, which is what git itself decides.
            let (authority, path) = url.split_once(':')?;
            if authority.contains('/') || authority.is_empty() {
                return None;
            }
            let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
            (host, path)
        };
        if host.is_empty() {
            return None;
        }
        let path = path.trim_matches('/');
        let path = path.strip_suffix(".git").unwrap_or(path);
        let (owner, name) = path.split_once('/')?;
        (is_segment(owner) && is_segment(name)).then(|| Self::new(host, owner, name))
    }

    /// `owner/name`.
    #[must_use]
    pub fn full_name(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }

    /// The same repository, by owner and name. See the module docs for why the host is not part of
    /// it.
    #[must_use]
    pub fn same_repository(&self, other: &Self) -> bool {
        self.owner.eq_ignore_ascii_case(&other.owner) && self.name.eq_ignore_ascii_case(&other.name)
    }

    #[must_use]
    pub fn same_host(&self, other: &Self) -> bool {
        self.host.eq_ignore_ascii_case(&other.host)
    }
}

impl std::fmt::Display for RepoSlug {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

/// The remote whose configured URL is `repo`.
///
/// Owner and name decide; a matching host, then the name `origin`, break ties, then config order.
/// See the module docs for why the host does not decide on its own.
#[must_use]
pub fn remote_for(repo: &RepoSlug, remotes: &[RemoteUrl]) -> Option<String> {
    let mut candidates: Vec<(&RemoteUrl, RepoSlug)> = remotes
        .iter()
        .filter_map(|remote| {
            RepoSlug::parse_url(&remote.url)
                .filter(|slug| slug.same_repository(repo))
                .map(|slug| (remote, slug))
        })
        .collect();
    // Stable, so config order survives among equals.
    candidates.sort_by_key(|(remote, slug)| (!slug.same_host(repo), remote.name != "origin"));
    candidates.first().map(|(remote, _)| remote.name.clone())
}

/// The forge host a repository known only by `owner/name` lives on, from the checkout's remotes.
///
/// A work item records `owner/name`, as the forge prints it, and the host comes back from the
/// remote that points there. Only a host with a dot in it counts: an SSH alias such as
/// `work-github` is a name only `~/.ssh/config` knows, and asking the forge's CLI about it would
/// read as a host nobody is signed in to. Without one, `github.com`.
#[must_use]
pub fn host_for(owner: &str, name: &str, remotes: &[RemoteUrl]) -> String {
    let wanted = RepoSlug::new("", owner, name);
    remotes
        .iter()
        .filter_map(|remote| RepoSlug::parse_url(&remote.url))
        .find(|slug| slug.same_repository(&wanted) && slug.host.contains('.'))
        .map_or_else(|| GITHUB_HOST.to_owned(), |slug| slug.host)
}

/// Where a bare pull request number is asked about: the host of `origin`, or of the first remote
/// that names a real host, or `github.com`. See [`host_for`] for what counts as a host.
#[must_use]
pub fn default_host(remotes: &[RemoteUrl]) -> String {
    let hosts = |only_origin: bool| {
        remotes
            .iter()
            .filter(move |remote| !only_origin || remote.name == "origin")
            .filter_map(|remote| RepoSlug::parse_url(&remote.url))
            .map(|slug| slug.host)
            .find(|host| host.contains('.'))
    };
    hosts(true)
        .or_else(|| hosts(false))
        .unwrap_or_else(|| GITHUB_HOST.to_owned())
}

/// The host a GitHub forge lives on unless a remote says otherwise.
pub const GITHUB_HOST: &str = "github.com";

/// One path segment of an `owner/name`: not empty, and nothing that would make it a path.
fn is_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && !segment
            .chars()
            .any(|c| c == '/' || c == '\\' || c.is_whitespace() || c.is_control())
}

/// A pull request as someone pasted it: a URL, or a number in the project's own repository.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrTarget {
    /// `None` for a bare number, which the forge's CLI resolves against the checkout's remotes the
    /// way it would in a terminal opened there.
    pub repo: Option<RepoSlug>,
    pub number: u64,
}

/// Why pasted text is not a pull request.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize)]
#[error("{0}")]
pub struct PrTargetError(pub String);

impl PrTarget {
    /// `123`, `#123`, or a pull request's URL, with or without a trailing tab (`/files`), query or
    /// fragment.
    ///
    /// # Errors
    ///
    /// When the text is none of those, with a sentence that says which form was expected.
    pub fn parse(input: &str) -> Result<Self, PrTargetError> {
        let text = input.trim();
        if text.is_empty() {
            return Err(PrTargetError(
                "Paste a pull request's URL, or type its number.".to_owned(),
            ));
        }
        if let Some(number) = parse_number(text.strip_prefix('#').unwrap_or(text)) {
            return Ok(Self { repo: None, number });
        }
        Self::parse_url(text).ok_or_else(|| {
            PrTargetError(format!(
                "`{text}` is not a pull request. Use its URL (…/owner/name/pull/123) or its number."
            ))
        })
    }

    fn parse_url(text: &str) -> Option<Self> {
        let (scheme, rest) = text.split_once("://")?;
        if !matches!(scheme.to_ascii_lowercase().as_str(), "https" | "http") {
            return None;
        }
        let rest = rest.split(['?', '#']).next()?;
        let mut parts = rest.split('/').filter(|part| !part.is_empty());
        let host = parts.next()?;
        let owner = parts.next()?;
        let name = parts.next()?;
        if parts.next()? != "pull" {
            return None;
        }
        let number = parse_number(parts.next()?)?;
        (is_segment(owner) && is_segment(name)).then(|| Self {
            repo: Some(RepoSlug::new(host, owner, name)),
            number,
        })
    }
}

fn parse_number(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse().ok().filter(|n| *n > 0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrState {
    Open,
    Closed,
    Merged,
    /// A state this build has no name for. See the module docs.
    Unknown,
}

/// Where a pull request's commits come from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrHead {
    /// `None` when the fork it came from has been deleted. The commits are still reachable through
    /// the base repository's pull request ref, which is why a review does not need it.
    pub repo: Option<RepoSlug>,
    pub branch: String,
    pub sha: CommitId,
}

/// Where a pull request would merge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrBase {
    pub repo: RepoSlug,
    pub branch: String,
    /// The base branch's commit as the forge last computed it. Absent from older forge replies.
    pub sha: Option<CommitId>,
}

/// Someone asked to review, by the forge's own name for them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Reviewer {
    /// A login, or a team's `org/slug`.
    pub name: String,
    pub team: bool,
}

/// A pull request's checks, counted.
///
/// Counts rather than a verdict, so the view can say "3 passed, 1 pending" instead of a single
/// word that hides the one still running.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChecksSummary {
    pub passed: u32,
    pub failed: u32,
    /// Queued, running, waiting, or finished with an outcome this build does not know.
    pub pending: u32,
    /// Skipped, neutral or cancelled: finished without a verdict on the code.
    pub skipped: u32,
}

/// What [`ChecksSummary`] adds up to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChecksState {
    /// No checks reported.
    None,
    Failing,
    Pending,
    Passing,
}

impl ChecksSummary {
    #[must_use]
    pub fn total(&self) -> u32 {
        self.passed + self.failed + self.pending + self.skipped
    }

    /// A failure outranks anything still running, and only a run with nothing pending passes.
    #[must_use]
    pub fn state(&self) -> ChecksState {
        if self.total() == 0 {
            ChecksState::None
        } else if self.failed > 0 {
            ChecksState::Failing
        } else if self.pending > 0 {
            ChecksState::Pending
        } else {
            ChecksState::Passing
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequest {
    pub number: u64,
    pub url: String,
    pub title: String,
    /// The author's login. `None` for a deleted account.
    pub author: Option<String>,
    pub state: PrState,
    pub draft: bool,
    pub head: PrHead,
    pub base: PrBase,
    /// The head branch lives in another repository than the base: a fork.
    pub cross_repository: bool,
    /// The author lets people with write access to the base push to the head branch.
    pub maintainer_can_modify: bool,
    pub requested_reviewers: Vec<Reviewer>,
    pub checks: ChecksSummary,
    /// GitHub's word on its reviews. `None` when it gave none, as for a repository that requires
    /// no review.
    #[serde(default)]
    pub review_decision: Option<ReviewDecision>,
    /// Whether it can merge as it stands, as GitHub's `mergeStateStatus` says. `None` when GitHub
    /// had not worked it out yet.
    #[serde(default)]
    pub merge_state: Option<MergeState>,
}

/// GitHub's `reviewDecision`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    Approved,
    ChangesRequested,
    ReviewRequired,
}

/// GitHub's `mergeStateStatus`: whether the pull request can merge, and if not, the reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeState {
    /// Mergeable, with every required check and review satisfied.
    Clean,
    /// Mergeable, but checks that are not required are failing.
    Unstable,
    /// Mergeable, with pre-receive hooks on the base.
    HasHooks,
    /// Branch protection blocks it: a review or a required check is missing.
    Blocked,
    /// The head is behind the base, which protection requires it not to be.
    Behind,
    /// It conflicts with the base.
    Dirty,
    /// A draft cannot be merged.
    Draft,
    /// A state this build has no name for, or GitHub's own "unknown".
    Unknown,
}

impl MergeState {
    /// Whether GitHub would take a merge now, as far as this state says.
    #[must_use]
    pub fn mergeable(self) -> bool {
        matches!(self, Self::Clean | Self::Unstable | Self::HasHooks)
    }
}

impl PullRequest {
    /// How the store keys a work item for this pull request.
    #[must_use]
    pub fn reference(&self) -> PullRequestRef {
        PullRequestRef {
            repo: self.base.repo.full_name(),
            number: self.number,
        }
    }
}

/// What you may do on a repository, as the forge grants it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    /// The forge reported nothing, or a level this build has no name for.
    Unknown,
    None,
    Read,
    Triage,
    Write,
    Maintain,
    Admin,
}

impl Permission {
    /// Whether you can push to the repository's own branches.
    ///
    /// Says nothing about a fork's branch: that also needs the author's
    /// [`PullRequest::maintainer_can_modify`], and is a question for the item that pushes.
    #[must_use]
    pub fn can_push(self) -> bool {
        self >= Self::Write
    }
}

/// Whether a branch is protected, and by what.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BranchProtection {
    pub branch: String,
    /// `None` when the forge would not say, which is what a branch that does not exist gets too.
    pub protected: Option<bool>,
    /// The kinds of rule in force (`pull_request`, `required_status_checks`, …), as the forge names
    /// them, from rulesets and classic protection alike. Empty does not mean unprotected: classic
    /// protection on a repository you cannot administer reports none.
    pub rules: Vec<String>,
}

/// Who the forge's CLI is signed in as.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForgeAccount {
    pub host: String,
    pub login: Option<String>,
    /// The CLI's version, as it printed it. `None` when it printed something this build could not
    /// read, which is not a reason to refuse it.
    pub version: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slug(url: &str) -> Option<(String, String, String)> {
        RepoSlug::parse_url(url).map(|s| (s.host, s.owner, s.name))
    }

    fn triple(host: &str, owner: &str, name: &str) -> (String, String, String) {
        (host.to_owned(), owner.to_owned(), name.to_owned())
    }

    #[test]
    fn every_remote_url_form_git_accepts_for_a_hosted_repository_is_read() {
        let want = Some(triple("github.com", "acme", "webapp"));
        assert_eq!(slug("https://github.com/acme/webapp.git"), want);
        assert_eq!(slug("https://github.com/acme/webapp"), want);
        assert_eq!(slug("https://github.com/acme/webapp/"), want);
        assert_eq!(slug("https://someone@github.com/acme/webapp.git"), want);
        assert_eq!(slug("git@github.com:acme/webapp.git"), want);
        assert_eq!(slug("git@github.com:acme/webapp"), want);
        assert_eq!(slug("ssh://git@github.com/acme/webapp.git"), want);
        assert_eq!(slug("ssh://git@github.com:22/acme/webapp.git"), want);
        assert_eq!(slug("git://github.com/acme/webapp.git"), want);
    }

    #[test]
    fn an_ssh_alias_keeps_its_own_host_so_only_owner_and_name_decide_the_match() {
        let alias = RepoSlug::parse_url("git@work-github:acme/webapp.git").unwrap();
        let canonical = RepoSlug::new("github.com", "Acme", "WebApp");
        assert_eq!(alias.host, "work-github");
        assert!(alias.same_repository(&canonical));
        assert!(!alias.same_host(&canonical));
    }

    #[test]
    fn a_local_path_or_a_url_with_the_wrong_shape_names_no_forge_repository() {
        assert_eq!(slug("/srv/git/webapp.git"), None);
        assert_eq!(slug("../webapp"), None);
        assert_eq!(slug("file:///srv/git/acme/webapp.git"), None);
        assert_eq!(slug("https://github.com/acme"), None);
        assert_eq!(slug("https://github.com/acme/webapp/extra"), None);
        assert_eq!(slug("git@github.com:"), None);
    }

    fn remote(name: &str, url: &str) -> RemoteUrl {
        RemoteUrl {
            name: name.to_owned(),
            url: url.to_owned(),
        }
    }

    fn webapp() -> RepoSlug {
        RepoSlug::new("github.com", "acme", "webapp")
    }

    #[test]
    fn the_remote_for_a_repository_is_found_whatever_else_is_configured() {
        let remotes = [
            remote("fork", "git@github.com:someone/webapp.git"),
            remote("upstream", "https://github.com/acme/webapp.git"),
        ];
        assert_eq!(remote_for(&webapp(), &remotes).as_deref(), Some("upstream"));
        assert_eq!(remote_for(&webapp(), &remotes[..1]), None);
    }

    #[test]
    fn a_matching_host_then_origin_breaks_a_tie_between_remotes_for_the_same_repository() {
        let remotes = [
            remote("alias", "git@work-github:acme/webapp.git"),
            remote("mirror", "https://github.com/acme/webapp.git"),
            remote("origin", "https://github.com/acme/webapp"),
        ];
        assert_eq!(remote_for(&webapp(), &remotes).as_deref(), Some("origin"));
        // An SSH alias alone still counts: its host is a name only ~/.ssh/config knows.
        assert_eq!(
            remote_for(&webapp(), &remotes[..1]).as_deref(),
            Some("alias")
        );
    }

    #[test]
    fn a_repositorys_host_comes_from_its_remote_unless_that_is_an_ssh_alias() {
        let enterprise = [remote("origin", "git@git.example.com:acme/webapp.git")];
        assert_eq!(host_for("acme", "webapp", &enterprise), "git.example.com");
        let alias = [remote("origin", "git@work-github:acme/webapp.git")];
        assert_eq!(host_for("acme", "webapp", &alias), "github.com");
        assert_eq!(host_for("acme", "other", &enterprise), "github.com");
        assert_eq!(default_host(&enterprise), "git.example.com");
        assert_eq!(default_host(&alias), "github.com");
        assert_eq!(default_host(&[]), "github.com");
    }

    #[test]
    fn a_number_with_or_without_a_hash_is_a_pull_request_in_the_checkouts_own_repository() {
        assert_eq!(
            PrTarget::parse(" 4031 ").unwrap(),
            PrTarget {
                repo: None,
                number: 4031
            }
        );
        assert_eq!(PrTarget::parse("#7").unwrap().number, 7);
    }

    #[test]
    fn a_pull_request_url_names_its_repository_whatever_tab_or_fragment_it_was_copied_from() {
        let expected = PrTarget {
            repo: Some(RepoSlug::new("github.com", "acme", "webapp")),
            number: 4031,
        };
        for url in [
            "https://github.com/acme/webapp/pull/4031",
            "https://github.com/acme/webapp/pull/4031/files",
            "https://github.com/acme/webapp/pull/4031/files?w=1#diff-1",
            "https://github.com/acme/webapp/pull/4031#issuecomment-1",
        ] {
            assert_eq!(PrTarget::parse(url).unwrap(), expected, "{url}");
        }
        let enterprise = PrTarget::parse("https://git.example.com/acme/webapp/pull/9").unwrap();
        assert_eq!(enterprise.repo.unwrap().host, "git.example.com");
    }

    #[test]
    fn text_that_is_not_a_pull_request_is_refused_with_the_forms_that_would_work() {
        for text in [
            "",
            "0",
            "-3",
            "12a",
            "https://github.com/acme/webapp/issues/4031",
            "https://github.com/acme/webapp/pull/",
            "https://github.com/acme/webapp/pull/abc",
            "ftp://github.com/acme/webapp/pull/1",
            "acme/webapp#1",
        ] {
            let error = PrTarget::parse(text).unwrap_err();
            assert!(
                error.0.contains("URL") || error.0.contains("number"),
                "{text}: {error}"
            );
        }
    }

    #[test]
    fn a_failure_outranks_a_running_check_and_only_a_finished_run_passes() {
        let checks = |passed, failed, pending, skipped| ChecksSummary {
            passed,
            failed,
            pending,
            skipped,
        };
        assert_eq!(checks(0, 0, 0, 0).state(), ChecksState::None);
        assert_eq!(checks(3, 1, 2, 0).state(), ChecksState::Failing);
        assert_eq!(checks(3, 0, 1, 0).state(), ChecksState::Pending);
        assert_eq!(checks(3, 0, 0, 4).state(), ChecksState::Passing);
        assert_eq!(checks(3, 1, 2, 4).total(), 10);
    }

    #[test]
    fn write_access_and_above_can_push_and_nothing_below_it_can() {
        assert!(Permission::Admin.can_push());
        assert!(Permission::Maintain.can_push());
        assert!(Permission::Write.can_push());
        assert!(!Permission::Triage.can_push());
        assert!(!Permission::Read.can_push());
        assert!(!Permission::None.can_push());
        assert!(!Permission::Unknown.can_push());
    }
}
