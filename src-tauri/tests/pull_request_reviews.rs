//! A teammate's pull request reviewed end to end: Run Review on its review worktree, a push by the
//! author, Re-review changes, and keeping the result private.
//!
//! What is real: the app, `GhForge` parsing `gh`'s own JSON, git on two repositories, the snapshot,
//! the SQLite store and the run tools. The forge and the pull request are `common::pull_requests`',
//! and the agents are `common::agents`' scripts.
//!
//! Each test is one thing a reviewer would notice if it broke: the review compared with the wrong
//! base, a push the review never notices, a re-review that reruns everything or moves a worktree
//! with your edits in it, the earlier head's results gone, or a "private" outcome that reached
//! GitHub. Every `gh` call any test makes is checked to be a read.

#![allow(clippy::unwrap_used, clippy::too_many_lines)]

use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::json;
use wtm_app_lib::flow::findings::Rerun;
use wtm_app_lib::flow::pull_request;
use wtm_app_lib::reviews;
use wtm_core::flow::{EarlierFate, FlowKind, OutcomeKind, PushToBranch};

mod common;
use common::agents::Behaviour;
use common::pull_requests::{Harness, finding_at, reviewers, short};

// ═══════════════════════════════ the tests ═══════════════════════════════

#[test]
fn run_review_on_a_forks_pull_request_compares_it_with_the_pull_requests_base_and_says_whose_it_is()
{
    let h = Harness::new(reviewers());
    let flow = h.start();
    let view = h.consolidated(flow);

    assert_eq!(view.flow.state.kind, FlowKind::ReviewPr);
    assert_eq!(view.review.base, "origin/main");
    let snapshot = view.snapshot.as_ref().unwrap();
    assert_eq!(snapshot.head_sha, h.first);
    assert_eq!(
        snapshot.base_sha,
        h.upstream.git(&["rev-parse", "main"]).trim(),
        "measured from where the pull request left its base"
    );
    let brief = &h.briefs("correctness")[0];
    assert!(brief.contains("  - src/export.rs"), "{brief}");
    assert!(
        !brief.contains("CHANGELOG.md"),
        "a commit on the base was reviewed as part of the pull request: {brief}"
    );
    assert!(
        brief.contains("Pull request #4031") && brief.contains("by @sam-rivera"),
        "{brief}"
    );
    assert!(brief.contains("from a fork"), "{brief}");

    let pr = view.pull_request.as_ref().unwrap();
    assert_eq!(pr.number, 4031);
    assert_eq!(pr.reviewed_head.as_deref(), Some(h.first.as_str()));
    assert_eq!(pr.moved_to, None);
    assert_eq!(
        pr.push_to_branch,
        Some(PushToBranch::ForkWithoutMaintainerEdits),
        "a fork whose author did not allow maintainer edits takes no push, write access or not"
    );
    assert_eq!(view.findings.len(), 2);
    h.gh.assert_only_reads();
}

#[test]
fn a_push_by_the_author_makes_the_review_stale_on_the_next_read_and_rerun_affected_is_refused() {
    let h = Harness::new(reviewers());
    let flow = h.start();
    h.consolidated(flow);
    let second = h.push(
        &h.first,
        &[("src/export.rs", "fn export() { for i in 0..n {} }\n")],
    );

    // Before the window reads the pull request again, nothing has said it moved.
    let before = h.engine.staleness(flow).unwrap();
    assert_eq!(before.pull_request_head, None);
    assert!(!before.changed);

    h.focus();
    let stale = h.engine.staleness(flow).unwrap();
    assert_eq!(stale.pull_request_head.as_deref(), Some(second.as_str()));
    assert!(
        !stale.pull_request_head_here,
        "the new commits have not been fetched"
    );
    assert!(stale.changed);
    assert!(
        !stale.complete,
        "what changed cannot be known before fetching"
    );
    assert_eq!(
        stale.stale_findings.len(),
        2,
        "with the new commits unknown, every finding is stale"
    );
    let view = h.view(flow);
    assert_eq!(
        view.pull_request.as_ref().unwrap().moved_to.as_deref(),
        Some(second.as_str())
    );

    let refused = h.engine.rerun(flow, Rerun::Affected).unwrap_err();
    assert!(
        refused.message.contains("Re-review changes"),
        "{}",
        refused.message
    );
    assert_eq!(
        h.head_of_review_worktree(),
        h.first,
        "a refused rerun moved the worktree"
    );
    h.gh.assert_only_reads();
}

#[test]
fn re_review_changes_moves_the_worktree_to_the_new_head_and_reruns_only_the_role_it_affects() {
    let h = Harness::new(reviewers());
    let flow = h.start();
    let first_round = h.consolidated(flow);
    let earlier_ids: Vec<_> = first_round.findings.iter().map(|f| f.finding.id).collect();
    let second = h.push(
        &h.first,
        &[("src/export.rs", "fn export() { for i in 0..n {} }\n")],
    );
    h.focus();

    let rereviewed =
        pull_request::rereview(&h.app, &h.engine, &h.project, flow, Some(&second)).unwrap();
    assert_eq!(rereviewed.roles, vec!["correctness".to_owned()]);
    let view = h.consolidated(flow);

    assert_eq!(h.head_of_review_worktree(), second);
    assert_eq!(
        h.clone
            .git_in(&h.worktree, &["rev-parse", "--abbrev-ref", "HEAD"])
            .trim(),
        "HEAD",
        "still detached"
    );
    assert_eq!(h.briefs("correctness").len(), 2);
    assert_eq!(
        h.briefs("docs").len(),
        1,
        "docs/ did not change, so docs did not run again"
    );

    // The rerun role is asked to verify what it said before, though it takes no `findings`.
    let again = &h.briefs("correctness")[1];
    assert!(
        again.contains(&format!(
            "The author pushed new commits since you reviewed this pull request at {}",
            short(&h.first)
        )),
        "{again}"
    );
    assert!(
        again.contains("] Off by one in the export loop (src/export.rs:1)")
            && again.contains("`repeats` set to its id"),
        "{again}"
    );

    let pr = view.pull_request.as_ref().unwrap();
    assert_eq!(pr.reviewed_head.as_deref(), Some(second.as_str()));
    assert_eq!(pr.moved_to, None);
    let titles: Vec<_> = view
        .findings
        .iter()
        .map(|f| f.finding.title.clone())
        .collect();
    assert_eq!(titles, vec!["The guide names the old flag".to_owned()]);

    // The first head's results are kept, with what became of each.
    assert_eq!(pr.earlier.len(), 1);
    let earlier = &pr.earlier[0];
    assert_eq!(earlier.head.head.head_sha, h.first);
    assert_eq!(earlier.head.head.next_head, second);
    assert_eq!(
        earlier
            .findings
            .iter()
            .map(|f| f.record.finding.id)
            .collect::<Vec<_>>(),
        earlier_ids
    );
    let fate = |title: &str| {
        earlier
            .findings
            .iter()
            .find(|f| f.record.finding.title == title)
            .unwrap()
            .fate
    };
    assert_eq!(
        fate("Off by one in the export loop"),
        EarlierFate::NotReportedAgain
    );
    assert_eq!(fate("The guide names the old flag"), EarlierFate::Unchanged);

    // Nothing left to follow: the review is at the pull request's head.
    let current = h.engine.staleness(flow).unwrap();
    assert_eq!(current.pull_request_head, None);
    assert!(!current.changed);
    h.gh.assert_only_reads();
}

#[test]
fn re_review_changes_refuses_a_head_that_moved_again_since_it_was_shown_and_moves_nothing() {
    let h = Harness::new(reviewers());
    let flow = h.start();
    h.consolidated(flow);
    let second = h.push(&h.first, &[("src/export.rs", "fn export() {}\n")]);
    h.focus();
    let third = h.push(&second, &[("src/export.rs", "fn export() { todo!() }\n")]);

    // The window showed the second head; the forge now says the third.
    let refused =
        pull_request::rereview(&h.app, &h.engine, &h.project, flow, Some(&second)).unwrap_err();
    assert!(
        refused.message.contains("moved again"),
        "{}",
        refused.message
    );
    assert_eq!(h.head_of_review_worktree(), h.first);
    let pr = h.view(flow).pull_request.unwrap();
    assert!(
        pr.earlier.is_empty(),
        "a refused re-review recorded a head as left"
    );
    assert_eq!(pr.moved_to.as_deref(), Some(third.as_str()));
}

#[test]
fn uncommitted_changes_in_the_review_worktree_stop_a_re_review_before_anything_moves() {
    let h = Harness::new(reviewers());
    let flow = h.start();
    h.consolidated(flow);
    let second = h.push(&h.first, &[("src/export.rs", "fn export() {}\n")]);
    h.focus();
    std::fs::write(h.worktree.join("src/export.rs"), "// trying something\n").unwrap();

    let refused =
        pull_request::rereview(&h.app, &h.engine, &h.project, flow, Some(&second)).unwrap_err();
    assert!(
        refused.message.contains("not committed"),
        "{}",
        refused.message
    );
    assert_eq!(h.head_of_review_worktree(), h.first);
    assert_eq!(
        std::fs::read_to_string(h.worktree.join("src/export.rs")).unwrap(),
        "// trying something\n",
        "your edit was replaced"
    );
    assert!(h.view(flow).pull_request.unwrap().earlier.is_empty());
    assert_eq!(h.briefs("correctness").len(), 1, "a role ran again");
}

#[test]
fn keeping_a_review_private_records_notes_at_the_reviewed_head_and_asks_the_forge_nothing() {
    let h = Harness::new(reviewers());
    let flow = h.start();
    let view = h.consolidated(flow);
    let calls = h.gh.calls().len();

    let kept = h
        .engine
        .keep_private(
            flow,
            "  Ask Sam whether the loop bound is meant to include n. ",
        )
        .unwrap();

    assert_eq!(kept.decision.kind, OutcomeKind::Private);
    assert_eq!(kept.decision.head_sha, h.first);
    assert_eq!(
        kept.decision.notes,
        "Ask Sam whether the loop bound is meant to include n."
    );
    assert_eq!(
        kept.decision.findings,
        view.findings
            .iter()
            .map(|f| f.finding.id)
            .collect::<Vec<_>>()
    );
    let outcomes = h.view(flow).pull_request.unwrap().outcomes;
    assert_eq!(outcomes.last(), Some(&kept));
    assert_eq!(h.gh.calls().len(), calls, "keeping notes private called gh");
}

#[test]
fn im_reviewing_this_is_kept_in_wtm_and_asks_the_forge_nothing() {
    let h = Harness::new(reviewers());
    let calls = h.gh.calls().len();

    let on = reviews::set_reviewing(&h.app, &h.project, h.item, true).unwrap();
    assert!(on.reviewing_since.is_some());
    let queued = reviews::queue(&h.app, &h.project).unwrap();
    assert_eq!(queued.items[0].reviewing_since, on.reviewing_since);
    let off = reviews::set_reviewing(&h.app, &h.project, h.item, false).unwrap();
    assert_eq!(off.reviewing_since, None);

    assert_eq!(h.gh.calls().len(), calls, "the flag reached gh");
}

#[test]
fn a_finding_the_rerun_role_names_in_repeats_is_still_reported_and_the_one_it_drops_is_not() {
    // The label the brief gives the second report, known once the first round has run.
    let label: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let named = Arc::clone(&label);
    let h = Harness::new(Box::new(move |role, attempt| match (role, attempt) {
        ("correctness", 1) => Behaviour::Submit(vec![json!({ "findings": [
            { "title": "Off by one in the export loop", "severity": "notable",
              "class": "suggestion", "body": "x", "path": "src/export.rs", "line": 1 },
            { "title": "Names are not escaped", "severity": "notable",
              "class": "suggestion", "body": "x", "path": "src/export.rs", "line": 5 },
        ]})]),
        // Run again: the escaping problem is still there, now lower down, and says so.
        ("correctness", 2) => Behaviour::Submit(vec![json!({ "findings": [
            { "title": "Names are still not escaped", "severity": "notable",
              "class": "suggestion", "body": "x", "path": "src/export.rs", "line": 9,
              "repeats": named.lock().clone() },
        ]})]),
        // And again, after a push that fixed it.
        ("correctness", _) => Behaviour::Submit(vec![json!({ "findings": [] })]),
        _ => Behaviour::Submit(vec![finding_at(
            "The guide names the old flag",
            "docs/guide.md",
        )]),
    }));
    let flow = h.start();
    let first_round = h.consolidated(flow);
    let step = first_round
        .steps
        .iter()
        .find(|s| s.role == "correctness")
        .unwrap()
        .id;
    let invocation = first_round
        .invocations
        .iter()
        .find(|i| i.step == step)
        .unwrap()
        .id;
    *label.lock() = format!("E{}.1", invocation.0);

    let second = h.push(
        &h.first,
        &[("src/export.rs", "fn export() { for i in 0..n {} }\n")],
    );
    h.focus();
    pull_request::rereview(&h.app, &h.engine, &h.project, flow, Some(&second)).unwrap();
    let view = h.consolidated(flow);

    let brief = &h.briefs("correctness")[1];
    assert!(
        brief.contains(&format!(
            "- [E{}.1] Names are not escaped (src/export.rs:5)",
            invocation.0
        )),
        "{brief}"
    );
    let earlier = &view.pull_request.as_ref().unwrap().earlier[0];
    let fate = |title: &str| {
        earlier
            .findings
            .iter()
            .find(|f| f.record.finding.title == title)
            .unwrap()
            .fate
    };
    assert_eq!(fate("Names are not escaped"), EarlierFate::StillReported);
    assert_eq!(
        fate("Off by one in the export loop"),
        EarlierFate::NotReportedAgain
    );
    assert_eq!(fate("The guide names the old flag"), EarlierFate::Unchanged);

    // A third head that fixes it: the second head's report is not reported again, and the first
    // head still says what the second round found, not what the third did.
    let third = h.push(&second, &[("src/export.rs", "fn export() {}\n")]);
    h.focus();
    pull_request::rereview(&h.app, &h.engine, &h.project, flow, Some(&third)).unwrap();
    let view = h.consolidated(flow);
    let heads = &view.pull_request.as_ref().unwrap().earlier;
    assert_eq!(heads.len(), 2);
    let fate_at = |head: usize, title: &str| {
        heads[head]
            .findings
            .iter()
            .find(|f| f.record.finding.title == title)
            .unwrap()
            .fate
    };
    assert_eq!(
        fate_at(0, "Names are not escaped"),
        EarlierFate::StillReported
    );
    assert_eq!(
        fate_at(1, "Names are still not escaped"),
        EarlierFate::NotReportedAgain
    );
}
