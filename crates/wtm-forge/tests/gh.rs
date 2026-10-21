//! `GhForge` against recorded `gh` replies and a fake runner.
//!
//! Two things are protected here. That the replies `gh` really prints parse into what a review
//! needs, fork and all, without falling over on a field or a state this build has not seen: the
//! fixtures are `gh` 2.100.0's own output with names replaced (`fixtures/README.md`). And that the
//! adapter only ever *reads*: every argv it hands the runner is checked against the short list of
//! read-only `gh` commands, because "no GitHub writes" is a promise nothing else in the app could
//! notice being broken.

#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::sync::Arc;

use pretty_assertions::assert_eq;
use wtm_core::error::ExecError;
use wtm_core::flow::{ChecksState, Permission, PrState, PrTarget, RepoSlug, Reviewer};
use wtm_core::ports::forge::{Forge, ForgeError};
use wtm_forge::GhForge;
use wtm_testkit::FakeRunner;

fn fixture(name: &str) -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name),
    )
    .unwrap()
}

fn harness(
    responses: Vec<Result<wtm_core::ports::exec::Output, ExecError>>,
) -> (GhForge, Arc<FakeRunner>) {
    let runner = Arc::new(FakeRunner::scripted(responses));
    (GhForge::new(Arc::clone(&runner) as _), runner)
}

fn repo() -> RepoSlug {
    RepoSlug::new("github.com", "acme", "webapp")
}

const ROOT: &str = "/x/webapp";

/// Whether an argv is one of the reads this adapter may run. Anything else is a write, or a
/// command nobody reviewed for being one.
fn is_read_only(argv: &[String]) -> bool {
    let words: Vec<&str> = argv.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["gh", "--version"] | ["gh", "pr", "view" | "list", ..] | ["gh", "repo", "view", ..] => {
            true
        }
        ["gh", "auth", "status", rest @ ..] => {
            !rest.contains(&"--show-token") && !rest.contains(&"-t")
        }
        // `gh api` turns into a POST as soon as a field is given, so the method must be named and
        // no field flag may appear.
        ["gh", "api", rest @ ..] => {
            rest.windows(2).any(|w| w == ["--method", "GET"])
                && !rest.iter().any(|arg| {
                    matches!(
                        *arg,
                        "-f" | "-F" | "--field" | "--raw-field" | "--input" | "-X"
                    )
                })
        }
        _ => false,
    }
}

fn assert_only_reads(runner: &FakeRunner) {
    for argv in runner.argvs() {
        assert!(is_read_only(&argv), "not a read: {}", argv.join(" "));
    }
}

#[test]
fn a_fork_pull_request_reads_as_a_fork_with_its_head_commit_reviewers_and_checks() {
    let (forge, runner) = harness(vec![FakeRunner::ok(&fixture("pr_fork.json"))]);
    let target = PrTarget::parse("https://github.com/acme/webapp/pull/4031/files").unwrap();

    let pr = forge.pull_request(Path::new(ROOT), &target).unwrap();

    assert_eq!(pr.number, 4031);
    assert_eq!(pr.state, PrState::Open);
    assert!(pr.cross_repository);
    assert!(pr.maintainer_can_modify);
    assert_eq!(pr.author.as_deref(), Some("sam-rivera"));
    assert_eq!(pr.base.repo, repo());
    assert_eq!(pr.base.branch, "main");
    assert_eq!(
        pr.head.repo,
        Some(RepoSlug::new("github.com", "sam-rivera", "webapp"))
    );
    assert_eq!(pr.head.branch, "main");
    assert_eq!(
        pr.head.sha.as_str(),
        "1156f977ff6d12d592bc152987b9d9b378f6718d"
    );
    assert_eq!(
        pr.requested_reviewers,
        [
            Reviewer {
                name: "dana-lee".to_owned(),
                team: false
            },
            Reviewer {
                name: "acme/frontend".to_owned(),
                team: true
            },
        ]
    );
    // A success, a skip, one still running, a failure, a passing status and a pending one.
    assert_eq!(
        (
            pr.checks.passed,
            pr.checks.failed,
            pr.checks.pending,
            pr.checks.skipped
        ),
        (2, 1, 2, 1)
    );
    assert_eq!(pr.checks.state(), ChecksState::Failing);

    assert_eq!(
        runner.argvs()[0][..6],
        [
            "gh",
            "pr",
            "view",
            "4031",
            "--repo",
            "github.com/acme/webapp"
        ]
    );
    assert_only_reads(&runner);
}

#[test]
fn a_bare_number_is_left_for_gh_to_resolve_against_the_checkout_it_runs_in() {
    let (forge, runner) = harness(vec![FakeRunner::ok(&fixture("pr_same_repo.json"))]);
    let pr = forge
        .pull_request(Path::new(ROOT), &PrTarget::parse("#35").unwrap())
        .unwrap();

    assert!(!pr.cross_repository);
    assert_eq!(pr.state, PrState::Merged);
    assert_eq!(pr.checks.state(), ChecksState::Passing);
    assert_eq!(
        pr.base.sha.unwrap().as_str(),
        "7a0aedbfb06cdf4daefb06bf89168a4eafd2ceb3"
    );

    let call = &runner.calls()[0];
    assert!(!call.argv.contains(&"--repo".to_owned()), "{:?}", call.argv);
    assert_eq!(call.cwd, Path::new(ROOT));
    assert_eq!(
        call.env.get("GH_PROMPT_DISABLED").map(String::as_str),
        Some("1"),
        "gh must never wait on a prompt nobody can see"
    );
}

#[test]
fn a_reply_missing_most_fields_or_holding_unknown_ones_still_reads() {
    let (forge, _) = harness(vec![FakeRunner::ok(&fixture("pr_sparse.json"))]);
    let pr = forge
        .pull_request(
            Path::new(ROOT),
            &PrTarget::parse("https://git.example.com/acme/webapp/pull/9").unwrap(),
        )
        .unwrap();

    assert_eq!(pr.state, PrState::Unknown);
    assert_eq!(pr.author, None);
    assert_eq!(pr.head.repo, None);
    assert_eq!(pr.base.repo.host, "git.example.com");
    // No word on whether it is a fork, and no head repository: read it as one, which only means
    // fetching `pull/9/head`, a ref every pull request has.
    assert!(pr.cross_repository);
    assert!(!pr.draft);
    assert!(!pr.maintainer_can_modify);
    assert_eq!(
        pr.requested_reviewers,
        [Reviewer {
            name: "ghost-importer".to_owned(),
            team: false
        }]
    );
    assert_eq!(pr.checks.state(), ChecksState::Pending);
}

#[test]
fn a_reply_without_a_head_commit_is_refused_rather_than_reviewed_at_nothing() {
    let (forge, _) = harness(vec![FakeRunner::ok(
        r#"{"number": 9, "url": "https://github.com/acme/webapp/pull/9", "headRefOid": ""}"#,
    )]);
    let error = forge
        .pull_request(Path::new(ROOT), &PrTarget::parse("9").unwrap())
        .unwrap_err();
    assert!(
        matches!(&error, ForgeError::Unreadable { message, .. } if message.contains("head commit")),
        "{error:?}"
    );
}

#[test]
fn the_pull_request_for_a_branch_is_the_repositorys_own_open_one_not_a_forks_namesake() {
    let (forge, runner) = harness(vec![FakeRunner::ok(&fixture("pr_list_head.json"))]);
    let pr = forge
        .pull_request_for_head(Path::new(ROOT), &repo(), "main")
        .unwrap()
        .unwrap();
    assert_eq!(pr.number, 3950);
    assert!(runner.argvs()[0].contains(&"--head".to_owned()));
    assert_only_reads(&runner);

    let (forge, _) = harness(vec![FakeRunner::ok("[]")]);
    assert_eq!(
        forge
            .pull_request_for_head(Path::new(ROOT), &repo(), "never-pushed")
            .unwrap(),
        None
    );
}

#[test]
fn your_permission_on_a_repository_is_read_and_an_unfamiliar_level_is_unknown() {
    let (forge, runner) = harness(vec![
        FakeRunner::ok(&fixture("repo_permission.json")),
        FakeRunner::ok(r#"{"viewerPermission":"SUPREME_LEADER"}"#),
    ]);
    assert_eq!(
        forge.permission(Path::new(ROOT), &repo()).unwrap(),
        Permission::Write
    );
    assert_eq!(
        forge.permission(Path::new(ROOT), &repo()).unwrap(),
        Permission::Unknown
    );
    assert_only_reads(&runner);
}

#[test]
fn a_branch_protected_by_a_ruleset_lists_each_kind_of_rule_once() {
    let (forge, runner) = harness(vec![
        FakeRunner::ok(&fixture("branch.json")),
        FakeRunner::ok(&fixture("branch_rules.json")),
    ]);
    let protection = forge
        .branch_protection(Path::new(ROOT), &repo(), "main")
        .unwrap();
    assert_eq!(protection.protected, Some(true));
    assert_eq!(
        protection.rules,
        [
            "deletion",
            "non_fast_forward",
            "pull_request",
            "required_status_checks"
        ]
    );
    let argvs = runner.argvs();
    assert_eq!(
        argvs[0],
        [
            "gh",
            "api",
            "--method",
            "GET",
            "--hostname",
            "github.com",
            "repos/acme/webapp/branches/main"
        ]
    );
    assert_eq!(argvs[1][6], "repos/acme/webapp/rules/branches/main");
    assert_only_reads(&runner);
}

#[test]
fn a_branch_the_forge_will_not_show_has_unknown_protection_rather_than_failing_the_review() {
    let (forge, runner) = harness(vec![FakeRunner::failed(
        1,
        "gh: Branch not found (HTTP 404)",
    )]);
    let protection = forge
        .branch_protection(Path::new(ROOT), &repo(), "release/4.2")
        .unwrap();
    assert_eq!(protection.protected, None);
    assert!(protection.rules.is_empty());
    assert_eq!(
        runner.call_count(),
        1,
        "no second request for a missing branch"
    );
}

#[test]
fn a_signed_in_gh_new_enough_reports_its_active_account() {
    let (forge, runner) = harness(vec![
        FakeRunner::ok(
            "gh version 2.100.0 (2026-09-03)\nhttps://github.com/cli/cli/releases/tag/v2.100.0\n",
        ),
        FakeRunner::ok(&fixture("auth_status.json")),
    ]);
    let account = forge.check(Path::new(ROOT), "github.com").unwrap();
    assert_eq!(account.login.as_deref(), Some("dana-lee"));
    assert_eq!(account.version.as_deref(), Some("2.100.0"));
    assert_eq!(
        runner.argvs()[1],
        [
            "gh",
            "auth",
            "status",
            "--hostname",
            "github.com",
            "--json",
            "hosts"
        ]
    );
    assert_only_reads(&runner);
}

#[test]
fn a_gh_older_than_json_sign_in_status_is_refused_with_the_version_it_needs() {
    let (forge, runner) = harness(vec![FakeRunner::ok("gh version 2.62.0 (2024-11-14)\n")]);
    let error = forge.check(Path::new(ROOT), "github.com").unwrap_err();
    assert_eq!(
        error,
        ForgeError::TooOld {
            program: "gh".to_owned(),
            found: "2.62.0".to_owned(),
            required: "2.81.0".to_owned(),
            reason: "the first release whose `gh auth status` answers in JSON".to_owned(),
        }
    );
    assert_eq!(runner.call_count(), 1, "nothing is asked of a gh this old");
}

#[test]
fn a_missing_gh_is_reported_as_not_installed_before_anything_runs() {
    let runner = Arc::new(FakeRunner::new().with_programs(&["git"]));
    let forge = GhForge::new(Arc::clone(&runner) as _);
    assert_eq!(
        forge.check(Path::new(ROOT), "github.com").unwrap_err(),
        ForgeError::NotInstalled {
            program: "gh".to_owned()
        }
    );
    assert_eq!(runner.call_count(), 0);
}

#[test]
fn a_rejected_token_or_no_account_at_all_is_signed_out_with_gh_s_reason() {
    for (reply, needle) in [
        ("auth_status_bad_token.json", "401 Unauthorized"),
        ("auth_status_none.json", "No account"),
    ] {
        let (forge, _) = harness(vec![
            FakeRunner::ok("gh version 2.100.0 (2026-09-03)\n"),
            FakeRunner::ok(&fixture(reply)),
        ]);
        match forge.check(Path::new(ROOT), "github.com").unwrap_err() {
            ForgeError::SignedOut { host, message, .. } => {
                assert_eq!(host, "github.com");
                assert!(message.contains(needle), "{reply}: {message}");
            }
            other => panic!("{reply}: expected signed out, got {other:?}"),
        }
    }
}

#[test]
fn gh_s_exit_codes_and_messages_become_errors_that_say_what_to_do() {
    let target = PrTarget::parse("https://github.com/acme/webapp/pull/1").unwrap();
    let cases = [
        (
            FakeRunner::failed(
                4,
                "To get started with GitHub CLI, please run:  gh auth login",
            ),
            "signed_out",
        ),
        (
            FakeRunner::failed(
                1,
                "GraphQL: Could not resolve to a PullRequest with the number of 1. (repository.pullRequest)",
            ),
            "not_found",
        ),
        (
            FakeRunner::failed(1, "error connecting to api.github.com"),
            "failed",
        ),
        (
            Err(ExecError::Timeout {
                argv: "gh pr view".to_owned(),
                timeout_ms: 30_000,
            }),
            "timeout",
        ),
        (FakeRunner::ok("not json"), "unreadable"),
    ];
    for (reply, kind) in cases {
        let (forge, _) = harness(vec![reply]);
        let error = forge.pull_request(Path::new(ROOT), &target).unwrap_err();
        let got = serde_json::to_value(&error).unwrap()["kind"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(got, kind, "{error}");
    }
}
