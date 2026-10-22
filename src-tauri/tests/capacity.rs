//! Usage capacity and run-session caps, driven by fake agents against a real repository and a real
//! database.
//!
//! What this protects is what a person relies on when a provider runs dry or too much runs at once:
//! a role at its limit walks its own fallback chain in order and never back to a provider that ran
//! out; the role that takes over is handed the run's own record rather than a conversation; a model
//! that bills usage credits needs that role's say-so; work over a cap waits in line, oldest first,
//! and says what it waits for; a run's children draw from the same places as everything else; and a
//! wait at a limit resumes by itself only while wtm was open and the Mac awake, asking the provider
//! again after a sleep or a relaunch. `review_engine.rs` has the engine's other holds; this file has
//! capacity's.
//!
//! Sessions are [`FakeHost`]'s, and so are the provider's limit readings, which a test scripts.
//! The clock is a `FakeClock`: `advance_ms` is time passing with the Mac awake, `sleep_ms` the wall
//! clock moving while the monotonic one does not, which is what a Mac asleep looks like to wtm.

#![allow(clippy::unwrap_used, clippy::too_many_lines)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde_json::{Value, json};
use wtm_app_lib::app::App;
use wtm_app_lib::flow::findings::Rerun;
use wtm_app_lib::flow::host::RoleHost;
use wtm_app_lib::flow::{Engine, FlowView, Notice, StartReview, Tuning};
use wtm_config::AppPaths;
use wtm_core::flow::{
    CapacityChoice, FlowId, Hold, InboxKind, InboxState, InvocationStatus, LimitReading, Phase,
    RoleProvider, StepStatus, SwitchReason, SwitchedBy,
};
use wtm_core::model::ProjectId;
use wtm_core::ports::clock::Clock;
use wtm_core::ports::{InboxStore, LeaseStore, RunStore};
use wtm_testkit::{FakeClock, GitFixture};

mod common;
use common::agents::{Behaviour, FakeHost, Heard, Script};

// ═══════════════════════════════ the harness ═══════════════════════════════

const ROLES: &str = r#"
[defaults.workflow.role.conventions]
objective = "Check the change against the repository's conventions"
provider = "claude"
inputs = ["diff"]
timeout_ms = 60000

[defaults.workflow.role.correctness]
objective = "Find bugs the change introduces"
provider = "codex"
inputs = ["diff", "answers"]
timeout_ms = 60000
fallback = ["correctness-codex-mini", "correctness-claude", "correctness-haiku"]

[defaults.workflow.role.correctness-codex-mini]
objective = "Find bugs the change introduces, on a smaller Codex model"
provider = "codex"
inputs = ["diff"]
timeout_ms = 60000

[defaults.workflow.role.correctness-claude]
objective = "Find bugs the change introduces, on Claude"
provider = "claude"
model = "sonnet"
inputs = ["diff"]
timeout_ms = 60000

[defaults.workflow.role.correctness-haiku]
objective = "Find bugs the change introduces, on a smaller Claude model"
provider = "claude"
model = "haiku"
inputs = ["diff"]
timeout_ms = 60000

[defaults.workflow.role.lonely]
objective = "Find bugs, with no fallback"
provider = "codex"
inputs = ["diff"]
timeout_ms = 60000

[defaults.workflow.role.limited]
objective = "Find bugs on Codex, falling back to Claude"
provider = "codex"
inputs = ["diff"]
timeout_ms = 60000
fallback = "limited-on-claude"

[defaults.workflow.role.limited-on-claude]
objective = "Find bugs on Claude"
provider = "claude"
inputs = ["diff"]
timeout_ms = 60000

[defaults.workflow.role.costly]
objective = "Review on Fable"
provider = "claude"
model = "fable"
inputs = ["diff"]
timeout_ms = 60000

[defaults.workflow.role.costly-ok]
objective = "Review on Fable, which you allowed"
provider = "claude"
model = "fable"
allow_credit_billing = true
inputs = ["diff"]
timeout_ms = 60000

[defaults.workflow.role.unnamed]
objective = "Review on whatever Claude defaults to"
provider = "claude"
inputs = ["diff"]
timeout_ms = 60000

[defaults.workflow.role.second]
objective = "The second run's role"
provider = "codex"
inputs = ["diff"]
timeout_ms = 60000

[defaults.workflow.role.third]
objective = "The third run's role"
provider = "codex"
inputs = ["diff"]
timeout_ms = 60000

[[defaults.workflow.recipe.chain.stage]]
roles = ["conventions"]

[[defaults.workflow.recipe.chain.stage]]
roles = ["correctness"]

[[defaults.workflow.recipe.lonely.stage]]
roles = ["lonely"]

[[defaults.workflow.recipe.lonely-pair.stage]]
roles = ["conventions", "lonely"]

[[defaults.workflow.recipe.parent-child.stage]]
roles = ["conventions", "limited"]

[[defaults.workflow.recipe.costly.stage]]
roles = ["costly"]

[[defaults.workflow.recipe.costly-ok.stage]]
roles = ["costly-ok"]

[[defaults.workflow.recipe.unnamed.stage]]
roles = ["unnamed"]

[[defaults.workflow.recipe.conventions.stage]]
roles = ["conventions"]

[[defaults.workflow.recipe.second.stage]]
roles = ["second"]

[[defaults.workflow.recipe.third.stage]]
roles = ["third"]
"#;

struct Harness {
    app: Arc<App>,
    host: Arc<FakeHost>,
    clock: Arc<FakeClock>,
    heard: Arc<Heard>,
    engine: Arc<Engine>,
    project: String,
    worktrees: Vec<String>,
    _fixture: GitFixture,
    _data: tempfile::TempDir,
}

/// The app's own clock starts here; `FakeClock::new` is 2026-07-28T12:00:00Z.
const START_S: u64 = 1_785_931_200;

fn finding(title: &str) -> Value {
    json!({ "findings": [{
        "title": title,
        "severity": "notable",
        "class": "suggestion",
        "body": "Worth a look.",
        "path": "src/export.rs",
        "line": 1,
    }]})
}

/// A harness whose config adds `caps` (a `[run_sessions]` table, or nothing) to the roles, with
/// `worktrees` worktrees to review.
fn harness(script: Script, caps: &str, worktrees: usize) -> Harness {
    let fixture = GitFixture::new();
    let data = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    std::fs::write(data.path().join("config.toml"), format!("{caps}\n{ROLES}")).unwrap();
    let app = Arc::new(App::with_paths(AppPaths::rooted(data.path())).unwrap());
    let root = app.register(fixture.root()).unwrap();
    let project = ProjectId::from_root(&root).to_string();
    let mut paths: Vec<PathBuf> = Vec::new();
    for n in 0..worktrees {
        let path = fixture.add_worktree(&format!("acme-wt{n}"), &format!("fix/export-{n}"));
        fixture.git_in(&path, &["config", "commit.gpgsign", "false"]);
        std::fs::create_dir_all(path.join("src")).unwrap();
        std::fs::write(
            path.join("src/export.rs"),
            "fn export() {}\nfn header() {}\n",
        )
        .unwrap();
        fixture.git_in(&path, &["add", "."]);
        fixture.git_in(&path, &["commit", "-m", "Add export"]);
        paths.push(path);
    }
    let listed = app.git.list_worktrees(&root).unwrap();
    let worktrees = paths
        .iter()
        .map(|path| {
            let canonical = path.canonicalize().unwrap();
            listed
                .iter()
                .find(|w| w.path.canonicalize().unwrap() == canonical)
                .unwrap()
                .id
                .as_str()
                .to_owned()
        })
        .collect();

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
        worktrees,
        _fixture: fixture,
        _data: data,
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
            nudges: 0,
        },
    );
    *host.engine.lock() = Arc::downgrade(&engine);
    engine
}

impl Harness {
    fn start_on(&self, worktree: usize, recipe: &str) -> FlowId {
        self.engine
            .start(&StartReview {
                project: self.project.clone(),
                worktree: self.worktrees[worktree].clone(),
                recipe: recipe.to_owned(),
                base: None,
            })
            .unwrap()
    }

    fn start(&self, recipe: &str) -> FlowId {
        self.start_on(0, recipe)
    }

    fn waiting(&self, flow: FlowId, role: &str) -> FlowView {
        until(&self.engine, flow, "waiting for capacity", |v| {
            v.steps
                .iter()
                .any(|s| s.role == role && s.status == StepStatus::WaitingCapacity)
                && v.inbox.iter().any(|i| i.kind == InboxKind::Capacity)
        })
    }

    fn briefs(&self, role: &str) -> Vec<String> {
        self.host
            .opened
            .lock()
            .iter()
            .filter(|o| o.role == role)
            .map(|o| o.brief.clone())
            .collect()
    }
}

/// Wait, in real time, for the engine's threads to get somewhere. Bounded, so a property that never
/// holds fails the test rather than hanging it.
fn until(engine: &Engine, flow: FlowId, what: &str, done: impl Fn(&FlowView) -> bool) -> FlowView {
    for _ in 0..4_000 {
        let view = engine.view(flow).unwrap();
        if done(&view) {
            return view;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("never {what}: {:#?}", engine.view(flow).unwrap());
}

fn consolidated(engine: &Engine, flow: FlowId) -> FlowView {
    until(engine, flow, "consolidated", |v| {
        v.flow.state.phase == Phase::Consolidated && !v.running
    })
}

fn step<'a>(view: &'a FlowView, role: &str) -> &'a wtm_core::flow::Step {
    view.steps
        .iter()
        .find(|s| s.role == role)
        .unwrap_or_else(|| panic!("no step {role}: {:#?}", view.steps))
}

fn statuses(view: &FlowView, role: &str) -> Vec<InvocationStatus> {
    let id = step(view, role).id;
    view.invocations
        .iter()
        .filter(|i| i.step == id)
        .map(|i| i.status)
        .collect()
}

fn open_capacity_items(view: &FlowView) -> usize {
    view.inbox
        .iter()
        .filter(|i| i.kind == InboxKind::Capacity && i.state == InboxState::Open)
        .count()
}

// ═══════════════════════════════ fallback chains ═══════════════════════════════

#[test]
fn a_limit_walks_the_roles_own_chain_in_order_and_never_back_to_a_provider_that_ran_out() {
    let h = harness(
        Box::new(|role, _| match role {
            "correctness" => Behaviour::Limit(None),
            // Fails on its own account, after its retries: the chain carries on from the same list.
            "correctness-claude" => Behaviour::EndWithoutSubmitting,
            _ => Behaviour::Submit(vec![finding(&format!("from {role}"))]),
        }),
        "",
        1,
    );

    let flow = h.start("chain");
    let view = consolidated(&h.engine, flow);

    assert_eq!(
        h.host.opened_roles(),
        [
            "conventions",
            "correctness",
            "correctness-claude",
            "correctness-haiku"
        ],
        "the Codex alternative is passed over: Codex had just run out"
    );
    assert_eq!(step(&view, "correctness").status, StepStatus::Replaced);
    assert_eq!(
        step(&view, "correctness-claude").status,
        StepStatus::Replaced
    );
    assert_eq!(step(&view, "correctness-haiku").status, StepStatus::Done);
    for role in ["correctness-claude", "correctness-haiku"] {
        assert_eq!(
            step(&view, role).stands_in_for.as_deref(),
            Some("correctness"),
            "{role} stands in for the role whose work it is"
        );
    }
    assert!(
        view.steps
            .iter()
            .all(|s| s.role != "correctness-codex-mini"),
        "no step for the alternative on the provider that ran out"
    );

    let switches: Vec<_> = view
        .switches
        .iter()
        .map(|s| {
            (
                s.handover.from.role.as_str(),
                s.handover.to.role.as_str(),
                s.reason,
                s.by,
            )
        })
        .collect();
    assert_eq!(
        switches,
        [
            (
                "correctness",
                "correctness-claude",
                SwitchReason::Limit,
                SwitchedBy::Config
            ),
            (
                "correctness-claude",
                "correctness-haiku",
                SwitchReason::Failure,
                SwitchedBy::Config
            ),
        ]
    );
    assert!(view.flow.state.hold.is_none(), "it never had to wait");
}

#[test]
fn a_chain_with_every_alternative_on_spent_providers_waits_and_asks_rather_than_switching() {
    let h = harness(
        Box::new(|role, _| match role {
            "correctness" | "correctness-claude" => Behaviour::Limit(None),
            _ => Behaviour::Submit(vec![finding("x")]),
        }),
        "",
        1,
    );
    let flow = h.start("chain");
    let view = h.waiting(flow, "correctness-claude");

    assert_eq!(
        h.host.opened_roles(),
        ["conventions", "correctness", "correctness-claude"],
        "Claude ran out too, so the Claude alternative is passed over as well"
    );
    let capacity = view.capacity.as_ref().unwrap();
    assert_eq!(capacity.role, "correctness-claude");
    let passed: Vec<_> = capacity
        .fallbacks
        .iter()
        .map(|f| (f.role.as_str(), f.passed_over.is_some()))
        .collect();
    assert_eq!(
        passed,
        [
            ("correctness-codex-mini", true),
            ("correctness-claude", true),
            ("correctness-haiku", true),
        ]
    );
    assert_eq!(
        view.flow.state.hold,
        Some(Hold::WaitingCapacity {
            provider: "claude".to_owned(),
            resets_at: None,
        })
    );
    h.engine.cancel(flow).unwrap();
}

// ═══════════════════════════════ the handover ═══════════════════════════════

#[test]
fn the_role_that_takes_over_is_handed_the_runs_own_record_and_no_conversation() {
    // First a review in which the role asks something, and you answer: an answer about the work
    // goes with the work, to whoever runs the role next.
    let h = harness(
        Box::new(|role, _| match role {
            "correctness" => Behaviour::AskThenSubmit(
                json!({
                    "question": "Does an empty export still write its header row?",
                    "why": "It decides whether the missing row is a bug",
                    "unblocks": "the empty-export finding",
                    "refs": ["src/export.rs:2"],
                }),
                finding("first round"),
            ),
            _ => Behaviour::Submit(vec![finding(&format!("from {role}"))]),
        }),
        "",
        1,
    );
    let first = h.start("chain");
    consolidated(&h.engine, first);
    let db = h.app.appdb.get().unwrap();
    let question = db.questions(first).unwrap().remove(0);
    h.engine
        .answer(question.id, None, "Yes, always the header row")
        .unwrap();

    h.host.rescript(Box::new(|role, _| match role {
        "conventions" => Behaviour::Submit(vec![finding("The header row is dropped")]),
        "correctness" => Behaviour::Limit(None),
        _ => Behaviour::Submit(vec![finding(&format!("from {role}"))]),
    }));
    let flow = h.start("chain");
    let view = consolidated(&h.engine, flow);

    let switch = &view.switches[0];
    let handover = &switch.handover;
    assert_eq!(handover.from.provider, RoleProvider::Codex);
    assert_eq!(handover.to.role, "correctness-claude");
    assert_eq!(handover.objective, "Find bugs the change introduces");
    assert_eq!(handover.progress, ["reading export.py"]);
    assert_eq!(handover.plan, None, "a review carries out no plan");
    let snapshot = view.snapshot.as_ref().unwrap();
    assert_eq!(handover.snapshot.as_ref().unwrap().tree, snapshot.tree_sha);
    let diff = handover.diff.as_ref().expect("git said what changed");
    assert!(
        diff.iter()
            .any(|f| f.path == "src/export.rs" && f.added == Some(2)),
        "{diff:?}"
    );
    assert_eq!(handover.findings.len(), 1);
    assert_eq!(handover.findings[0].title, "The header row is dropped");
    assert_eq!(handover.findings[0].by, ["conventions"]);
    assert_eq!(handover.answers.len(), 1);
    assert_eq!(handover.answers[0].answer, "Yes, always the header row");

    let brief = h.briefs("correctness-claude").remove(0);
    for said in [
        "You take over another session's work.",
        "Snapshot: head",
        "Diff stat: 1 file, +2 −0",
        "The header row is dropped at src/export.rs:1 (conventions)",
        "Does an empty export still write its header row?: Yes, always the header row",
        "conversation is not transferred",
    ] {
        assert!(brief.contains(said), "missing {said:?} in:\n{brief}");
    }
    assert_eq!(
        step(&view, "correctness-claude").handover.as_deref(),
        Some(handover.render().as_str()),
        "the step keeps exactly what the brief carried"
    );
}

// ═══════════════════════════════ credits ═══════════════════════════════

#[test]
fn a_model_that_bills_usage_credits_is_refused_without_its_roles_opt_in_and_nothing_is_recorded() {
    let h = harness(
        Box::new(|_, _| Behaviour::Submit(vec![finding("x")])),
        "",
        1,
    );

    let refused = h
        .engine
        .start(&StartReview {
            project: h.project.clone(),
            worktree: h.worktrees[0].clone(),
            recipe: "costly".to_owned(),
            base: None,
        })
        .unwrap_err();
    assert_eq!(refused.credits.len(), 1);
    assert_eq!(refused.credits[0].role, "costly");
    assert!(
        refused.message.contains("allow_credit_billing = true"),
        "{}",
        refused.message
    );
    assert!(
        h.engine
            .latest(&h.project, &h.worktrees[0])
            .unwrap()
            .is_none()
    );
    assert!(h.host.opened_roles().is_empty());

    let flow = h.start("costly-ok");
    let view = consolidated(&h.engine, flow);
    assert_eq!(step(&view, "costly-ok").status, StepStatus::Done);
}

#[test]
fn a_session_that_reports_a_credit_billing_model_the_role_never_named_is_closed_and_its_role_fails()
{
    let h = harness(Box::new(|_, _| Behaviour::Silent), "", 1);
    h.host
        .ready_models
        .lock()
        .insert("claude".to_owned(), "claude-fable-5-1".to_owned());

    let flow = h.start("unnamed");
    let view = consolidated(&h.engine, flow);

    let unnamed = step(&view, "unnamed");
    assert_eq!(unnamed.status, StepStatus::Failed);
    assert!(
        unnamed
            .detail
            .as_deref()
            .unwrap()
            .contains("bills usage credits"),
        "{unnamed:?}"
    );
    assert_eq!(
        h.host.open_now.load(Ordering::SeqCst),
        0,
        "its session was closed"
    );
    assert!(view.inbox.iter().any(|i| i.kind == InboxKind::Failure));
}

// ═══════════════════════════════ caps ═══════════════════════════════

#[test]
fn work_over_the_total_cap_waits_visibly_and_runs_when_a_place_frees() {
    let go = Arc::new(AtomicBool::new(false));
    let held = Arc::clone(&go);
    // Both roles stay mid-turn until released, so whichever asked first holds the one place and
    // the other is seen waiting, whatever order their threads happened to start in.
    let h = harness(
        Box::new(move |role, _| Behaviour::Hold(Arc::clone(&held), finding(role))),
        "[run_sessions]\ntotal = 1\n",
        1,
    );

    let flow = h.start("lonely-pair");
    let queued = until(&h.engine, flow, "one role queued", |v| {
        !v.queued.is_empty() && h.host.open_now.load(Ordering::SeqCst) == 1
    });
    assert_eq!(queued.queued.len(), 1);
    assert_eq!(queued.queued[0].words, "queued: 1 of 1 run sessions open");
    assert_eq!(queued.holding.len(), 1);
    let waiting = queued.queued[0].seat.step.unwrap();
    assert_eq!(
        queued
            .steps
            .iter()
            .find(|s| s.id == waiting)
            .unwrap()
            .status,
        StepStatus::Pending,
        "a queued role has not started: no invocation, no session"
    );
    assert!(
        !queued.invocations.iter().any(|i| i.step == waiting),
        "nothing is recorded as opening while it waits"
    );

    go.store(true, Ordering::SeqCst);
    let view = consolidated(&h.engine, flow);
    assert_eq!(h.host.open_most.load(Ordering::SeqCst), 1);
    assert!(view.queued.is_empty() && view.holding.is_empty());
    assert!(
        h.heard
            .filed
            .lock()
            .iter()
            .all(|i| i.kind != InboxKind::Capacity),
        "a queue is routine: it files nothing"
    );
}

#[test]
fn sessions_waiting_for_a_providers_places_get_them_oldest_first_across_runs() {
    let go = Arc::new(AtomicBool::new(false));
    let held = Arc::clone(&go);
    let h = harness(
        Box::new(move |role, _| match role {
            "lonely" => Behaviour::Hold(Arc::clone(&held), finding("first")),
            _ => Behaviour::Submit(vec![finding(role)]),
        }),
        "[run_sessions]\ncodex = 1\n",
        3,
    );

    let first = h.start_on(0, "lonely");
    until(&h.engine, first, "the first holding Codex's place", |v| {
        v.holding.len() == 1
    });
    let second = h.start_on(1, "second");
    until(&h.engine, second, "the second queued", |v| {
        !v.queued.is_empty()
    });
    let third = h.start_on(2, "third");
    let view = until(&h.engine, third, "the third queued", |v| {
        !v.queued.is_empty()
    });
    assert_eq!(view.queued[0].words, "queued: Codex at 1/1");
    assert_eq!(view.queued[0].position, 2, "behind the second run's role");
    let everyone = h.engine.seats_view();
    assert_eq!(everyone.seating.queued.len(), 2);
    assert_eq!(everyone.seating.queued[0].seat.role, "second");
    assert!(everyone.labels.contains_key(&first));

    go.store(true, Ordering::SeqCst);
    for flow in [first, second, third] {
        consolidated(&h.engine, flow);
    }
    assert_eq!(h.host.opened_roles(), ["lonely", "second", "third"]);
    assert_eq!(h.host.open_most.load(Ordering::SeqCst), 1);
}

#[test]
fn a_fallbacks_session_is_counted_with_its_runs_other_sessions_and_gets_no_place_of_its_own() {
    // One Claude place, held by the run's own conventions role. The Codex role beside it reaches
    // its limit and hands over to a Claude fallback: the child waits for the parent run's Claude
    // place like any other session, and the Codex place its parent held is not lent to it.
    let go = Arc::new(AtomicBool::new(false));
    let held = Arc::clone(&go);
    let h = harness(
        Box::new(move |role, _| match role {
            "conventions" => Behaviour::Hold(Arc::clone(&held), finding("held")),
            "limited" => Behaviour::Limit(None),
            _ => Behaviour::Submit(vec![finding(role)]),
        }),
        "[run_sessions]\nclaude = 1\n",
        1,
    );

    let flow = h.start("parent-child");
    let view = until(&h.engine, flow, "the fallback queued", |v| {
        v.queued.iter().any(|q| q.seat.role == "limited-on-claude")
            && v.steps
                .iter()
                .any(|s| s.role == "limited" && s.status == StepStatus::Replaced)
    });
    let child = view
        .queued
        .iter()
        .find(|q| q.seat.role == "limited-on-claude")
        .unwrap();
    assert_eq!(child.words, "queued: Claude at 1/1");
    assert_eq!(step(&view, "limited").status, StepStatus::Replaced);
    assert_eq!(
        view.holding
            .iter()
            .map(|s| s.role.as_str())
            .collect::<Vec<_>>(),
        ["conventions"],
        "the parent let its Codex place go when its session ended"
    );

    go.store(true, Ordering::SeqCst);
    let view = consolidated(&h.engine, flow);
    assert_eq!(step(&view, "limited-on-claude").status, StepStatus::Done);
}

#[test]
fn a_role_waiting_at_a_limit_holds_no_place_so_the_runs_other_roles_are_not_starved() {
    let h = harness(
        Box::new(|role, _| match role {
            "lonely" => Behaviour::Limit(Some(START_S + 600)),
            _ => Behaviour::Submit(vec![finding(role)]),
        }),
        "[run_sessions]\ntotal = 1\n",
        1,
    );
    let flow = h.start("lonely-pair");
    let view = until(&h.engine, flow, "conventions done beside the wait", |v| {
        let status = |role: &str| v.steps.iter().find(|s| s.role == role).map(|s| s.status);
        status("conventions") == Some(StepStatus::Done)
            && status("lonely") == Some(StepStatus::WaitingCapacity)
    });
    assert!(view.holding.is_empty(), "the waiting role holds nothing");
    h.engine.cancel(flow).unwrap();
}

// ═══════════════════════════════ waiting, sleeping, relaunching ═══════════════════════════════

#[test]
fn a_reset_that_passes_while_wtm_is_open_and_awake_runs_the_role_without_asking_again() {
    let h = harness(
        Box::new(|role, attempt| match (role, attempt) {
            ("lonely", 1) => Behaviour::Limit(Some(START_S + 600)),
            _ => Behaviour::Submit(vec![finding("after the reset")]),
        }),
        "",
        1,
    );
    let flow = h.start("lonely");
    h.waiting(flow, "lonely");
    h.clock.advance_ms(600_001);
    let view = consolidated(&h.engine, flow);
    assert_eq!(
        statuses(&view, "lonely"),
        [InvocationStatus::LimitReached, InvocationStatus::Submitted]
    );
    assert_eq!(h.host.limits_asked.load(Ordering::SeqCst), 0);
}

#[test]
fn a_mac_that_slept_past_the_reset_asks_the_provider_before_the_role_runs_again() {
    let h = harness(
        Box::new(|role, attempt| match (role, attempt) {
            ("lonely", 1) => Behaviour::Limit(Some(START_S + 600)),
            _ => Behaviour::Submit(vec![finding("after the recheck")]),
        }),
        "",
        1,
    );
    h.host.readings.lock().extend([
        LimitReading::Limited {
            resets_at: Some(START_S + 7_200),
        },
        LimitReading::Available,
    ]);
    let flow = h.start("lonely");
    h.waiting(flow, "lonely");

    // Asleep for twenty minutes: the reset passed with the lid shut, and that is not running time.
    h.clock.sleep_ms(1_200_000);
    let view = until(&h.engine, flow, "rechecked", |v| {
        step(v, "lonely").resets_at == Some(START_S + 7_200)
    });
    assert_eq!(
        h.host.attempts_of("lonely"),
        1,
        "nothing ran on the old reset"
    );
    assert_eq!(h.host.limits_asked.load(Ordering::SeqCst), 1);
    assert!(
        view.capacity
            .as_ref()
            .unwrap()
            .recheck
            .as_deref()
            .unwrap()
            .contains("still at its limit")
    );
    assert_eq!(
        view.flow.state.hold,
        Some(Hold::WaitingCapacity {
            provider: "codex".to_owned(),
            resets_at: Some(START_S + 7_200),
        }),
        "the hold names the reset the provider gave now"
    );

    // Asleep again, past the new reset: this time the provider has room.
    h.clock.sleep_ms(7_200_000);
    let view = consolidated(&h.engine, flow);
    assert_eq!(h.host.limits_asked.load(Ordering::SeqCst), 2);
    assert_eq!(
        statuses(&view, "lonely"),
        [InvocationStatus::LimitReached, InvocationStatus::Submitted]
    );
    assert_eq!(open_capacity_items(&view), 0);
}

#[test]
fn a_wait_at_a_limit_survives_a_quit_and_the_next_launch_asks_the_provider_before_running_it() {
    let h = harness(
        Box::new(|role, attempt| match (role, attempt) {
            ("lonely", 1) => Behaviour::Limit(Some(START_S + 600)),
            _ => Behaviour::Submit(vec![finding("after the relaunch")]),
        }),
        "",
        1,
    );
    let flow = h.start("lonely");
    h.waiting(flow, "lonely");

    h.engine.shutdown();
    let view = h.engine.view(flow).unwrap();
    assert!(
        matches!(view.flow.state.hold, Some(Hold::WaitingCapacity { .. })),
        "no turn was cut off, so the quit is not an interruption: {:?}",
        view.flow.state.hold
    );
    assert_eq!(step(&view, "lonely").status, StepStatus::WaitingCapacity);
    assert!(view.inbox.iter().all(|i| i.kind != InboxKind::Interrupted));
    until(&h.engine, flow, "the old engine's thread gone", |v| {
        !v.running
    });

    // A process killed mid-wait never lets its reviewer lease go; plant the one it would leave.
    let db = h.app.appdb.get().unwrap();
    let worktree = db.review(flow).unwrap().unwrap().worktree_path;
    db.acquire_lease(
        &wtm_core::flow::NewLease {
            worktree: worktree.clone(),
            flow,
            kind: wtm_core::flow::LeaseKind::Reviewer,
            holder: "lonely".to_owned(),
        },
        h.clock.now_unix_ms(),
    )
    .unwrap();

    // wtm was closed through the reset. The next launch asks before it runs anything.
    h.clock.advance_ms(3_600_000);
    h.host.readings.lock().push_back(LimitReading::Available);
    let relaunched = engine_for(&h.app, &h.host, &h.clock, &h.heard);
    assert_eq!(
        relaunched.recover().unwrap(),
        [],
        "nothing to call interrupted"
    );
    let view = consolidated(&relaunched, flow);
    assert_eq!(h.host.limits_asked.load(Ordering::SeqCst), 1);
    assert_eq!(
        statuses(&view, "lonely"),
        [InvocationStatus::LimitReached, InvocationStatus::Submitted]
    );
    assert!(view.flow.state.hold.is_none());
    assert!(
        db.open_leases(Some(&worktree)).unwrap().is_empty(),
        "the dead process's lease was let go, and the picked-up run's with its end"
    );
}

#[test]
fn at_launch_a_provider_that_cannot_say_whether_it_reset_leaves_the_role_waiting_for_you() {
    let h = harness(
        Box::new(|role, attempt| match (role, attempt) {
            ("lonely", 1) => Behaviour::Limit(Some(START_S + 600)),
            _ => Behaviour::Submit(vec![finding("tried by you")]),
        }),
        "",
        1,
    );
    let flow = h.start("lonely");
    h.waiting(flow, "lonely");
    h.engine.shutdown();
    until(&h.engine, flow, "the old engine's thread gone", |v| {
        !v.running
    });

    h.clock.advance_ms(3_600_000);
    let relaunched = engine_for(&h.app, &h.host, &h.clock, &h.heard);
    relaunched.recover().unwrap();
    let view = until(&relaunched, flow, "asking you", |v| {
        step(v, "lonely").resets_at.is_none()
            && step(v, "lonely")
                .detail
                .as_deref()
                .is_some_and(|d| d.contains("did not say"))
    });
    assert_eq!(
        h.host.attempts_of("lonely"),
        1,
        "an unknown reading is never a guess"
    );
    assert_eq!(step(&view, "lonely").status, StepStatus::WaitingCapacity);
    std::thread::sleep(std::time::Duration::from_millis(50));
    assert_eq!(h.host.attempts_of("lonely"), 1);

    relaunched.resume(flow).unwrap();
    let view = consolidated(&relaunched, flow);
    assert_eq!(step(&view, "lonely").status, StepStatus::Done);
}

// ═══════════════════════════════ your choice at a limit ═══════════════════════════════

#[test]
fn with_no_fallback_you_can_run_the_role_once_on_another_provider_and_a_rerun_goes_back() {
    let h = harness(
        Box::new(|role, attempt| match (role, attempt) {
            ("lonely", 1) => Behaviour::Limit(Some(START_S + 600)),
            _ => Behaviour::Submit(vec![finding(&format!("attempt {attempt}"))]),
        }),
        "",
        1,
    );
    let flow = h.start("lonely");
    let view = h.waiting(flow, "lonely");
    let waiting = step(&view, "lonely").id;
    assert!(view.capacity.as_ref().unwrap().fallbacks.is_empty());
    let item = view
        .inbox
        .iter()
        .find(|i| i.kind == InboxKind::Capacity)
        .unwrap();
    assert!(
        item.body.contains("No fallback is configured"),
        "{}",
        item.body
    );
    assert!(item.body.contains("run it once on another provider"));

    // The same place it ran out is refused: it would meet the same limit.
    assert!(
        h.engine
            .choose_capacity(
                flow,
                waiting,
                &CapacityChoice::RunOnce {
                    provider: RoleProvider::Codex,
                    model: None
                }
            )
            .is_err()
    );
    // A credit-billing model the role has not opted into is refused, with the reason.
    let refused = h
        .engine
        .choose_capacity(
            flow,
            waiting,
            &CapacityChoice::RunOnce {
                provider: RoleProvider::Claude,
                model: Some("fable".to_owned()),
            },
        )
        .unwrap_err();
    assert!(
        refused.message.contains("bills usage credits"),
        "{}",
        refused.message
    );
    assert_eq!(
        step(&h.engine.view(flow).unwrap(), "lonely").status,
        StepStatus::WaitingCapacity
    );

    h.engine
        .choose_capacity(
            flow,
            waiting,
            &CapacityChoice::RunOnce {
                provider: RoleProvider::Claude,
                model: Some("opus".to_owned()),
            },
        )
        .unwrap();
    let view = consolidated(&h.engine, flow);
    let opened = h.host.opened.lock().last().cloned().unwrap();
    assert_eq!(
        (opened.provider.as_str(), opened.model.as_deref()),
        ("claude", Some("opus"))
    );
    assert!(
        opened
            .brief
            .contains("You take over another session's work.")
    );
    assert!(
        opened.brief.contains("you chose to run it here once"),
        "{}",
        opened.brief
    );
    let once = step(&view, "lonely");
    assert_eq!(once.status, StepStatus::Done);
    assert_eq!(once.provider, RoleProvider::Claude);
    assert_eq!(
        view.switches
            .iter()
            .map(|s| (s.reason, s.by, s.from_step == s.to_step))
            .collect::<Vec<_>>(),
        [(SwitchReason::Once, SwitchedBy::You, true)]
    );
    assert_eq!(open_capacity_items(&view), 0);

    h.engine.rerun(flow, Rerun::All).unwrap();
    let view = consolidated(&h.engine, flow);
    assert_eq!(
        step(&view, "lonely").provider,
        RoleProvider::Codex,
        "once was that round"
    );
    assert_eq!(h.host.opened.lock().last().unwrap().provider, "codex");
}

#[test]
fn finishing_without_a_role_at_its_limit_consolidates_the_review_and_keeps_why() {
    let h = harness(
        Box::new(|role, _| match role {
            "lonely" => Behaviour::Limit(None),
            _ => Behaviour::Submit(vec![finding(role)]),
        }),
        "",
        1,
    );
    let flow = h.start("lonely-pair");
    let view = h.waiting(flow, "lonely");
    let waiting = step(&view, "lonely").id;

    h.engine
        .choose_capacity(flow, waiting, &CapacityChoice::Skip)
        .unwrap();
    let view = consolidated(&h.engine, flow);
    let skipped = step(&view, "lonely");
    assert_eq!(skipped.status, StepStatus::Skipped);
    assert!(skipped.detail.as_deref().unwrap().contains("without it"));
    assert_eq!(step(&view, "conventions").status, StepStatus::Done);
    assert!(view.flow.state.hold.is_none());
    assert_eq!(open_capacity_items(&view), 0);
    assert_eq!(h.host.attempts_of("lonely"), 1);
}

#[test]
fn choosing_to_wait_puts_the_question_away_and_the_role_still_runs_at_its_reset() {
    let h = harness(
        Box::new(|role, attempt| match (role, attempt) {
            ("lonely", 1) => Behaviour::Limit(Some(START_S + 600)),
            _ => Behaviour::Submit(vec![finding("at the reset")]),
        }),
        "",
        1,
    );
    let flow = h.start("lonely");
    let view = h.waiting(flow, "lonely");
    let waiting = step(&view, "lonely").id;
    let item = view
        .inbox
        .iter()
        .find(|i| i.kind == InboxKind::Capacity)
        .unwrap()
        .id;

    h.engine
        .choose_capacity(flow, waiting, &CapacityChoice::Wait)
        .unwrap();
    let view = h.engine.view(flow).unwrap();
    assert_eq!(open_capacity_items(&view), 0);
    let db = h.app.appdb.get().unwrap();
    assert_eq!(
        db.inbox_item(item).unwrap().unwrap().state,
        InboxState::Dismissed,
        "put away, not settled: the limit still holds"
    );
    assert_eq!(step(&view, "lonely").status, StepStatus::WaitingCapacity);

    h.clock.advance_ms(600_001);
    let view = consolidated(&h.engine, flow);
    assert_eq!(step(&view, "lonely").status, StepStatus::Done);
}
