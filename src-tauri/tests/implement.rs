//! Implementing an issue through the real `App` and engine: a real repository and Start Work, the
//! real app database, run tools, checks and review engine, a fake tracker, and fake agents in place
//! of the CLIs. The writer's edits are real files in the issue's worktree, and its commits are the
//! app's, made with git.
//!
//! What is protected is the plan's I5 scenario and what it rests on:
//!
//! - **Nothing implements before the gate.** No writer, lease or implementation before the plan is
//!   approved, nor before the design is, when the plan needs one; the approval that opens the gate
//!   starts the writer.
//! - **The loop is the plan's.** Checks run by the app, a review of the new tree, fixes within the
//!   plan through the same writer and conversation, and only the checks and review roles a fix round
//!   touched run again.
//! - **Work outside the plan is a question.** A commit outside it is refused; handing in with it
//!   holds the run; "change the plan" records the next plan revision and sends the run back for
//!   approval, and "keep to the plan" tells the writer to put it back. A finding outside the plan is
//!   asked about and left, not fixed.
//! - **The loop stops at its bounds**, with what is left listed and why, and never loops.
//! - **The lease outlives a crash**, and Resume carries the writer on in its conversation.

#![allow(clippy::unwrap_used, clippy::too_many_lines)]

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use wtm_app_lib::app::{App, WORK_VIEW_PREF};
use wtm_app_lib::flow::host::RoleHost;
use wtm_app_lib::flow::implement::ImplementView;
use wtm_app_lib::flow::start::{StartOutcome, StartWork};
use wtm_app_lib::flow::{Engine, Notice, Tuning};
use wtm_app_lib::tracker;
use wtm_config::AppPaths;
use wtm_core::flow::{
    CheckStatus, FlowId, Hold, ImplementAsk, ImplementOutcome, InboxKind, Issue, IssueStatus,
    LeaseKind, LeftBecause, Phase, RoundKind, Sprint, SprintState, StatusCategory, StopReason,
};
use wtm_core::model::ProjectId;
use wtm_core::ports::clock::Clock;
use wtm_core::ports::config::{ConfigStore, TrustDecision};
use wtm_core::ports::{ImplementStore, LeaseStore, PlanStore, Tracker, TrackerWriter};
use wtm_core::usecase::SyncReason;
use wtm_testkit::{FakeClock, FakeTracker, GitFixture};

mod common;
use common::agents::{Act, Behaviour, FakeHost, Heard, Script};

// ═══════════════════════════════ the harness ═══════════════════════════════

/// The New Worktree form, as Start Work fills it.
const CONFIG: &str = r#"
[[field]]
key = "issue"
label = "Issue"
kind = "text"
required = true
pattern = "^[A-Z]+-[0-9]+$"

[[field]]
key = "base"
label = "Base branch"
kind = "text"
required = true
default = "main"

[naming]
branch = "feature/{{ issue }}"
directory = "{{ issue }}"
branch_must_match = "^[A-Za-z0-9][A-Za-z0-9._/-]*$"
"#;

/// The tracker, a planner and a designer, the implementer and its review recipe, and three checks:
/// `lint` reads everything, `src-test` only `src/`, `docs-check` only `docs/`. `src-test` fails
/// until the export says it was fixed, so the first round always has a check to make pass.
const WORKFLOW: &str = "tracker = \"jira\"\n\
     planner = \"planner\"\n\
     {designer}\
     [projects.\"{root}\".config.workflow.statuses]\n\
     todo = \"To Do\"\n\
     started = \"In Progress\"\n\
     needs_review = \"In Review\"\n\
     done = [\"Done\"]\n\
     [projects.\"{root}\".config.workflow.queue]\n\
     query = \"assignee = currentUser()\"\n\
     board = 3\n\
     [projects.\"{root}\".config.workflow.implement]\n\
     writer = \"implementer\"\n\
     recipe = \"standard\"\n\
     fix_rounds = {rounds}\n\
     fix_attempts = {attempts}\n\
     [projects.\"{root}\".config.workflow.role.planner]\n\
     objective = \"Investigate the issue and plan its implementation\"\n\
     provider = \"claude\"\n\
     inputs = [\"issue\"]\n\
     output = \"plan\"\n\
     timeout_ms = 60000\n\
     [projects.\"{root}\".config.workflow.role.designer]\n\
     objective = \"Draw the screens the approved plan describes\"\n\
     provider = \"claude\"\n\
     inputs = [\"issue\", \"plan\"]\n\
     timeout_ms = 60000\n\
     [projects.\"{root}\".config.workflow.role.implementer]\n\
     objective = \"Implement the approved plan\"\n\
     provider = \"claude\"\n\
     profile = \"writer\"\n\
     inputs = [\"issue\", \"plan\", \"design\"]\n\
     timeout_ms = 60000\n\
     [projects.\"{root}\".config.workflow.role.correctness]\n\
     objective = \"Find bugs the change introduces\"\n\
     provider = \"claude\"\n\
     inputs = [\"diff\", \"plan\"]\n\
     timeout_ms = 60000\n\
     [projects.\"{root}\".config.workflow.role.docs-review]\n\
     objective = \"Check the documentation the change adds\"\n\
     provider = \"claude\"\n\
     inputs = [\"diff\"]\n\
     paths = [\"docs/**\"]\n\
     timeout_ms = 60000\n\
     [[projects.\"{root}\".config.workflow.recipe.standard.stage]]\n\
     roles = [\"correctness\", \"docs-review\"]\n\
     [projects.\"{root}\".config.workflow.check.lint]\n\
     run = [\"true\"]\n\
     [projects.\"{root}\".config.workflow.check.src-test]\n\
     run = [\"sh\", \"-c\", \"grep -q fixed src/export.rs\"]\n\
     paths = [\"src/**\"]\n\
     [projects.\"{root}\".config.workflow.check.docs-check]\n\
     run = [\"true\"]\n\
     paths = [\"docs/**\"]\n";

const KEY: &str = "ACME-9";

fn plan(design_needed: bool) -> Value {
    json!({
        "kind": "plan",
        "payload": {
            "summary": "Export the units as CSV, with a page in the docs",
            "steps": ["Write the exporter in src/export.rs", "Document it in docs/export.md"],
            "acceptanceCriteria": ["The export lists every unit"],
            "verification": ["`src-test` passes"],
            "areas": ["src/**", "docs/**"],
            "designNeeded": design_needed,
            "designReason": "An export, not a screen",
        },
    })
}

fn no_findings() -> Value {
    json!({ "findings": [] })
}

fn bug_at(path: &str, title: &str) -> Value {
    json!({ "findings": [{
        "title": title,
        "severity": "notable",
        "class": "confirmed_bug",
        "verification": "ran the export over three units; one was missing",
        "body": "The loop stops one unit early.",
        "path": path,
        "line": 1,
        "endLine": 1,
    }]})
}

/// The first round's work: the exporter and its page, checkpointed, handed in.
fn first_round() -> Behaviour {
    Behaviour::Acts(vec![
        Act::Write("src/export.rs", "pub fn export() {}\n"),
        Act::Write("docs/export.md", "# Export\n"),
        Act::Checkpoint("Export the units"),
        Act::Handoff("Added the exporter and its page"),
    ])
}

/// A fix round's work: the export says it was fixed, which `src-test` looks for.
fn fixing() -> Behaviour {
    Behaviour::Acts(vec![
        Act::Write("src/export.rs", "pub fn export() {} // fixed\n"),
        Act::Checkpoint("Export every unit"),
        Act::Handoff("Fixed the off-by-one"),
    ])
}

/// Planner, designer and reviewers as most tests want them; the writer by `writer`.
fn script(writer: impl Fn(u32) -> Behaviour + Send + Sync + 'static) -> Script {
    Box::new(move |role, attempt| match role {
        "planner" => Behaviour::Tools(vec![("flow_submit", plan(false))], true),
        "implementer" => writer(attempt),
        "correctness" if attempt == 1 => {
            Behaviour::Submit(vec![bug_at("src/export.rs", "Off by one in the export")])
        }
        "correctness" | "docs-review" => Behaviour::Submit(vec![no_findings()]),
        _ => Behaviour::Silent,
    })
}

struct Harness {
    _fixture: GitFixture,
    app: Arc<App>,
    engine: Arc<Engine>,
    host: Arc<FakeHost>,
    project: String,
    _config: tempfile::TempDir,
}

impl Harness {
    fn new(script: Script) -> Self {
        Self::configured(script, false, 2, 1)
    }

    fn configured(script: Script, designer: bool, rounds: u32, attempts: u32) -> Self {
        let fixture = GitFixture::new();
        fixture.commit("wtm.toml", CONFIG, "add wtm config");
        fixture.commit("README.md", "# Units\n", "a readme");
        fixture.commit("src/lib.rs", "pub mod units;\n", "a library");
        let config = tempfile::tempdir().unwrap();
        let paths = AppPaths::rooted(config.path());
        let config_file = paths.config_file.clone();

        let mut app = App::with_paths(paths).unwrap();
        let tracker = Arc::new(
            FakeTracker::new()
                .with_sprint(Sprint {
                    id: 41,
                    name: "Sprint 41".to_owned(),
                    state: SprintState::Active,
                    goal: None,
                    starts_at: None,
                    ends_at: None,
                    board: Some(3),
                })
                .with_issue(
                    Issue {
                        key: KEY.to_owned(),
                        summary: "Export the unit list".to_owned(),
                        status: IssueStatus {
                            name: "To Do".to_owned(),
                            category: StatusCategory::ToDo,
                        },
                        kind: Some("Story".to_owned()),
                        priority: None,
                        assignee: None,
                        labels: Vec::new(),
                    },
                    &[41],
                ),
        );
        tracker.describe(KEY, "Export the units as CSV.");
        app.tracker = Arc::clone(&tracker) as Arc<dyn Tracker>;
        app.tracker_writer = Arc::clone(&tracker) as Arc<dyn TrackerWriter>;

        let root = app.register(fixture.root()).unwrap();
        app.config
            .set_trust(&root.join("wtm.toml"), TrustDecision::Approve)
            .unwrap();
        app.config.set_user_pref(WORK_VIEW_PREF, "on").unwrap();
        let mut text = std::fs::read_to_string(&config_file).unwrap_or_default();
        let shown = root.display().to_string();
        write!(
            text,
            "\n[projects.\"{shown}\".config.workflow]\n{}",
            WORKFLOW
                .replace("{root}", &shown)
                .replace("{rounds}", &rounds.to_string())
                .replace("{attempts}", &attempts.to_string())
                .replace(
                    "{designer}",
                    if designer {
                        "designer = \"designer\"\n"
                    } else {
                        ""
                    }
                )
        )
        .unwrap();
        std::fs::write(&config_file, text).unwrap();

        let app = Arc::new(app);
        let host = FakeHost::new(Arc::clone(&app), script);
        let engine = Self::engine(&app, &host);
        let h = Self {
            project: ProjectId::from_root(&root).to_string(),
            _fixture: fixture,
            app,
            engine,
            host,
            _config: config,
        };
        tracker::sync(&h.app, &h.project, SyncReason::Demand).unwrap();
        h
    }

    fn engine(app: &Arc<App>, host: &Arc<FakeHost>) -> Arc<Engine> {
        let engine = Engine::new(
            Arc::clone(app),
            Arc::clone(host) as Arc<dyn RoleHost>,
            Arc::new(FakeClock::new()) as Arc<dyn Clock>,
            Arc::new(Heard::default()) as Arc<dyn Notice>,
            Tuning {
                tick_ms: 5,
                skills_wait_ms: 1_000,
                settle_ms: 1_000,
                nudges: 1,
            },
        );
        *host.engine.lock() = Arc::downgrade(&engine);
        engine
    }

    fn db(&self) -> Arc<wtm_appdb::AppDb> {
        self.app.appdb.get().unwrap()
    }

    fn start(&self) -> FlowId {
        match self
            .engine
            .start_work(&StartWork {
                project: self.project.clone(),
                key: KEY.to_owned(),
                choice: None,
                by_drop: false,
                confirmed: false,
            })
            .unwrap()
        {
            StartOutcome::Started { flow, .. } => flow,
            other => panic!("expected a start, got {other:?}"),
        }
    }

    fn view(&self, flow: FlowId) -> ImplementView {
        self.engine.implement_view(flow).unwrap()
    }

    /// Wait until the run satisfies `done`, with nothing running.
    fn until(
        &self,
        flow: FlowId,
        what: &str,
        done: impl Fn(&ImplementView) -> bool,
    ) -> ImplementView {
        for _ in 0..6_000 {
            let view = self.view(flow);
            if !view.running && done(&view) {
                return view;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let view = self.view(flow);
        panic!(
            "never {what}: {:?}, answers {:#?}",
            view.flow.state,
            self.host.answers.lock()
        );
    }

    /// Start the issue and wait for its plan to wait for a decision.
    fn planned(&self) -> FlowId {
        let flow = self.start();
        self.until(flow, "planned", |v| {
            v.flow.state.phase == Phase::Planned
                && matches!(v.flow.state.hold, Some(Hold::AwaitingApproval))
        });
        flow
    }

    fn approve_plan(&self, flow: FlowId) {
        let view = self.engine.plan_view(flow).unwrap();
        self.engine
            .approve_plan(
                flow,
                view.plans.last().unwrap().revision,
                view.issue_revision_now.as_deref().unwrap(),
                false,
            )
            .unwrap();
    }

    fn ready(&self, flow: FlowId) -> ImplementView {
        self.until(flow, "ready to test", |v| {
            v.flow.state.phase == Phase::ReadyToTest
        })
    }

    fn worktree(&self, flow: FlowId) -> PathBuf {
        PathBuf::from(&self.view(flow).implementation.unwrap().worktree)
    }

    /// The answers the run tools gave `role`, in order.
    fn told(&self, role: &str) -> Vec<(bool, String)> {
        self.host
            .answers
            .lock()
            .iter()
            .filter(|(r, _, _)| r == role)
            .map(|(_, ok, text)| (*ok, text.clone()))
            .collect()
    }

    /// The turns the `n`th session of `role` was sent, from 1.
    fn turns(&self, role: &str, n: usize) -> Vec<String> {
        self.host
            .opened
            .lock()
            .iter()
            .filter(|o| o.role == role)
            .nth(n - 1)
            .map(|o| o.turns.lock().clone())
            .unwrap_or_default()
    }

    fn opened(&self, role: &str, n: usize) -> common::agents::Opened {
        self.host
            .opened
            .lock()
            .iter()
            .filter(|o| o.role == role)
            .nth(n - 1)
            .cloned()
            .unwrap()
    }

    /// The conversation the `n`th session of `role` reported, as its invocation recorded it.
    fn conversation(&self, flow: FlowId, n: usize) -> Option<String> {
        self.view(flow)
            .sessions
            .get(n - 1)
            .and_then(|i| i.provider_session.clone())
    }
}

/// `git log` subjects in `worktree`, newest first.
fn subjects(worktree: &Path) -> Vec<String> {
    // A test is a sanctioned place to spawn directly.
    #[allow(clippy::disallowed_methods)]
    let out = std::process::Command::new("git")
        .args(["log", "--format=%s"])
        .current_dir(worktree)
        .output()
        .unwrap();
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

// ═══════════════════════════════ the gates ═══════════════════════════════

#[test]
fn nothing_implements_until_the_plan_is_approved_and_approving_it_starts_the_writer() {
    let h = Harness::new(script(|attempt| match attempt {
        1 => first_round(),
        _ => fixing(),
    }));
    let flow = h.planned();

    // Before the approval: refused, and nothing recorded or held.
    let refused = h.engine.start_implementing(flow).unwrap_err().message;
    assert!(refused.contains("Not yet"), "{refused}");
    assert_eq!(h.host.attempts_of("implementer"), 0);
    assert!(h.db().implementation(flow).unwrap().is_none());
    assert!(h.db().open_leases(None).unwrap().is_empty());
    assert!(!h.view(flow).can_start);

    h.approve_plan(flow);
    let view = h.ready(flow);
    let writer = h.opened("implementer", 1);
    // The writer was given the plan, its criteria and its areas, and told what it may change.
    assert!(
        writer.brief.contains("The export lists every unit"),
        "{}",
        writer.brief
    );
    assert!(
        writer
            .brief
            .contains("You may change only what the plan covers: `src/**`, `docs/**`, `docs/export.md`, `src/export.rs`"),
        "{}",
        writer.brief
    );
    assert_eq!(writer.cwd.as_deref(), Some(h.worktree(flow).as_path()));
    assert_eq!(
        view.implementation.as_ref().unwrap().outcome,
        Some(ImplementOutcome::Ready)
    );
}

#[test]
fn a_plan_that_needs_a_design_implements_nothing_until_the_design_is_approved() {
    let h = Harness::configured(
        Box::new(|role, attempt| match role {
            "planner" => Behaviour::Tools(vec![("flow_submit", plan(true))], true),
            "designer" => Behaviour::Design {
                brief: "The export button",
                html: "<!doctype html><html><body><button data-od-id=\"export\">Export</button></body></html>",
                reuse: "Button: the project's Button",
            },
            "implementer" if attempt == 1 => first_round(),
            "implementer" => fixing(),
            "correctness" if attempt == 1 => {
                Behaviour::Submit(vec![bug_at("src/export.rs", "Off by one in the export")])
            }
            _ => Behaviour::Submit(vec![no_findings()]),
        }),
        true,
        2,
        1,
    );
    let flow = h.planned();
    h.approve_plan(flow);
    let design = h.engine.design_view(flow).unwrap();
    let mut design = design;
    for _ in 0..4_000 {
        if !design.running && design.flow.state.phase == Phase::DesignReady {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
        design = h.engine.design_view(flow).unwrap();
    }
    assert_eq!(design.flow.state.phase, Phase::DesignReady);

    // The plan is approved, the design is not: no writer, no lease, no implementation.
    assert_eq!(h.host.attempts_of("implementer"), 0);
    assert!(h.db().implementation(flow).unwrap().is_none());
    assert!(h.db().open_leases(None).unwrap().is_empty());
    let refused = h.engine.start_implementing(flow).unwrap_err().message;
    assert!(
        refused.contains("design revision 1 waits for your approval"),
        "{refused}"
    );

    let revision = design.revisions.last().unwrap();
    h.engine
        .approve_design(flow, 1, &revision.revision.fingerprint)
        .unwrap();
    h.ready(flow);
    let writer = h.opened("implementer", 1);
    assert!(
        writer.brief.contains("The approved design: revision 1"),
        "the writer builds the design you approved: {}",
        writer.brief
    );
}

// ═══════════════════════════════ the loop ═══════════════════════════════

#[test]
fn an_implementation_is_checked_reviewed_fixed_in_its_conversation_and_handed_over() {
    let h = Harness::new(script(|attempt| match attempt {
        1 => first_round(),
        _ => fixing(),
    }));
    let flow = h.planned();
    h.approve_plan(flow);
    let view = h.ready(flow);
    let implementation = view.implementation.clone().unwrap();

    // Two writer turns: the plan, then one fix round, in one conversation.
    assert_eq!(h.host.attempts_of("implementer"), 2);
    let first = h.conversation(flow, 1).unwrap();
    assert_eq!(
        h.opened("implementer", 2).resume.as_deref(),
        Some(first.as_str())
    );
    assert_eq!(h.conversation(flow, 2).as_deref(), Some(first.as_str()));
    let fix_turn = h.turns("implementer", 2).join("\n");
    assert!(fix_turn.contains("Fix round 1"), "{fix_turn}");
    assert!(fix_turn.contains("Off by one in the export"), "{fix_turn}");
    assert!(fix_turn.contains("Check `src-test` failed"), "{fix_turn}");

    // The rounds say what each set out to do.
    let kinds: Vec<RoundKind> = view.rounds.iter().map(|r| r.kind).collect();
    assert_eq!(kinds, [RoundKind::Implement, RoundKind::Fix]);
    assert_eq!(view.rounds[1].findings.len(), 1);
    assert_eq!(view.rounds[1].checks, ["src-test"]);

    // The app made the commits, with the writer's messages, and kept them.
    let messages: Vec<&str> = view
        .checkpoints
        .iter()
        .map(|c| c.message.as_str())
        .collect();
    assert_eq!(messages, ["Export the units", "Export every unit"]);
    let log = subjects(&h.worktree(flow));
    assert_eq!(&log[..2], ["Export every unit", "Export the units"]);

    // The checks are the app's evidence: src-test failed, then passed after the fix.
    let src: Vec<CheckStatus> = view
        .checks
        .iter()
        .filter(|c| c.check == "src-test")
        .map(|c| c.status)
        .collect();
    assert_eq!(src, [CheckStatus::Failed, CheckStatus::Passed]);

    // Handed over clean, with the worktree let go and one item saying so.
    assert_eq!(implementation.stopped, Some(StopReason::Clean));
    assert!(implementation.remaining.is_empty());
    assert!(h.db().open_leases(None).unwrap().is_empty());
    let inbox = h.engine.inbox(None).unwrap();
    let ready: Vec<_> = inbox
        .iter()
        .filter(|i| i.kind == InboxKind::ReviewReady)
        .collect();
    assert_eq!(ready.len(), 1, "{inbox:#?}");
    assert_eq!(ready[0].flow, flow);
    assert_eq!(ready[0].title, "ACME-9 is ready to test");
    assert!(
        ready[0].body.contains("src-test passed"),
        "{}",
        ready[0].body
    );
    // The review read the change against the plan it implements.
    let reviewer = h.opened("correctness", 1);
    assert!(
        reviewer
            .brief
            .contains("The approved plan this change implements, revision 1"),
        "{}",
        reviewer.brief
    );
    assert!(reviewer.brief.contains("The export lists every unit"));
    // The review is the issue's own, in the worktree's Reviews tab.
    let review = implementation.review_flow.unwrap();
    assert_eq!(
        h.engine
            .latest(&h.project, &implementation.worktree)
            .unwrap()
            .unwrap()
            .flow
            .id,
        review
    );
}

#[test]
fn the_hand_over_shows_the_diff_the_evidence_and_each_criterion_and_revise_goes_back_to_the_writer()
{
    let h = Harness::new(script(|attempt| match attempt {
        1 => Behaviour::Acts(vec![
            Act::Write("src/export.rs", "pub fn export() {}\n"),
            Act::Write("docs/export.md", "# Export\n"),
            Act::Checkpoint("Export the units"),
            Act::HandoffClaiming(
                "Added the exporter and its page",
                vec![(1, false, "one unit is still missing")],
            ),
        ]),
        2 => Behaviour::Acts(vec![
            Act::Write("src/export.rs", "pub fn export() {} // fixed\n"),
            Act::Checkpoint("Export every unit"),
            Act::HandoffClaiming(
                "Fixed the off-by-one",
                vec![(1, true, "src/export.rs exports every unit")],
            ),
        ]),
        _ => Behaviour::Acts(vec![
            Act::Write(
                "src/export.rs",
                "pub fn export() {} // fixed, with a header\n",
            ),
            Act::Checkpoint("Write a header row"),
            Act::Handoff("Added the header row"),
        ]),
    }));
    let flow = h.planned();
    h.approve_plan(flow);
    h.ready(flow);

    let handoff = h.engine.handoff_view(flow).unwrap();
    assert!(handoff.can_revise);
    let files: Vec<&str> = handoff.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(files, ["docs/export.md", "src/export.rs"]);
    assert!(
        handoff.patch.contains("+pub fn export() {} // fixed"),
        "{}",
        handoff.patch
    );
    assert!(handoff.uncommitted.is_empty());
    assert!(
        handoff.since.is_none(),
        "nothing changed since the evidence"
    );
    // Every check Verifying runs, with what it printed; never a check that did not run.
    let checks: Vec<(&str, CheckStatus, bool)> = handoff
        .checks
        .iter()
        .map(|c| (c.run.check.as_str(), c.run.status, c.stale))
        .collect();
    assert_eq!(
        checks,
        [
            ("docs-check", CheckStatus::Passed, false),
            ("lint", CheckStatus::Passed, false),
            ("src-test", CheckStatus::Passed, false),
        ]
    );
    // The writer's newest word on the criterion, as its word.
    assert_eq!(handoff.coverage.len(), 1);
    assert_eq!(handoff.coverage[0].text, "The export lists every unit");
    let claim = handoff.coverage[0].claim.clone().unwrap();
    assert!(claim.met);
    assert_eq!(claim.evidence, "src/export.rs exports every unit");

    // An edit by hand makes the evidence stale, and says which checks it touches.
    let worktree = h.worktree(flow);
    std::fs::write(worktree.join("docs/export.md"), "# Export\n\nBy hand.\n").unwrap();
    let edited = h.engine.handoff_view(flow).unwrap();
    assert_eq!(edited.uncommitted, ["docs/export.md"]);
    let since = edited.since.clone().unwrap();
    assert_eq!(since.paths, ["docs/export.md"]);
    let stale: Vec<&str> = edited
        .checks
        .iter()
        .filter(|c| c.stale)
        .map(|c| c.run.check.as_str())
        .collect();
    assert_eq!(stale, ["docs-check", "lint"], "src-test reads only src/");

    // Revise needs your words.
    assert!(h.engine.revise(flow, "   ").is_err());
    h.engine
        .revise(flow, "Put a header row first, and keep my docs edit")
        .unwrap();
    let view = h.ready(flow);
    // The same writer, in the same conversation, given your words.
    assert_eq!(h.host.attempts_of("implementer"), 3);
    let first = h.conversation(flow, 1).unwrap();
    assert_eq!(
        h.opened("implementer", 3).resume.as_deref(),
        Some(first.as_str())
    );
    let turn = h.turns("implementer", 3).join("\n");
    assert!(
        turn.contains(
            "<wtm_revision>\nPut a header row first, and keep my docs edit\n</wtm_revision>"
        ),
        "{turn}"
    );
    let kinds: Vec<RoundKind> = view.rounds.iter().map(|r| r.kind).collect();
    assert_eq!(
        kinds,
        [RoundKind::Implement, RoundKind::Fix, RoundKind::Revise]
    );
    assert_eq!(
        view.rounds[2].note.as_deref(),
        Some("Put a header row first, and keep my docs edit")
    );
    assert_eq!(
        view.rounds[2].handoff.as_ref().map(|h| h.summary.as_str()),
        Some("Added the header row")
    );
    // Your edit went in with the revision, through the app's commit, and the checks it touched ran
    // again; the review found nothing more, so it is handed over clean.
    assert_eq!(subjects(&worktree)[0], "Write a header row");
    let after = h.engine.handoff_view(flow).unwrap();
    assert!(after.uncommitted.is_empty());
    assert!(after.since.is_none());
    assert_eq!(
        view.implementation.unwrap().stopped,
        Some(StopReason::Clean)
    );
    assert!(h.db().open_leases(None).unwrap().is_empty());
    // The writer's earlier word on the criterion stands: the revision said nothing about it.
    assert!(after.coverage[0].claim.as_ref().unwrap().met);
}

#[test]
fn a_fix_round_runs_again_only_the_checks_and_review_roles_its_changes_touched() {
    let h = Harness::new(script(|attempt| match attempt {
        1 => first_round(),
        _ => fixing(),
    }));
    let flow = h.planned();
    h.approve_plan(flow);
    let view = h.ready(flow);
    let count = |check: &str| view.checks.iter().filter(|c| c.check == check).count();

    // The fix changed src/export.rs only: lint reads everything and src-test reads src/, so both
    // ran again; docs-check reads docs/, and did not.
    assert_eq!(view.rounds[1].changed, ["src/export.rs"]);
    assert_eq!(
        (count("lint"), count("src-test"), count("docs-check")),
        (2, 2, 1)
    );
    // The same for the review: correctness read again, docs-review kept its result.
    assert_eq!(h.host.attempts_of("correctness"), 2);
    assert_eq!(h.host.attempts_of("docs-review"), 1);
    // And the Reviews tab agrees: the docs check it did not run again is not stale, because the
    // only file that changed is not one its paths say can change its result.
    let review = view.implementation.as_ref().unwrap().review_flow.unwrap();
    let stale = h.engine.staleness(review).unwrap();
    let docs = view
        .checks
        .iter()
        .find(|c| c.check == "docs-check")
        .unwrap();
    assert!(
        !stale.stale_checks.contains(&docs.id),
        "{:?} counts docs-check stale",
        stale.stale_checks
    );
}

// ═══════════════════════════════ the plan's scope ═══════════════════════════════

/// A writer that also changes the README, which the plan does not cover, and hands in with it.
fn wandering() -> Behaviour {
    Behaviour::Acts(vec![
        Act::Write("src/export.rs", "pub fn export() {} // fixed\n"),
        Act::Write("docs/export.md", "# Export\n"),
        Act::Write("README.md", "# Units\n\nNow with an export.\n"),
        Act::Checkpoint("Export the units, and say so in the README"),
        Act::Handoff("Added the exporter; the README needed a line"),
    ])
}

#[test]
fn a_commit_outside_the_plan_is_refused_and_yes_sends_a_wider_plan_back_for_approval() {
    let h = Harness::new(Box::new(|role, attempt| match role {
        "planner" => Behaviour::Tools(vec![("flow_submit", plan(false))], true),
        "implementer" if attempt == 1 => wandering(),
        "implementer" => Behaviour::Acts(vec![
            Act::Checkpoint("Export the units, and say so in the README"),
            Act::Handoff("Committed the README line too"),
        ]),
        _ => Behaviour::Submit(vec![no_findings()]),
    }));
    let flow = h.planned();
    let base = h.engine.plan_view(flow).unwrap();
    h.approve_plan(flow);

    let held = h.until(flow, "held for the README", |v| {
        matches!(v.flow.state.hold, Some(Hold::AwaitingAnswer))
    });
    // The checkpoint was refused, with the file named, and nothing was committed.
    let told = h.told("implementer");
    let (ok, refusal) = told
        .iter()
        .find(|(_, text)| text.contains("Not committed"))
        .unwrap();
    assert!(!ok);
    assert!(refusal.contains("`README.md`"), "{refusal}");
    assert!(held.checkpoints.is_empty());
    assert_eq!(held.flow.state.phase, Phase::Implementing);
    let asked = &held.questions[0];
    assert_eq!(asked.link.kind, ImplementAsk::Outliers);
    assert_eq!(asked.link.paths, ["README.md"]);
    assert_eq!(asked.question.options[0], "Change the plan to include it");
    // Still the writer's worktree: nothing reads it half-done.
    assert_eq!(held.lease.as_ref().unwrap().kind, LeaseKind::Writer);
    // Discuss is the planner's machinery: a question the implementation asked offers none, or it
    // would resume the planner's conversation about the writer's files.
    let thread = h.engine.question_thread(asked.question.id).unwrap();
    assert!(!thread.can_discuss);
    assert!(
        h.engine
            .discuss(asked.question.id, "Why the README?")
            .is_err()
    );
    assert_eq!(h.host.attempts_of("planner"), 1);

    // Yes: the plan's next revision covers it, and it goes back to you.
    h.engine
        .answer(
            asked.question.id,
            Some(0),
            "The README is where the export is announced.",
        )
        .unwrap();
    let planned = h.until(flow, "back at Planned", |v| {
        v.flow.state.phase == Phase::Planned
            && matches!(v.flow.state.hold, Some(Hold::AwaitingApproval))
    });
    let plans = h.db().plans(flow).unwrap();
    assert_eq!(plans.len(), base.plans.len() + 1);
    let wider = &plans.last().unwrap().plan;
    assert!(wider.areas.contains(&"README.md".to_owned()), "{wider:?}");
    assert!(
        wider.risks.iter().any(|r| r.contains("announced")),
        "{wider:?}"
    );
    assert_eq!(
        h.host.attempts_of("implementer"),
        1,
        "no writer while it waits"
    );
    assert_eq!(planned.lease.as_ref().unwrap().kind, LeaseKind::Writer);

    // Approving it carries the writer on, in its conversation, and the README is committed now.
    h.approve_plan(flow);
    let view = h.ready(flow);
    let first = h.conversation(flow, 1).unwrap();
    assert_eq!(
        h.opened("implementer", 2).resume.as_deref(),
        Some(first.as_str())
    );
    let turn = h.turns("implementer", 2).join("\n");
    assert!(turn.contains("approved plan revision 2"), "{turn}");
    assert!(turn.contains("`README.md`"), "{turn}");
    assert_eq!(view.implementation.unwrap().plan_revision, 2);
    assert!(
        view.checkpoints
            .iter()
            .any(|c| c.paths.contains(&"README.md".to_owned()))
    );
}

#[test]
fn keeping_to_the_plan_tells_the_writer_to_put_the_files_back() {
    let h = Harness::new(Box::new(|role, attempt| match role {
        "planner" => Behaviour::Tools(vec![("flow_submit", plan(false))], true),
        "implementer" if attempt == 1 => wandering(),
        "implementer" => Behaviour::Acts(vec![
            Act::Restore("README.md"),
            Act::Checkpoint("Export the units"),
            Act::Handoff("Put the README back"),
        ]),
        _ => Behaviour::Submit(vec![no_findings()]),
    }));
    let flow = h.planned();
    h.approve_plan(flow);
    let held = h.until(flow, "held for the README", |v| {
        matches!(v.flow.state.hold, Some(Hold::AwaitingAnswer))
    });
    h.engine
        .answer(held.questions[0].question.id, Some(1), "")
        .unwrap();
    let view = h.ready(flow);
    let turn = h.turns("implementer", 2).join("\n");
    assert!(turn.contains("kept to the plan"), "{turn}");
    assert!(turn.contains("`README.md`"), "{turn}");
    assert_eq!(view.implementation.unwrap().plan_revision, 1);
    assert!(
        view.checkpoints
            .iter()
            .all(|c| !c.paths.contains(&"README.md".to_owned())),
        "{:?}",
        view.checkpoints
    );
}

#[test]
fn a_finding_outside_the_plan_is_asked_about_and_left_and_a_wider_plan_fixes_it_later() {
    let h = Harness::new(Box::new(|role, attempt| match role {
        "planner" => Behaviour::Tools(vec![("flow_submit", plan(false))], true),
        "implementer" if attempt == 1 => Behaviour::Acts(vec![
            Act::Write("src/export.rs", "pub fn export() {} // fixed\n"),
            Act::Write("docs/export.md", "# Export\n"),
            Act::Checkpoint("Export the units"),
            Act::Handoff("Added the exporter"),
        ]),
        "implementer" => Behaviour::Acts(vec![
            Act::Write("README.md", "# Units\n\nExports too.\n"),
            Act::Checkpoint("Mention the export"),
            Act::Handoff("Fixed the README"),
        ]),
        "correctness" if attempt == 1 => {
            Behaviour::Submit(vec![bug_at("README.md", "The README says nothing exports")])
        }
        _ => Behaviour::Submit(vec![no_findings()]),
    }));
    let flow = h.planned();
    h.approve_plan(flow);
    let view = h.ready(flow);

    // Not fixed: outside the plan. Asked about, and listed as left, with why.
    assert_eq!(h.host.attempts_of("implementer"), 1);
    let implementation = view.implementation.clone().unwrap();
    assert_eq!(implementation.stopped, Some(StopReason::NothingFixable));
    assert_eq!(
        implementation.remaining[0].because,
        LeftBecause::OutsidePlan {
            paths: vec!["README.md".to_owned()]
        }
    );
    let asked = view
        .questions
        .iter()
        .find(|q| q.link.kind == ImplementAsk::Findings)
        .unwrap();
    assert_eq!(asked.question.options[1], "Leave it for later");

    // Answered after the hand-over: the wider plan goes back for approval, and approving it opens
    // the implementation again, under the lease again, with a fresh budget for the new plan.
    h.engine.answer(asked.question.id, Some(0), "").unwrap();
    h.until(flow, "back at Planned", |v| {
        v.flow.state.phase == Phase::Planned
            && matches!(v.flow.state.hold, Some(Hold::AwaitingApproval))
    });
    h.approve_plan(flow);
    let again = h.ready(flow);
    assert_eq!(h.host.attempts_of("implementer"), 2);
    let implementation = again.implementation.unwrap();
    assert_eq!(implementation.plan_revision, 2);
    assert_eq!(implementation.stopped, Some(StopReason::Clean));
    assert!(h.db().open_leases(None).unwrap().is_empty());
}

// ═══════════════════════════════ the bounds ═══════════════════════════════

/// A reviewer that reports the same problem however often the writer fixes it: a planted finding
/// nothing can satisfy.
fn unfixable(role: &str, attempt: u32, writer: Behaviour) -> Behaviour {
    match role {
        "planner" => Behaviour::Tools(vec![("flow_submit", plan(false))], true),
        "implementer" if attempt == 1 => first_round(),
        "implementer" => writer,
        "correctness" => Behaviour::Submit(vec![bug_at(
            "src/export.rs",
            "Units are exported out of order",
        )]),
        _ => Behaviour::Submit(vec![no_findings()]),
    }
}

#[test]
fn a_problem_still_reported_after_its_attempt_is_left_and_the_loop_stops() {
    let h = Harness::configured(
        Box::new(|role, attempt| {
            unfixable(
                role,
                attempt,
                Behaviour::Acts(vec![
                    Act::Write(
                        "src/export.rs",
                        format!("pub fn export() {{}} // fixed {attempt}\n").leak(),
                    ),
                    Act::Checkpoint("Try to sort the units"),
                    Act::Handoff("Sorted them, I think"),
                ]),
            )
        }),
        false,
        2,
        1,
    );
    let flow = h.planned();
    h.approve_plan(flow);
    let view = h.ready(flow);
    // One attempt at the problem, then left: never a third writer turn.
    assert_eq!(h.host.attempts_of("implementer"), 2);
    let implementation = view.implementation.unwrap();
    assert_eq!(implementation.stopped, Some(StopReason::NothingFixable));
    assert_eq!(
        implementation.remaining[0].because,
        LeftBecause::AttemptsSpent { attempts: 1 }
    );
    let inbox = h.engine.inbox(None).unwrap();
    let ready = inbox
        .iter()
        .find(|i| i.kind == InboxKind::ReviewReady)
        .unwrap();
    assert!(
        ready
            .body
            .contains("Units are exported out of order (still there after 1 attempt)"),
        "{}",
        ready.body
    );
}

#[test]
fn the_fix_loop_stops_at_its_round_bound_with_what_it_could_still_fix_listed() {
    let h = Harness::configured(
        Box::new(|role, attempt| {
            unfixable(
                role,
                attempt,
                Behaviour::Acts(vec![
                    Act::Write(
                        "src/export.rs",
                        format!("pub fn export() {{}} // fixed {attempt}\n").leak(),
                    ),
                    Act::Checkpoint("Try to sort the units"),
                    Act::Handoff("Sorted them, I think"),
                ]),
            )
        }),
        false,
        2,
        3,
    );
    let flow = h.planned();
    h.approve_plan(flow);
    let view = h.ready(flow);
    // The plan's round and two fix rounds, then the bound: three writer turns, not four.
    assert_eq!(h.host.attempts_of("implementer"), 3);
    assert_eq!(
        view.rounds
            .iter()
            .filter(|r| r.kind == RoundKind::Fix)
            .count(),
        2
    );
    let implementation = view.implementation.unwrap();
    assert_eq!(
        implementation.stopped,
        Some(StopReason::Bounded { rounds: 2 })
    );
    assert_eq!(
        implementation.remaining[0].because,
        LeftBecause::RoundsSpent
    );
    let inbox = h.engine.inbox(None).unwrap();
    let ready = inbox
        .iter()
        .find(|i| i.kind == InboxKind::ReviewReady)
        .unwrap();
    assert!(
        ready.body.contains("stopped at its bound of 2 rounds"),
        "{}",
        ready.body
    );
}

// ═══════════════════════════════ a crash ═══════════════════════════════

#[test]
fn a_crash_mid_turn_keeps_the_writer_lease_and_resume_carries_on_in_the_same_conversation() {
    let go = Arc::new(AtomicBool::new(false));
    let wait = Arc::clone(&go);
    let h = Harness::new(Box::new(move |role, attempt| match role {
        "planner" => Behaviour::Tools(vec![("flow_submit", plan(false))], true),
        "implementer" if attempt == 1 => Behaviour::Acts(vec![
            Act::Write("src/export.rs", "pub fn export() {}\n"),
            Act::Checkpoint("Export the units"),
            Act::Write("docs/export.md", "# Export\n"),
            Act::WaitFor(Arc::clone(&wait)),
            Act::Handoff("Never sent: the app is gone"),
        ]),
        "implementer" => Behaviour::Acts(vec![
            Act::Write("src/export.rs", "pub fn export() {} // fixed\n"),
            Act::Checkpoint("Finish the export and its page"),
            Act::Handoff("Finished"),
        ]),
        _ => Behaviour::Submit(vec![no_findings()]),
    }));
    let flow = h.planned();
    h.approve_plan(flow);
    for _ in 0..4_000 {
        if h.view(flow).checkpoints.len() == 1 {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(h.view(flow).checkpoints.len(), 1);

    // The crash: the process is gone, and with it its sessions. A new engine on the same database.
    let first = h.conversation(flow, 1).unwrap();
    h.host.sessions.lock().clear();
    go.store(true, Ordering::SeqCst);
    let relaunched = Harness::engine(&h.app, &h.host);
    let interrupted = relaunched.recover().unwrap();
    assert!(interrupted.contains(&flow), "{interrupted:?}");
    let state = relaunched.implement_view(flow).unwrap().flow.state;
    assert_eq!(state.phase, Phase::Implementing);
    assert!(matches!(state.hold, Some(Hold::Interrupted { .. })));
    // The worktree is still the writer's: its uncommitted page is there, and a review is refused.
    let leases = h.db().open_leases(None).unwrap();
    assert_eq!(leases.len(), 1, "{leases:?}");
    assert_eq!((leases[0].flow, leases[0].kind), (flow, LeaseKind::Writer));
    let worktree = h.worktree(flow);
    assert!(worktree.join("docs/export.md").exists());
    let review = relaunched
        .start(&wtm_app_lib::flow::StartReview {
            project: h.project.clone(),
            worktree: h.view(flow).implementation.unwrap().worktree,
            recipe: "standard".to_owned(),
            base: None,
        })
        .unwrap_err();
    assert!(
        review.message.contains("is implementing an issue"),
        "{review:?}"
    );

    relaunched.resume(flow).unwrap();
    let mut view = relaunched.implement_view(flow).unwrap();
    for _ in 0..6_000 {
        if !view.running && view.flow.state.phase == Phase::ReadyToTest {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
        view = relaunched.implement_view(flow).unwrap();
    }
    assert_eq!(view.flow.state.phase, Phase::ReadyToTest);
    // One round, resumed in its conversation and told it was cut off; nothing sent twice.
    assert_eq!(view.rounds.len(), 1);
    assert_eq!(
        h.opened("implementer", 2).resume.as_deref(),
        Some(first.as_str())
    );
    let turn = h.turns("implementer", 2).join("\n");
    assert!(turn.contains("was cut off"), "{turn}");
    assert!(h.db().open_leases(None).unwrap().is_empty());
    let pages = view
        .checkpoints
        .iter()
        .flat_map(|c| c.paths.iter())
        .filter(|p| *p == "docs/export.md")
        .count();
    assert_eq!(pages, 1, "the page it left uncommitted is in the work once");
}

#[test]
fn stopping_the_run_stops_the_writer_and_lets_the_worktree_go() {
    let go = Arc::new(AtomicBool::new(false));
    let wait = Arc::clone(&go);
    let h = Harness::new(Box::new(move |role, _| match role {
        "planner" => Behaviour::Tools(vec![("flow_submit", plan(false))], true),
        "implementer" => Behaviour::Acts(vec![
            Act::Write("src/export.rs", "pub fn export() {}\n"),
            Act::WaitFor(Arc::clone(&wait)),
        ]),
        _ => Behaviour::Submit(vec![no_findings()]),
    }));
    let flow = h.planned();
    h.approve_plan(flow);
    for _ in 0..4_000 {
        if h.host.attempts_of("implementer") == 1 && h.view(flow).running {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    h.engine.cancel(flow).unwrap();
    go.store(true, Ordering::SeqCst);
    let view = h.until(flow, "stopped", |v| v.flow.state.end.is_some());
    assert_eq!(
        view.implementation.unwrap().outcome,
        Some(ImplementOutcome::Cancelled)
    );
    assert!(h.db().open_leases(None).unwrap().is_empty());
}
