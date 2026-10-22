//! Planning an issue through the real `App` and engine: a real repository and Start Work, the real
//! app database and run tools, a fake tracker, and fake agents in place of the CLIs.
//!
//! What is protected is the plan's I3 scenario and what it rests on:
//!
//! - **A question ends the turn, and its answer resumes the same conversation.** `flow_ask` holds
//!   the run `AwaitingAnswer`, the engine interrupts the turn and closes the session, and the answer
//!   — whenever it comes, a relaunch in between included — opens a new session that continues the
//!   provider conversation the question was asked in, with a new run token, its first turn the
//!   answer.
//! - **Discuss** talks the question over in that conversation and leaves the run waiting.
//! - **Nothing reaches the tracker** from planning, asking or answering. Send to Jira's draft
//!   records nothing; only the second click posts, once, with a lost reply or a crash in between
//!   settled by reading the issue's comments, never by sending again.
//! - **A plan approval pins the plan and the issue**, and an edit to the issue afterwards makes it
//!   stale, with a diff of what changed.
//!
//! `planner_real_cli.rs` runs the question-and-resume against the real Claude and Codex CLIs.

#![allow(clippy::unwrap_used, clippy::too_many_lines)]

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use serde_json::{Value, json};
use wtm_app_lib::app::{App, WORK_VIEW_PREF};
use wtm_app_lib::flow::planner::PlanView;
use wtm_app_lib::flow::start::{StartOutcome, StartWork};
use wtm_app_lib::flow::{Engine, Tuning};
use wtm_app_lib::tracker;
use wtm_config::AppPaths;
use wtm_core::flow::{
    ApprovalIntent, ApprovalSubject, CommentPayload, CommentRequest, DiffKind, FlowId, Hold,
    InboxKind, InboxTarget, InvocationStatus, Issue, IssueStatus, NewApproval, NewOperation,
    OperationChange, OperationPayload, OperationResult, OperationState, Phase, Pinned,
    PlanStanding, PlanVerdict, Speaker, Sprint, SprintState, StatusCategory, TrackerKind,
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

/// The tracker, its statuses and queue, and a planner.
const WORKFLOW: &str = "tracker = \"jira\"\n\
     planner = \"planner\"\n\
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
     inputs = [\"issue\", \"instructions\"]\n\
     output = \"plan\"\n\
     timeout_ms = 60000\n";

const KEY: &str = "ACME-7";

const DESCRIPTION: &str = "Users export the unit list.\nAs a spreadsheet.";

fn question() -> Value {
    json!({
        "question": "CSV or XLSX for the export?",
        "why": "The issue says \"spreadsheet\", and XLSX needs a new dependency.",
        "refs": ["src/export.py:4", "ACME-7 description: As a spreadsheet."],
        "recommendation": "CSV: it reuses the existing writer.",
        "unblocks": "the plan",
        "options": ["CSV", "XLSX"],
    })
}

fn plan(summary: &str) -> Value {
    json!({
        "kind": "plan",
        "payload": {
            "summary": summary,
            "steps": ["Add the export button", "Write rows with the CSV writer"],
            "acceptanceCriteria": ["A CSV downloads from the unit list"],
            "verification": ["A test that reads the file back"],
            "areas": ["src/export.py", "src/views/**"],
            "designNeeded": false,
        },
    })
}

fn ask() -> Behaviour {
    Behaviour::Tools(vec![("flow_ask", question())], false)
}

fn submit(summary: &'static str) -> Behaviour {
    Behaviour::Tools(vec![("flow_submit", plan(summary))], true)
}

struct Harness {
    _fixture: GitFixture,
    app: Arc<App>,
    engine: Arc<Engine>,
    host: Arc<FakeHost>,
    heard: Arc<Heard>,
    tracker: Arc<FakeTracker>,
    project: String,
    _root: PathBuf,
    flow_db: PathBuf,
    _config: tempfile::TempDir,
}

impl Harness {
    fn new(script: Script) -> Self {
        let fixture = GitFixture::new();
        fixture.commit("wtm.toml", CONFIG, "add wtm config");
        fixture.commit(
            "src/export.py",
            "def export(units):\n    return units\n",
            "an export",
        );
        let config = tempfile::tempdir().unwrap();
        let paths = AppPaths::rooted(config.path());
        let flow_db = paths.flow_db.clone();
        let config_file = paths.config_file.clone();

        let mut app = App::with_paths(paths).unwrap();
        let tracker = Arc::new(
            FakeTracker::new()
                .with_sprint(Sprint {
                    id: 41,
                    name: "Sprint 41".to_owned(),
                    state: SprintState::Active,
                    goal: Some("Ship the bulk export".to_owned()),
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
                )
                .with_issue(
                    Issue {
                        key: "ACME-8".to_owned(),
                        summary: "Someone else's work".to_owned(),
                        status: IssueStatus {
                            name: "In Progress".to_owned(),
                            category: StatusCategory::InProgress,
                        },
                        kind: None,
                        priority: None,
                        assignee: None,
                        labels: Vec::new(),
                    },
                    &[41],
                ),
        );
        tracker.describe(KEY, DESCRIPTION);
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
            WORKFLOW.replace("{root}", &shown)
        )
        .unwrap();
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
            tracker,
            flow_db,
            _config: config,
        };
        tracker::sync(&h.app, &h.project, SyncReason::Demand).unwrap();
        h
    }

    /// An engine over the harness's app, as a launch makes one: the fake agents' tool calls go to
    /// it from now on.
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

    fn db(&self) -> Arc<wtm_appdb::AppDb> {
        self.app.appdb.get().unwrap()
    }

    /// Start the issue; the planner starts by itself once its worktree is ready.
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

    fn view(&self, flow: FlowId) -> PlanView {
        self.engine.plan_view(flow).unwrap()
    }

    /// Wait until the run's planning satisfies `done`, with nothing running.
    fn until(&self, flow: FlowId, what: &str, done: impl Fn(&PlanView) -> bool) -> PlanView {
        for _ in 0..4_000 {
            let view = self.view(flow);
            if !view.running && done(&view) {
                return view;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("never {what}: {:#?}", self.view(flow).flow.state);
    }

    fn waiting_for_answer(&self, flow: FlowId) -> PlanView {
        self.until(flow, "held for an answer", |v| {
            matches!(v.flow.state.hold, Some(Hold::AwaitingAnswer))
        })
    }

    fn planned(&self, flow: FlowId) -> PlanView {
        self.until(flow, "planned", |v| {
            v.flow.state.phase == Phase::Planned
                && matches!(v.flow.state.hold, Some(Hold::AwaitingApproval))
        })
    }

    fn count(&self, table: &str) -> i64 {
        rusqlite::Connection::open(&self.flow_db)
            .unwrap()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    /// Everything the run sent the tracker other than reads and Start's own move.
    fn tracker_writes(&self) -> Vec<String> {
        self.tracker
            .calls()
            .into_iter()
            .filter(|call| {
                !(call.starts_with("search")
                    || call.starts_with("issue ")
                    || call.starts_with("sprint")
                    || call.starts_with("comments ")
                    || call.starts_with("check")
                    || call == &format!("move {KEY} In Progress"))
            })
            .collect()
    }
}

/// The question first, then a plan when the conversation resumes with the answer.
fn ask_then_plan() -> Script {
    Box::new(|_role, attempt| match attempt {
        1 => ask(),
        _ => submit("Export the unit list as CSV, in the list's column order."),
    })
}

// ═══════════════════════════════ asking and resuming ═══════════════════════════════

#[test]
fn a_planners_question_holds_the_run_and_ends_its_turn_and_the_answer_resumes_the_same_conversation()
 {
    let h = Harness::new(ask_then_plan());
    let flow = h.start();

    let waiting = h.waiting_for_answer(flow);
    assert_eq!(waiting.flow.state.phase, Phase::Investigating);
    let asked = &waiting.questions[0].question;
    assert_eq!(asked.text, "CSV or XLSX for the export?");
    assert_eq!(asked.blocks, Some(Phase::Planned));
    assert_eq!(asked.options, ["CSV", "XLSX"]);
    assert!(
        asked.recommendation.as_deref() == Some("CSV: it reuses the existing writer."),
        "{asked:?}"
    );
    // The engine ended the turn itself and closed the session: nothing waits on a CLI.
    assert_eq!(h.host.interrupts.load(Ordering::SeqCst), 1);
    assert_eq!(h.host.open_now.load(Ordering::SeqCst), 0);
    let first = &waiting.sessions[0];
    assert_eq!(first.status, InvocationStatus::Interrupted);
    assert_eq!(first.provider_session.as_deref(), Some("conv-1"));
    // The question is in the inbox, as the one item about it.
    let items = h.engine.inbox(None).unwrap();
    assert!(items.iter().any(|i| i.kind == InboxKind::Question
        && i.target == InboxTarget::Question { question: asked.id }));

    h.engine
        .answer(asked.id, Some(0), "Keep the list's column order.")
        .unwrap();
    let planned = h.planned(flow);

    let opened = h.host.opened.lock().clone();
    assert_eq!(opened.len(), 2, "one session to ask, one to continue");
    assert_eq!(opened[0].resume, None);
    assert_eq!(
        opened[1].resume.as_deref(),
        Some("conv-1"),
        "the answer continues the conversation the question was asked in"
    );
    assert_ne!(opened[0].token, opened[1].token, "with a token of its own");
    let turn = opened[1].turns.lock()[0].clone();
    assert!(turn.contains("\"CSV\""), "{turn}");
    assert!(turn.contains("Keep the list's column order."), "{turn}");
    assert_eq!(
        planned.sessions[1].provider_session.as_deref(),
        Some("conv-1"),
        "the same provider session, recorded on both"
    );
    assert_eq!(planned.plans.len(), 1);
    assert_eq!(planned.plans[0].revision, 1);
    assert_eq!(planned.standing, PlanStanding::Waiting { revision: 1 });
    // The question's item was settled by the answer; the plan waits for you.
    let open = h.engine.inbox(None).unwrap();
    assert!(!open.iter().any(|i| i.kind == InboxKind::Question));
    assert!(
        open.iter()
            .any(|i| i.kind == InboxKind::Approval && i.title.contains("plan revision 1"))
    );
    assert_eq!(
        h.tracker_writes(),
        Vec::<String>::new(),
        "nothing reached the tracker"
    );
}

#[test]
fn an_answer_given_after_a_relaunch_resumes_the_conversation_the_question_was_asked_in() {
    let h = Harness::new(ask_then_plan());
    let flow = h.start();
    let asked = h.waiting_for_answer(flow).questions[0].question.id;

    // Quit, and launch again: a new engine over the same database, as the app makes one.
    h.engine.shutdown();
    let relaunched = Harness::engine(&h.app, &h.host, &h.heard);
    relaunched.recover().unwrap();
    let after = relaunched.plan_view(flow).unwrap();
    assert!(
        matches!(after.flow.state.hold, Some(Hold::AwaitingAnswer)),
        "a run waiting for an answer needs no process, and is not interrupted: {:?}",
        after.flow.state
    );

    relaunched.answer(asked, None, "XLSX, please.").unwrap();
    for _ in 0..4_000 {
        let view = relaunched.plan_view(flow).unwrap();
        if view.flow.state.phase == Phase::Planned && !view.running {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let view = relaunched.plan_view(flow).unwrap();
    assert_eq!(
        view.flow.state.phase,
        Phase::Planned,
        "{:?}",
        view.flow.state
    );
    let opened = h.host.opened.lock().clone();
    assert_eq!(opened[1].resume.as_deref(), Some("conv-1"));
    assert!(opened[1].turns.lock()[0].contains("XLSX, please."));
}

#[test]
fn discussing_a_question_continues_the_conversation_for_a_reply_and_the_run_keeps_waiting() {
    let h = Harness::new(Box::new(|_role, attempt| match attempt {
        1 => ask(),
        2 => Behaviour::Reply("Two reports import the CSV writer's column helpers."),
        _ => submit("Export as CSV."),
    }));
    let flow = h.start();
    let asked = h.waiting_for_answer(flow).questions[0].question.id;
    let thread = h.engine.question_thread(asked).unwrap();
    assert!(thread.can_discuss);

    h.engine
        .discuss(asked, "Does anything else use the writer?")
        .unwrap();
    let view = h.until(flow, "replied", |v| v.questions[0].discussion.len() == 2);

    let discussion = &view.questions[0].discussion;
    assert_eq!(discussion[0].from, Speaker::You);
    assert_eq!(discussion[1].from, Speaker::Agent);
    assert_eq!(
        discussion[1].text,
        "Two reports import the CSV writer's column helpers."
    );
    assert!(matches!(view.flow.state.hold, Some(Hold::AwaitingAnswer)));
    assert!(view.questions[0].question.answer.is_none());
    let opened = h.host.opened.lock().clone();
    assert_eq!(opened[1].resume.as_deref(), Some("conv-1"));
    assert!(opened[1].turns.lock()[0].contains("Does anything else use the writer?"));
    assert_eq!(h.host.open_now.load(Ordering::SeqCst), 0);

    // A discussion turn asks and hands in nothing; the answer is still what carries the run on.
    h.engine.answer(asked, Some(0), "").unwrap();
    let planned = h.planned(flow);
    assert_eq!(planned.sessions.len(), 3);
    assert!(
        planned
            .sessions
            .iter()
            .all(|s| s.provider_session.as_deref() == Some("conv-1"))
    );
}

#[test]
fn quitting_mid_turn_interrupts_the_planner_and_resume_continues_its_conversation() {
    let h = Harness::new(Box::new(|_role, attempt| match attempt {
        1 => Behaviour::Silent,
        _ => submit("Export as CSV."),
    }));
    let flow = h.start();
    for _ in 0..2_000 {
        if h.host.open_now.load(Ordering::SeqCst) == 1 {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    h.engine.shutdown();
    let relaunched = Harness::engine(&h.app, &h.host, &h.heard);
    relaunched.recover().unwrap();
    let view = relaunched.plan_view(flow).unwrap();
    assert!(
        matches!(view.flow.state.hold, Some(Hold::Interrupted { .. })),
        "{:?}",
        view.flow.state
    );
    assert_eq!(view.sessions[0].status, InvocationStatus::Interrupted);

    relaunched.resume(flow).unwrap();
    for _ in 0..4_000 {
        let now = relaunched.plan_view(flow).unwrap();
        if now.flow.state.phase == Phase::Planned && !now.running {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let opened = h.host.opened.lock().clone();
    assert_eq!(opened[1].resume.as_deref(), Some("conv-1"));
    assert!(opened[1].turns.lock()[0].contains("cut off"));
}

// ═══════════════════════════════ Send to Jira ═══════════════════════════════

#[test]
fn send_to_jira_drafts_without_writing_and_posts_once_on_the_second_click_even_when_the_reply_is_lost()
 {
    let h = Harness::new(Box::new(|_role, attempt| match attempt {
        1 => ask(),
        2 => Behaviour::Reply("It reads the rows the list shows."),
        _ => submit("Export as CSV."),
    }));
    let flow = h.start();
    let asked = h.waiting_for_answer(flow).questions[0].question.id;
    h.engine
        .discuss(asked, "What does it read the rows from?")
        .unwrap();
    h.until(flow, "replied", |v| v.questions[0].discussion.len() == 2);
    h.engine
        .answer(asked, Some(0), "Keep the column order.")
        .unwrap();
    h.planned(flow);

    // The first click: a draft, and nothing else anywhere.
    let draft = h.engine.comment_draft(asked).unwrap();
    assert_eq!(draft.key, KEY);
    assert_eq!(draft.tracker, TrackerKind::Jira);
    assert!(draft.body.contains("CSV or XLSX for the export?"));
    assert!(draft.body.contains("Answer: CSV. Keep the column order."));
    assert!(
        !draft.body.contains("What does it read the rows from?")
            && !draft.body.contains("It reads the rows the list shows."),
        "the discussion is not the tracker's: {}",
        draft.body
    );
    assert_eq!(h.tracker.comments_sent(), Vec::<String>::new());
    let posts = |h: &Harness| {
        h.db()
            .approvals(flow)
            .unwrap()
            .into_iter()
            .filter(|a| a.approval.intent == ApprovalIntent::Post)
            .count()
    };
    assert_eq!(posts(&h), 0, "a draft is not an approval");

    // The second click posts the text as edited, and the tracker's reply is lost on the way back.
    let edited = format!("{}\n(From the plan for {KEY}.)", draft.body);
    h.tracker.lose_next_comment_reply();
    let sent = h.engine.post_comment(asked, &edited).unwrap();
    let done = (0..2_000)
        .find_map(|_| {
            let thread = h.engine.question_thread(asked).unwrap();
            let view = thread
                .comments
                .into_iter()
                .find(|c| c.approval == sent.approval)?;
            if view.operation.state == OperationState::Done {
                return Some(view);
            }
            std::thread::sleep(Duration::from_millis(5));
            None
        })
        .expect("the comment settles");
    assert!(matches!(
        done.operation.result,
        Some(OperationResult::Commented { adopted: false, .. })
    ));
    assert_eq!(h.tracker.comments_sent().len(), 1);
    let on_issue = h.tracker.comments_on(KEY);
    assert_eq!(on_issue.len(), 1);
    assert!(on_issue[0].body.contains("(From the plan for ACME-7.)"));

    // The same text sent again is the same operation, which is done: nothing more is posted.
    h.engine.post_comment(asked, &edited).unwrap();
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(h.tracker.comments_sent().len(), 1);
    assert_eq!(h.tracker.comments_on(KEY).len(), 1);
    assert_eq!(posts(&h), 2, "two approvals of one write");
}

#[test]
fn a_comment_a_crash_left_mid_send_is_settled_by_reading_the_issue_and_never_sent_again() {
    let h = Harness::new(ask_then_plan());
    let flow = h.start();
    let asked = h.waiting_for_answer(flow).questions[0].question.id;
    h.engine.answer(asked, Some(0), "").unwrap();
    h.planned(flow);
    let db = h.db();

    let executing = |body: &str| {
        let request = CommentRequest {
            payload: CommentPayload {
                tracker: TrackerKind::Jira,
                key: KEY.to_owned(),
                body: body.to_owned(),
            },
            question: asked,
        };
        let approved = db
            .approve(
                &NewApproval::comment(flow, &request),
                &[NewOperation::new(
                    flow,
                    0,
                    OperationPayload::PostComment(request.payload.clone()),
                )],
                1,
            )
            .unwrap();
        let op = approved.operations[0].id;
        db.change_operation(op, &OperationChange::sending(), 2)
            .unwrap();
        (approved.approval.id, op)
    };
    // One that reached the issue before wtm stopped, one that did not.
    let (_, arrived) = executing("Question: CSV or XLSX?\nAnswer: CSV.");
    h.tracker
        .add_comment(KEY, "You", "Question: CSV or XLSX? Answer: CSV.");
    let (_, lost) = executing("Question: CSV or XLSX?\nAnswer: XLSX.");

    let relaunched = Harness::engine(&h.app, &h.host, &h.heard);
    relaunched.recover_publications().unwrap();

    let arrived = db.operation(arrived).unwrap().unwrap();
    assert_eq!(arrived.state, OperationState::Done);
    assert!(matches!(
        arrived.result,
        Some(OperationResult::Commented { adopted: true, .. })
    ));
    let lost = db.operation(lost).unwrap().unwrap();
    assert_eq!(lost.state, OperationState::Failed);
    assert_eq!(
        h.tracker.comments_sent(),
        Vec::<String>::new(),
        "recovery reads, and sends nothing"
    );
}

// ═══════════════════════════════ approval ═══════════════════════════════

#[test]
fn approving_a_plan_pins_the_plan_and_the_issue_and_an_edit_to_the_issue_makes_it_stale_with_a_diff()
 {
    let h = Harness::new(Box::new(|_role, _| submit("Export as CSV.")));
    let flow = h.start();
    let planned = h.planned(flow);
    let issue = planned.issue_now.clone().unwrap();
    assert_eq!(issue.description.as_deref(), Some(DESCRIPTION));

    let decision = h
        .engine
        .approve_plan(flow, 1, &issue.revision(), true)
        .unwrap();
    let PlanVerdict::Approved {
        approval,
        skip_design,
    } = decision.verdict
    else {
        panic!("approved");
    };
    assert!(skip_design);
    let approved = h.db().approved(approval).unwrap().unwrap();
    assert_eq!(approved.approval.intent, ApprovalIntent::Plan);
    assert_eq!(
        approved.approval.binding.0.get(&Pinned::PlanRevision),
        Some(&"1".to_owned())
    );
    assert_eq!(
        approved.approval.binding.0.get(&Pinned::IssueRevision),
        Some(&issue.revision())
    );
    assert!(matches!(approved.subject, ApprovalSubject::Plan(_)));
    let view = h.view(flow);
    assert_eq!(view.flow.state.phase, Phase::Planned);
    assert_eq!(
        view.flow.state.hold, None,
        "Planned, approved: nothing waits"
    );
    assert_eq!(
        view.standing,
        PlanStanding::Approved {
            approval,
            revision: 1,
            skip_design: true
        }
    );

    // Someone edits the issue in the tracker; the next read of it finds the approval stale.
    h.tracker.describe(
        KEY,
        "Users export the unit list.\nAs XLSX, with a summary sheet.",
    );
    h.engine.read_planned_issue(flow).unwrap();
    let PlanStanding::Stale { changes, diff, .. } = h.view(flow).standing else {
        panic!(
            "an edited issue makes the approval stale: {:?}",
            h.view(flow).standing
        );
    };
    assert_eq!(changes[0].pin, Pinned::IssueRevision);
    assert_eq!(diff[0].part, "Description");
    assert!(
        diff[0]
            .lines
            .iter()
            .any(|l| l.kind == DiffKind::Removed && l.text == "As a spreadsheet.")
    );
    assert!(
        diff[0]
            .lines
            .iter()
            .any(|l| l.kind == DiffKind::Added && l.text == "As XLSX, with a summary sheet.")
    );
    assert!(
        h.engine
            .inbox(None)
            .unwrap()
            .iter()
            .any(|i| i.target == InboxTarget::Approval { approval }),
        "the change is news"
    );

    // Approving again against the issue as it was is refused; as it is now, it holds.
    let refused = h
        .engine
        .approve_plan(flow, 1, &issue.revision(), false)
        .unwrap_err();
    assert!(
        refused.contains("changed while you were reading"),
        "{refused}"
    );
    let now = h.view(flow).issue_now.unwrap();
    h.engine
        .approve_plan(flow, 1, &now.revision(), false)
        .unwrap();
    assert!(matches!(
        h.view(flow).standing,
        PlanStanding::Approved { .. }
    ));
    assert_eq!(h.tracker_writes(), Vec::<String>::new());
}

#[test]
fn asking_for_changes_continues_the_conversation_and_the_next_plan_is_the_next_revision() {
    let h = Harness::new(Box::new(|_role, attempt| match attempt {
        1 => submit("Export as CSV."),
        _ => submit("Export as CSV, in the list's column order."),
    }));
    let flow = h.start();
    h.planned(flow);

    h.engine
        .request_plan_changes(flow, 1, "Keep the list's column order.")
        .unwrap();
    let view = h.until(flow, "revised", |v| v.plans.len() == 2);

    assert_eq!(view.plans[1].revision, 2);
    assert_eq!(view.standing, PlanStanding::Waiting { revision: 2 });
    assert!(matches!(view.flow.state.hold, Some(Hold::AwaitingApproval)));
    let opened = h.host.opened.lock().clone();
    assert_eq!(opened[1].resume.as_deref(), Some("conv-1"));
    let turn = opened[1].turns.lock()[0].clone();
    assert!(
        turn.contains("plan revision 1") && turn.contains("column order"),
        "{turn}"
    );
    // Revision 1 is in the record, and cannot be approved now.
    assert!(
        h.engine
            .approve_plan(flow, 1, &view.issue_now.unwrap().revision(), false)
            .unwrap_err()
            .contains("not the newest")
    );
    assert_eq!(h.count("plans"), 2);
}

#[test]
fn an_issue_edited_before_changes_are_asked_for_is_what_the_next_revision_is_planned_against() {
    // Found in the click-through: a changes turn kept the issue from the first read, so revision 2
    // was recorded against text the issue no longer had.
    let h = Harness::new(Box::new(|_role, attempt| match attempt {
        1 => submit("Export as CSV."),
        _ => submit("Export as XLSX, as the issue now asks."),
    }));
    let flow = h.start();
    let first = h.planned(flow);
    let before = first.plans[0].issue_revision.clone();

    h.tracker.describe(
        KEY,
        "Users export the unit list.\nAs XLSX, with a summary sheet.",
    );
    h.engine
        .request_plan_changes(flow, 1, "The issue says XLSX now.")
        .unwrap();
    let view = h.until(flow, "revised", |v| v.plans.len() == 2);

    let second = &view.plans[1];
    assert_ne!(second.issue_revision, before);
    assert_eq!(
        second.issue.description.as_deref(),
        Some("Users export the unit list.\nAs XLSX, with a summary sheet.")
    );
    assert!(view.issue_changes.is_empty(), "{:?}", view.issue_changes);
    assert!(
        h.host.opened.lock()[1]
            .brief
            .contains("As XLSX, with a summary sheet."),
        "the resumed planner's brief carries the edited issue"
    );
}

#[test]
fn the_planner_is_given_the_issue_its_sprint_goal_and_the_repositorys_instructions_and_says_so() {
    let h = Harness::new(Box::new(|_role, _| submit("Export as CSV.")));
    let flow = h.start();
    let view = h.planned(flow);

    let context = view.planning.unwrap().context.unwrap();
    assert_eq!(context.issue.key, KEY);
    assert_eq!(context.issue.description.as_deref(), Some(DESCRIPTION));
    assert_eq!(context.sprint.as_deref(), Some("Sprint 41"));
    assert_eq!(context.sprint_goal.as_deref(), Some("Ship the bulk export"));
    assert_eq!(context.role, "planner");
    let brief = h.host.opened.lock()[0].brief.clone();
    assert!(brief.contains("As a spreadsheet."), "{brief}");
    assert!(brief.contains("Ship the bulk export"), "{brief}");
    assert!(
        brief.contains("\"acceptanceCriteria\""),
        "the plan's schema is in the brief"
    );
    // The planner reads the issue's worktree, which is where its conversation is resumed.
    let cwd = h.host.opened.lock()[0].cwd.clone().unwrap();
    assert!(cwd.ends_with(KEY), "{}", cwd.display());
}
