//! `AcliTracker` against recorded `acli` replies and a fake runner.
//!
//! Two things are protected here. That the replies `acli` really prints parse into what the sprint
//! queue, the board and the issue workspace need, including the paginated listing that prints one
//! JSON document per page and the sign-in check that answers in text: the fixtures are acli
//! 1.3.18's own output with every identifying value replaced (`fixtures/README.md`). And that the
//! adapter reads, and writes in exactly two ways: every argv it hands the runner is checked against
//! the list of read-only `acli` commands, or, for a move, against the one transition of one issue
//! by key, or, for a comment, against the one comment on one issue by key, because "nothing else
//! is written to Jira" is a promise nothing else in the app could notice being broken.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use pretty_assertions::assert_eq;
use wtm_core::error::ExecError;
use wtm_core::flow::{IssueQuery, SPRINT_FIELD, SprintRef, SprintState, StatusCategory};
use wtm_core::ports::exec::Output;
use wtm_core::ports::tracker::{Tracker, TrackerError, TrackerWriter};
use wtm_testkit::FakeRunner;
use wtm_tracker::AcliTracker;

fn fixture(name: &str) -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name),
    )
    .unwrap()
}

fn harness(responses: Vec<Result<Output, ExecError>>) -> (AcliTracker, Arc<FakeRunner>) {
    let runner = Arc::new(FakeRunner::scripted(responses));
    (AcliTracker::new(Arc::clone(&runner) as _), runner)
}

/// A failure printed on both streams, as `acli` prints a missing sprint or board. A `Result`, the
/// element type of the runner's scripted queue, as `FakeRunner::failed` is.
#[allow(clippy::unnecessary_wraps)]
fn failed_both(stdout: &str, stderr: &str) -> Result<Output, ExecError> {
    Ok(Output {
        code: 1,
        stdout: stdout.to_owned(),
        stderr: stderr.to_owned(),
        duration_ms: 1,
    })
}

const ROOT: &str = "/x/webapp";

/// Whether an argv is one of the reads this adapter may run. Anything else is a write, or a command
/// nobody reviewed for being one: `transition`, `comment`, `edit`, `assign`, `create`, `delete`,
/// and `--web`, which opens a browser.
fn is_read_only(argv: &[String]) -> bool {
    let words: Vec<&str> = argv.iter().map(String::as_str).collect();
    let read = matches!(
        words.as_slice(),
        ["acli", "--version"]
            | ["acli", "jira", "auth", "status"]
            | ["acli", "jira", "workitem", "search" | "view", ..]
            | ["acli", "jira", "board", "list-sprints", ..]
            | ["acli", "jira", "sprint", "view" | "list-workitems", ..]
            | ["acli", "jira", "workitem", "comment", "list", ..]
    );
    read && !words.iter().any(|word| *word == "--web" || *word == "-w")
}

/// Whether an argv is the one write this adapter makes: a transition of one issue by its key, by
/// status name, without a prompt. A `--jql` or `--filter` selection, or a list of keys, would move
/// more than one issue.
fn is_one_move(argv: &[String]) -> bool {
    let words: Vec<&str> = argv.iter().map(String::as_str).collect();
    match words.as_slice() {
        [
            "acli",
            "jira",
            "workitem",
            "transition",
            key,
            status,
            "--yes",
        ] => {
            key.strip_prefix("--key=")
                .is_some_and(|key| !key.is_empty() && !key.contains(','))
                && status.starts_with("--status=")
        }
        _ => false,
    }
}

/// Whether an argv is the other write: one comment on one issue by its key, its body a flag value
/// so it can never be read as another flag.
fn is_one_comment(argv: &[String]) -> bool {
    let words: Vec<&str> = argv.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["acli", "jira", "workitem", "comment", "create", key, body] => {
            key.strip_prefix("--key=")
                .is_some_and(|key| !key.is_empty() && !key.contains(','))
                && body.starts_with("--body=")
        }
        _ => false,
    }
}

fn assert_only_reads(runner: &FakeRunner) {
    for argv in runner.argvs() {
        assert!(is_read_only(&argv), "not a read: {argv:?}");
    }
}

#[test]
fn the_check_reads_the_version_past_its_upgrade_notice_and_the_site_from_text() {
    let (tracker, runner) = harness(vec![
        FakeRunner::ok(&fixture("version.txt")),
        FakeRunner::ok(&fixture("auth_status.txt")),
    ]);

    let account = tracker.check(Path::new(ROOT)).unwrap();

    assert_eq!(account.site, "acme.atlassian.net");
    assert_eq!(account.login.as_deref(), Some("you@example.com"));
    assert_eq!(account.version.as_deref(), Some("1.3.18"));
    assert_only_reads(&runner);
}

#[test]
fn an_acli_older_than_the_recorded_release_is_refused_naming_both_versions() {
    let (tracker, runner) = harness(vec![FakeRunner::ok("acli version 1.2.0-stable\n")]);

    match tracker.check(Path::new(ROOT)).unwrap_err() {
        TrackerError::TooOld {
            found, required, ..
        } => {
            assert_eq!(found, "1.2.0");
            assert_eq!(required, "1.3.18");
        }
        other => panic!("expected TooOld, got {other:?}"),
    }
    assert_eq!(
        runner.call_count(),
        1,
        "nothing is asked of an acli too old to trust"
    );
}

#[test]
fn a_signed_out_acli_is_reported_as_signed_out_whatever_its_exit_status() {
    let (tracker, _) = harness(vec![
        FakeRunner::ok(&fixture("version.txt")),
        FakeRunner::failed(1, "✗ Error: not authenticated, run acli jira auth login"),
    ]);
    assert!(matches!(
        tracker.check(Path::new(ROOT)).unwrap_err(),
        TrackerError::SignedOut { .. }
    ));

    let (tracker, _) = harness(vec![
        FakeRunner::ok(&fixture("version.txt")),
        FakeRunner::ok("✗ Not authenticated\n"),
    ]);
    assert!(matches!(
        tracker.check(Path::new(ROOT)).unwrap_err(),
        TrackerError::SignedOut { .. }
    ));
}

#[test]
fn a_missing_acli_is_not_installed_and_nothing_runs() {
    let runner = Arc::new(FakeRunner::new().with_programs(&["gh"]));
    let tracker = AcliTracker::new(Arc::clone(&runner) as _);

    assert!(matches!(
        tracker.check(Path::new(ROOT)).unwrap_err(),
        TrackerError::NotInstalled { .. }
    ));
    assert_eq!(runner.call_count(), 0);
}

#[test]
fn a_search_reads_rows_with_their_status_category_and_runs_the_query_as_one_argument() {
    let (tracker, runner) = harness(vec![FakeRunner::ok(&fixture("search.json"))]);
    let query = IssueQuery {
        query: "assignee = currentUser() ORDER BY rank".to_owned(),
        sprint: None,
        limit: 50,
    };

    let issues = tracker.search(Path::new(ROOT), &query).unwrap();

    let rows: Vec<_> = issues
        .iter()
        .map(|i| (i.key.as_str(), i.status.name.as_str(), i.status.category))
        .collect();
    assert_eq!(
        rows,
        [
            ("ACME-101", "Done", StatusCategory::Done),
            ("ACME-102", "In Progress", StatusCategory::InProgress),
            ("ACME-103", "Done", StatusCategory::Done),
            ("ACME-104", "In Review", StatusCategory::InProgress),
        ]
    );
    assert_eq!(issues[1].assignee.as_deref(), Some("Sam Rivera"));
    assert_eq!(issues[0].labels, ["export", "backend"]);
    assert_eq!(issues[0].kind.as_deref(), Some("Task"));
    assert_eq!(
        runner.argvs()[0],
        [
            "acli",
            "jira",
            "workitem",
            "search",
            "--jql=assignee = currentUser() ORDER BY rank",
            "--fields=key,summary,status,assignee,issuetype,priority,labels",
            "--limit=50",
            "--paginate",
            "--json",
        ]
    );
    assert_only_reads(&runner);
}

#[test]
fn a_query_that_begins_with_a_dash_stays_the_query_and_not_a_flag() {
    let (tracker, runner) = harness(vec![FakeRunner::ok("[]")]);
    let query = IssueQuery {
        query: "-x".to_owned(),
        sprint: None,
        limit: 5,
    };
    tracker.search(Path::new(ROOT), &query).unwrap();
    assert!(runner.argvs()[0].contains(&"--jql=-x".to_owned()));
}

#[test]
fn a_sprint_listing_reads_every_page_printed_one_after_another() {
    let (tracker, runner) = harness(vec![FakeRunner::ok(&fixture("sprint_list_workitems.json"))]);
    let query = IssueQuery {
        query: "assignee = currentUser()".to_owned(),
        sprint: Some(SprintRef {
            board: 3,
            sprint: 1123,
        }),
        limit: 40,
    };

    let issues = tracker.search(Path::new(ROOT), &query).unwrap();

    let keys: Vec<_> = issues.iter().map(|i| i.key.as_str()).collect();
    assert_eq!(
        keys,
        [
            "ACME-105", "ACME-106", "ACME-107", "ACME-135", "ACME-136", "ACME-137"
        ]
    );
    // The listing drops labels whatever `--fields` asks for; a row reads with none.
    assert!(issues.iter().all(|issue| issue.labels.is_empty()));
    let argv = &runner.argvs()[0];
    assert_eq!(&argv[..3], ["acli", "jira", "sprint"]);
    assert!(argv.contains(&"--sprint=1123".to_owned()));
    assert!(argv.contains(&"--board=3".to_owned()));
    assert!(argv.contains(&"--jql=assignee = currentUser()".to_owned()));
    assert_only_reads(&runner);
}

#[test]
fn a_listing_longer_than_the_limit_is_cut_to_it() {
    let (tracker, _) = harness(vec![FakeRunner::ok(&fixture("sprint_list_workitems.json"))]);
    let query = IssueQuery {
        query: "assignee = currentUser()".to_owned(),
        sprint: Some(SprintRef {
            board: 3,
            sprint: 1123,
        }),
        limit: 4,
    };
    assert_eq!(tracker.search(Path::new(ROOT), &query).unwrap().len(), 4);
}

#[test]
fn a_board_lists_its_sprints_with_the_goal_and_an_empty_goal_reads_as_none() {
    let (tracker, runner) = harness(vec![FakeRunner::ok(&fixture("board_list_sprints.json"))]);

    let sprints = tracker
        .sprints(
            Path::new(ROOT),
            3,
            &[SprintState::Active, SprintState::Future],
        )
        .unwrap();

    assert_eq!(sprints.len(), 3);
    assert_eq!(sprints[0].name, "Sprint 41");
    assert_eq!(sprints[0].state, SprintState::Active);
    assert_eq!(sprints[0].goal.as_deref(), Some("Ship the bulk export"));
    assert_eq!(
        sprints[0].ends_at.as_deref(),
        Some("2026-10-23T22:00:00.000Z")
    );
    assert_eq!(sprints[1].state, SprintState::Future);
    assert_eq!(sprints[1].goal, None);
    assert!(runner.argvs()[0].contains(&"--state=active,future".to_owned()));
    assert_only_reads(&runner);
}

#[test]
fn a_sprint_is_read_with_its_goal_and_board() {
    let (tracker, runner) = harness(vec![FakeRunner::ok(&fixture("sprint_view.json"))]);

    let sprint = tracker.sprint(Path::new(ROOT), 1123).unwrap();

    assert_eq!(sprint.id, 1123);
    assert_eq!(sprint.goal.as_deref(), Some("Ship the bulk export"));
    assert_eq!(sprint.board, Some(34));
    assert_eq!(
        runner.argvs()[0],
        ["acli", "jira", "sprint", "view", "--id=1123", "--json"]
    );
}

#[test]
fn an_issue_is_read_whole_with_its_parent_links_description_and_mapped_fields() {
    let (tracker, runner) = harness(vec![FakeRunner::ok(&fixture("workitem_view.json"))]);
    let fields = BTreeMap::from([
        (SPRINT_FIELD.to_owned(), "customfield_10020".to_owned()),
        ("owner".to_owned(), "customfield_10050".to_owned()),
        ("points".to_owned(), "customfield_10099".to_owned()),
    ]);

    let detail = tracker.issue(Path::new(ROOT), "ACME-113", &fields).unwrap();

    assert_eq!(detail.issue.key, "ACME-113");
    assert_eq!(detail.issue.status.name, "In Progress");
    assert_eq!(detail.issue.assignee.as_deref(), Some("Alex Chen"));
    let parent = detail.parent.unwrap();
    assert_eq!(
        (parent.key.as_str(), parent.summary.as_deref()),
        ("ACME-111", Some("Example issue 11"))
    );
    let links: Vec<_> = detail
        .links
        .iter()
        .map(|l| {
            (
                l.relation.as_str(),
                l.key.as_str(),
                l.status.as_ref().map(|s| s.name.as_str()),
            )
        })
        .collect();
    assert_eq!(
        links,
        [
            ("relates to", "OPS-145", Some("Declined")),
            ("relates to", "OPS-146", Some("Declined")),
        ]
    );
    let description = detail.description.unwrap();
    assert!(description.contains("Placeholder text."), "{description}");
    assert!(
        !description.contains('{'),
        "rich text is flattened: {description}"
    );
    assert_eq!(detail.sprints.len(), 1);
    assert_eq!(detail.sprints[0].name, "Sprint 41");
    assert_eq!(detail.sprints[0].board, Some(34));
    let shown: Vec<_> = detail
        .fields
        .iter()
        .map(|f| (f.name.as_str(), f.value.as_deref()))
        .collect();
    // In config order, the sprint field excepted; a field the issue has no value for says so.
    assert_eq!(shown, [("owner", Some("Alex Chen")), ("points", None)]);
    assert_eq!(
        detail.updated.as_deref(),
        Some("2026-10-20T15:28:31.025-0500")
    );
    let argv = &runner.argvs()[0];
    assert_eq!(&argv[..5], ["acli", "jira", "workitem", "view", "ACME-113"]);
    assert!(
        argv.contains(
            &"--fields=key,summary,status,issuetype,priority,assignee,labels,description,\
              issuelinks,parent,updated,customfield_10050,customfield_10099,customfield_10020"
                .to_owned()
        ),
        "{argv:?}"
    );
    assert_only_reads(&runner);
}

#[test]
fn a_key_that_could_be_read_as_a_flag_is_refused_before_anything_runs() {
    let (tracker, runner) = harness(vec![]);
    let err = tracker
        .issue(Path::new(ROOT), "--web", &BTreeMap::new())
        .unwrap_err();
    assert!(matches!(err, TrackerError::NotFound { .. }));
    assert_eq!(runner.call_count(), 0);
}

#[test]
fn a_missing_issue_sprint_or_board_is_not_found_whichever_stream_says_so() {
    let (tracker, _) = harness(vec![FakeRunner::failed(1, &fixture("not_found.stderr"))]);
    assert!(matches!(
        tracker
            .issue(Path::new(ROOT), "ACME-9", &BTreeMap::new())
            .unwrap_err(),
        TrackerError::NotFound { .. }
    ));

    let (tracker, _) = harness(vec![failed_both(
        &fixture("sprint_not_found.stdout"),
        &fixture("sprint_not_found.stderr"),
    )]);
    assert!(matches!(
        tracker.sprint(Path::new(ROOT), 9).unwrap_err(),
        TrackerError::NotFound { .. }
    ));

    let (tracker, _) = harness(vec![failed_both(
        &fixture("board_not_found.stdout"),
        &fixture("board_not_found.stderr"),
    )]);
    assert!(matches!(
        tracker
            .sprints(Path::new(ROOT), 9, &[SprintState::Active])
            .unwrap_err(),
        TrackerError::NotFound { .. }
    ));
}

#[test]
fn a_query_jira_cannot_parse_is_a_bad_query_in_jiras_own_words() {
    let (tracker, _) = harness(vec![FakeRunner::failed(1, &fixture("bad_query.stderr"))]);
    let query = IssueQuery {
        query: "this is not jql".to_owned(),
        sprint: None,
        limit: 5,
    };
    match tracker.search(Path::new(ROOT), &query).unwrap_err() {
        TrackerError::BadQuery { message } => {
            assert!(
                message.starts_with("failed to parse JQL query"),
                "{message}"
            );
            assert!(
                !message.contains('✗'),
                "the decoration is dropped: {message}"
            );
        }
        other => panic!("expected BadQuery, got {other:?}"),
    }
}

#[test]
fn a_field_acli_will_not_return_is_unsupported_rather_than_a_failure() {
    let (tracker, _) = harness(vec![FakeRunner::failed(
        1,
        &fixture("field_not_allowed.stderr"),
    )]);
    let query = IssueQuery {
        query: "project = ACME".to_owned(),
        sprint: None,
        limit: 5,
    };
    assert!(matches!(
        tracker.search(Path::new(ROOT), &query).unwrap_err(),
        TrackerError::Unsupported { .. }
    ));
}

#[test]
fn listing_transitions_is_unsupported_and_runs_nothing() {
    let (tracker, runner) = harness(vec![]);
    assert!(matches!(
        tracker.transitions(Path::new(ROOT), "ACME-1").unwrap_err(),
        TrackerError::Unsupported { .. }
    ));
    assert_eq!(runner.call_count(), 0);
}

#[test]
fn a_reply_that_is_not_json_is_unreadable_and_says_so() {
    let (tracker, _) = harness(vec![FakeRunner::ok("Something went sideways\n")]);
    let query = IssueQuery {
        query: "project = ACME".to_owned(),
        sprint: None,
        limit: 5,
    };
    assert!(matches!(
        tracker.search(Path::new(ROOT), &query).unwrap_err(),
        TrackerError::Unreadable { .. }
    ));
}

#[test]
fn a_hung_acli_times_out_naming_the_command() {
    let (tracker, _) = harness(vec![Err(ExecError::Timeout {
        argv: "acli jira sprint view --id=1 --json".to_owned(),
        timeout_ms: 60_000,
    })]);
    match tracker.sprint(Path::new(ROOT), 1).unwrap_err() {
        TrackerError::Timeout { argv, timeout_ms } => {
            assert!(argv.starts_with("acli jira sprint view"), "{argv}");
            assert_eq!(timeout_ms, 60_000);
        }
        other => panic!("expected Timeout, got {other:?}"),
    }
}

#[test]
fn the_read_only_list_refuses_every_acli_write() {
    let argv = |words: &[&str]| words.iter().map(|w| (*w).to_owned()).collect::<Vec<_>>();
    for write in [
        argv(&[
            "acli",
            "jira",
            "workitem",
            "transition",
            "--key=ACME-1",
            "--status=Done",
        ]),
        argv(&[
            "acli",
            "jira",
            "workitem",
            "comment",
            "create",
            "--key=ACME-1",
        ]),
        argv(&["acli", "jira", "workitem", "edit", "--key=ACME-1"]),
        argv(&["acli", "jira", "workitem", "view", "ACME-1", "--web"]),
        argv(&["acli", "jira", "sprint", "update", "--id=1"]),
        argv(&["acli", "jira", "auth", "logout"]),
    ] {
        assert!(!is_read_only(&write), "{write:?} must not pass as a read");
    }
}

#[test]
fn a_move_is_one_transition_of_one_issue_by_key_and_status_without_a_prompt() {
    let (tracker, runner) = harness(vec![FakeRunner::ok("✓ Work item ACME-12 transitioned\n")]);

    tracker
        .move_to(Path::new(ROOT), "ACME-12", "In Progress")
        .unwrap();

    let argvs = runner.argvs();
    assert_eq!(argvs.len(), 1);
    assert!(is_one_move(&argvs[0]), "{:?}", argvs[0]);
    assert_eq!(argvs[0][4], "--key=ACME-12");
    assert_eq!(argvs[0][5], "--status=In Progress");
    assert!(!is_read_only(&argvs[0]), "a move is not a read");
}

#[test]
fn a_key_list_or_a_key_read_as_a_flag_moves_nothing_and_runs_nothing() {
    let (tracker, runner) = harness(Vec::new());
    for key in ["ACME-1,ACME-2", "--jql=project=ACME", "acme-1", ""] {
        assert!(
            matches!(
                tracker.move_to(Path::new(ROOT), key, "Done"),
                Err(TrackerError::NotFound { .. })
            ),
            "{key}"
        );
    }
    assert!(matches!(
        tracker.move_to(Path::new(ROOT), "ACME-1", "  "),
        Err(TrackerError::Unsupported { .. })
    ));
    assert_eq!(runner.call_count(), 0);
}

#[test]
fn a_move_jira_refuses_is_a_failure_in_the_clis_own_words() {
    let (tracker, _) = harness(vec![FakeRunner::failed(
        1,
        "✗ Error: no transition to status 'Done' is available for ACME-12",
    )]);
    match tracker
        .move_to(Path::new(ROOT), "ACME-12", "Done")
        .unwrap_err()
    {
        TrackerError::Failed { message, argv } => {
            assert!(message.contains("no transition"), "{message}");
            assert!(argv.contains("transition"), "{argv}");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

#[test]
fn the_one_move_shape_admits_no_selection_list_or_other_write() {
    let argv = |words: &[&str]| words.iter().map(|w| (*w).to_owned()).collect::<Vec<_>>();
    for not_one in [
        argv(&[
            "acli",
            "jira",
            "workitem",
            "transition",
            "--key=ACME-1,ACME-2",
            "--status=Done",
            "--yes",
        ]),
        argv(&[
            "acli",
            "jira",
            "workitem",
            "transition",
            "--jql=project = ACME",
            "--status=Done",
            "--yes",
        ]),
        argv(&[
            "acli",
            "jira",
            "workitem",
            "transition",
            "--filter=10001",
            "--status=Done",
            "--yes",
        ]),
        argv(&[
            "acli",
            "jira",
            "workitem",
            "transition",
            "--key=ACME-1",
            "--status=Done",
        ]),
        argv(&[
            "acli",
            "jira",
            "workitem",
            "comment",
            "create",
            "--key=ACME-1",
        ]),
    ] {
        assert!(
            !is_one_move(&not_one),
            "{not_one:?} must not pass as one move"
        );
    }
}

#[test]
fn an_issues_comments_are_read_across_pages_with_their_text_as_acli_flattens_it() {
    let (tracker, runner) = harness(vec![FakeRunner::ok(&fixture("comment_list.json"))]);

    let comments = tracker.comments(Path::new(ROOT), "ACME-12").unwrap();

    assert_eq!(
        comments
            .iter()
            .map(|c| (c.id.as_str(), c.author.as_deref()))
            .collect::<Vec<_>>(),
        [
            ("90011", Some("Robin Quill")),
            ("90017", Some("Avery Stone")),
            ("90102", Some("Robin Quill")),
        ]
    );
    // A mention comes back without its name, and a typographic quote as it was.
    assert!(comments[1].body.starts_with(", I\u{2019}ll"));
    assert_only_reads(&runner);
    assert_eq!(
        runner.argvs()[0][5..],
        ["--key=ACME-12", "--paginate", "--json"]
    );
}

#[test]
fn a_comment_is_one_create_on_one_issue_with_the_body_as_a_flag_value() {
    let (tracker, runner) = harness(vec![FakeRunner::ok("✓ Comment added to ACME-12\n")]);

    tracker
        .comment(
            Path::new(ROOT),
            "ACME-12",
            "--not a flag\nQuestion: CSV or XLSX?\nAnswer: CSV.",
        )
        .unwrap();

    let argvs = runner.argvs();
    assert_eq!(argvs.len(), 1);
    assert!(is_one_comment(&argvs[0]), "{:?}", argvs[0]);
    assert_eq!(
        argvs[0][6],
        "--body=--not a flag\nQuestion: CSV or XLSX?\nAnswer: CSV."
    );
    assert!(!is_read_only(&argvs[0]), "a comment is not a read");
    assert!(!is_one_move(&argvs[0]), "nor a move");
}

#[test]
fn a_comment_on_a_key_list_or_a_body_acli_would_read_as_a_document_runs_nothing() {
    let (tracker, runner) = harness(Vec::new());
    for key in ["ACME-1,ACME-2", "--jql=project=ACME", ""] {
        assert!(
            matches!(
                tracker.comment(Path::new(ROOT), key, "hello"),
                Err(TrackerError::NotFound { .. })
            ),
            "{key}"
        );
    }
    for body in ["  ", "{\"type\":\"doc\"}"] {
        assert!(matches!(
            tracker.comment(Path::new(ROOT), "ACME-1", body),
            Err(TrackerError::Unsupported { .. })
        ));
    }
    assert_eq!(runner.call_count(), 0);
}
