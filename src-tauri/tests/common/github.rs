//! A fake GitHub for the issue's own pull request: pull requests kept in memory, whose heads are
//! whatever a local bare repository — the fixture's `origin` — holds for their branch, so a push
//! through real git moves them as it would on GitHub.

#![allow(dead_code, clippy::unwrap_used)]

use std::path::{Path, PathBuf};

use parking_lot::Mutex;
use wtm_core::flow::{
    BranchProtection, ChangedLines, ChecksSummary, ForgeAccount, MergeMethod, MergeState,
    Permission, PrBase, PrHead, PrState, PrTarget, PullRequest, RemoteComment, RemoteReview,
    RemoteReviewState, RepoSlug, ReviewEvent,
};
use wtm_core::model::CommitId;
use wtm_core::ports::forge::{
    Forge, ForgeError, OwnPullWriter, PullRequestWriter, RemotePullRequest, ReviewWriter,
    SentComment,
};

#[derive(Debug, Clone)]
pub(crate) struct FakePr {
    pub(crate) number: u64,
    pub(crate) base: String,
    pub(crate) branch: String,
    pub(crate) title: String,
    pub(crate) body: String,
    pub(crate) draft: bool,
    pub(crate) state: PrState,
}

pub(crate) struct FakeGitHub {
    /// The bare repository the fixture's `origin` points at.
    pub(crate) bare: PathBuf,
    pub(crate) repo: RepoSlug,
    pub(crate) prs: Mutex<Vec<FakePr>>,
    pub(crate) reviews: Mutex<Vec<(u64, RemoteReview, Vec<RemoteComment>)>>,
    /// Every write, as `verb #n`, in order.
    pub(crate) writes: Mutex<Vec<String>>,
}

impl FakeGitHub {
    pub(crate) fn new(bare: &Path) -> Self {
        Self {
            bare: bare.to_path_buf(),
            repo: RepoSlug::new("github.com", "acme", "units"),
            prs: Mutex::new(Vec::new()),
            reviews: Mutex::new(Vec::new()),
            writes: Mutex::new(Vec::new()),
        }
    }

    /// Where `branch` is on the remote, as `git` reads the bare repository.
    pub(crate) fn head_of(&self, branch: &str) -> String {
        // A test is a sanctioned place to spawn directly.
        #[allow(clippy::disallowed_methods)]
        let out = std::process::Command::new("git")
            .args(["rev-parse", &format!("refs/heads/{branch}")])
            .current_dir(&self.bare)
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    fn reading(&self, pr: &FakePr) -> PullRequest {
        PullRequest {
            number: pr.number,
            url: format!("https://github.com/acme/units/pull/{}", pr.number),
            title: pr.title.clone(),
            author: Some("you".to_owned()),
            state: pr.state,
            draft: pr.draft,
            head: PrHead {
                repo: Some(self.repo.clone()),
                branch: pr.branch.clone(),
                sha: CommitId::new(self.head_of(&pr.branch)),
            },
            base: PrBase {
                repo: self.repo.clone(),
                branch: pr.base.clone(),
                sha: None,
            },
            cross_repository: false,
            maintainer_can_modify: true,
            requested_reviewers: Vec::new(),
            checks: ChecksSummary {
                passed: 3,
                failed: 0,
                pending: 0,
                skipped: 0,
            },
            review_decision: None,
            merge_state: Some(if pr.draft {
                MergeState::Draft
            } else {
                MergeState::Clean
            }),
        }
    }

    fn remote(&self, pr: &FakePr) -> RemotePullRequest {
        RemotePullRequest {
            number: pr.number,
            url: format!("https://github.com/acme/units/pull/{}", pr.number),
            state: pr.state,
            head: format!("acme:{}", pr.branch),
            base: pr.base.clone(),
            body: pr.body.clone(),
            draft: pr.draft,
            head_sha: self.head_of(&pr.branch),
        }
    }

    /// Someone reviewed pull request `number`.
    pub(crate) fn review(
        &self,
        number: u64,
        id: u64,
        author: &str,
        state: RemoteReviewState,
        body: &str,
    ) {
        self.reviews.lock().push((
            number,
            RemoteReview {
                id,
                node_id: format!("PRR_{id}"),
                author: Some(author.to_owned()),
                state,
                body: body.to_owned(),
                commit_id: None,
                url: format!("https://github.com/acme/units/pull/{number}#pullrequestreview-{id}"),
            },
            vec![RemoteComment {
                id: id * 10,
                path: "src/export.rs".to_owned(),
                body: format!("{author}'s line comment"),
            }],
        ));
    }

    fn not_found(what: &str) -> ForgeError {
        ForgeError::NotFound {
            what: what.to_owned(),
        }
    }
}

impl Forge for FakeGitHub {
    fn check(&self, _: &Path, host: &str) -> Result<ForgeAccount, ForgeError> {
        Ok(ForgeAccount {
            host: host.to_owned(),
            login: Some("you".to_owned()),
            version: Some("2.100.0".to_owned()),
        })
    }

    fn pull_request(&self, _: &Path, target: &PrTarget) -> Result<PullRequest, ForgeError> {
        let prs = self.prs.lock();
        let pr = prs
            .iter()
            .find(|p| p.number == target.number)
            .ok_or_else(|| Self::not_found("that pull request"))?;
        Ok(self.reading(pr))
    }

    fn pull_request_for_head(
        &self,
        _: &Path,
        _: &RepoSlug,
        branch: &str,
    ) -> Result<Option<PullRequest>, ForgeError> {
        Ok(self
            .prs
            .lock()
            .iter()
            .find(|p| p.branch == branch)
            .map(|p| self.reading(p)))
    }

    fn permission(&self, _: &Path, _: &RepoSlug) -> Result<Permission, ForgeError> {
        Ok(Permission::Admin)
    }

    fn branch_protection(
        &self,
        _: &Path,
        _: &RepoSlug,
        branch: &str,
    ) -> Result<BranchProtection, ForgeError> {
        Ok(BranchProtection {
            branch: branch.to_owned(),
            protected: Some(false),
            rules: Vec::new(),
        })
    }

    fn changed_lines(
        &self,
        _: &Path,
        _: &RepoSlug,
        _: &str,
        _: &str,
    ) -> Result<ChangedLines, ForgeError> {
        Err(Self::not_found("a diff"))
    }
}

impl PullRequestWriter for FakeGitHub {
    fn pull_requests_from(
        &self,
        _: &Path,
        _: &RepoSlug,
        head: &str,
    ) -> Result<Vec<RemotePullRequest>, ForgeError> {
        let mut found: Vec<RemotePullRequest> = self
            .prs
            .lock()
            .iter()
            .filter(|p| format!("acme:{}", p.branch) == head)
            .map(|p| self.remote(p))
            .collect();
        found.sort_by_key(|p| std::cmp::Reverse(p.number));
        Ok(found)
    }

    fn open_pull_request(
        &self,
        _: &Path,
        _: &RepoSlug,
        _: &str,
        _: &str,
        _: &str,
        _: &str,
    ) -> Result<RemotePullRequest, ForgeError> {
        panic!("the issue's own pull request is never opened as a follow-up")
    }
}

impl OwnPullWriter for FakeGitHub {
    fn open_draft(
        &self,
        _: &Path,
        _: &RepoSlug,
        base: &str,
        head: &str,
        title: &str,
        body: &str,
    ) -> Result<RemotePullRequest, ForgeError> {
        let mut prs = self.prs.lock();
        let number = 40 + u64::try_from(prs.len()).unwrap();
        let pr = FakePr {
            number,
            base: base.to_owned(),
            branch: head.to_owned(),
            title: title.to_owned(),
            body: body.to_owned(),
            draft: true,
            state: PrState::Open,
        };
        prs.push(pr.clone());
        self.writes.lock().push(format!("open #{number}"));
        Ok(self.remote(&pr))
    }

    fn update(
        &self,
        _: &Path,
        _: &RepoSlug,
        number: u64,
        title: &str,
        body: &str,
    ) -> Result<RemotePullRequest, ForgeError> {
        let mut prs = self.prs.lock();
        let pr = prs
            .iter_mut()
            .find(|p| p.number == number)
            .ok_or_else(|| Self::not_found("that pull request"))?;
        title.clone_into(&mut pr.title);
        body.clone_into(&mut pr.body);
        self.writes.lock().push(format!("update #{number}"));
        Ok(self.remote(pr))
    }

    fn mark_ready(&self, _: &Path, _: &RepoSlug, number: u64) -> Result<(), ForgeError> {
        let mut prs = self.prs.lock();
        let pr = prs
            .iter_mut()
            .find(|p| p.number == number)
            .ok_or_else(|| Self::not_found("that pull request"))?;
        pr.draft = false;
        self.writes.lock().push(format!("ready #{number}"));
        Ok(())
    }

    fn merge(
        &self,
        _: &Path,
        _: &RepoSlug,
        number: u64,
        head: &str,
        method: MergeMethod,
    ) -> Result<(), ForgeError> {
        let mut prs = self.prs.lock();
        let pr = prs
            .iter_mut()
            .find(|p| p.number == number)
            .ok_or_else(|| Self::not_found("that pull request"))?;
        self.writes
            .lock()
            .push(format!("merge #{number} {method:?} at {}", &head[..7]));
        if self.head_of(&pr.branch) != head {
            return Err(ForgeError::Rejected {
                what: format!("#{number} merged at {head}"),
                reason: "Head branch was modified. Review and try the merge again.".to_owned(),
            });
        }
        pr.state = PrState::Merged;
        Ok(())
    }
}

impl ReviewWriter for FakeGitHub {
    fn reviews(
        &self,
        _: &Path,
        _: &RepoSlug,
        number: u64,
    ) -> Result<Vec<RemoteReview>, ForgeError> {
        Ok(self
            .reviews
            .lock()
            .iter()
            .filter(|(n, _, _)| *n == number)
            .map(|(_, r, _)| r.clone())
            .collect())
    }

    fn review_comments(
        &self,
        _: &Path,
        _: &RepoSlug,
        _: u64,
        review: u64,
    ) -> Result<Vec<RemoteComment>, ForgeError> {
        Ok(self
            .reviews
            .lock()
            .iter()
            .find(|(_, r, _)| r.id == review)
            .map(|(_, _, c)| c.clone())
            .unwrap_or_default())
    }

    fn create_pending_review(
        &self,
        _: &Path,
        _: &RepoSlug,
        _: u64,
        _: &str,
        _: &str,
        _: &[SentComment],
    ) -> Result<RemoteReview, ForgeError> {
        panic!("your own pull request is never reviewed through the outbox")
    }

    fn add_review_comment(
        &self,
        _: &Path,
        _: &RepoSlug,
        _: &str,
        _: &SentComment,
    ) -> Result<(), ForgeError> {
        panic!("your own pull request is never reviewed through the outbox")
    }

    fn submit_review(
        &self,
        _: &Path,
        _: &RepoSlug,
        _: u64,
        _: u64,
        _: ReviewEvent,
        _: &str,
    ) -> Result<RemoteReview, ForgeError> {
        panic!("your own pull request is never reviewed through the outbox")
    }
}
