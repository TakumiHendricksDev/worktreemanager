//! The review pipeline's decisions, against fakes: what it fetches, whether it runs setup, and
//! when it makes nothing at all.
//!
//! It exists because a review worktree is someone else's branch, and the two promises that make
//! opening one safe are both "does not happen" promises that no screenshot can show: planning
//! mutates nothing, and the project's setup does not run unless it was asked for. Each test below
//! states one of them against a fake git that records every mutation. What a real `git` does with
//! a fork's pull request ref is `review_worktrees.rs`'s job.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use wtm_core::WtmError;
use wtm_core::flow::{ChecksSummary, PrBase, PrHead, PrState, PullRequest, RepoSlug};
use wtm_core::model::{CommitId, CreateOutcome, PreflightSeverity, Project, ProjectId};
use wtm_core::ports::exec::CancelToken;
use wtm_core::ports::progress::NullProgress;
use wtm_core::usecase::{
    CreatePipeline, ReuseReason, ReviewCheckout, ReviewOutcome, ReviewRequest, ReviewSetup,
    ReviewTarget,
};
use wtm_render::Engine;
use wtm_testkit::{FakeClock, FakeFileStore, FakeGit, FakePty, FakeRunner, NullPtySink};

const REPO: &str = "/x/webapp";
const HEAD: &str = "7c9e1d4a5b6c7d8e9f0a1b2c3d4e5f6a7b8c9d0e";
const URL: &str = "https://github.com/acme/webapp.git";

fn project(setup: bool) -> Project {
    Project {
        id: ProjectId::from_root(std::path::Path::new(REPO)),
        root: PathBuf::from(REPO),
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
        setup: setup.then(|| wtm_core::model::SetupSpec {
            command: wtm_core::model::CommandSpec {
                run: vec![
                    "./bin/setup.sh".to_owned(),
                    "{{ worktree.path }}".to_owned(),
                ],
                cwd: wtm_core::model::CwdBase::RepoRoot,
                env: BTreeMap::new(),
                timeout_ms: Some(1000),
                pty: true,
                when: None,
                on_failure: wtm_core::model::OnFailure::Keep,
                args_when: vec![],
            },
            concurrency: wtm_core::model::Concurrency::default(),
        }),
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

/// Pull request 4031 on acme/webapp, from a fork unless `fork` is false.
fn pull_request(fork: bool) -> PullRequest {
    PullRequest {
        number: 4031,
        url: "https://github.com/acme/webapp/pull/4031".to_owned(),
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
            branch: "feature/csv-export".to_owned(),
            sha: CommitId::new(HEAD),
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
    }
}

fn request(project: Project, pr: PullRequest) -> ReviewRequest {
    ReviewRequest {
        project,
        ambient: wtm_core::ports::template::Context::new(),
        pull_request: pr,
        expected_head: Some(CommitId::new(HEAD)),
        checkout: ReviewCheckout::Detached,
        prepare_environment: false,
        acknowledged: vec![],
        rows: 24,
        cols: 80,
    }
}

struct Harness {
    pipeline: CreatePipeline,
    git: Arc<FakeGit>,
    pty: Arc<FakePty>,
}

fn harness(git: FakeGit) -> Harness {
    let git = Arc::new(git);
    let pty = Arc::new(FakePty::new());
    let files = FakeFileStore::new();
    // The setup script the preflight looks for, relative to the repository root.
    files.add_file("/x/webapp/./bin/setup.sh", "#!/bin/sh\n");
    Harness {
        pipeline: CreatePipeline {
            git: Arc::clone(&git) as Arc<dyn wtm_core::ports::git::Git>,
            runner: Arc::new(FakeRunner::new()),
            pty: Arc::clone(&pty) as Arc<dyn wtm_core::ports::pty::PtyHost>,
            engine: Arc::new(Engine::new()),
            files: Arc::new(files),
            clock: Arc::new(FakeClock::new()),
        },
        git,
        pty,
    }
}

/// A repository whose `origin` is acme/webapp, where the head arrives only by fetching `refspec`.
fn repo_fetching(refspec: &str) -> FakeGit {
    FakeGit::with_main(REPO, "main")
        .with_remote_url("origin", URL)
        .with_fetchable(refspec, HEAD)
}

fn execute(h: &Harness, req: &ReviewRequest) -> Result<ReviewOutcome, WtmError> {
    h.pipeline.execute_review(
        req,
        &NullProgress,
        Arc::new(NullPtySink),
        &CancelToken::new(),
    )
}

#[test]
fn previewing_a_review_worktree_touches_nothing() {
    let h = harness(repo_fetching("pull/4031/head"));
    let mut req = request(project(true), pull_request(true));
    req.prepare_environment = true;

    let preview = h
        .pipeline
        .preview_review(&req, &NullProgress, &CancelToken::new())
        .unwrap();

    assert!(matches!(preview.plan.target, ReviewTarget::Create { .. }));
    assert!(
        !h.git.was_mutated(),
        "planning mutated the repository: {:?}",
        h.git.mutations()
    );
    assert!(h.pty.spawns().is_empty(), "planning ran setup");
}

#[test]
fn a_forks_pull_request_is_fetched_from_its_pull_ref_and_checked_out_detached_at_the_head() {
    let h = harness(repo_fetching("pull/4031/head"));
    let req = request(project(false), pull_request(true));

    let preview = h
        .pipeline
        .preview_review(&req, &NullProgress, &CancelToken::new())
        .unwrap();
    let fetch = preview.plan.fetch.as_ref().unwrap();
    assert_eq!(
        (fetch.remote.as_str(), fetch.refspec.as_str()),
        ("origin", "pull/4031/head")
    );
    assert!(!preview.plan.head_present);
    match &preview.plan.target {
        ReviewTarget::Create {
            directory,
            branch,
            git_argv,
        } => {
            assert_eq!(directory, &PathBuf::from("/x/webapp-pr-4031"));
            assert_eq!(branch, &None);
            assert_eq!(
                git_argv,
                &[
                    "git",
                    "-c",
                    "core.hooksPath=/dev/null",
                    "worktree",
                    "add",
                    "--detach",
                    "--",
                    "/x/webapp-pr-4031",
                    HEAD
                ]
            );
        }
        other @ ReviewTarget::Reuse { .. } => panic!("expected a new worktree, got {other:?}"),
    }

    let outcome = execute(&h, &req).unwrap();
    assert_eq!(
        h.git.mutations(),
        [
            "fetch origin pull/4031/head".to_owned(),
            "add_worktree /x/webapp-pr-4031".to_owned()
        ]
    );
    match outcome {
        ReviewOutcome::Created {
            outcome: CreateOutcome::Created { worktree, .. },
        } => assert_eq!(worktree.head.unwrap().as_str(), HEAD),
        other => panic!("expected a created worktree, got {other:?}"),
    }
}

#[test]
fn an_open_pull_request_from_the_same_repository_is_fetched_by_its_branch() {
    let h = harness(repo_fetching("feature/csv-export"));
    let req = request(project(false), pull_request(false));

    execute(&h, &req).unwrap();
    assert_eq!(
        h.git.mutations()[0],
        "fetch origin feature/csv-export",
        "a same-repository branch is fetched by name, which keeps its remote-tracking ref current"
    );
}

#[test]
fn the_projects_setup_is_skipped_unless_the_review_asks_to_prepare_the_environment() {
    let h = harness(repo_fetching("pull/4031/head"));
    let req = request(project(true), pull_request(true));

    let preview = h
        .pipeline
        .preview_review(&req, &NullProgress, &CancelToken::new())
        .unwrap();
    assert_eq!(preview.plan.setup, ReviewSetup::Skipped);

    match execute(&h, &req).unwrap() {
        ReviewOutcome::Created {
            outcome: CreateOutcome::Created { setup_session, .. },
        } => assert_eq!(setup_session, None),
        other => panic!("expected a created worktree, got {other:?}"),
    }
    assert!(
        h.pty.spawns().is_empty(),
        "setup ran without Prepare environment: {:?}",
        h.pty.spawns()
    );
}

#[test]
fn preparing_the_environment_runs_the_projects_setup_in_the_review_worktree() {
    let h = harness(repo_fetching("pull/4031/head"));
    let mut req = request(project(true), pull_request(true));
    req.prepare_environment = true;

    let preview = h
        .pipeline
        .preview_review(&req, &NullProgress, &CancelToken::new())
        .unwrap();
    assert_eq!(
        preview.plan.setup,
        ReviewSetup::Run {
            argv: vec!["./bin/setup.sh".to_owned(), "/x/webapp-pr-4031".to_owned()],
            cwd: PathBuf::from(REPO),
        }
    );

    execute(&h, &req).unwrap();
    let spawns = h.pty.spawns();
    assert_eq!(spawns.len(), 1);
    assert_eq!(spawns[0].argv, ["./bin/setup.sh", "/x/webapp-pr-4031"]);
}

#[test]
fn a_worktree_already_on_the_head_is_reused_and_nothing_is_fetched_created_or_run() {
    let git = repo_fetching("pull/4031/head").with_worktree_at("/x/somewhere", None, HEAD);
    let h = harness(git);
    let mut req = request(project(true), pull_request(true));
    req.prepare_environment = true;

    let preview = h
        .pipeline
        .preview_review(&req, &NullProgress, &CancelToken::new())
        .unwrap();
    assert!(matches!(
        preview.plan.target,
        ReviewTarget::Reuse {
            reason: ReuseReason::HeadCommit,
            at_head: true,
            ..
        }
    ));
    assert_eq!(preview.plan.setup, ReviewSetup::NotApplicable);
    assert!(preview.plan.fetch.is_none());

    match execute(&h, &req).unwrap() {
        ReviewOutcome::Reused { worktree, at_head } => {
            assert_eq!(worktree.path, PathBuf::from("/x/somewhere"));
            assert!(at_head);
        }
        other @ ReviewOutcome::Created { .. } => {
            panic!("expected the existing worktree, got {other:?}")
        }
    }
    assert!(!h.git.was_mutated(), "{:?}", h.git.mutations());
    assert!(h.pty.spawns().is_empty());
}

#[test]
fn an_earlier_review_worktree_behind_the_head_is_reused_with_a_warning_not_moved() {
    let git = repo_fetching("pull/4031/head").with_worktree_at(
        "/x/webapp-pr-4031",
        None,
        "0ld0000000000000000000000000000000000000",
    );
    let h = harness(git);
    let req = request(project(false), pull_request(true));

    let preview = h
        .pipeline
        .preview_review(&req, &NullProgress, &CancelToken::new())
        .unwrap();
    assert!(
        preview
            .warnings
            .iter()
            .any(|w| w.id == "reused_behind_head"),
        "{:?}",
        preview.warnings
    );
    assert!(matches!(
        execute(&h, &req).unwrap(),
        ReviewOutcome::Reused { at_head: false, .. }
    ));
    assert!(!h.git.was_mutated());
}

#[test]
fn a_head_that_moved_since_it_was_shown_blocks_the_worktree_until_it_is_looked_at_again() {
    let h = harness(repo_fetching("pull/4031/head"));
    let mut req = request(project(false), pull_request(true));
    req.expected_head = Some(CommitId::new("1111111111111111111111111111111111111111"));
    // Acknowledging cannot waive it: nobody has looked at this commit.
    req.acknowledged = vec!["head_moved".to_owned()];

    match execute(&h, &req) {
        Err(WtmError::Preflight(items)) => {
            assert_eq!(items.len(), 1);
            assert_eq!(items[0].id, "head_moved");
            assert!(!items[0].overridable);
        }
        other => panic!("expected a preflight refusal, got {other:?}"),
    }
    assert!(!h.git.was_mutated());
}

#[test]
fn without_a_remote_for_the_base_repository_a_missing_head_is_refused_before_anything_runs() {
    let git = FakeGit::with_main(REPO, "main")
        .with_remotes(&[])
        .with_remote_url("fork", "git@github.com:sam/webapp.git");
    let h = harness(git);
    let req = request(project(false), pull_request(true));

    let preview = h
        .pipeline
        .preview_review(&req, &NullProgress, &CancelToken::new())
        .unwrap();
    let item = preview
        .preflight
        .iter()
        .find(|item| item.id == "no_remote")
        .unwrap();
    assert_eq!(item.severity, PreflightSeverity::Error);
    assert!(item.message.contains("acme/webapp"), "{}", item.message);
    assert!(execute(&h, &req).is_err());
    assert!(!h.git.was_mutated());
}

#[test]
fn a_head_the_fetch_did_not_bring_is_refused_instead_of_checking_out_something_else() {
    // origin is right, but the fetch brings nothing: the head was force-pushed away.
    let git = FakeGit::with_main(REPO, "main").with_remote_url("origin", URL);
    let h = harness(git);
    let req = request(project(false), pull_request(true));

    let error = execute(&h, &req).unwrap_err();
    assert!(error.to_string().contains("force-pushed"), "{error}");
    assert_eq!(h.git.mutations(), ["fetch origin pull/4031/head"]);
}

#[test]
fn a_branch_checkout_makes_an_untracked_review_branch_and_refuses_one_left_over() {
    let h = harness(repo_fetching("pull/4031/head"));
    let mut req = request(project(false), pull_request(true));
    req.checkout = ReviewCheckout::Branch;
    let preview = h
        .pipeline
        .preview_review(&req, &NullProgress, &CancelToken::new())
        .unwrap();
    match &preview.plan.target {
        ReviewTarget::Create { git_argv, .. } => assert_eq!(
            git_argv,
            &[
                "git",
                "-c",
                "core.hooksPath=/dev/null",
                "worktree",
                "add",
                "--no-track",
                "-b",
                "pr-4031",
                "--",
                "/x/webapp-pr-4031",
                HEAD
            ]
        ),
        other @ ReviewTarget::Reuse { .. } => panic!("expected a new worktree, got {other:?}"),
    }

    let leftover = harness(repo_fetching("pull/4031/head").with_local_branches(&["pr-4031"]));
    let preview = leftover
        .pipeline
        .preview_review(&req, &NullProgress, &CancelToken::new())
        .unwrap();
    assert!(
        preview
            .preflight
            .iter()
            .any(|item| item.id == "branch_exists"),
        "{:?}",
        preview.preflight
    );
}

// ── moving a review worktree to a new head ──

const OLD: &str = "0d1d0000000000000000000000000000000000aa";
const REVIEW_DIR: &str = "/x/webapp-pr-4031";

fn review_worktree(branch: Option<&str>, head: &str) -> wtm_core::model::Worktree {
    wtm_core::model::Worktree {
        id: wtm_core::model::WorktreeId::from_path(std::path::Path::new(REVIEW_DIR)),
        path: PathBuf::from(REVIEW_DIR),
        head: Some(CommitId::new(head)),
        checkout: branch.map_or(wtm_core::model::Checkout::Detached, |b| {
            wtm_core::model::Checkout::Branch {
                branch: wtm_core::model::BranchRef::new(b),
            }
        }),
        is_main: false,
        is_bare: false,
        locked: None,
        prunable: None,
    }
}

fn advance(
    h: &Harness,
    worktree: wtm_core::model::Worktree,
    reviewed: Option<&str>,
) -> Result<wtm_core::usecase::AdvanceOutcome, WtmError> {
    h.pipeline
        .advance_review(&wtm_core::usecase::AdvanceRequest {
            project: project(true),
            pull_request: pull_request(true),
            worktree,
            reviewed: reviewed.map(CommitId::new),
        })
}

fn refusal(result: Result<wtm_core::usecase::AdvanceOutcome, WtmError>) -> String {
    match result {
        Err(WtmError::Preflight(items)) => items[0].id.clone(),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn a_review_worktree_moves_to_a_forks_new_head_by_its_pull_ref_with_nothing_else_touched() {
    let git = repo_fetching("pull/4031/head").with_worktree_at(REVIEW_DIR, None, OLD);
    let h = harness(git);

    let outcome = advance(&h, review_worktree(None, OLD), Some(OLD)).unwrap();

    assert_eq!(
        outcome,
        wtm_core::usecase::AdvanceOutcome::Moved {
            from: Some(CommitId::new(OLD)),
            to: CommitId::new(HEAD),
            fetched: Some(wtm_core::usecase::ReviewFetch {
                remote: "origin".to_owned(),
                refspec: "pull/4031/head".to_owned(),
            }),
        }
    );
    assert_eq!(
        h.git.mutations(),
        vec![
            "fetch origin pull/4031/head".to_owned(),
            format!("checkout_review_head {REVIEW_DIR} --detach {HEAD}"),
        ]
    );
    assert!(h.pty.spawns().is_empty(), "moving a review ran setup");
}

#[test]
fn a_review_worktree_on_its_review_branch_takes_the_branch_with_it() {
    let git = repo_fetching("pull/4031/head").with_worktree_at(REVIEW_DIR, Some("pr-4031"), OLD);
    let h = harness(git);

    advance(&h, review_worktree(Some("pr-4031"), OLD), Some(OLD)).unwrap();

    assert_eq!(
        h.git.mutations().last().unwrap(),
        &format!("checkout_review_head {REVIEW_DIR} pr-4031 {HEAD}")
    );
}

#[test]
fn a_review_worktree_already_at_the_head_is_neither_fetched_nor_moved() {
    let h = harness(repo_fetching("pull/4031/head"));
    let outcome = advance(&h, review_worktree(None, HEAD), Some(OLD)).unwrap();
    assert_eq!(outcome, wtm_core::usecase::AdvanceOutcome::AtHead);
    assert!(!h.git.was_mutated(), "{:?}", h.git.mutations());
}

#[test]
fn uncommitted_changes_in_a_review_worktree_stop_it_being_moved() {
    let git = repo_fetching("pull/4031/head").with_status(
        REVIEW_DIR,
        wtm_core::model::WorkingTreeStatus {
            dirty_tracked: true,
            ..Default::default()
        },
    );
    let h = harness(git);
    assert_eq!(
        refusal(advance(&h, review_worktree(None, OLD), Some(OLD))),
        "review_worktree_changed"
    );
    assert!(!h.git.was_mutated(), "{:?}", h.git.mutations());
}

#[test]
fn a_worktree_on_a_branch_of_your_own_is_not_a_review_checkout_and_is_never_moved() {
    let h = harness(repo_fetching("pull/4031/head"));
    assert_eq!(
        refusal(advance(
            &h,
            review_worktree(Some("feature/csv-export"), OLD),
            Some(OLD)
        )),
        "not_a_review_checkout"
    );
    assert!(!h.git.was_mutated(), "{:?}", h.git.mutations());
}

#[test]
fn a_review_worktree_at_a_commit_the_review_never_read_is_left_where_it_is() {
    let h = harness(repo_fetching("pull/4031/head"));
    let elsewhere = "e15e000000000000000000000000000000000000";
    assert_eq!(
        refusal(advance(&h, review_worktree(None, elsewhere), Some(OLD))),
        "review_worktree_moved"
    );
    assert!(!h.git.was_mutated(), "{:?}", h.git.mutations());
}

#[test]
fn a_new_head_the_fetch_does_not_bring_is_refused_rather_than_checked_out() {
    // Force-pushed again between the forge's answer and the fetch: the ref now brings another
    // commit, and the one the review was told about is not here.
    let git = FakeGit::with_main(REPO, "main")
        .with_remote_url("origin", URL)
        .with_fetchable("pull/4031/head", "f0f0000000000000000000000000000000000000");
    let h = harness(git);
    let error = advance(&h, review_worktree(None, OLD), Some(OLD)).unwrap_err();
    assert!(matches!(error, WtmError::Git(_)), "{error:?}");
    assert_eq!(
        h.git.mutations(),
        vec!["fetch origin pull/4031/head".to_owned()],
        "it checked out after a fetch that did not bring the head"
    );
}
