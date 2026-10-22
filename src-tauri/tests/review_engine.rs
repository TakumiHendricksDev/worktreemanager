//! The review engine, driven by fake agents against a real repository and a real database.
//!
//! What is real here: the app, its git, a `GitFixture` worktree with committed and uncommitted
//! changes, the snapshot and its detached checkout, the SQLite store, the run tokens and the run
//! tools' own parsing — a fake agent calls `flow_tools::answer_with` with the token its session was
//! issued, so a schema refusal reaches it exactly as it would over the socket. What is fake is the
//! session: [`FakeHost`] stands in for `flow_session::open`, and each role's behaviour is a script.
//! The clock is a `FakeClock`, so a timeout or a usage-limit reset happens when a test moves time,
//! not after a test has waited for it.
//!
//! Each test is one property a person would notice if it broke: roles of a stage running together,
//! a retry after a timeout, a malformed submission bounced back to the agent, a limit pausing the
//! run until its reset, a quit leaving an honest Interrupted record. The real CLIs have their own
//! test, `review_real_cli.rs`.

#![allow(clippy::unwrap_used, clippy::too_many_lines)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use serde_json::{Value, json};
use wtm_app_lib::app::App;
use wtm_app_lib::flow::findings::Rerun;
use wtm_app_lib::flow::host::RoleHost;
use wtm_app_lib::flow::{Engine, FlowView, Notice, StartReview, Tuning};
use wtm_config::AppPaths;
use wtm_core::flow::{
    CheckStatus, FlowId, Hold, InboxItem, InboxKind, InboxState, InboxTarget, InterruptCause,
    InvocationStatus, NewRule, Phase, RoleProvider, RuleDraft, RuleKind, RuleScope, SkillProblem,
    StepStatus, Submission,
};
use wtm_core::model::ProjectId;
use wtm_core::ports::clock::Clock;
use wtm_core::ports::{InboxStore, RuleStore};
use wtm_testkit::{FakeClock, GitFixture};

mod common;
use common::agents::{Behaviour, FakeHost, Heard, Script};

// ═══════════════════════════════ the harness ═══════════════════════════════

const ROLES: &str = r#"
[defaults.workflow.role.conventions]
objective = "Check the change against the repository's conventions"
provider = "claude"
inputs = ["diff", "instructions"]
timeout_ms = 60000
retries = 1

[defaults.workflow.role.correctness]
objective = "Find bugs the change introduces"
provider = "codex"
inputs = ["diff"]
timeout_ms = 60000
retries = 1

[defaults.workflow.role.summary]
objective = "Summarise what the first stage found"
provider = "claude"
inputs = ["diff"]
timeout_ms = 60000

[defaults.workflow.role.fragile]
objective = "A role with no retries and no fallback"
provider = "codex"
inputs = ["diff"]
timeout_ms = 60000

[defaults.workflow.role.skilled]
objective = "A role that needs a skill"
provider = "codex"
skills = ["/team-review"]
inputs = ["diff"]

[defaults.workflow.role.skilled-claude]
objective = "A Claude role that needs a skill"
provider = "claude"
skills = ["team-review"]
inputs = ["diff"]

[defaults.workflow.role.limited]
objective = "Find bugs, on Codex, falling back to Claude"
provider = "codex"
inputs = ["diff"]
fallback = "limited-on-claude"

[defaults.workflow.role.limited-on-claude]
objective = "Find bugs, on Claude"
provider = "claude"
inputs = ["diff"]

[[defaults.workflow.recipe.standard.stage]]
roles = ["conventions", "correctness"]

[[defaults.workflow.recipe.standard.stage]]
roles = ["summary"]

[defaults.workflow.recipe.one-at-a-time]
concurrency = 1

[[defaults.workflow.recipe.one-at-a-time.stage]]
roles = ["conventions", "correctness"]

[[defaults.workflow.recipe.pair.stage]]
roles = ["conventions", "correctness"]

[[defaults.workflow.recipe.fragile.stage]]
roles = ["conventions", "fragile"]

[[defaults.workflow.recipe.skills.stage]]
roles = ["skilled", "skilled-claude"]

[[defaults.workflow.recipe.fallback.stage]]
roles = ["limited"]

[defaults.workflow.role.export-only]
objective = "Review the export"
provider = "codex"
paths = ["src/export.rs"]
inputs = ["diff", "findings"]
timeout_ms = 60000

[defaults.workflow.role.draft-only]
objective = "Review the draft"
provider = "claude"
paths = ["src/draft.rs"]
inputs = ["diff", "rules", "instructions"]
timeout_ms = 60000

[[defaults.workflow.recipe.scoped.stage]]
roles = ["export-only", "draft-only"]

[defaults.workflow.check.passes]
run = ["true"]

[defaults.workflow.check.fails]
label = "Unit tests"
run = ["sh", "-c", "echo '2 of 3 tests failed'; exit 1"]

[defaults.workflow.check.e2e]
run = ["sh", "-c", "touch ran-e2e"]
needs = ["web"]

# What `needs` names has to be declared. Never started here: this engine has no supervisor, so the
# check is blocked (`runtime_checks.rs` gives it one).
[[defaults.environment.service]]
id = "web"
start = ["sh", "-c", "exit 0"]
health = { command = ["true"] }

[defaults.workflow.role.checker]
objective = "Run the checks"
provider = "codex"
checks = ["passes", "fails", "e2e"]
inputs = ["diff"]
timeout_ms = 60000

[[defaults.workflow.recipe.checks.stage]]
roles = ["checker"]
"#;

struct Harness {
    app: Arc<App>,
    host: Arc<FakeHost>,
    clock: Arc<FakeClock>,
    heard: Arc<Heard>,
    engine: Arc<Engine>,
    project: String,
    worktree: String,
    worktree_path: PathBuf,
    fixture: GitFixture,
    /// The app's config and data directories; a test that relaunches opens a second app on them.
    data: tempfile::TempDir,
}

fn finding(title: &str) -> Value {
    json!({ "findings": [{
        "title": title,
        "severity": "notable",
        "class": "suggestion",
        "body": "Worth a look.",
        "path": "src/export.rs",
        "line": 3,
    }]})
}

fn finding_at(title: &str, path: &str, line: u32) -> Value {
    json!({ "findings": [{
        "title": title,
        "severity": "notable",
        "class": "suggestion",
        "body": "Worth a look.",
        "path": path,
        "line": line,
    }]})
}

fn harness(script: Script) -> Harness {
    let fixture = GitFixture::new();
    // Under the target directory rather than the system temp directory, as `flow_session.rs`
    // explains: nothing here runs a canary, but the paths are the same shape the app's are.
    let data = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    std::fs::write(data.path().join("config.toml"), ROLES).unwrap();
    let app = Arc::new(App::with_paths(AppPaths::rooted(data.path())).unwrap());
    let root = app.register(fixture.root()).unwrap();
    let project = ProjectId::from_root(&root).to_string();
    let worktree_path = fixture.add_worktree("acme-wt", "fix/export");
    fixture.git_in(&worktree_path, &["config", "commit.gpgsign", "false"]);
    std::fs::create_dir_all(worktree_path.join("src")).unwrap();
    std::fs::write(worktree_path.join("src/export.rs"), "fn export() {}\n").unwrap();
    fixture.git_in(&worktree_path, &["add", "."]);
    fixture.git_in(&worktree_path, &["commit", "-m", "Add export"]);
    // Uncommitted, and untracked: a review of your own work reads both.
    std::fs::write(worktree_path.join("src/draft.rs"), "// not yet committed\n").unwrap();
    let listed = app.git.list_worktrees(&root).unwrap();
    let worktree = listed
        .iter()
        .find(|w| !w.is_main)
        .unwrap()
        .id
        .as_str()
        .to_owned();

    let host = FakeHost::new(Arc::clone(&app), script);
    let clock = Arc::new(FakeClock::new());
    let heard = Arc::new(Heard::default());
    let engine = engine_for(&app, &host, &clock, &heard);
    Harness {
        app,
        host,
        clock,
        heard,
        engine,
        project,
        worktree,
        worktree_path,
        fixture,
        data,
    }
}

fn engine_for(
    app: &Arc<App>,
    host: &Arc<FakeHost>,
    clock: &Arc<FakeClock>,
    heard: &Arc<Heard>,
) -> Arc<Engine> {
    let engine = Engine::new(
        Arc::clone(app),
        Arc::clone(host) as Arc<dyn RoleHost>,
        Arc::clone(clock) as Arc<dyn Clock>,
        Arc::clone(heard) as Arc<dyn Notice>,
        Tuning {
            tick_ms: 5,
            skills_wait_ms: 30_000,
            settle_ms: 30_000,
            nudges: 1,
        },
    );
    *host.engine.lock() = Arc::downgrade(&engine);
    engine
}

impl Harness {
    fn start(&self, recipe: &str) -> FlowId {
        self.engine
            .start(&StartReview {
                project: self.project.clone(),
                worktree: self.worktree.clone(),
                recipe: recipe.to_owned(),
                base: None,
            })
            .unwrap()
    }

    fn view(&self, flow: FlowId) -> FlowView {
        self.engine.view(flow).unwrap()
    }

    /// Commit a change to `path` in the reviewed worktree, as you would between two reviews.
    fn commit(&self, path: &str, contents: &str) {
        std::fs::write(self.worktree_path.join(path), contents).unwrap();
        self.fixture.git_in(&self.worktree_path, &["add", "."]);
        self.fixture.git_in(
            &self.worktree_path,
            &["commit", "-m", "A change after the review"],
        );
    }

    /// The briefs a role was opened with, oldest first.
    fn briefs(&self, role: &str) -> Vec<String> {
        self.host
            .opened
            .lock()
            .iter()
            .filter(|o| o.role == role)
            .map(|o| o.brief.clone())
            .collect()
    }

    /// Wait, in real time, for the engine's threads to get somewhere. Bounded, so a property that
    /// never holds fails the test rather than hanging it.
    fn until(&self, flow: FlowId, what: &str, done: impl Fn(&FlowView) -> bool) -> FlowView {
        for _ in 0..4_000 {
            let view = self.view(flow);
            if done(&view) {
                return view;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("never {what}: {:#?}", self.view(flow));
    }

    fn consolidated(&self, flow: FlowId) -> FlowView {
        self.until(flow, "consolidated", |v| {
            v.flow.state.phase == Phase::Consolidated && !v.running
        })
    }

    fn step<'a>(view: &'a FlowView, role: &str) -> &'a wtm_core::flow::Step {
        view.steps.iter().find(|s| s.role == role).unwrap()
    }

    /// A role's invocations' statuses, oldest first; empty before the run has planned its steps.
    fn statuses(view: &FlowView, role: &str) -> Vec<InvocationStatus> {
        let Some(step) = view.steps.iter().find(|s| s.role == role).map(|s| s.id) else {
            return Vec::new();
        };
        view.invocations
            .iter()
            .filter(|i| i.step == step)
            .map(|i| i.status)
            .collect()
    }
}

/// The kinds of a run's open items that wait on you: everything but "review ready".
fn needs_you(view: &FlowView) -> Vec<InboxKind> {
    view.inbox
        .iter()
        .map(|item| item.kind)
        .filter(|kind| *kind != InboxKind::ReviewReady)
        .collect()
}

/// How many announcements of `kind` the engine made.
fn heard(h: &Harness, kind: InboxKind) -> usize {
    h.heard
        .filed
        .lock()
        .iter()
        .filter(|item| item.kind == kind)
        .count()
}

fn titles(view: &FlowView, role: &str) -> Vec<String> {
    match &Harness::step(view, role).result {
        Some(Submission::Findings(report)) => {
            report.findings.iter().map(|f| f.title.clone()).collect()
        }
        other => panic!("{role} has no findings: {other:?}"),
    }
}

// ═══════════════════════════════ the tests ═══════════════════════════════

#[test]
fn the_roles_of_a_stage_run_together_and_the_next_stage_waits_for_all_of_them() {
    let h = harness(Box::new(|role, _| match role {
        "summary" => Behaviour::Submit(vec![finding("from the summary")]),
        other => Behaviour::Rendezvous(2, finding(&format!("from {other}"))),
    }));

    let flow = h.start("standard");
    let view = h.consolidated(flow);

    assert_eq!(h.host.open_most.load(Ordering::SeqCst), 2);
    let opened = h.host.opened_roles();
    assert_eq!(opened.len(), 3);
    assert_eq!(
        opened[2], "summary",
        "the second stage opens last: {opened:?}"
    );
    for role in ["conventions", "correctness", "summary"] {
        assert_eq!(
            Harness::step(&view, role).status,
            StepStatus::Done,
            "{role}"
        );
    }
    assert_eq!(titles(&view, "correctness"), ["from correctness"]);
    assert!(view.flow.state.hold.is_none());
    assert!(needs_you(&view).is_empty());
}

#[test]
fn every_role_reads_the_snapshot_recorded_before_any_session_opened() {
    let h = harness(Box::new(|_, _| Behaviour::Submit(vec![finding("x")])));

    let flow = h.start("pair");
    let view = h.consolidated(flow);

    let snapshot = view.snapshot.clone().expect("a snapshot was recorded");
    for opened in h.host.opened.lock().iter() {
        let cwd = opened.cwd.clone().unwrap();
        assert!(
            cwd.ends_with(PathBuf::from("checkouts").join(&snapshot.tree_sha)),
            "{} read {}",
            opened.role,
            cwd.display()
        );
        assert!(
            opened.cwd_existed,
            "the checkout existed while {} read it",
            opened.role
        );
        assert!(opened.brief.contains(&snapshot.base_sha));
        assert!(
            opened.brief.contains("src/draft.rs"),
            "uncommitted work is in the change"
        );
    }
    let checkout = h.host.opened.lock()[0].cwd.clone().unwrap();
    assert!(
        !checkout.exists(),
        "the checkout is removed when its stage ends"
    );
    assert!(
        view.invocations
            .iter()
            .all(|i| i.snapshot == view.review.snapshot)
    );
}

#[test]
fn a_recipes_concurrency_cap_runs_a_stage_one_role_at_a_time() {
    let h = harness(Box::new(|_, _| Behaviour::Submit(vec![finding("x")])));

    let flow = h.start("one-at-a-time");
    let view = h.consolidated(flow);

    assert_eq!(h.host.open_most.load(Ordering::SeqCst), 1);
    assert_eq!(Harness::step(&view, "conventions").status, StepStatus::Done);
    assert_eq!(Harness::step(&view, "correctness").status, StepStatus::Done);
}

#[test]
fn a_role_that_times_out_is_retried_and_its_second_attempt_counts() {
    let h = harness(Box::new(|role, attempt| match (role, attempt) {
        ("correctness", 1) => Behaviour::Silent,
        _ => Behaviour::Submit(vec![finding(&format!("{role} attempt {attempt}"))]),
    }));

    let flow = h.start("pair");
    h.until(flow, "the first attempt is open", |v| {
        Harness::statuses(v, "correctness") == [InvocationStatus::Running]
    });
    h.clock.advance_ms(60_001);
    let view = h.consolidated(flow);

    assert_eq!(
        Harness::statuses(&view, "correctness"),
        [InvocationStatus::TimedOut, InvocationStatus::Submitted]
    );
    let step = Harness::step(&view, "correctness");
    assert_eq!(step.status, StepStatus::Done);
    assert_eq!(step.failures, 1);
    assert_eq!(titles(&view, "correctness"), ["correctness attempt 2"]);
    assert!(
        needs_you(&view).is_empty(),
        "a retry that worked needs nobody"
    );
}

#[test]
fn a_role_out_of_retries_fails_into_the_inbox_and_the_review_still_consolidates() {
    let h = harness(Box::new(|role, _| match role {
        "fragile" => Behaviour::Silent,
        _ => Behaviour::Submit(vec![finding("kept")]),
    }));

    let flow = h.start("fragile");
    h.until(flow, "the fragile role is running", |v| {
        Harness::statuses(v, "fragile") == [InvocationStatus::Running]
    });
    h.clock.advance_ms(60_001);
    let view = h.consolidated(flow);

    let step = Harness::step(&view, "fragile");
    assert_eq!(step.status, StepStatus::Failed);
    assert!(
        step.detail
            .as_deref()
            .unwrap()
            .contains("no result after 1 min")
    );
    assert_eq!(titles(&view, "conventions"), ["kept"]);
    assert_eq!(needs_you(&view), [InboxKind::Failure]);
    let failed = view
        .inbox
        .iter()
        .find(|i| i.kind == InboxKind::Failure)
        .unwrap();
    assert_eq!(
        failed.target,
        InboxTarget::Step {
            step: step.id,
            role: "fragile".to_owned()
        },
        "a click on it lands on the role"
    );
    assert_eq!(heard(&h, InboxKind::Failure), 1, "notified once");
}

#[test]
fn a_submission_that_fails_its_schema_goes_back_to_the_agent_and_its_fixed_retry_is_the_result() {
    let mut broken = finding("first try");
    broken["findings"][0]["severity"] = json!("catastrophic");
    broken["findings"][0]["class"] = json!("confirmed_bug");
    let fixed = finding("second try");
    let h = harness(Box::new(move |role, _| match role {
        "correctness" => Behaviour::Submit(vec![broken.clone(), fixed.clone()]),
        _ => Behaviour::Submit(vec![finding("x")]),
    }));

    let flow = h.start("pair");
    let view = h.consolidated(flow);

    let answers: Vec<(bool, String)> = h
        .host
        .answers
        .lock()
        .iter()
        .filter(|(role, ..)| role == "correctness")
        .map(|(_, ok, text)| (*ok, text.clone()))
        .collect();
    assert_eq!(answers.len(), 2);
    assert!(!answers[0].0);
    assert!(
        answers[0].1.contains("/findings/0/severity"),
        "{}",
        answers[0].1
    );
    assert!(answers[0].1.contains("call `flow_submit` again"));
    assert!(answers[1].0, "{}", answers[1].1);
    assert_eq!(titles(&view, "correctness"), ["second try"]);
    assert_eq!(
        Harness::statuses(&view, "correctness"),
        [InvocationStatus::Submitted],
        "fixed within the same turn, not a second attempt"
    );
}

#[test]
fn a_submission_of_a_kind_the_role_does_not_hand_in_is_refused_for_the_role_to_resubmit() {
    let h = harness(Box::new(|role, _| match role {
        "conventions" => Behaviour::Calls(vec![
            json!({ "kind": "plan", "payload": {
                "summary": "s",
                "steps": ["a"],
                "acceptanceCriteria": ["b"],
                "verification": ["c"],
                "areas": ["src"],
                "designNeeded": false,
            } }),
            json!({ "kind": "findings", "payload": finding("the right kind") }),
        ]),
        _ => Behaviour::Submit(vec![finding("x")]),
    }));

    let flow = h.start("pair");
    let view = h.consolidated(flow);

    let answers: Vec<(bool, String)> = h
        .host
        .answers
        .lock()
        .iter()
        .filter(|(role, ..)| role == "conventions")
        .map(|(_, ok, text)| (*ok, text.clone()))
        .collect();
    assert!(!answers[0].0);
    assert!(
        answers[0]
            .1
            .contains("this role hands in `findings`, not `plan`"),
        "{}",
        answers[0].1
    );
    assert_eq!(titles(&view, "conventions"), ["the right kind"]);
}

#[test]
fn a_role_that_ends_its_turn_without_submitting_is_asked_once_more_before_the_attempt_fails() {
    let h = harness(Box::new(|role, attempt| match (role, attempt) {
        ("correctness", 1) => Behaviour::SubmitWhenNudged(finding("after the nudge")),
        ("correctness", _) => Behaviour::EndWithoutSubmitting,
        _ => Behaviour::Submit(vec![finding("x")]),
    }));

    let flow = h.start("pair");
    let view = h.consolidated(flow);

    assert_eq!(titles(&view, "correctness"), ["after the nudge"]);
    assert_eq!(
        Harness::statuses(&view, "correctness"),
        [InvocationStatus::Submitted]
    );
}

#[test]
fn a_usage_limit_holds_the_run_until_the_reset_and_then_the_role_runs_again_without_spending_a_retry()
 {
    let resets_at = 1_785_931_800; // ten minutes after the fake clock's start
    let h = harness(Box::new(move |role, attempt| match (role, attempt) {
        ("correctness", 1) => Behaviour::Limit(Some(resets_at)),
        _ => Behaviour::Submit(vec![finding(&format!("{role} {attempt}"))]),
    }));

    let flow = h.start("pair");
    let held = h.until(flow, "held for capacity", |v| {
        v.flow.state.hold.is_some()
            && !v.inbox.is_empty()
            && v.steps
                .iter()
                .any(|s| s.role == "conventions" && s.status == StepStatus::Done)
    });
    assert_eq!(
        held.flow.state.hold,
        Some(Hold::WaitingCapacity {
            provider: "codex".to_owned(),
            resets_at: Some(resets_at),
        })
    );
    let waiting = Harness::step(&held, "correctness");
    assert_eq!(waiting.status, StepStatus::WaitingCapacity);
    assert_eq!(waiting.resets_at, Some(resets_at));
    assert_eq!(
        titles(&held, "conventions"),
        ["conventions 1"],
        "the other role carried on"
    );
    assert_eq!(held.inbox.len(), 1);
    assert_eq!(held.inbox[0].kind, InboxKind::Capacity);
    assert!(held.inbox[0].body.contains("never uses an API key"));

    // Nothing happens before the reset, however long the wait in real time.
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert_eq!(h.host.attempts_of("correctness"), 1);

    h.clock.advance_ms(600_001);
    let view = h.consolidated(flow);

    assert!(view.flow.state.hold.is_none());
    assert_eq!(titles(&view, "correctness"), ["correctness 2"]);
    let step = Harness::step(&view, "correctness");
    assert_eq!(step.failures, 0, "a limit is not the role's failure");
    assert_eq!(
        Harness::statuses(&view, "correctness"),
        [InvocationStatus::LimitReached, InvocationStatus::Submitted]
    );
    assert!(
        needs_you(&view).is_empty(),
        "the capacity item resolved itself"
    );
    assert_eq!(heard(&h, InboxKind::Capacity), 1);
}

#[test]
fn a_limit_hands_over_to_the_roles_configured_fallback_on_another_provider_with_a_written_summary()
{
    let h = harness(Box::new(|role, _| match role {
        "limited" => Behaviour::Limit(None),
        _ => Behaviour::Submit(vec![finding("found on Claude")]),
    }));

    let flow = h.start("fallback");
    let view = h.consolidated(flow);

    let first = Harness::step(&view, "limited");
    assert_eq!(first.status, StepStatus::Replaced);
    let fallback = Harness::step(&view, "limited-on-claude");
    assert_eq!(fallback.status, StepStatus::Done);
    assert_eq!(fallback.stands_in_for.as_deref(), Some("limited"));
    let handover = fallback.handover.as_deref().unwrap();
    assert!(handover.contains("- reading export.py"), "{handover}");
    assert!(handover.contains("usage limit"), "{handover}");
    assert!(view.flow.state.hold.is_none(), "it never waited");
    let brief = &h.host.opened.lock()[1].brief;
    assert!(
        brief.contains("You take over another session's work"),
        "{brief}"
    );
    assert_eq!(needs_you(&view), [InboxKind::Capacity]);
}

#[test]
fn quitting_mid_review_interrupts_it_keeps_finished_results_and_sends_nothing_again() {
    let h = harness(Box::new(|role, _| match role {
        "correctness" => Behaviour::Silent,
        _ => Behaviour::Submit(vec![finding("kept across the quit")]),
    }));

    let flow = h.start("pair");
    h.until(flow, "one role done and one running", |v| {
        v.steps
            .iter()
            .any(|s| s.role == "conventions" && s.status == StepStatus::Done)
            && Harness::statuses(v, "correctness") == [InvocationStatus::Running]
    });

    h.engine.shutdown();

    let view = h.view(flow);
    assert_eq!(
        view.flow.state.hold,
        Some(Hold::Interrupted {
            cause: InterruptCause::Quit
        })
    );
    assert_eq!(
        Harness::step(&view, "correctness").status,
        StepStatus::Interrupted
    );
    assert_eq!(
        Harness::statuses(&view, "correctness"),
        [InvocationStatus::Interrupted]
    );
    assert_eq!(titles(&view, "conventions"), ["kept across the quit"]);
    let item = view
        .inbox
        .iter()
        .find(|i| i.kind == InboxKind::Interrupted)
        .unwrap();
    assert!(
        item.body.contains("1 of 2 roles had finished"),
        "{}",
        item.body
    );
    assert!(item.body.contains("nothing was sent again"));

    // The next launch: a new engine over the same database finds nothing more to do, and sends
    // nothing by itself.
    let sent = h.host.turns_sent.load(Ordering::SeqCst);
    let relaunched = engine_for(&h.app, &h.host, &h.clock, &h.heard);
    assert_eq!(relaunched.recover().unwrap(), []);
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert_eq!(h.host.turns_sent.load(Ordering::SeqCst), sent);

    // Resumed by a person, only the cut-off role runs again.
    h.host.rescript(Box::new(|_, _| {
        Behaviour::Submit(vec![finding("after the resume")])
    }));
    relaunched.resume(flow).unwrap();
    let view = relaunched_consolidated(&relaunched, flow);
    assert_eq!(titles(&view, "correctness"), ["after the resume"]);
    assert_eq!(titles(&view, "conventions"), ["kept across the quit"]);
    assert_eq!(
        h.host.attempts_of("conventions"),
        1,
        "a finished role is not re-sent"
    );
    assert!(needs_you(&view).is_empty());
}

fn relaunched_consolidated(engine: &Engine, flow: FlowId) -> FlowView {
    for _ in 0..4_000 {
        let view = engine.view(flow).unwrap();
        if view.flow.state.phase == Phase::Consolidated && !view.running {
            return view;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("never consolidated: {:#?}", engine.view(flow).unwrap());
}

#[test]
fn a_review_found_mid_run_at_launch_is_marked_interrupted_by_a_crash_and_nothing_is_resent() {
    let h = harness(Box::new(|role, _| match role {
        "correctness" => Behaviour::Silent,
        _ => Behaviour::Submit(vec![finding("x")]),
    }));
    let flow = h.start("pair");
    // Conventions finished too, so every turn the first engine will ever send has been sent before
    // the count below is taken: a role still opening could send its first turn after it, and the
    // test would blame the relaunch for it.
    h.until(flow, "correctness running and conventions done", |v| {
        Harness::statuses(v, "correctness") == [InvocationStatus::Running]
            && Harness::step(v, "conventions").status == StepStatus::Done
    });

    // The process died: no shutdown ran. A new engine is what the next launch has.
    let sent = h.host.turns_sent.load(Ordering::SeqCst);
    let relaunched = engine_for(&h.app, &h.host, &h.clock, &h.heard);
    assert_eq!(relaunched.recover().unwrap(), [flow]);

    let view = relaunched.view(flow).unwrap();
    assert_eq!(
        view.flow.state.hold,
        Some(Hold::Interrupted {
            cause: InterruptCause::Crash
        })
    );
    assert_eq!(
        Harness::step(&view, "correctness").status,
        StepStatus::Interrupted
    );
    assert_eq!(h.host.turns_sent.load(Ordering::SeqCst), sent);
    // The old engine's threads are this test's leftovers; stop them writing.
    h.engine.shutdown();
}

#[test]
fn a_second_start_of_the_same_worktree_focuses_the_review_already_running() {
    let h = harness(Box::new(|_, _| Behaviour::Silent));
    let first = h.start("pair");
    let second = h.start("pair");
    assert_eq!(first, second);
    h.engine.cancel(first).unwrap();
}

#[test]
fn a_role_whose_provider_lacks_its_skill_fails_before_any_turn_and_names_the_provider() {
    let h = harness(Box::new(|_, _| Behaviour::Submit(vec![finding("x")])));
    h.host
        .skills
        .lock()
        .insert("codex".to_owned(), vec!["something-else".to_owned()]);
    // Claude lists the run's plugin copy of the skill, under the plugin's name.
    h.host
        .skills
        .lock()
        .insert("claude".to_owned(), vec!["wtm:team-review".to_owned()]);

    let flow = h.start("skills");
    let view = h.consolidated(flow);

    let step = Harness::step(&view, "skilled");
    assert_eq!(step.status, StepStatus::Failed);
    let detail = step.detail.as_deref().unwrap();
    assert!(
        detail.contains("Codex does not list the skill `team-review`"),
        "{detail}"
    );
    assert_eq!(
        step.skill_problem,
        Some(SkillProblem::Missing {
            role: "skilled".to_owned(),
            provider: RoleProvider::Codex,
            skill: "team-review".to_owned(),
        }),
        "the problem is kept typed, so the Reviews tab can say it without reading prose"
    );
    assert_eq!(h.host.attempts_of("skilled"), 1, "no retry can add a skill");
    assert_eq!(
        Harness::step(&view, "skilled-claude").status,
        StepStatus::Done,
        "the plugin's prefix is the run's own, and counts"
    );
    assert_eq!(
        h.host.turns_sent.load(Ordering::SeqCst),
        1,
        "only the Claude role got a turn"
    );
    let brief = h
        .host
        .opened
        .lock()
        .iter()
        .find(|o| o.role == "skilled-claude")
        .unwrap()
        .brief
        .clone();
    assert!(brief.contains("`wtm:team-review`"), "{brief}");
}

#[test]
fn a_refused_preflight_starts_nothing_and_records_nothing() {
    let h = harness(Box::new(|_, _| Behaviour::Submit(vec![finding("x")])));
    *h.host.refuse_preflight.lock() =
        Some("Claude would bill an API key, not your claude.ai plan".to_owned());

    let refused = h.engine.start(&StartReview {
        project: h.project.clone(),
        worktree: h.worktree.clone(),
        recipe: "pair".to_owned(),
        base: None,
    });

    let refused = refused.unwrap_err();
    assert!(refused.message.contains("API key"), "{refused}");
    assert_eq!(
        refused.billing,
        ["claude", "codex"],
        "a billing refusal names the providers, which the Reviews tab says by name"
    );
    assert!(h.host.opened_roles().is_empty());
    assert!(h.engine.latest(&h.project, &h.worktree).unwrap().is_none());
}

#[test]
fn a_provider_permission_prompt_in_a_run_session_lands_in_the_inbox_once() {
    let h = harness(Box::new(|role, _| match role {
        "correctness" => Behaviour::AskPermissionThenSubmit(finding("x")),
        _ => Behaviour::Submit(vec![finding("x")]),
    }));

    let flow = h.start("pair");
    let view = h.consolidated(flow);

    let asked: Vec<&InboxItem> = view
        .inbox
        .iter()
        .filter(|i| i.kind == InboxKind::Approval)
        .collect();
    assert_eq!(asked.len(), 1, "two prompts, one item");
    assert!(asked[0].body.contains("`git push`"));
    assert_eq!(heard(&h, InboxKind::Approval), 1);
}

#[test]
fn cancelling_a_review_ends_it_and_closes_its_sessions() {
    let h = harness(Box::new(|_, _| Behaviour::Silent));
    let flow = h.start("pair");
    h.until(flow, "both roles running", |v| {
        v.invocations
            .iter()
            .filter(|i| i.status == InvocationStatus::Running)
            .count()
            == 2
    });

    h.engine.cancel(flow).unwrap();
    let view = h.until(flow, "the run thread stopped", |v| !v.running);

    assert_eq!(view.flow.state.end, Some(wtm_core::flow::End::Cancelled));
    assert_eq!(h.host.open_now.load(Ordering::SeqCst), 0);
    for step in &view.steps {
        assert_eq!(step.status, StepStatus::Cancelled, "{}", step.role);
    }
}

#[test]
fn the_worktree_path_reviewed_is_the_worktrees_own() {
    let h = harness(Box::new(|_, _| Behaviour::Submit(vec![finding("x")])));
    let flow = h.start("pair");
    let view = h.consolidated(flow);
    assert_eq!(
        PathBuf::from(&view.review.worktree_path)
            .canonicalize()
            .unwrap(),
        h.worktree_path.canonicalize().unwrap()
    );
    assert_eq!(view.review.base, "main");
}

// ═══════════════════════════════ consolidation, staleness, reruns ═══════════════════════════════

#[test]
fn two_roles_reporting_overlapping_lines_become_one_finding_with_both_reports_as_its_evidence() {
    let h = harness(Box::new(|role, _| match role {
        "conventions" => Behaviour::Submit(vec![finding_at("Loop ends early", "src/export.rs", 3)]),
        _ => Behaviour::Submit(vec![json!({ "findings": [
            {
                "title": "Off by one in export",
                "severity": "blocking",
                "class": "confirmed_bug",
                "verification": "ran the range test; the last row is missing",
                "body": "The loop stops one row early.",
                "path": "src/export.rs",
                "line": 2,
                "endLine": 4,
            },
            {
                "title": "Draft left in",
                "severity": "minor",
                "class": "suggestion",
                "body": "Remove before merging.",
                "path": "src/draft.rs",
            },
        ]})]),
    }));

    let flow = h.start("pair");
    let view = h.consolidated(flow);

    assert_eq!(view.findings.len(), 2, "{:#?}", view.findings);
    let merged = view
        .findings
        .iter()
        .find(|f| {
            f.finding
                .location
                .as_ref()
                .is_some_and(|l| l.path == "src/export.rs")
        })
        .unwrap();
    assert_eq!(
        merged.finding.title, "Off by one in export",
        "the confirmed bug leads"
    );
    assert_eq!(merged.finding.severity, wtm_core::flow::Severity::Blocking);
    let roles: Vec<&str> = merged
        .evidence
        .iter()
        .map(|e| e.source.role.as_str())
        .collect();
    assert_eq!(roles.len(), 2);
    assert!(
        roles.contains(&"conventions") && roles.contains(&"correctness"),
        "{roles:?}"
    );
    let whole_file = view
        .findings
        .iter()
        .find(|f| {
            f.finding
                .location
                .as_ref()
                .is_some_and(|l| l.path == "src/draft.rs")
        })
        .unwrap();
    assert!(
        !whole_file.lines,
        "a report with a file and no line is about the whole file"
    );
}

#[test]
fn a_new_commit_marks_findings_on_the_files_it_touched_stale_and_leaves_the_rest_current() {
    let h = harness(Box::new(|role, _| match role {
        "export-only" => Behaviour::Submit(vec![finding_at("Export", "src/export.rs", 1)]),
        _ => Behaviour::Submit(vec![finding_at("Draft", "src/draft.rs", 1)]),
    }));
    let flow = h.start("scoped");
    let view = h.consolidated(flow);
    assert!(
        !h.engine.staleness(flow).unwrap().changed,
        "nothing moved yet"
    );

    h.commit("src/export.rs", "fn export() { todo!() }\n");
    let stale = h.engine.staleness(flow).unwrap();

    assert!(stale.changed);
    assert_eq!(stale.paths, ["src/export.rs"]);
    let id_of = |title: &str| {
        view.findings
            .iter()
            .find(|f| f.finding.title == title)
            .unwrap()
            .finding
            .id
    };
    assert_eq!(stale.stale_findings, [id_of("Export")]);
    assert_eq!(
        stale.affected,
        ["export-only"],
        "the draft role's file did not change, so nothing it said is out of date"
    );
}

#[test]
fn a_role_that_reported_on_a_file_outside_its_paths_is_rerun_when_that_file_changes() {
    // Found by the first real run: a conventions role scoped to one directory reported a bug in a
    // file elsewhere, the file was fixed, and Rerun affected left that stale report with no role
    // to run again.
    let h = harness(Box::new(|_, _| {
        Behaviour::Submit(vec![finding_at("Draft", "src/draft.rs", 1)])
    }));
    let flow = h.start("scoped");
    h.consolidated(flow);

    h.commit("src/draft.rs", "// reviewed\n");
    let stale = h.engine.staleness(flow).unwrap();

    assert_eq!(
        stale.affected,
        ["export-only", "draft-only"],
        "export-only's paths did not change, but its own report did"
    );
}

#[test]
fn committing_exactly_the_uncommitted_work_a_review_read_changes_nothing_about_it() {
    let h = harness(Box::new(|_, _| {
        Behaviour::Submit(vec![finding_at("Draft", "src/draft.rs", 1)])
    }));
    let flow = h.start("pair");
    h.consolidated(flow);

    // The review read the untracked draft. Committing it moves the head and not the tree.
    h.fixture.git_in(&h.worktree_path, &["add", "."]);
    h.fixture
        .git_in(&h.worktree_path, &["commit", "-m", "Commit the draft"]);

    let stale = h.engine.staleness(flow).unwrap();
    assert!(!stale.changed, "{stale:#?}");
    assert!(stale.stale_findings.is_empty());
}

#[test]
fn rerun_affected_runs_only_the_roles_whose_files_changed_and_keeps_the_other_roles_findings() {
    let h = harness(Box::new(|role, attempt| match (role, attempt) {
        ("export-only", 1) => {
            Behaviour::Submit(vec![finding_at("Export, first", "src/export.rs", 1)])
        }
        ("export-only", _) => {
            Behaviour::Submit(vec![finding_at("Export, again", "src/export.rs", 1)])
        }
        _ => Behaviour::Submit(vec![finding_at("Draft", "src/draft.rs", 1)]),
    }));
    let flow = h.start("scoped");
    let first = h.consolidated(flow);
    let draft_finding = first
        .findings
        .iter()
        .find(|f| f.finding.title == "Draft")
        .unwrap()
        .finding
        .id;
    let first_snapshot = first.review.snapshot.unwrap();

    h.commit("src/export.rs", "fn export() { todo!() }\n");
    h.engine.rerun(flow, Rerun::Affected).unwrap();
    let after = h.until(flow, "consolidated after the rerun", |v| {
        v.flow.state.phase == Phase::Consolidated
            && !v.running
            && h.host.attempts_of("export-only") == 2
    });

    assert_eq!(h.host.attempts_of("export-only"), 2);
    assert_eq!(
        h.host.attempts_of("draft-only"),
        1,
        "its file did not change"
    );
    assert_ne!(
        after.review.snapshot,
        Some(first_snapshot),
        "a rerun reads a new snapshot"
    );
    let titles: Vec<&str> = after
        .findings
        .iter()
        .map(|f| f.finding.title.as_str())
        .collect();
    assert_eq!(titles.len(), 2, "{titles:?}");
    assert!(titles.contains(&"Export, again") && !titles.contains(&"Export, first"));
    assert!(
        after.findings.iter().any(|f| f.finding.id == draft_finding),
        "the role that did not run keeps its finding, id and all"
    );
    let rerun_brief = h.briefs("export-only").pop().unwrap();
    assert!(
        rerun_brief.contains("] Export, first (src/export.rs:1)"),
        "the rerun role is asked to check what it said before: {rerun_brief}"
    );
    let stale = h.engine.staleness(flow).unwrap();
    assert!(
        !stale.changed && stale.stale_findings.is_empty(),
        "{stale:#?}"
    );
}

#[test]
fn a_rerun_with_nothing_changed_is_refused_and_runs_nothing() {
    let h = harness(Box::new(|_, _| Behaviour::Submit(vec![finding("x")])));
    let flow = h.start("scoped");
    h.consolidated(flow);

    let refused = h.engine.rerun(flow, Rerun::Affected).unwrap_err();

    assert!(
        refused.message.contains("nothing to run again"),
        "{refused}"
    );
    assert_eq!(h.host.opened_roles().len(), 2);
}

#[test]
fn a_dismissal_survives_a_rerun_whose_role_reports_the_same_problem_again() {
    let h = harness(Box::new(|_, _| {
        Behaviour::Submit(vec![finding_at("Export", "src/export.rs", 1)])
    }));
    let flow = h.start("scoped");
    let view = h.consolidated(flow);
    let id = view.findings[0].finding.id;
    h.engine.dismiss(id, Some("the stub is intended")).unwrap();

    h.commit("src/export.rs", "// moved\nfn export() {}\n");
    h.engine.rerun(flow, Rerun::Affected).unwrap();
    let after = h.until(flow, "consolidated after the rerun", |v| {
        v.flow.state.phase == Phase::Consolidated
            && !v.running
            && h.host.attempts_of("export-only") == 2
    });

    let again = &after.findings[0];
    assert_ne!(again.finding.id, id, "a new report is a new finding");
    assert_eq!(
        again.finding.dismissed.as_ref().map(|d| d.reason.as_str()),
        Some("the stub is intended")
    );
    assert!(
        h.engine.dismiss(id, Some("  ")).is_err(),
        "a dismissal says why"
    );
}

#[test]
fn a_check_a_role_asks_for_is_run_by_the_app_and_only_an_exit_of_zero_reads_as_passed() {
    let h = harness(Box::new(|_, _| {
        Behaviour::CheckThenSubmit(vec!["passes", "fails", "e2e", "deploy"], finding("x"))
    }));
    let flow = h.start("checks");
    let view = h.consolidated(flow);

    let status = |check: &str| {
        view.checks
            .iter()
            .find(|c| c.check == check)
            .map(|c| (c.status, c.exit_code))
    };
    assert_eq!(status("passes"), Some((CheckStatus::Passed, Some(0))));
    assert_eq!(status("fails"), Some((CheckStatus::Failed, Some(1))));
    assert_eq!(status("e2e"), Some((CheckStatus::Blocked, None)));
    assert_eq!(
        status("deploy"),
        None,
        "a check the role was not given leaves no record"
    );

    let checkout = view.invocations[0].cwd.clone();
    assert!(
        !PathBuf::from(&checkout).join("ran-e2e").exists()
            && !h.worktree_path.join("ran-e2e").exists(),
        "a check that needs a service is never started"
    );
    assert!(
        view.checks.iter().all(|c| c.cwd == checkout),
        "a check runs where its role reads"
    );
    let answers = h.host.answers.lock().clone();
    let said = |needle: &str| {
        answers
            .iter()
            .find(|(_, _, text)| text.contains(needle))
            .cloned()
    };
    let (_, ok, text) = said("`fails` failed").unwrap();
    assert!(ok && text.contains("2 of 3 tests failed"), "{text}");
    let (_, ok, _) = said("`deploy` is not one of this role's checks").unwrap();
    assert!(!ok);
    let (_, _, blocked) = said("`e2e` was not run").unwrap();
    assert!(
        blocked.contains("blocked: environment not started")
            && blocked.contains("neither passed nor failed"),
        "{blocked}"
    );
    assert_eq!(
        view.expected_checks
            .iter()
            .map(|c| c.id.as_str())
            .collect::<Vec<_>>(),
        ["passes", "fails", "e2e"]
    );

    h.commit("src/export.rs", "fn export() { 1 }\n");
    let stale = h.engine.staleness(flow).unwrap();
    assert_eq!(
        stale.stale_checks.len(),
        3,
        "a check ran against the whole tree"
    );
}

#[test]
fn a_rule_and_the_instruction_files_for_the_change_are_in_the_roles_brief_and_listed_as_applied() {
    let h = harness(Box::new(|_, _| Behaviour::Submit(vec![finding("x")])));
    std::fs::write(
        h.worktree_path.join("CLAUDE.md"),
        "Prefer small functions.\n",
    )
    .unwrap();
    let project = wtm_core::model::ProjectId::from_root(std::path::Path::new(&h.project));
    let db = h.app.appdb.get().unwrap();
    let rule = db
        .create_rule(
            &NewRule::written(
                project,
                RuleDraft {
                    kind: RuleKind::Required,
                    text: "Drafts never land on main".to_owned(),
                    rationale: String::new(),
                    scope: RuleScope {
                        areas: Vec::new(),
                        paths: vec!["src/draft.rs".to_owned()],
                    },
                    examples: Vec::new(),
                    references: Vec::new(),
                },
            )
            .unwrap(),
            1,
        )
        .unwrap();

    let flow = h.start("scoped");
    let view = h.consolidated(flow);

    let brief = h.briefs("draft-only").pop().unwrap();
    assert!(
        brief.contains(&format!("<rule id=\"R-{}\" revision=\"1\"", rule.id)),
        "{brief}"
    );
    assert!(
        brief.contains("<instructions path=\"CLAUDE.md\">"),
        "{brief}"
    );
    assert!(
        brief.contains("Prefer small functions."),
        "read from the snapshot checkout"
    );
    assert_eq!(view.applied.rules.len(), 1);
    assert_eq!(view.applied.rules[0].rule, rule.id);
    assert_eq!(view.applied.rules[0].roles, ["draft-only"]);
    assert_eq!(view.applied.sources, ["CLAUDE.md"]);
    assert_eq!(
        db.applications(flow).unwrap().len(),
        1,
        "recorded once, for that attempt"
    );
    let export_brief = h.briefs("export-only").pop().unwrap();
    assert!(
        !export_brief.contains("<rule"),
        "a role without the rules input is given none"
    );
}

// ═══════════════════════════════ the inbox ═══════════════════════════════

fn store(h: &Harness) -> Arc<wtm_appdb::AppDb> {
    h.app.appdb.get().unwrap()
}

#[test]
fn a_finished_review_says_it_is_ready_once_and_a_rerun_makes_way_for_the_next() {
    let h = harness(Box::new(|role, attempt| {
        let line = if role == "conventions" { 1 } else { 2 };
        Behaviour::Submit(vec![finding_at(
            &format!("{role} {attempt}"),
            "src/export.rs",
            line,
        )])
    }));

    let flow = h.start("pair");
    let view = h.consolidated(flow);

    let ready: Vec<&InboxItem> = view
        .inbox
        .iter()
        .filter(|i| i.kind == InboxKind::ReviewReady)
        .collect();
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].target, InboxTarget::Flow);
    assert_eq!(ready[0].title, "Review ready: 2 findings");
    assert!(
        ready[0].body.contains("(src/export.rs:1)"),
        "{}",
        ready[0].body
    );
    assert_eq!(
        ready[0].project.as_ref().map(ToString::to_string),
        Some(h.project.clone())
    );
    assert_eq!(ready[0].worktree.as_deref(), Some(h.worktree.as_str()));
    assert_eq!(heard(&h, InboxKind::ReviewReady), 1);
    let first = ready[0].id;

    h.engine.rerun(flow, Rerun::All).unwrap();
    let view = h.until(flow, "the rerun consolidated", |v| {
        v.flow.state.phase == Phase::Consolidated
            && !v.running
            && v.inbox
                .iter()
                .any(|i| i.kind == InboxKind::ReviewReady && i.id != first)
    });

    assert_eq!(
        store(&h).inbox_item(first).unwrap().unwrap().state,
        InboxState::Resolved,
        "the earlier ready item was settled when the run started again"
    );
    assert_eq!(
        view.inbox
            .iter()
            .filter(|i| i.kind == InboxKind::ReviewReady)
            .count(),
        1
    );
    assert_eq!(heard(&h, InboxKind::ReviewReady), 2);
}

#[test]
fn routine_progress_changes_the_run_and_notifies_nothing() {
    let h = harness(Box::new(|_, _| {
        Behaviour::ProgressThenSubmit(json!({ "findings": [] }))
    }));

    let flow = h.start("pair");
    let view = h.consolidated(flow);

    assert!(
        view.invocations
            .iter()
            .any(|i| i.progress.as_deref() == Some("writing it up")),
        "the progress was recorded"
    );
    let kinds: Vec<InboxKind> = h.heard.filed.lock().iter().map(|i| i.kind).collect();
    assert_eq!(
        kinds,
        [InboxKind::ReviewReady],
        "the run's one announcement is that it is ready"
    );
    assert_eq!(view.inbox[0].title, "Review ready: nothing found");
}

#[test]
fn a_question_a_role_asks_is_kept_for_you_and_its_answer_reaches_the_role_when_it_runs_again() {
    let h = harness(Box::new(|role, attempt| match (role, attempt) {
        ("correctness", 1) => Behaviour::AskThenSubmit(
            json!({
                "question": "Is an empty export an error?",
                "why": "It decides whether the empty case is a bug.",
                "refs": ["src/export.rs:1", "ExportWriter"],
                "recommendation": "Valid: write the header row",
                "unblocks": "Reporting the empty case one way or the other",
                "options": ["An error", "Valid, header only"],
            }),
            finding_at("assumed valid", "src/export.rs", 1),
        ),
        _ => Behaviour::Submit(vec![finding_at(
            &format!("{role} {attempt}"),
            "src/export.rs",
            2,
        )]),
    }));

    let flow = h.start("pair");
    let view = h.consolidated(flow);

    let item = view
        .inbox
        .iter()
        .find(|i| i.kind == InboxKind::Question)
        .expect("the question is in the inbox");
    let question = item.question.as_ref().expect("with the question itself");
    assert_eq!(
        item.target,
        InboxTarget::Question {
            question: question.id
        }
    );
    assert_eq!(question.role.as_deref(), Some("correctness"));
    assert_eq!(question.options, ["An error", "Valid, header only"]);
    assert_eq!(
        question.recommendation.as_deref(),
        Some("Valid: write the header row")
    );
    assert_eq!(
        question.context,
        [
            wtm_core::flow::ContextLink::File {
                path: "src/export.rs".to_owned(),
                line: Some(1),
                note: None,
            },
            wtm_core::flow::ContextLink::Mention {
                text: "ExportWriter".to_owned()
            },
        ]
    );
    assert_eq!(heard(&h, InboxKind::Question), 1);
    let told = h
        .host
        .answers
        .lock()
        .iter()
        .find(|(role, _, text)| role == "correctness" && text.contains("Filed as question"))
        .cloned()
        .expect("the role heard back");
    assert!(told.1, "asking is not an error");
    assert!(told.2.contains("does not stop for answers"), "{}", told.2);
    assert_eq!(
        Harness::step(&view, "correctness").status,
        StepStatus::Done,
        "the role carried on rather than waiting"
    );

    let answered = h
        .engine
        .answer(question.id, Some(1), "  and say so in the docs ")
        .unwrap();
    assert_eq!(
        answered.answer.as_ref().unwrap().text,
        "and say so in the docs"
    );
    assert_eq!(
        store(&h).inbox_item(item.id).unwrap().unwrap().state,
        InboxState::Resolved
    );
    let again = h.engine.answer(question.id, Some(0), "");
    assert!(
        again
            .as_ref()
            .is_err_and(|e| e.contains("already answered")),
        "{again:?}"
    );

    h.engine.rerun(flow, Rerun::All).unwrap();
    h.until(flow, "the rerun consolidated", |v| {
        v.flow.state.phase == Phase::Consolidated
            && !v.running
            && h.briefs("correctness").len() == 2
    });
    let brief = &h.briefs("correctness")[1];
    assert!(
        brief.contains(r#"Answer: "Valid, header only", adding "and say so in the docs""#),
        "{brief}"
    );
    assert!(
        !h.briefs("conventions")[1].contains("Is an empty export an error?"),
        "only the role that asked is told"
    );

    // Run again is a new run of the same worktree, and the answer still reaches the role.
    let again = h.start("pair");
    assert_ne!(
        again, flow,
        "a consolidated review is superseded by a new run"
    );
    h.consolidated(again);
    let brief = &h.briefs("correctness")[2];
    assert!(
        brief.contains(r#"Answer: "Valid, header only""#),
        "a new run of the same work is told too: {brief}"
    );
}

#[test]
fn an_item_outlives_the_app_that_filed_it_and_only_a_person_or_its_cause_closes_it() {
    let h = harness(Box::new(|role, _| match role {
        "fragile" => Behaviour::Silent,
        _ => Behaviour::Submit(vec![finding("kept")]),
    }));
    let flow = h.start("fragile");
    h.until(flow, "the fragile role is running", |v| {
        Harness::statuses(v, "fragile") == [InvocationStatus::Running]
    });
    h.clock.advance_ms(60_001);
    h.consolidated(flow);
    let before: Vec<InboxItem> = h.engine.inbox(None).unwrap();
    assert_eq!(before.len(), 2, "{before:#?}");
    let announced = h.heard.filed.lock().len();

    // Quit and launch again: a new app and engine over the same files. Reading the inbox, as a
    // window, a badge and a notification click all do, closes nothing.
    let app = Arc::new(App::with_paths(AppPaths::rooted(h.data.path())).unwrap());
    let heard_again = Arc::new(Heard::default());
    let relaunched = engine_for(&app, &h.host, &h.clock, &heard_again);
    assert_eq!(relaunched.recover().unwrap(), []);
    for _ in 0..3 {
        assert_eq!(relaunched.inbox(None).unwrap(), before);
        relaunched.view(flow).unwrap();
    }
    assert_eq!(relaunched.inbox(Some(&h.project)).unwrap(), before);
    assert!(
        heard_again.filed.lock().is_empty() && h.heard.filed.lock().len() == announced,
        "nothing was announced again"
    );

    // A person dismisses the failure: the item goes, and what it was about does not change.
    let failure = before
        .iter()
        .find(|i| i.kind == InboxKind::Failure)
        .unwrap();
    let dismissed = relaunched.dismiss_item(failure.id).unwrap().unwrap();
    assert_eq!(dismissed.state, InboxState::Dismissed);
    assert_eq!(relaunched.dismiss_item(failure.id).unwrap(), None);
    let view = relaunched.view(flow).unwrap();
    assert_eq!(Harness::step(&view, "fragile").status, StepStatus::Failed);
    assert_eq!(needs_you(&view), Vec::<InboxKind>::new());
}

#[test]
fn stopping_a_review_settles_everything_it_had_asked_of_you() {
    let h = harness(Box::new(|role, _| match role {
        "correctness" => Behaviour::AskThenSubmit(
            json!({
                "question": "Which base?",
                "why": "Two bases look plausible.",
                "unblocks": "Choosing the diff",
            }),
            json!({ "findings": [] }),
        ),
        _ => Behaviour::Silent,
    }));
    let flow = h.start("pair");
    h.until(flow, "the question was asked", |v| {
        v.inbox.iter().any(|i| i.kind == InboxKind::Question)
    });

    h.engine.cancel(flow).unwrap();

    assert!(h.engine.inbox(None).unwrap().is_empty());
}
