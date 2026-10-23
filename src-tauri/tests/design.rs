//! Designing an issue through the real `App` and engine: a real repository and Start Work, the real
//! app database, run tools and prototype store, a fake tracker, and fake agents in place of the
//! CLIs.
//!
//! What is protected is the plan's I4 scenario and what it rests on:
//!
//! - **A plan that needs a design cannot reach Implementing without an approved revision.** The
//!   gate is the engine's, on every route into Implementing; Start implementing is refused with the
//!   reason until a revision is approved, and a run whose plan skipped the design is not held.
//! - **Every revision stays addressable**, with its fingerprint, brief, the feedback it answered and
//!   its reuse notes, and its artifact still reads back after newer ones exist.
//! - **An approval is invalidated** by a newer revision and by a change to the project files the
//!   revision was drawn from, the same rules Open Design applies in a chat.
//! - **Only the designer has the prototype tools.** Other run roles' tokens are refused them.
//! - **Changes you ask for continue the designer's conversation**, carrying your note and the
//!   revision's comments.

#![allow(clippy::unwrap_used, clippy::too_many_lines)]

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use wtm_app_lib::app::{App, WORK_VIEW_PREF};
use wtm_app_lib::flow::designer::DesignView;
use wtm_app_lib::flow::planner::PlanView;
use wtm_app_lib::flow::start::{StartOutcome, StartWork};
use wtm_app_lib::flow::{Engine, Tuning};
use wtm_app_lib::handoff::CodeCall;
use wtm_app_lib::tracker;
use wtm_config::AppPaths;
use wtm_core::flow::{
    ApprovalIntent, Blocked, DesignStanding, FlowId, Hold, InboxKind, Issue, IssueStatus, Phase,
    Pinned, PlanStanding, Replaced, Sprint, SprintState, StatusCategory,
};
use wtm_core::model::ProjectId;
use wtm_core::ports::clock::Clock;
use wtm_core::ports::config::{ConfigStore, TrustDecision};
use wtm_core::ports::{OutboxStore, Tracker, TrackerWriter};
use wtm_core::usecase::SyncReason;
use wtm_testkit::{FakeClock, FakeTracker, GitFixture};

mod common;
use common::agents::{Behaviour, FakeHost, Heard, Script};

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

/// The tracker, its statuses and queue, and a planner. `{designer}` is the designer's line.
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
     [projects.\"{root}\".config.workflow.role.ui-check]\n\
     objective = \"Check the user interface against the approved design\"\n\
     provider = \"claude\"\n\
     inputs = [\"diff\", \"design\"]\n\
     timeout_ms = 60000\n\
     [[projects.\"{root}\".config.workflow.recipe.ui.stage]]\n\
     roles = [\"ui-check\"]\n";

const KEY: &str = "ACME-9";

const HEADER: &str = "<header class=\"page-header\"><slot /></header>\n";
const TOKENS: &str = ":root { --accent: #3b5bdb; --space: 12px; }\n";

const FIRST: &str = "<!doctype html><html><body><header data-od-id=\"header\">Units</header>\
     <table data-od-id=\"units\"></table></body></html>";
const SECOND: &str = "<!doctype html><html><body><header class=\"page-header\" \
     data-od-id=\"header\">Units</header><table data-od-id=\"units\"></table></body></html>";

fn plan(design_needed: bool) -> Value {
    json!({
        "kind": "plan",
        "payload": {
            "summary": "A screen that lists units and exports them",
            "steps": ["Add the units screen", "Add the export action"],
            "acceptanceCriteria": ["The units screen lists every unit"],
            "verification": ["A test renders the screen"],
            "areas": ["src/**"],
            "designNeeded": design_needed,
            "designReason": "A new screen",
        },
    })
}

/// The planner plans; the designer publishes revision after revision, one per turn.
fn script(design_needed: bool) -> Script {
    Box::new(move |role, attempt| match role {
        "planner" => Behaviour::Tools(vec![("flow_submit", plan(design_needed))], true),
        "ui-check" => Behaviour::Submit(vec![json!({ "findings": [] })]),
        "designer" => Behaviour::Design {
            brief: "A units screen with the page header and the export action",
            html: if attempt == 1 { FIRST } else { SECOND },
            reuse: if attempt == 1 {
                "Header: a plain <header>, approximating PageHeader"
            } else {
                "Header: PageHeader from src/components/PageHeader.svelte"
            },
        },
        _ => Behaviour::Silent,
    })
}

struct Harness {
    _fixture: GitFixture,
    app: Arc<App>,
    engine: Arc<Engine>,
    host: Arc<FakeHost>,
    heard: Arc<Heard>,
    project: String,
    _root: PathBuf,
    _config: tempfile::TempDir,
}

impl Harness {
    fn new(script: Script) -> Self {
        Self::configured(script, true)
    }

    fn configured(script: Script, designer: bool) -> Self {
        Self::with_config(script, designer, "")
    }

    /// `extra` is appended to your config as it is, such as a `[run_sessions]` table.
    fn with_config(script: Script, designer: bool, extra: &str) -> Self {
        let fixture = GitFixture::new();
        fixture.commit("wtm.toml", CONFIG, "add wtm config");
        fixture.commit("src/components/PageHeader.svelte", HEADER, "a header");
        fixture.commit("src/styles/tokens.css", TOKENS, "tokens");
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
                        summary: "A screen for the unit list".to_owned(),
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
        tracker.describe(KEY, "Add a screen that lists units, with an export action.");
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
            WORKFLOW.replace("{root}", &shown).replace(
                "{designer}",
                if designer {
                    "designer = \"designer\"\n"
                } else {
                    ""
                }
            )
        )
        .unwrap();
        text.push_str(extra);
        std::fs::write(&config_file, text).unwrap();

        let app = Arc::new(app);
        let host = FakeHost::new(Arc::clone(&app), script);
        let heard = Arc::new(Heard::default());
        let engine = Self::engine(&app, &host, &heard);
        let h = Self {
            project: ProjectId::from_root(&root).to_string(),
            _root: root,
            _fixture: fixture,
            app,
            engine,
            host,
            heard,
            _config: config,
        };
        tracker::sync(&h.app, &h.project, SyncReason::Demand).unwrap();
        h
    }

    fn engine(app: &Arc<App>, host: &Arc<FakeHost>, heard: &Arc<Heard>) -> Arc<Engine> {
        let engine = Engine::new(
            Arc::clone(app),
            Arc::clone(host) as _,
            Arc::new(FakeClock::new()) as Arc<dyn Clock>,
            Arc::clone(heard) as _,
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

    fn plan(&self, flow: FlowId) -> PlanView {
        self.engine.plan_view(flow).unwrap()
    }

    fn design(&self, flow: FlowId) -> DesignView {
        self.engine.design_view(flow).unwrap()
    }

    /// Wait until the run's design satisfies `done`, with nothing running.
    fn until(&self, flow: FlowId, what: &str, done: impl Fn(&DesignView) -> bool) -> DesignView {
        for _ in 0..4_000 {
            let view = self.design(flow);
            if !view.running && done(&view) {
                return view;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("never {what}: {:#?}", self.design(flow).flow.state);
    }

    /// Start the issue, wait for its plan, and approve it.
    fn planned_and_approved(&self, skip_design: bool) -> FlowId {
        let flow = self.planned();
        self.approve_plan(flow, skip_design);
        flow
    }

    /// Start the issue and wait for its plan to wait for a decision.
    fn planned(&self) -> FlowId {
        let flow = self.start();
        for _ in 0..4_000 {
            let view = self.plan(flow);
            if !view.running && matches!(view.flow.state.hold, Some(Hold::AwaitingApproval)) {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        flow
    }

    fn approve_plan(&self, flow: FlowId, skip_design: bool) {
        let view = self.plan(flow);
        self.engine
            .approve_plan(
                flow,
                view.plans.last().unwrap().revision,
                view.issue_revision_now.as_deref().unwrap(),
                skip_design,
            )
            .unwrap();
    }

    /// Wait until the run's newest revision is `revision` and waits for a decision.
    fn ready(&self, flow: FlowId, revision: u32) -> DesignView {
        self.until(flow, &format!("design revision {revision} ready"), |v| {
            v.flow.state.phase == Phase::DesignReady
                && matches!(v.flow.state.hold, Some(Hold::AwaitingApproval))
                && v.revisions.last().map(|r| r.revision.revision) == Some(revision)
        })
    }

    fn approve(&self, flow: FlowId, revision: u32) {
        let view = self.design(flow);
        let shown = view
            .revisions
            .iter()
            .find(|r| r.revision.revision == revision)
            .unwrap();
        self.engine
            .approve_design(flow, revision, &shown.revision.fingerprint)
            .unwrap();
    }

    fn worktree(&self, flow: FlowId) -> PathBuf {
        PathBuf::from(&self.design(flow).design.unwrap().worktree)
    }
}

fn refusal(result: Result<(), wtm_app_lib::flow::Refusal>) -> String {
    result
        .expect_err("Start implementing must be refused")
        .message
}

// ═══════════════════════════════ the gate ═══════════════════════════════

#[test]
fn a_plan_that_needs_a_design_cannot_reach_implementing_until_a_revision_is_approved() {
    let h = Harness::new(script(true));
    let flow = h.planned_and_approved(false);

    // Approving a plan that needs a design moved the run to Designing, and the designer published.
    let ready = h.ready(flow, 1);
    assert_eq!(
        ready.standing,
        DesignStanding::Waiting {
            revision: 1,
            replaced: None
        }
    );
    assert!(ready.required);
    // The designer's step says what it did, and is not left looking busy.
    let step = ready.step.as_ref().unwrap();
    assert_eq!(step.status, wtm_core::flow::StepStatus::Pending);
    assert_eq!(
        step.detail.as_deref(),
        Some("published revision 1; waiting for your decision")
    );
    let inbox = h.engine.inbox(None).unwrap();
    assert!(
        inbox
            .iter()
            .any(|i| i.kind == InboxKind::Approval
                && i.title == "ACME-9: design revision 1 is ready"),
        "{inbox:#?}"
    );

    // Without an approved revision the gate holds, says why, and records nothing.
    let refused = refusal(h.engine.start_implementing(flow));
    assert!(
        refused.contains("design revision 1 waits for your approval"),
        "{refused}"
    );
    assert_eq!(
        ready.gate.blocked,
        Some(Blocked::DesignNotApproved { revision: 1 })
    );
    assert_eq!(h.design(flow).flow.state.phase, Phase::DesignReady);

    h.approve(flow, 1);
    let approved = h.design(flow);
    assert!(matches!(
        approved.standing,
        DesignStanding::Approved { revision: 1, .. }
    ));
    assert!(approved.gate.ready);
    // The approval pins the revision, its artifact and its sources.
    let approval = h
        .app
        .appdb
        .get()
        .unwrap()
        .approvals(flow)
        .unwrap()
        .into_iter()
        .map(|approved| approved.approval)
        .find(|a| a.intent == ApprovalIntent::Design)
        .unwrap();
    assert_eq!(
        approval
            .binding
            .0
            .get(&Pinned::DesignRevision)
            .map(String::as_str),
        Some("1")
    );
    assert_eq!(
        approval.binding.0.get(&Pinned::Payload),
        Some(&approved.revisions[0].revision.fingerprint)
    );
    assert!(approval.binding.0.contains_key(&Pinned::Sources));

    h.engine.start_implementing(flow).unwrap();
    assert_eq!(h.design(flow).flow.state.phase, Phase::Implementing);
}

#[test]
fn a_plan_approved_with_the_design_skipped_goes_straight_to_implementing() {
    let h = Harness::new(script(true));
    let flow = h.planned_and_approved(true);
    let view = h.design(flow);
    assert_eq!(view.flow.state.phase, Phase::Planned);
    assert!(view.design.is_none() && !view.required && view.gate.ready);
    assert!(h.host.opened_roles().iter().all(|role| role != "designer"));
    h.engine.start_implementing(flow).unwrap();
    assert_eq!(h.design(flow).flow.state.phase, Phase::Implementing);
}

#[test]
fn a_plan_that_needs_a_design_waits_at_designing_when_no_designer_is_configured() {
    let h = Harness::configured(script(true), false);
    let flow = h.planned_and_approved(false);
    let view = h.design(flow);
    assert_eq!(view.flow.state.phase, Phase::Designing);
    assert!(view.designer.is_none() && !view.can_start);
    assert!(
        h.engine
            .inbox(None)
            .unwrap()
            .iter()
            .any(|i| i.kind == InboxKind::Failure && i.title.contains("needs a design")),
    );
    assert!(refusal(h.engine.start_implementing(flow)).contains("Designing"));
    assert_eq!(view.gate.blocked, Some(Blocked::NoDesign));
}

#[test]
fn a_design_you_ask_for_on_a_plan_that_did_not_need_one_must_be_approved_before_implementing() {
    let h = Harness::new(script(false));
    let flow = h.planned_and_approved(false);
    let planned = h.design(flow);
    assert_eq!(planned.flow.state.phase, Phase::Planned);
    assert!(planned.gate.ready && planned.can_request);

    h.engine
        .request_design(flow, "Show the empty state too")
        .unwrap();
    let ready = h.ready(flow, 1);
    assert!(ready.required);
    let opened = h.host.opened.lock().clone();
    let designer = opened.iter().find(|o| o.role == "designer").unwrap();
    assert!(
        designer.brief.contains("Show the empty state too"),
        "{}",
        designer.brief
    );
    assert!(refusal(h.engine.start_implementing(flow)).contains("waits for your approval"));
    h.approve(flow, 1);
    h.engine.start_implementing(flow).unwrap();
}

// ═══════════════════════════════ revisions and invalidation ═══════════════════════════════

#[test]
fn a_new_revision_replaces_the_approval_of_the_old_one_and_continues_the_same_conversation() {
    let h = Harness::new(script(true));
    let flow = h.planned_and_approved(false);
    h.ready(flow, 1);
    h.engine
        .comment_on_design(flow, 1, "Header: use PageHeader")
        .unwrap();
    h.approve(flow, 1);

    h.engine
        .request_design_changes(flow, 1, "Keep the table, change the header")
        .unwrap();
    let second = h.ready(flow, 2);

    // The approval of revision 1 no longer stands: revision 2 replaced it.
    let Some(replaced) = (match &second.standing {
        DesignStanding::Waiting { replaced, .. } => replaced.clone(),
        other => panic!("expected revision 2 to wait, got {other:?}"),
    }) else {
        panic!("the approval of revision 1 must show as replaced");
    };
    assert_eq!(replaced.revision, 1);
    assert_eq!(
        second.gate.blocked,
        Some(Blocked::DesignNotApproved { revision: 2 })
    );
    assert!(refusal(h.engine.start_implementing(flow)).contains("design revision 2"));
    // And revision 1 cannot be approved again over revision 2's head.
    let first = &second.revisions[0].revision;
    assert!(
        h.engine
            .approve_design(flow, 1, &first.fingerprint)
            .unwrap_err()
            .contains("not the newest")
    );

    // The changes went to the same conversation, with your note and the revision's comment.
    let opened = h.host.opened.lock().clone();
    let designers: Vec<_> = opened.iter().filter(|o| o.role == "designer").collect();
    assert_eq!(designers.len(), 2);
    assert!(designers.iter().all(|o| o.design), "a designer's token");
    assert_eq!(designers[0].resume, None);
    let first_conversation = h.design(flow).sessions[0].provider_session.clone();
    assert_eq!(designers[1].resume, first_conversation);
    let turn = designers[1].turns.lock()[0].clone();
    assert!(turn.contains("Keep the table, change the header"), "{turn}");
    assert!(turn.contains("Header: use PageHeader"), "{turn}");

    h.approve(flow, 2);
    assert!(h.design(flow).gate.ready);
    h.engine.start_implementing(flow).unwrap();
}

#[test]
fn every_revision_stays_addressable_with_its_fingerprint_brief_feedback_and_reuse_notes() {
    let h = Harness::new(script(true));
    let flow = h.planned_and_approved(false);
    h.ready(flow, 1);
    h.engine
        .comment_on_design(flow, 1, "Header: use PageHeader")
        .unwrap();
    h.engine
        .request_design_changes(flow, 1, "Use the project's header")
        .unwrap();
    let view = h.ready(flow, 2);

    assert_eq!(view.revisions.len(), 2);
    let (one, two) = (&view.revisions[0], &view.revisions[1]);
    assert_eq!(one.revision.revision, 1);
    assert_eq!(two.revision.revision, 2);
    assert_eq!(
        one.revision.prototype, two.revision.prototype,
        "one design's history, in one prototype"
    );
    assert_ne!(one.revision.fingerprint, two.revision.fingerprint);
    assert_eq!(
        one.revision.brief,
        "A units screen with the page header and the export action"
    );
    assert!(one.revision.feedback.is_none());
    let feedback = two.revision.feedback.as_deref().unwrap();
    assert!(feedback.contains("Use the project's header"), "{feedback}");
    assert!(feedback.contains("Header: use PageHeader"), "{feedback}");
    assert!(two.revision.reuse.contains("PageHeader.svelte"));
    assert!(
        one.revision
            .sources
            .iter()
            .any(|s| s.path == "src/components/PageHeader.svelte"),
        "{:?}",
        one.revision.sources
    );
    // The first revision's artifact is still there, still reads as published, after the second.
    assert!(one.readable && two.readable);
    let artifact = PathBuf::from(one.artifact.as_ref().unwrap());
    assert_eq!(
        std::fs::read_to_string(artifact.join("prototype.html")).unwrap(),
        FIRST
    );
    // Under wtm's data directory, never in the worktree.
    assert!(!artifact.starts_with(h.worktree(flow)));
    // Its comment is kept with it.
    assert_eq!(view.comments.len(), 1);
    assert_eq!(view.comments[0].revision, 1);
}

#[test]
fn a_change_to_a_file_the_design_was_drawn_from_makes_its_approval_stale_and_blocks_implementing() {
    let h = Harness::new(script(true));
    let flow = h.planned_and_approved(false);
    h.ready(flow, 1);
    h.approve(flow, 1);
    assert!(h.design(flow).gate.ready);

    std::fs::write(
        h.worktree(flow).join("src/components/PageHeader.svelte"),
        "<header class=\"page-header page-header--tall\"><slot /></header>\n",
    )
    .unwrap();
    let stale = h.design(flow);
    let DesignStanding::Stale {
        revision, changes, ..
    } = &stale.standing
    else {
        panic!("expected a stale approval, got {:?}", stale.standing);
    };
    assert_eq!(*revision, 1);
    assert_eq!(
        changes.iter().map(|c| c.pin).collect::<Vec<_>>(),
        [Pinned::Sources]
    );
    assert_eq!(
        stale.revisions[0].changed_sources,
        ["src/components/PageHeader.svelte"]
    );
    assert_eq!(
        stale.gate.blocked,
        Some(Blocked::DesignStale { revision: 1 })
    );
    assert!(refusal(h.engine.start_implementing(flow)).contains("no longer covers it"));
    assert!(
        h.engine
            .inbox(None)
            .unwrap()
            .iter()
            .any(|i| i.kind == InboxKind::Approval
                && i.title.contains("approved design no longer covers")),
    );

    // A fresh revision, drawn from the files as they are now, is what can be approved.
    h.engine
        .request_design_changes(flow, 1, "The header changed; redraw it")
        .unwrap();
    h.ready(flow, 2);
    h.approve(flow, 2);
    h.engine.start_implementing(flow).unwrap();
}

#[test]
fn a_revisions_approval_needs_the_fingerprint_that_was_shown() {
    let h = Harness::new(script(true));
    let flow = h.planned_and_approved(false);
    h.ready(flow, 1);
    assert!(
        h.engine
            .approve_design(flow, 1, "sha-of-something-else")
            .unwrap_err()
            .contains("not the revision you were looking at")
    );
    assert!(matches!(
        h.design(flow).standing,
        DesignStanding::Waiting { .. }
    ));
}

// ═══════════════════════════════ what the approved design is an input to ═══════════════════════════════

/// Run the `ui` recipe on the issue's worktree, and return the brief its UI reviewer was given.
fn ui_review_brief(h: &Harness, flow: FlowId, attempt: usize) -> String {
    h.engine
        .start(&wtm_app_lib::flow::StartReview {
            project: h.project.clone(),
            worktree: h.design(flow).design.unwrap().worktree,
            recipe: "ui".to_owned(),
            base: Some("main".to_owned()),
        })
        .unwrap();
    for _ in 0..4_000 {
        let opened = h.host.opened.lock().clone();
        let briefs: Vec<_> = opened.iter().filter(|o| o.role == "ui-check").collect();
        if briefs.len() >= attempt {
            return briefs[attempt - 1].brief.clone();
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("the UI reviewer never opened");
}

#[test]
fn a_ui_reviewer_is_given_the_approved_revision_and_its_reuse_notes_and_nothing_unapproved() {
    let h = Harness::new(script(true));
    let flow = h.planned_and_approved(false);
    let ready = h.ready(flow, 1);

    // Before an approval there is no design to give: the role is told it lacks one.
    let before = ui_review_brief(&h, flow, 1);
    assert!(before.contains("an approved design"), "{before}");
    assert!(!before.contains("The approved design:"), "{before}");

    h.approve(flow, 1);
    let after = ui_review_brief(&h, flow, 2);
    let revision = &ready.revisions[0];
    assert!(after.contains("The approved design: revision 1"), "{after}");
    assert!(after.contains(&revision.revision.fingerprint), "{after}");
    assert!(
        after.contains(revision.artifact.as_deref().unwrap()),
        "the artifact's path: {after}"
    );
    assert!(
        after.contains("<wtm_design_reuse>") && after.contains("approximating PageHeader"),
        "the reuse notes, fenced: {after}"
    );
}

// ═══════════════════════════════ run-session caps ═══════════════════════════════

#[test]
fn a_designer_waits_for_a_run_session_place_like_any_role_and_records_nothing_while_it_waits() {
    let release = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let held = Arc::clone(&release);
    let h = Harness::with_config(
        Box::new(move |role, attempt| match role {
            "ui-check" => Behaviour::Hold(Arc::clone(&held), json!({ "findings": [] })),
            other => script(true)(other, attempt),
        }),
        true,
        "\n[run_sessions]\ntotal = 1\n",
    );
    let flow = h.planned();
    // A review of the issue's worktree takes the one place, and keeps it until released.
    h.engine
        .start(&wtm_app_lib::flow::StartReview {
            project: h.project.clone(),
            worktree: h.plan(flow).planning.unwrap().worktree,
            recipe: "ui".to_owned(),
            base: Some("main".to_owned()),
        })
        .unwrap();
    for _ in 0..4_000 {
        if h.host.opened_roles().iter().any(|r| r == "ui-check") {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    h.approve_plan(flow, false);
    // The designer's thread is running, waiting for a place, and nothing is opened or recorded.
    for _ in 0..4_000 {
        if h.design(flow).running {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    std::thread::sleep(Duration::from_millis(300));
    let waiting = h.design(flow);
    assert_eq!(waiting.flow.state.phase, Phase::Designing);
    assert!(waiting.running);
    assert!(waiting.sessions.is_empty(), "no invocation while it waits");
    assert!(!h.host.opened_roles().iter().any(|r| r == "designer"));

    release.store(true, std::sync::atomic::Ordering::SeqCst);
    h.ready(flow, 1);
    assert_eq!(
        h.host.open_most.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "never two run sessions at once under a total of one"
    );
}

// ═══════════════════════════════ tool scoping ═══════════════════════════════

#[test]
fn only_the_designers_token_reaches_the_prototype_tools() {
    let h = Harness::new(script(true));
    let flow = h.planned_and_approved(false);
    h.ready(flow, 1);
    let opened = h.host.opened.lock().clone();
    let planner = opened.iter().find(|o| o.role == "planner").unwrap();
    let designer = opened.iter().find(|o| o.role == "designer").unwrap();
    assert!(!planner.design && designer.design);

    // The planner's token is refused, by its scope, whatever it asks.
    let planner_token = h.app.handoff.issue(wtm_app_lib::handoff::Caller {
        scope: wtm_app_lib::app::SessionScope::run(
            &h.project,
            &h.design(flow).design.unwrap().worktree,
            wtm_app_lib::app::FlowRole {
                flow,
                role: "planner".to_owned(),
                authority: wtm_agent::authority::Authority::Reviewer,
                browser: None,
                design: false,
            },
        ),
        provider: "claude".to_owned(),
        effort: None,
        mode: None,
        session: None,
    });
    let refused = wtm_app_lib::prototype_tools::answer_for_run(
        &h.app,
        Some(&h.engine),
        &planner_token,
        &CodeCall {
            tool: "prototype_prepare".to_owned(),
            args: json!({ "brief": "anything" }),
        },
    );
    assert!(!refused.ok);
    assert!(
        refused
            .error
            .as_deref()
            .is_some_and(|e| e.contains("only an issue's designer")),
        "{refused:?}"
    );
    // A run tool listing for any role but a designer's names no prototype tool.
    for authority in [
        wtm_agent::authority::Authority::Reviewer,
        wtm_agent::authority::Authority::Writer,
    ] {
        assert!(
            wtm_app_lib::flow_tools::definitions(authority)
                .iter()
                .all(|t| !t["name"].as_str().unwrap().starts_with("prototype_"))
        );
    }
    // And the designer's own token, once its turn is over, publishes nothing more.
    let late = wtm_app_lib::prototype_tools::answer_for_run(
        &h.app,
        Some(&h.engine),
        &designer.token,
        &CodeCall {
            tool: "prototype_prepare".to_owned(),
            args: json!({ "brief": "one more" }),
        },
    );
    assert!(!late.ok, "{late:?}");
    assert_eq!(h.design(flow).revisions.len(), 1);
}

#[test]
fn a_designer_hands_in_its_design_by_publishing_never_by_flow_submit() {
    let h = Harness::new(Box::new(|role, _| match role {
        "planner" => Behaviour::Tools(vec![("flow_submit", plan(true))], true),
        _ => Behaviour::Tools(
            vec![(
                "flow_submit",
                json!({ "kind": "handoff", "payload": {
                    "summary": "s", "changed": [], "checks": [], "openQuestions": []
                }}),
            )],
            true,
        ),
    }));
    let flow = h.planned_and_approved(false);
    let view = h.until(flow, "the designer stopped", |v| {
        v.step
            .as_ref()
            .is_some_and(|s| s.status == wtm_core::flow::StepStatus::Failed)
    });
    assert!(view.revisions.is_empty());
    assert_eq!(view.flow.state.phase, Phase::Designing);
    let answers = h.host.answers.lock().clone();
    assert!(
        answers.iter().any(|(role, ok, text)| role == "designer"
            && !ok
            && text.contains("`prototype_publish`, not `flow_submit`")),
        "{answers:#?}"
    );
}

// ═══════════════════════════════ quitting ═══════════════════════════════

#[test]
fn a_designer_cut_off_by_a_quit_is_held_and_resumes_in_the_same_conversation() {
    let h = Harness::new(Box::new(|role, attempt| match (role, attempt) {
        ("planner", _) => Behaviour::Tools(vec![("flow_submit", plan(true))], true),
        ("designer", 1) => Behaviour::Silent,
        _ => Behaviour::Design {
            brief: "A units screen",
            html: FIRST,
            reuse: "Header: plain header",
        },
    }));
    let flow = h.planned_and_approved(false);
    for _ in 0..4_000 {
        if !h.design(flow).sessions.is_empty()
            && h.design(flow).sessions[0].provider_session.is_some()
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    h.engine.shutdown();
    let held = h.until(flow, "held interrupted", |v| {
        matches!(v.flow.state.hold, Some(Hold::Interrupted { .. }))
    });
    assert_eq!(held.flow.state.phase, Phase::Designing);

    let relaunched = Harness::engine(&h.app, &h.host, &h.heard);
    relaunched.recover().unwrap();
    assert!(relaunched.design_view(flow).unwrap().can_start);
    relaunched.start_designer(flow).unwrap();
    for _ in 0..4_000 {
        let view = relaunched.design_view(flow).unwrap();
        if view.flow.state.phase == Phase::DesignReady && !view.running {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let view = relaunched.design_view(flow).unwrap();
    assert_eq!(view.revisions.len(), 1);
    let opened = h.host.opened.lock().clone();
    let designers: Vec<_> = opened.iter().filter(|o| o.role == "designer").collect();
    assert_eq!(designers.len(), 2);
    assert_eq!(
        designers[1].resume, view.sessions[0].provider_session,
        "the conversation the quit cut off"
    );
    let _ = PlanStanding::None;
    let _ = Replaced {
        approval: wtm_core::flow::ApprovalId(0),
        revision: 0,
    };
}
