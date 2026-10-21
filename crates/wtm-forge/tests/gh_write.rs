//! `GhForge`'s write side against recorded `gh` replies and a fake runner.
//!
//! The replies are what `gh` 2.100.0 printed on 2026-10-21 for a throwaway pull request, with the
//! names replaced (`fixtures/README.md`). Three things are protected. That a write is exactly one
//! of the three requests the write side may make, with its text on stdin and never in the argv.
//! That GitHub's refusals are told apart the way the outbox needs them: the one-pending-review 422,
//! a refusal that would recur, and a failure whose outcome is unknown — including GraphQL's silent
//! `thread: null`. And that the reads the outbox reconciles with find a marker where GitHub put it.

#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::sync::Arc;

use pretty_assertions::assert_eq;
use serde_json::Value;
use wtm_core::error::ExecError;
use wtm_core::flow::{FileLines, LineRange, RemoteReviewState, RepoSlug, ReviewEvent};
use wtm_core::ports::exec::Output;
use wtm_core::ports::forge::{Forge, ForgeError, ReviewWriter, SentComment};
use wtm_forge::GhForge;
use wtm_testkit::FakeRunner;

const ROOT: &str = "/x/webapp";
const HEAD: &str = "69cfa420c76c9aebe88f72d305778234c58ebae8";

fn fixture(name: &str) -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name),
    )
    .unwrap()
}

fn harness(responses: Vec<Result<Output, ExecError>>) -> (GhForge, Arc<FakeRunner>) {
    let runner = Arc::new(FakeRunner::scripted(responses));
    (GhForge::new(Arc::clone(&runner) as _), runner)
}

/// What `gh api` prints when GitHub refuses: the reply on stdout, `gh`'s one line on stderr. In the
/// runner's own element type, so it sits in a scripted list beside `FakeRunner::ok`.
#[allow(clippy::unnecessary_wraps)]
fn refused(name: &str) -> Result<Output, ExecError> {
    Ok(Output {
        code: 1,
        stdout: fixture(&format!("{name}.json")),
        stderr: fixture(&format!("{name}.stderr")),
        duration_ms: 1,
    })
}

fn repo() -> RepoSlug {
    RepoSlug::new("github.com", "acme", "webapp")
}

fn comment(line: u32, start_line: Option<u32>, body: &str) -> SentComment {
    SentComment {
        path: "export.py".to_owned(),
        start_line,
        line,
        body: body.to_owned(),
    }
}

fn stdin_of(runner: &FakeRunner, call: usize) -> Value {
    serde_json::from_str(runner.calls()[call].stdin.as_deref().unwrap()).unwrap()
}

/// Whether an argv is one of the write side's three writes or one of its reads.
fn is_allowed(argv: &[String]) -> bool {
    let words: Vec<&str> = argv.iter().map(String::as_str).collect();
    match words.as_slice() {
        [
            "gh",
            "api",
            "--method",
            "POST",
            "--hostname",
            _,
            path,
            "--input",
            "-",
        ] => {
            let parts: Vec<&str> = path.split('/').collect();
            matches!(
                parts.as_slice(),
                ["repos", _, _, "pulls", _, "reviews"]
                    | ["repos", _, _, "pulls", _, "reviews", _, "events"]
            )
        }
        ["gh", "api", "graphql", "--hostname", _, "--input", "-"] => true,
        ["gh", "api", "--method", "GET", rest @ ..] => !rest
            .iter()
            .any(|arg| arg.starts_with("-f") || arg.starts_with("-F")),
        _ => false,
    }
}

#[test]
fn starting_a_pending_review_sends_its_comments_on_stdin_pinned_to_the_commit() {
    let (forge, runner) = harness(vec![FakeRunner::ok(&fixture("review_create_pending.json"))]);
    let review = forge
        .create_pending_review(
            Path::new(ROOT),
            &repo(),
            1,
            HEAD,
            "<!-- wtm:0123456789abcdef -->",
            &[
                comment(
                    20,
                    None,
                    "Drops the last unit.\n\n<!-- wtm:0123456789abcdef c0 -->",
                ),
                comment(13, Some(12), "Two lines."),
            ],
        )
        .unwrap();

    assert_eq!(review.id, 5_605_156_587);
    assert_eq!(review.node_id, "PRR_kwDOVSoqEs8AAAABThfm6w");
    assert_eq!(review.state, RemoteReviewState::Pending);
    assert_eq!(review.author.as_deref(), Some("dana-lee"));

    let argv = &runner.argvs()[0];
    assert_eq!(
        argv,
        &[
            "gh",
            "api",
            "--method",
            "POST",
            "--hostname",
            "github.com",
            "repos/acme/webapp/pulls/1/reviews",
            "--input",
            "-"
        ]
    );
    let sent = stdin_of(&runner, 0);
    assert_eq!(sent["commit_id"], HEAD);
    assert!(sent.get("event").is_none(), "an event would submit it");
    assert_eq!(sent["comments"][0]["side"], "RIGHT");
    assert!(sent["comments"][0].get("start_line").is_none());
    assert_eq!(sent["comments"][1]["start_line"], 12);
    assert_eq!(sent["comments"][1]["start_side"], "RIGHT");
    assert!(
        !argv.iter().any(|arg| arg.contains("Drops the last unit")),
        "comment text belongs on stdin, not in an argv"
    );
}

#[test]
fn a_second_pending_review_is_told_apart_from_every_other_refusal() {
    let (forge, _) = harness(vec![refused("review_pending_exists")]);
    let error = forge
        .create_pending_review(Path::new(ROOT), &repo(), 1, HEAD, "", &[])
        .unwrap_err();
    assert_eq!(
        error,
        ForgeError::PendingReviewExists {
            what: "a pending review on acme/webapp#1".to_owned()
        }
    );
    assert!(!error.outcome_unknown());
}

#[test]
fn a_comment_off_the_diff_is_refused_in_github_s_own_words_and_would_be_refused_again() {
    let (forge, _) = harness(vec![refused("review_line_unresolved")]);
    let error = forge
        .create_pending_review(
            Path::new(ROOT),
            &repo(),
            1,
            HEAD,
            "",
            &[comment(26, None, "x")],
        )
        .unwrap_err();
    match &error {
        ForgeError::Rejected { reason, .. } => assert_eq!(reason, "Line could not be resolved"),
        other => panic!("expected a rejection, got {other:?}"),
    }
    assert!(!error.outcome_unknown());
}

#[test]
fn graphql_s_silent_null_thread_is_a_refusal_and_a_vanished_review_is_not_found() {
    let (forge, runner) = harness(vec![
        FakeRunner::ok(&fixture("review_thread_added.json")),
        FakeRunner::ok(&fixture("review_thread_null.json")),
        refused("review_thread_bad_node"),
    ]);
    let node = "PRR_kwDOVSoqEs8AAAABThfm6w";
    forge
        .add_review_comment(
            Path::new(ROOT),
            &repo(),
            node,
            &comment(13, Some(12), "Two"),
        )
        .unwrap();
    let sent = stdin_of(&runner, 0);
    assert_eq!(sent["variables"]["review"], node);
    assert_eq!(sent["variables"]["startLine"], 12);
    assert_eq!(sent["variables"]["startSide"], "RIGHT");
    assert!(
        sent["query"]
            .as_str()
            .unwrap()
            .contains("addPullRequestReviewThread")
    );

    let outside = forge
        .add_review_comment(Path::new(ROOT), &repo(), node, &comment(26, None, "x"))
        .unwrap_err();
    assert!(
        matches!(&outside, ForgeError::Rejected { reason, .. } if reason.contains("export.py:26")),
        "{outside:?}"
    );
    let gone = forge
        .add_review_comment(
            Path::new(ROOT),
            &repo(),
            "PRR_doesnotexist",
            &comment(20, None, "x"),
        )
        .unwrap_err();
    assert!(matches!(gone, ForgeError::NotFound { .. }), "{gone:?}");
}

#[test]
fn submitting_sends_the_state_and_body_and_reads_back_the_submitted_review() {
    let (forge, runner) = harness(vec![FakeRunner::ok(&fixture("review_submitted.json"))]);
    let review = forge
        .submit_review(
            Path::new(ROOT),
            &repo(),
            1,
            5_605_156_587,
            ReviewEvent::Comment,
            "Two things before merge.\n\n<!-- wtm:fedcba9876543210 -->",
        )
        .unwrap();
    assert_eq!(review.state, RemoteReviewState::Commented);
    assert!(review.body.contains("<!-- wtm:fedcba9876543210 -->"));
    assert_eq!(
        runner.argvs()[0][6],
        "repos/acme/webapp/pulls/1/reviews/5605156587/events"
    );
    let sent = stdin_of(&runner, 0);
    assert_eq!(sent["event"], "COMMENT");
    for (event, name) in [
        (ReviewEvent::RequestChanges, "REQUEST_CHANGES"),
        (ReviewEvent::Approve, "APPROVE"),
    ] {
        let (forge, runner) = harness(vec![FakeRunner::ok(&fixture("review_submitted.json"))]);
        forge
            .submit_review(Path::new(ROOT), &repo(), 1, 1, event, "b")
            .unwrap();
        assert_eq!(stdin_of(&runner, 0)["event"], name);
    }
}

#[test]
fn your_pending_review_is_listed_with_its_body_so_its_marker_can_be_found() {
    let (forge, runner) = harness(vec![FakeRunner::ok(&fixture("review_reviews.json"))]);
    let reviews = forge.reviews(Path::new(ROOT), &repo(), 1).unwrap();
    assert_eq!(
        reviews
            .iter()
            .map(|r| (r.id, r.state, r.body.as_str()))
            .collect::<Vec<_>>(),
        [
            (
                5_605_156_587,
                RemoteReviewState::Pending,
                "<!-- wtm:0123456789abcdef -->"
            ),
            (
                5_604_959_998,
                RemoteReviewState::Commented,
                "Summary of the probe.\n\n<!-- wtm:probe op=2 -->"
            ),
        ]
    );
    assert_eq!(
        runner.argvs()[0],
        [
            "gh",
            "api",
            "--method",
            "GET",
            "--hostname",
            "github.com",
            "--paginate",
            "--slurp",
            "repos/acme/webapp/pulls/1/reviews?per_page=100"
        ]
    );
}

#[test]
fn a_reviews_comments_are_read_with_their_markers() {
    let (forge, _) = harness(vec![FakeRunner::ok(&fixture(
        "review_review_comments.json",
    ))]);
    let comments = forge
        .review_comments(Path::new(ROOT), &repo(), 1, 5_605_156_587)
        .unwrap();
    assert_eq!(
        comments.iter().map(|c| c.body.as_str()).collect::<Vec<_>>(),
        [
            "Drops the last unit.\n\n<!-- wtm:0123456789abcdef c0 -->",
            "Two lines.\n\n<!-- wtm:0123456789abcdef c1 -->"
        ]
    );
}

#[test]
fn the_forge_s_own_diff_of_a_commit_is_read_as_the_lines_a_comment_can_sit_on() {
    let (forge, runner) = harness(vec![FakeRunner::ok(&fixture("review_compare.json"))]);
    let lines = forge
        .changed_lines(Path::new(ROOT), &repo(), "10e9db6", HEAD)
        .unwrap();
    assert_eq!(
        lines.files.get("export.py"),
        Some(&FileLines::Hunks(vec![LineRange { start: 9, end: 23 }]))
    );
    assert_eq!(lines.files.len(), 1);
    assert_eq!(
        runner.argvs()[0][6],
        format!("repos/acme/webapp/compare/10e9db6...{HEAD}")
    );
}

#[test]
fn a_timeout_or_an_unexplained_failure_is_an_unknown_outcome_to_be_read_before_sending_again() {
    let (forge, _) = harness(vec![
        Err(ExecError::Timeout {
            argv: "gh api".to_owned(),
            timeout_ms: 30_000,
        }),
        FakeRunner::failed(1, "gh: Bad Gateway (HTTP 502)"),
    ]);
    let timed_out = forge
        .submit_review(Path::new(ROOT), &repo(), 1, 1, ReviewEvent::Comment, "b")
        .unwrap_err();
    assert!(matches!(timed_out, ForgeError::Timeout { .. }));
    assert!(timed_out.outcome_unknown());
    let gateway = forge
        .submit_review(Path::new(ROOT), &repo(), 1, 1, ReviewEvent::Comment, "b")
        .unwrap_err();
    assert!(matches!(gateway, ForgeError::Failed { .. }), "{gateway:?}");
    assert!(gateway.outcome_unknown());
}

#[test]
fn every_call_the_write_side_makes_is_one_of_its_three_writes_or_a_read() {
    let (forge, runner) = harness(vec![
        FakeRunner::ok(&fixture("review_reviews.json")),
        FakeRunner::ok(&fixture("review_review_comments.json")),
        FakeRunner::ok(&fixture("review_create_pending.json")),
        FakeRunner::ok(&fixture("review_thread_added.json")),
        FakeRunner::ok(&fixture("review_submitted.json")),
        FakeRunner::ok(&fixture("review_compare.json")),
    ]);
    let root = Path::new(ROOT);
    forge.reviews(root, &repo(), 1).unwrap();
    forge.review_comments(root, &repo(), 1, 5).unwrap();
    forge
        .create_pending_review(root, &repo(), 1, HEAD, "b", &[comment(20, None, "c")])
        .unwrap();
    forge
        .add_review_comment(root, &repo(), "PRR_x", &comment(20, None, "c"))
        .unwrap();
    forge
        .submit_review(root, &repo(), 1, 5, ReviewEvent::Comment, "b")
        .unwrap();
    forge.changed_lines(root, &repo(), "a", "b").unwrap();
    for argv in runner.argvs() {
        assert!(is_allowed(&argv), "not an allowed call: {}", argv.join(" "));
    }
    assert_eq!(runner.call_count(), 6);
}
