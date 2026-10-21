//! Review worktrees against a real `git`: a fork's pull request ref, and a second review of the
//! same pull request.
//!
//! `review_pipeline.rs` proves what the pipeline decides. These prove that what it decides works
//! on a repository: that `pull/<n>/head` brings a commit no branch of yours can reach, that the
//! worktree lands detached on exactly that commit without a branch or remote being added, and that
//! asking twice leaves one worktree. A fake git would agree with any refspec it was handed.
//!
//! The forge is a second [`GitFixture`] playing the base repository: its pull request ref is made
//! with `update-ref`, the way the forge keeps one, and the reviewing clone reaches it through a
//! `url.<path>.insteadOf` rule, so its `origin` can be the forge URL the pull request names while
//! the bytes come from disk. The adapters run with your own git config, as the app does.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use wtm_config::RealFileStore;
use wtm_core::flow::{ChecksSummary, PrBase, PrHead, PrState, PullRequest, RepoSlug};
use wtm_core::model::{CommitId, CreateOutcome, Project, ProjectId};
use wtm_core::ports::exec::{CancelToken, CommandRunner};
use wtm_core::ports::progress::NullProgress;
use wtm_core::usecase::{CreatePipeline, ReviewCheckout, ReviewOutcome, ReviewRequest};
use wtm_exec::Runner;
use wtm_git::GitCli;
use wtm_render::Engine;
use wtm_testkit::{FakeClock, FakePty, GitFixture, NullPtySink};

const FORGE_URL: &str = "https://github.com/acme/webapp.git";

/// The base repository on the forge, and a clone of it you review from.
struct Forge {
    upstream: GitFixture,
    clone: GitFixture,
    pipeline: CreatePipeline,
}

impl Forge {
    fn new() -> Self {
        let upstream = GitFixture::new();
        let clone = GitFixture::new();
        clone.git(&["remote", "set-url", "origin", FORGE_URL]);
        clone.git(&[
            "config",
            &format!("url.{}.insteadOf", upstream.root().display()),
            FORGE_URL,
        ]);

        let runner: Arc<dyn CommandRunner> = Arc::new(Runner::with_probed_path(None));
        let pipeline = CreatePipeline {
            git: Arc::new(GitCli::new(Arc::clone(&runner))),
            runner,
            pty: Arc::new(FakePty::new()),
            engine: Arc::new(Engine::new()),
            files: Arc::new(RealFileStore::new()),
            clock: Arc::new(FakeClock::new()),
        };
        Self {
            upstream,
            clone,
            pipeline,
        }
    }

    /// A commit only `refs/pull/<number>/head` reaches, as a fork's is on the base repository.
    fn fork_pull_request(&self, number: u64) -> String {
        self.upstream.git(&["checkout", "-q", "-b", "contribution"]);
        self.upstream
            .commit("export.csv", "id,name\n", "Export units as CSV");
        let sha = self.upstream.git(&["rev-parse", "HEAD"]).trim().to_owned();
        self.upstream
            .git(&["update-ref", &format!("refs/pull/{number}/head"), &sha]);
        self.upstream.git(&["checkout", "-q", "main"]);
        self.upstream.git(&["branch", "-q", "-D", "contribution"]);
        sha
    }

    fn project(&self) -> Project {
        Project {
            id: ProjectId::from_root(self.clone.root()),
            root: self.clone.root().to_path_buf(),
            schema_version: 1,
            meta: wtm_core::model::ProjectMeta::default(),
            fields: vec![],
            lookups: vec![],
            computed: vec![],
            naming: wtm_core::model::NamingSpec {
                branch: "task/{{ name }}".to_owned(),
                directory: "{{ name }}".to_owned(),
                dir_base: wtm_core::model::DirBase::RepoParent,
                branch_must_match: None,
            },
            create: wtm_core::model::CreateSpec::default(),
            setup: None,
            remove: wtm_core::model::RemoveSpec::default(),
            display: wtm_core::model::DisplaySpec::default(),
            browser: wtm_core::model::BrowserSpec::default(),
            database: BTreeMap::new(),
            actions: vec![],
            agent: BTreeMap::new(),
            workflow: None,
            guards: wtm_core::model::GuardSpec::default(),
        }
    }

    fn request(&self, number: u64, sha: &str, fork: bool, branch: &str) -> ReviewRequest {
        ReviewRequest {
            project: self.project(),
            ambient: wtm_core::ports::template::Context::new(),
            pull_request: PullRequest {
                number,
                url: format!("https://github.com/acme/webapp/pull/{number}"),
                title: "Export units as CSV".to_owned(),
                author: Some("sam".to_owned()),
                state: PrState::Open,
                draft: false,
                head: PrHead {
                    repo: Some(RepoSlug::new(
                        "github.com",
                        if fork { "sam" } else { "acme" },
                        "webapp",
                    )),
                    branch: branch.to_owned(),
                    sha: CommitId::new(sha),
                },
                base: PrBase {
                    repo: RepoSlug::new("github.com", "acme", "webapp"),
                    branch: "main".to_owned(),
                    sha: None,
                },
                cross_repository: fork,
                maintainer_can_modify: false,
                requested_reviewers: vec![],
                checks: ChecksSummary::default(),
            },
            expected_head: Some(CommitId::new(sha)),
            checkout: ReviewCheckout::Detached,
            prepare_environment: false,
            acknowledged: vec![],
            rows: 24,
            cols: 80,
        }
    }

    fn execute(&self, req: &ReviewRequest) -> ReviewOutcome {
        self.pipeline
            .execute_review(
                req,
                &NullProgress,
                Arc::new(NullPtySink),
                &CancelToken::new(),
            )
            .unwrap()
    }

    /// Every ref and the worktree list, for "nothing else changed".
    fn refs(&self) -> String {
        self.clone
            .git(&["for-each-ref", "--format=%(refname) %(objectname)"])
    }

    fn worktree_count(&self) -> usize {
        self.clone
            .git(&["worktree", "list", "--porcelain"])
            .lines()
            .filter(|line| line.starts_with("worktree "))
            .count()
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    a.canonicalize().unwrap() == b.canonicalize().unwrap()
}

#[test]
fn a_forks_pull_request_ref_is_fetched_and_checked_out_detached_at_its_head_commit() {
    let forge = Forge::new();
    let sha = forge.fork_pull_request(7);
    // The fork's branch is called `main`, like yours: its name must not matter.
    let req = forge.request(7, &sha, true, "main");
    let (code, _) = forge.clone.git_try(&["cat-file", "-e", &sha]);
    assert_ne!(
        code, 0,
        "the fixture must not already have the fork's commit"
    );
    let refs_before = forge.refs();
    let remotes_before = forge.clone.git(&["remote", "-v"]);

    let worktree = match forge.execute(&req) {
        ReviewOutcome::Created {
            outcome: CreateOutcome::Created { worktree, .. },
        } => worktree,
        other => panic!("expected a new worktree, got {other:?}"),
    };

    assert!(same_path(
        &worktree.path,
        &forge.clone.parent().join("repo-pr-7")
    ));
    assert_eq!(
        forge
            .clone
            .git_in(&worktree.path, &["rev-parse", "HEAD"])
            .trim(),
        sha
    );
    assert_eq!(
        forge
            .clone
            .git_in(&worktree.path, &["rev-parse", "--abbrev-ref", "HEAD"])
            .trim(),
        "HEAD",
        "the review worktree must be detached"
    );
    assert_eq!(
        forge.refs(),
        refs_before,
        "reviewing a fork must not add a branch or a remote-tracking ref"
    );
    assert_eq!(
        forge.clone.git(&["remote", "-v"]),
        remotes_before,
        "reviewing a fork must not add the fork as a remote"
    );
}

#[test]
fn reviewing_the_same_pull_request_again_reuses_its_worktree_instead_of_adding_another() {
    let forge = Forge::new();
    let sha = forge.fork_pull_request(7);
    let req = forge.request(7, &sha, true, "main");

    let first = match forge.execute(&req) {
        ReviewOutcome::Created {
            outcome: CreateOutcome::Created { worktree, .. },
        } => worktree,
        other => panic!("expected a new worktree, got {other:?}"),
    };
    assert_eq!(forge.worktree_count(), 2);
    let refs_after_first = forge.refs();

    // Again, and again asking for a branch: the worktree that holds the head is the answer.
    let mut again = forge.request(7, &sha, true, "main");
    again.checkout = ReviewCheckout::Branch;
    for req in [&req, &again] {
        match forge.execute(req) {
            ReviewOutcome::Reused { worktree, at_head } => {
                assert!(same_path(&worktree.path, &first.path));
                assert!(at_head);
            }
            other @ ReviewOutcome::Created { .. } => {
                panic!("expected the first worktree back, got {other:?}")
            }
        }
    }
    assert_eq!(
        forge.worktree_count(),
        2,
        "a second review added a worktree"
    );
    assert_eq!(
        forge.refs(),
        refs_after_first,
        "a reused review created a branch"
    );
}

#[test]
fn a_pull_request_from_the_same_repository_updates_its_remote_tracking_branch() {
    let forge = Forge::new();
    forge
        .upstream
        .git(&["checkout", "-q", "-b", "feature/csv-export"]);
    forge
        .upstream
        .commit("export.csv", "id,name\n", "Export units as CSV");
    let sha = forge.upstream.git(&["rev-parse", "HEAD"]).trim().to_owned();
    forge.upstream.git(&["checkout", "-q", "main"]);

    let req = forge.request(8, &sha, false, "feature/csv-export");
    match forge.execute(&req) {
        ReviewOutcome::Created {
            outcome: CreateOutcome::Created { worktree, .. },
        } => assert_eq!(
            forge
                .clone
                .git_in(&worktree.path, &["rev-parse", "HEAD"])
                .trim(),
            sha
        ),
        other => panic!("expected a new worktree, got {other:?}"),
    }
    assert_eq!(
        forge
            .clone
            .git(&["rev-parse", "refs/remotes/origin/feature/csv-export"])
            .trim(),
        sha
    );
    // Still no local branch: a review reads, it does not adopt.
    assert_eq!(
        forge
            .clone
            .git(&["for-each-ref", "--format=%(refname)", "refs/heads"])
            .trim(),
        "refs/heads/main"
    );
}
