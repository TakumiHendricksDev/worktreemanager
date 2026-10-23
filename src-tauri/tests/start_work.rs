//! Start Work through the real `App` and engine: a real repository with a committed `wtm.toml`, the
//! New Worktree form's own pipeline, the real app database, and a fake tracker that moves its own
//! issues and can lose a reply.
//!
//! What is protected is the plan's I2 scenario and what it rests on:
//!
//! - **Once.** Start twice, or Start and then drop the card on the start column, gives exactly one
//!   worktree and one run, and the second focuses the first. So does a Start still resolving or
//!   planning when another made the worktree, rather than refusing to make it again.
//! - **The resolution order.** The bound worktree, then a worktree carrying the key, then the
//!   project's offered branches, then a new worktree, with a preview whenever there is a choice and
//!   nothing recorded before one is picked.
//! - **A crash mid-create** resumes through the pipeline's own Retry setup on the worktree that
//!   is there, or makes it again from the same plan when it is not, never a second one.
//! - **The move reads first.** An issue already where Start would move it is adopted, one that
//!   moved elsewhere stops the move and asks, a lost reply is found by reading again, and a move a
//!   crash cut off is settled by reading and never sent again.
//! - **The board.** A backward drag changes the team status and nothing else: the run's state,
//!   its job and its worktree are as they were, and the inbox offers to pause.

#![allow(clippy::unwrap_used)]

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use wtm_agent::session::AgentSink;
use wtm_app_lib::app::{App, WORK_VIEW_PREF};
use wtm_app_lib::flow::host::RoleHost;
use wtm_app_lib::flow::moves::MoveAction;
use wtm_app_lib::flow::start::{DropOutcome, IssueStartView, StartOutcome, StartWork};
use wtm_app_lib::flow::{Engine, Notice, Refusal, Tuning};
use wtm_app_lib::flow_session::RoleSession;
use wtm_app_lib::tracker;
use wtm_config::AppPaths;
use wtm_core::flow::{
    Attention, Candidate, FlowEvent, FlowId, FlowKind, Hold, HoldKind, InboxItem, InboxKind,
    InboxTarget, Issue, IssueStatus, MoveIssuePayload, NewOperation, NewStartJob, NewWorkItem,
    OperationChange, OperationPayload, OperationState, Phase, PublicationState, Sprint,
    SprintState, StartJobState, StatusCategory, TrackerKind, WorkLink,
};
use wtm_core::model::{ProjectId, SessionId};
use wtm_core::ports::clock::Clock;
use wtm_core::ports::config::{ConfigStore, TrustDecision};
use wtm_core::ports::{FlowStore, InboxStore, OutboxStore, StartStore, Tracker, TrackerWriter};
use wtm_core::usecase::SyncReason;
use wtm_testkit::{FakeClock, FakeTracker, GitFixture};

/// The New Worktree form: the issue's key and a base, a branch and a directory named after the
/// key, `story/` branches for the key offered to adopt, and a setup that leaves a mark.
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

[[create.existing_branch_match]]
pattern = "story/{{ issue }}*"
scope = "local"

[setup]
run = ["sh", "-c", "echo ready >> .setup-ran"]
cwd = "worktree"
"#;

const WORKFLOW: &str = "tracker = \"jira\"\n\
     [projects.\"{root}\".config.workflow.statuses]\n\
     todo = \"To Do\"\n\
     started = \"In Progress\"\n\
     needs_review = \"In Review\"\n\
     done = [\"Done\"]\n\
     [projects.\"{root}\".config.workflow.queue]\n\
     query = \"assignee = currentUser()\"\n\
     board = 3\n";

fn issue(key: &str, status: &str, category: StatusCategory) -> Issue {
    Issue {
        key: key.to_owned(),
        summary: format!("Summary of {key}"),
        status: IssueStatus {
            name: status.to_owned(),
            category,
        },
        kind: Some("Story".to_owned()),
        priority: None,
        assignee: None,
        labels: Vec::new(),
    }
}

/// A host for runs that open no sessions: Start Work opens none.
struct NoSessions;

impl RoleHost for NoSessions {
    fn preflight(&self, _providers: &[&str]) -> Result<(), Refusal> {
        Ok(())
    }
    fn open(&self, _spec: &RoleSession, _sink: Arc<dyn AgentSink>) -> Result<SessionId, String> {
        Err("Start Work opens no sessions".to_owned())
    }
    fn send(&self, _session: &SessionId, _text: &str) -> Result<(), String> {
        Err("no sessions".to_owned())
    }
    fn close(&self, _session: &SessionId) {}
}

#[derive(Default)]
struct Filed(Mutex<Vec<InboxItem>>);

impl Notice for Filed {
    fn changed(&self, _flow: FlowId) {}
    fn filed(&self, item: &InboxItem) {
        self.0.lock().push(item.clone());
    }
}

struct Harness {
    fixture: GitFixture,
    app: Arc<App>,
    engine: Arc<Engine>,
    tracker: Arc<FakeTracker>,
    project: String,
    root: PathBuf,
    flow_db: PathBuf,
    _config: tempfile::TempDir,
}

impl Harness {
    fn new() -> Self {
        Self::with(|_| {})
    }

    /// `before` shapes the repository before the app reads it.
    fn with(before: impl FnOnce(&GitFixture)) -> Self {
        let fixture = GitFixture::new();
        fixture.commit("wtm.toml", CONFIG, "add wtm config");
        before(&fixture);
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
                    goal: None,
                    starts_at: None,
                    ends_at: None,
                    board: Some(3),
                })
                .with_issue(issue("ACME-1", "To Do", StatusCategory::ToDo), &[41])
                .with_issue(
                    issue("ACME-2", "In Progress", StatusCategory::InProgress),
                    &[41],
                )
                .with_issue(
                    issue("ACME-3", "In Review", StatusCategory::InProgress),
                    &[41],
                )
                .with_issue(
                    issue("ACME-4", "Blocked", StatusCategory::InProgress),
                    &[41],
                ),
        );
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
        let engine = Self::engine(&app);
        let h = Self {
            project: ProjectId::from_root(&root).to_string(),
            root,
            fixture,
            app,
            engine,
            tracker,
            flow_db,
            _config: config,
        };
        tracker::sync(&h.app, &h.project, SyncReason::Demand).unwrap();
        h
    }

    fn engine(app: &Arc<App>) -> Arc<Engine> {
        Engine::new(
            Arc::clone(app),
            Arc::new(NoSessions),
            Arc::new(FakeClock::new()) as Arc<dyn Clock>,
            Arc::new(Filed::default()),
            Tuning {
                tick_ms: 5,
                skills_wait_ms: 1_000,
                settle_ms: 1_000,
                nudges: 1,
            },
        )
    }

    fn db(&self) -> Arc<wtm_appdb::AppDb> {
        self.app.appdb.get().unwrap()
    }

    fn request(&self, key: &str) -> StartWork {
        StartWork {
            project: self.project.clone(),
            key: key.to_owned(),
            choice: None,
            by_drop: false,
            confirmed: false,
        }
    }

    fn start(&self, key: &str) -> StartOutcome {
        self.engine.start_work(&self.request(key)).unwrap()
    }

    fn view(&self, key: &str) -> IssueStartView {
        self.engine.issue_start(&self.project, key).unwrap()
    }

    /// Wait until nothing is being made for `key` and its moves have settled.
    fn settled(&self, key: &str) -> IssueStartView {
        for _ in 0..2_000 {
            let view = self.view(key);
            let making = view.working || view.job.as_ref().is_some_and(|j| j.state.is_busy());
            let moving = view.moves.iter().any(|m| {
                matches!(
                    m.state,
                    PublicationState::Posting
                        | PublicationState::Checking
                        | PublicationState::Waiting
                )
            });
            if !making && !moving {
                return view;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("{key} never settled: {:?}", self.view(key));
    }

    /// Worktrees other than the main checkout.
    fn worktrees(&self) -> Vec<String> {
        self.app
            .git
            .list_worktrees(&self.root)
            .unwrap()
            .into_iter()
            .filter(|w| !w.is_main)
            .map(|w| w.id.as_str().to_owned())
            .collect()
    }

    fn count(&self, table: &str) -> i64 {
        rusqlite::Connection::open(&self.flow_db)
            .unwrap()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    fn sibling(&self, name: &str) -> PathBuf {
        self.fixture.parent().join(name)
    }
}

fn started(outcome: &StartOutcome) -> FlowId {
    match outcome {
        StartOutcome::Started { flow, .. } => *flow,
        other => panic!("expected a start, got {other:?}"),
    }
}

fn same(a: &Path, b: &str) -> bool {
    std::fs::canonicalize(a).unwrap() == std::fs::canonicalize(b).unwrap()
}

// ═══════════════════════════════ once ═══════════════════════════════

#[test]
fn starting_an_issue_twice_gives_one_worktree_and_one_run_and_the_second_start_focuses_the_first() {
    let h = Harness::new();

    let first = started(&h.start("ACME-1"));
    let again = h.start("ACME-1");
    let view = h.settled("ACME-1");
    let third = h.start("ACME-1");

    assert!(
        matches!(again, StartOutcome::Focused { flow } if flow == first),
        "{again:?}"
    );
    assert!(
        matches!(third, StartOutcome::Focused { flow } if flow == first),
        "{third:?}"
    );
    assert_eq!(h.worktrees().len(), 1, "{:?}", h.worktrees());
    assert_eq!(h.count("flows"), 1);
    assert_eq!(h.count("work_items"), 1);
    assert_eq!(h.count("start_jobs"), 1);

    let job = view.job.unwrap();
    assert_eq!(job.state, StartJobState::Ready, "{job:?}");
    assert!(same(&h.sibling("ACME-1"), &job.worktree));
    let flow = view.flow.unwrap();
    assert_eq!(flow.state.kind, FlowKind::Implement);
    assert_eq!(
        flow.state.phase,
        Phase::Preparing,
        "the run stops after preparing: planning is a later item"
    );
    assert!(flow.state.hold.is_none() && flow.state.end.is_none());
    assert!(
        Path::new(&job.worktree).join(".setup-ran").exists(),
        "the project's setup ran in it, through the form's pipeline"
    );
}

#[test]
fn two_starts_arriving_together_record_one_run_between_them() {
    let h = Harness::new();
    let outcomes: Vec<StartOutcome> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..4)
            .map(|_| scope.spawn(|| h.engine.start_work(&h.request("ACME-1")).unwrap()))
            .collect();
        handles.into_iter().map(|t| t.join().unwrap()).collect()
    });
    h.settled("ACME-1");

    let started = outcomes
        .iter()
        .filter(|o| matches!(o, StartOutcome::Started { .. }))
        .count();
    assert_eq!(started, 1, "{outcomes:?}");
    assert_eq!(h.worktrees().len(), 1);
    assert_eq!(h.count("flows"), 1);
}

/// A `[[lookup]]` standing in for a slow tracker. Each call takes the next number with `mkdir`,
/// which only one caller can win, and call `nth` waits until `go` appears beside the numbers. The
/// form runs its lookups before it reads the repository, so a Start let go plans from whatever is
/// there by then.
fn held_lookup(gate: &Path, nth: u32) -> String {
    let script = concat!(
        r#"n=0; until mkdir "$1/call-$n" 2>/dev/null; do n=$((n+1)); done; "#,
        r#"if [ "$n" = "$2" ]; then until [ -e "$1/go" ]; do sleep 0.01; done; fi; "#,
        "echo ok",
    );
    format!(
        r#"
[[lookup]]
id = "gate"
run = ["sh", "-c", '{script}', "gate", '{gate}', "{nth}"]
format = "text"
on_error = "fail"
timeout_ms = 30000
"#,
        gate = gate.display(),
    )
}

/// Start ACME-1 on a thread the gate holds at the lookup's call `nth`, start it again meanwhile,
/// and let the first go once the second's worktree is made. The four threads above get this
/// interleaving only when the scheduler happens to give it; this gets it every time.
fn held_while_another_starts(nth: u32) -> (Harness, Result<StartOutcome, Refusal>, FlowId) {
    let gate = tempfile::tempdir().unwrap();
    let h = Harness::with(|fixture| {
        let config = format!("{CONFIG}{}", held_lookup(gate.path(), nth));
        fixture.commit("wtm.toml", &config, "hold the form's lookup");
    });
    let (held, winner) = std::thread::scope(|scope| {
        let held = scope.spawn(|| h.engine.start_work(&h.request("ACME-1")));
        let holding = gate.path().join(format!("call-{nth}"));
        for _ in 0..2_000 {
            if holding.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(holding.exists(), "the first Start never reached call {nth}");
        let winner = started(&h.start("ACME-1"));
        h.settled("ACME-1");
        std::fs::write(gate.path().join("go"), "").unwrap();
        (held.join().unwrap(), winner)
    });
    (h, held, winner)
}

#[test]
fn a_start_held_up_resolving_while_another_start_makes_the_worktree_focuses_that_run() {
    let (h, held, winner) = held_while_another_starts(0);

    assert!(
        matches!(held, Ok(StartOutcome::Focused { flow }) if flow == winner),
        "{held:?}"
    );
    assert_eq!(h.worktrees().len(), 1);
    assert_eq!(h.count("flows"), 1);
    assert_eq!(h.count("start_jobs"), 1);
}

#[test]
fn a_start_held_up_planning_while_another_start_makes_the_worktree_focuses_that_run() {
    let (h, held, winner) = held_while_another_starts(1);

    assert!(
        matches!(held, Ok(StartOutcome::Focused { flow }) if flow == winner),
        "{held:?}"
    );
    assert_eq!(h.worktrees().len(), 1);
    assert_eq!(h.count("flows"), 1);
    assert_eq!(h.count("start_jobs"), 1);
}

#[test]
fn a_directory_in_the_way_that_no_start_made_is_still_refused_with_nothing_recorded() {
    let h = Harness::new();
    // Where the form would put ACME-1's worktree, made by hand rather than by a Start.
    std::fs::create_dir(h.sibling("ACME-1")).unwrap();
    std::fs::write(h.sibling("ACME-1").join("notes"), "mine").unwrap();

    let refusal = h.engine.start_work(&h.request("ACME-1")).unwrap_err();

    assert!(refusal.message.contains("already exists"), "{refusal}");
    assert_eq!(h.count("work_items"), 0);
    assert_eq!(h.count("flows"), 0);
    assert_eq!(h.count("start_jobs"), 0);
}

#[test]
fn starting_then_dropping_the_card_on_the_start_column_gives_one_worktree_and_one_run() {
    let h = Harness::new();
    let first = started(&h.start("ACME-1"));
    h.settled("ACME-1");

    let mut drop = h.request("ACME-1");
    drop.by_drop = true;
    let dropped = h.engine.start_work(&drop).unwrap();
    // And dragged on to another column after that: a status change, still the same run.
    let moved = h
        .engine
        .move_issue(&h.project, "ACME-1", "In Review", false)
        .unwrap();
    let view = h.settled("ACME-1");

    assert!(
        matches!(dropped, StartOutcome::Focused { flow } if flow == first),
        "{dropped:?}"
    );
    assert!(
        matches!(moved, DropOutcome::Moving { flow } if flow == first),
        "{moved:?}"
    );
    assert_eq!(h.worktrees().len(), 1);
    assert_eq!(h.count("flows"), 1);
    assert_eq!(view.flow.unwrap().id, first);
    assert_eq!(h.tracker.status_of("ACME-1").as_deref(), Some("In Review"));
}

// ═══════════════════════════════ the resolution order ═══════════════════════════════

#[test]
fn a_worktree_whose_branch_carries_the_key_is_adopted_and_nothing_is_created() {
    let h = Harness::with(|fixture| {
        fixture.add_worktree("timezones", "feature/ACME-1-timezones");
    });

    let outcome = h.start("ACME-1");
    let view = h.settled("ACME-1");

    match outcome {
        StartOutcome::Started {
            candidate: Candidate::KeyInName { worktree },
            ..
        } => assert!(same(&h.sibling("timezones"), &worktree.id)),
        other => panic!("expected the worktree carrying the key, got {other:?}"),
    }
    assert_eq!(h.worktrees().len(), 1, "no second worktree");
    let job = view.job.unwrap();
    assert_eq!(job.state, StartJobState::Ready);
    assert_eq!(job.branch.as_deref(), Some("feature/ACME-1-timezones"));
    assert!(
        !Path::new(&job.worktree).join(".setup-ran").exists(),
        "an adopted worktree is used as it is: its setup is not run again"
    );
    let item = view.work_item.unwrap();
    assert!(h.db().links(item).unwrap().iter().any(|l| matches!(
        &l.link,
        WorkLink::Worktree { path } if same(&h.sibling("timezones"), path)
    )));
}

#[test]
fn two_worktrees_carrying_the_key_are_a_preview_in_order_and_nothing_is_recorded_until_one_is_picked()
 {
    let h = Harness::with(|fixture| {
        fixture.add_worktree("a", "feature/ACME-1-a");
        fixture.add_worktree("b", "bugfix/ACME-1-b");
        fixture.branch("story/ACME-1-old");
    });

    let preview = match h.start("ACME-1") {
        StartOutcome::Choose { preview } => preview,
        other => panic!("expected a preview, got {other:?}"),
    };
    let wtm_core::flow::Resolved::Choose { candidates } = &preview.resolved else {
        panic!("{:?}", preview.resolved);
    };
    let kinds: Vec<&str> = candidates
        .iter()
        .map(|c| match c {
            Candidate::KeyInName { .. } => "key",
            Candidate::BranchMatch { .. } => "branch",
            Candidate::New { .. } => "new",
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(kinds, ["key", "key", "branch", "new"]);
    assert_eq!(
        (
            h.count("flows"),
            h.count("work_items"),
            h.count("start_jobs")
        ),
        (0, 0, 0),
        "a preview records nothing"
    );

    let mut pick = h.request("ACME-1");
    pick.choice = Some(candidates[1].id());
    let flow = started(&h.engine.start_work(&pick).unwrap());
    let view = h.settled("ACME-1");
    assert_eq!(view.flow.unwrap().id, flow);
    assert!(same(&h.sibling("b"), &view.job.unwrap().worktree));
    assert_eq!(h.worktrees().len(), 2);
}

#[test]
fn an_offered_branch_is_adopted_into_a_new_worktree_once_it_is_picked() {
    let h = Harness::with(|fixture| fixture.branch("story/ACME-1-old"));

    let preview = match h.start("ACME-1") {
        StartOutcome::Choose { preview } => preview,
        other => panic!("a lone offered branch is a choice, got {other:?}"),
    };
    let offered = preview
        .resolved
        .find("branch:story/ACME-1-old")
        .unwrap()
        .clone();

    let mut pick = h.request("ACME-1");
    pick.choice = Some(offered.id());
    started(&h.engine.start_work(&pick).unwrap());
    let view = h.settled("ACME-1");

    let job = view.job.unwrap();
    assert_eq!(job.state, StartJobState::Ready, "{job:?}");
    assert_eq!(job.branch.as_deref(), Some("story/ACME-1-old"));
    assert_eq!(h.worktrees().len(), 1);
}

#[test]
fn the_bound_worktree_is_used_again_when_the_issue_is_started_after_its_run_was_stopped() {
    let h = Harness::new();
    let first = started(&h.start("ACME-1"));
    let bound = h.settled("ACME-1").job.unwrap().worktree;
    h.engine.cancel_start(first).unwrap();
    // A worktree carrying the key appears meanwhile; the bound one still wins.
    h.fixture.add_worktree("other", "feature/ACME-1-other");

    let second = h.start("ACME-1");
    let view = h.settled("ACME-1");

    match second {
        StartOutcome::Started {
            flow,
            candidate: Candidate::Bound { worktree },
        } => {
            assert_ne!(flow, first, "a new run");
            assert!(same(Path::new(&bound), &worktree.id));
        }
        other => panic!("expected the bound worktree, got {other:?}"),
    }
    assert_eq!(view.job.unwrap().worktree, bound);
    assert_eq!(h.worktrees().len(), 2, "the one Start made, and the other");
}

#[test]
fn an_issue_wtm_has_never_read_is_refused_with_nothing_recorded() {
    let h = Harness::new();
    let refusal = h.engine.start_work(&h.request("ACME-99")).unwrap_err();
    assert!(refusal.message.contains("sync"), "{refusal}");
    assert_eq!(h.count("work_items"), 0);
}

// ═══════════════════════════════ a crash mid-create ═══════════════════════════════

/// The record a Start leaves just before `git worktree add`, as a process that then died would.
fn cut_off_start(h: &Harness, key: &str) -> FlowId {
    let db = h.db();
    let item = db
        .create_work_item(
            &NewWorkItem {
                project: ProjectId::from_root(&h.root),
                title: format!("Summary of {key}"),
                issue: Some(wtm_core::flow::IssueRef {
                    tracker: "jira".to_owned(),
                    key: key.to_owned(),
                }),
                pull_request: None,
            },
            1,
        )
        .unwrap();
    let flow = db.start_flow(item.id, FlowKind::Implement, 1).unwrap().id;
    let path = h.sibling(key).to_string_lossy().into_owned();
    db.record_start_job(
        &NewStartJob {
            work_item: item.id,
            flow,
            project: ProjectId::from_root(&h.root),
            key: key.to_owned(),
            candidate: Candidate::New {
                branch: Some(format!("feature/{key}")),
                directory: path.clone(),
                base: "main".to_owned(),
            },
            worktree: path.clone(),
            branch: Some(format!("feature/{key}")),
            state: StartJobState::Creating,
        },
        2,
    )
    .unwrap();
    db.link(item.id, &WorkLink::Worktree { path }, 2).unwrap();
    db.record(
        flow,
        &FlowEvent::Advanced {
            to: Phase::Preparing,
        },
        3,
    )
    .unwrap();
    flow
}

#[test]
fn a_start_cut_off_after_its_worktree_was_made_resumes_by_setting_that_worktree_up_not_making_another()
 {
    let h = Harness::new();
    let flow = cut_off_start(&h, "ACME-1");
    // `git worktree add` finished; the setup never ran.
    h.fixture.add_worktree("ACME-1", "feature/ACME-1");

    let relaunched = Harness::engine(&h.app);
    relaunched.recover().unwrap();

    let state = h.db().flow(flow).unwrap().unwrap().state;
    assert!(
        matches!(state.hold, Some(Hold::Interrupted { .. })),
        "{state:?}"
    );
    let job = h.db().start_job_of(flow).unwrap().unwrap();
    assert_eq!(job.state, StartJobState::Interrupted);
    assert!(
        h.db()
            .inbox(Some(flow), None)
            .unwrap()
            .iter()
            .any(|i| i.kind == InboxKind::Interrupted),
        "the inbox says so"
    );

    // Start focuses the interrupted run rather than starting another.
    let again = relaunched.start_work(&h.request("ACME-1")).unwrap();
    assert!(
        matches!(again, StartOutcome::Focused { flow: f } if f == flow),
        "{again:?}"
    );

    // The inbox's Resume, which reaches Start Work through the engine's own resume.
    relaunched.resume(flow).unwrap();
    let h = Harness {
        engine: relaunched,
        ..h
    };
    let view = h.settled("ACME-1");

    assert_eq!(view.job.as_ref().unwrap().state, StartJobState::Ready);
    assert_eq!(h.worktrees().len(), 1, "never a second worktree");
    assert!(
        h.sibling("ACME-1").join(".setup-ran").exists(),
        "Retry setup ran the project's setup in the worktree it found"
    );
    let state = view.flow.unwrap().state;
    assert!(state.hold.is_none() && state.end.is_none(), "{state:?}");
    assert!(
        view.inbox.iter().all(|i| i.kind != InboxKind::Interrupted),
        "resuming settled the interruption"
    );
}

#[test]
fn a_start_cut_off_before_its_worktree_was_made_resumes_by_making_it_from_the_same_plan() {
    let h = Harness::new();
    let flow = cut_off_start(&h, "ACME-1");

    let relaunched = Harness::engine(&h.app);
    relaunched.recover().unwrap();
    relaunched.resume_start(flow, false).unwrap();
    let h = Harness {
        engine: relaunched,
        ..h
    };
    let view = h.settled("ACME-1");

    let job = view.job.unwrap();
    assert_eq!(job.state, StartJobState::Ready, "{job:?}");
    assert!(same(&h.sibling("ACME-1"), &job.worktree));
    assert_eq!(h.worktrees().len(), 1);
}

#[test]
fn using_an_interrupted_starts_worktree_as_it_is_runs_nothing_in_it() {
    let h = Harness::new();
    let flow = cut_off_start(&h, "ACME-1");
    h.fixture.add_worktree("ACME-1", "feature/ACME-1");
    let relaunched = Harness::engine(&h.app);
    relaunched.recover().unwrap();

    relaunched.resume_start(flow, true).unwrap();

    let job = h.db().start_job_of(flow).unwrap().unwrap();
    assert_eq!(job.state, StartJobState::Ready);
    assert!(!h.sibling("ACME-1").join(".setup-ran").exists());
}

// ═══════════════════════════════ the move ═══════════════════════════════

#[test]
fn starting_moves_the_issue_to_the_start_status_once_reading_it_before_and_after() {
    let h = Harness::new();
    started(&h.start("ACME-1"));
    let view = h.settled("ACME-1");

    assert_eq!(
        h.tracker.status_of("ACME-1").as_deref(),
        Some("In Progress")
    );
    assert_eq!(h.tracker.moves(), ["move ACME-1 In Progress"]);
    let calls = h.tracker.calls();
    let sent = calls.iter().position(|c| c.starts_with("move ")).unwrap();
    assert!(
        calls[..sent].iter().any(|c| c == "issue ACME-1"),
        "read before the move: {calls:?}"
    );
    assert!(
        calls[sent..].iter().any(|c| c == "issue ACME-1"),
        "read after the move: {calls:?}"
    );
    assert_eq!(view.moves.len(), 1);
    assert_eq!(view.moves[0].state, PublicationState::Published);

    // A second Start is a focus, and sends nothing.
    h.start("ACME-1");
    h.settled("ACME-1");
    assert_eq!(h.tracker.moves().len(), 1);
}

#[test]
fn an_issue_already_in_the_start_status_is_adopted_and_no_move_is_sent() {
    let h = Harness::new();
    // Someone started it in the tracker after the last sync: the mirror still says To Do.
    h.tracker
        .move_issue("ACME-1", "In Progress", StatusCategory::InProgress);

    started(&h.start("ACME-1"));
    let view = h.settled("ACME-1");

    assert!(h.tracker.moves().is_empty(), "{:?}", h.tracker.calls());
    let op = &view.moves[0].operation;
    assert_eq!(op.state, OperationState::Done);
    assert!(matches!(
        op.result,
        Some(wtm_core::flow::OperationResult::Moved { adopted: true, .. })
    ));
}

#[test]
fn an_issue_that_moved_elsewhere_stops_the_move_asks_and_sends_nothing_until_you_say() {
    let h = Harness::new();
    h.tracker
        .move_issue("ACME-1", "Blocked", StatusCategory::InProgress);

    started(&h.start("ACME-1"));
    let view = h.settled("ACME-1");

    assert!(h.tracker.moves().is_empty());
    assert_eq!(h.tracker.status_of("ACME-1").as_deref(), Some("Blocked"));
    let moved = &view.moves[0];
    assert_eq!(moved.operation.state, OperationState::NeedsAttention);
    assert!(matches!(
        &moved.operation.attention,
        Some(Attention::StatusMoved(m)) if m.current == "Blocked" && m.expected == "To Do"
    ));
    assert!(view.inbox.iter().any(|i| i.kind == InboxKind::Approval
        && i.target
            == InboxTarget::Approval {
                approval: moved.approval
            }));
    assert_eq!(
        view.job.unwrap().state,
        StartJobState::Ready,
        "the worktree is made either way"
    );

    // You say move it anyway: a new move, from where it is now.
    h.engine
        .settle_move(moved.approval, MoveAction::Anyway)
        .unwrap();
    let view = h.settled("ACME-1");
    assert_eq!(h.tracker.moves(), ["move ACME-1 In Progress"]);
    assert!(view.inbox.iter().all(|i| i.kind != InboxKind::Approval));
}

#[test]
fn a_move_whose_reply_was_lost_is_found_done_by_reading_again_and_is_never_sent_twice() {
    let h = Harness::new();
    h.tracker.lose_next_move_reply();

    started(&h.start("ACME-1"));
    let view = h.settled("ACME-1");

    assert_eq!(h.tracker.moves().len(), 1);
    let op = &view.moves[0].operation;
    assert_eq!(op.state, OperationState::Done);
    assert!(matches!(
        op.result,
        Some(wtm_core::flow::OperationResult::Moved { adopted: true, .. })
    ));
}

#[test]
fn a_move_a_crash_cut_off_is_settled_by_reading_the_issue_and_never_sent_again() {
    let h = Harness::new();
    let flow = cut_off_start(&h, "ACME-1");
    let payload = MoveIssuePayload {
        tracker: TrackerKind::Jira,
        key: "ACME-1".to_owned(),
        from: "To Do".to_owned(),
        to: "In Progress".to_owned(),
    };
    let request = wtm_core::flow::MoveRequest {
        payload: payload.clone(),
        cause: wtm_core::flow::MoveCause::Start,
    };
    let approved = h
        .db()
        .approve(
            &wtm_core::flow::NewApproval::move_issue(flow, &request),
            &[NewOperation::new(
                flow,
                0,
                OperationPayload::MoveIssue(payload),
            )],
            4,
        )
        .unwrap();
    h.db()
        .change_operation(approved.operations[0].id, &OperationChange::sending(), 5)
        .unwrap();
    // The send reached the tracker; the process died before the reply.
    h.tracker
        .move_issue("ACME-1", "In Progress", StatusCategory::InProgress);

    let relaunched = Harness::engine(&h.app);
    relaunched.recover_publications().unwrap();

    let op = h
        .db()
        .operation(approved.operations[0].id)
        .unwrap()
        .unwrap();
    assert_eq!(op.state, OperationState::Done);
    assert!(h.tracker.moves().is_empty(), "nothing was sent again");
}

// ═══════════════════════════════ the board ═══════════════════════════════

#[test]
fn dragging_an_issue_backward_moves_only_its_status_and_offers_to_pause_its_run() {
    let h = Harness::new();
    let flow = started(&h.start("ACME-1"));
    let before = h.settled("ACME-1");

    let outcome = h
        .engine
        .move_issue(&h.project, "ACME-1", "To Do", false)
        .unwrap();
    let after = h.settled("ACME-1");

    assert!(matches!(outcome, DropOutcome::Moving { flow: f } if f == flow));
    assert_eq!(h.tracker.status_of("ACME-1").as_deref(), Some("To Do"));
    let (was, is) = (before.flow.unwrap(), after.flow.unwrap());
    assert_eq!(was.state, is.state, "the run is exactly as it was");
    assert_eq!(was.seq, is.seq, "not one event was added to it");
    assert_eq!(before.job, after.job, "nor its job");
    assert_eq!(
        h.worktrees(),
        [before.job.unwrap().worktree],
        "nor its worktree"
    );

    let offer = after
        .inbox
        .iter()
        .find(|i| i.kind == InboxKind::Approval && i.target == InboxTarget::Flow)
        .expect("the inbox offers to pause the run");
    assert!(offer.title.contains("Pause"), "{}", offer.title);

    // Pausing is a hold on the run, and that is all.
    h.engine.pause_run(flow).unwrap();
    let paused = h.view("ACME-1");
    assert_eq!(paused.flow.unwrap().state.hold, Some(Hold::Paused));
    assert!(
        paused.inbox.iter().all(|i| i.target != InboxTarget::Flow),
        "the offer is answered"
    );
    assert_eq!(h.worktrees().len(), 1);
    h.engine.unpause_run(flow).unwrap();
    assert!(h.view("ACME-1").flow.unwrap().state.hold.is_none());
}

#[test]
fn moving_an_issue_out_of_review_needs_your_confirmation_and_nothing_moves_until_it_is_given() {
    let h = Harness::new();
    let mut start = h.request("ACME-3");
    start.by_drop = true;
    let asked = h.engine.start_work(&start).unwrap();
    assert!(
        matches!(&asked, StartOutcome::Confirm { from, to } if from == "In Review" && to == "In Progress"),
        "{asked:?}"
    );
    assert_eq!(
        h.count("work_items"),
        0,
        "nothing recorded before you confirm"
    );

    start.confirmed = true;
    let flow = started(&h.engine.start_work(&start).unwrap());
    h.settled("ACME-3");
    assert_eq!(
        h.tracker.status_of("ACME-3").as_deref(),
        Some("In Progress")
    );

    // Back to review, then out of it by a drop elsewhere: asked again.
    h.engine
        .move_issue(&h.project, "ACME-3", "In Review", false)
        .unwrap();
    h.settled("ACME-3");
    tracker::sync(&h.app, &h.project, SyncReason::Demand).unwrap();
    let out = h
        .engine
        .move_issue(&h.project, "ACME-3", "To Do", false)
        .unwrap();
    assert!(matches!(out, DropOutcome::Confirm { .. }), "{out:?}");
    assert_eq!(h.tracker.status_of("ACME-3").as_deref(), Some("In Review"));
    let out = h
        .engine
        .move_issue(&h.project, "ACME-3", "To Do", true)
        .unwrap();
    assert!(matches!(out, DropOutcome::Moving { flow: f } if f == flow));
}

#[test]
fn the_start_button_leaves_an_issue_past_to_do_where_it_is() {
    let h = Harness::new();
    started(&h.start("ACME-4"));
    let view = h.settled("ACME-4");
    assert!(view.moves.is_empty());
    assert!(h.tracker.moves().is_empty());
    assert_eq!(h.tracker.status_of("ACME-4").as_deref(), Some("Blocked"));
}

#[test]
fn an_issue_with_no_run_is_not_moved_from_the_board_and_a_drop_on_the_start_column_is_a_start() {
    let h = Harness::new();
    let refused = h
        .engine
        .move_issue(&h.project, "ACME-1", "In Review", false)
        .unwrap_err();
    assert!(refused.message.contains("start"), "{refused}");
    assert!(h.tracker.moves().is_empty());
    assert_eq!(h.count("work_items"), 0);

    let start = h
        .engine
        .move_issue(&h.project, "ACME-1", "In Progress", false)
        .unwrap();
    assert!(matches!(start, DropOutcome::Start), "{start:?}");
}

#[test]
fn a_sync_after_start_shows_the_issues_run_and_bound_worktree_in_its_queue_row() {
    let h = Harness::new();
    let flow = started(&h.start("ACME-1"));
    let job = h.settled("ACME-1").job.unwrap();

    let queue = tracker::sync(&h.app, &h.project, SyncReason::AfterAction)
        .unwrap()
        .queue;
    let row = queue
        .issues
        .iter()
        .find(|r| r.issue.key == "ACME-1")
        .unwrap();
    let run = row.run.as_ref().unwrap();
    assert_eq!(run.flow, flow);
    assert_eq!(run.start, Some(StartJobState::Ready));
    assert_eq!(row.worktrees.first(), Some(&job.worktree));
    assert_eq!(row.issue.status.name, "In Progress");
    assert_eq!(
        h.db()
            .inbox(Some(flow), Some(&ProjectId::from_root(&h.root)))
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn a_paused_run_is_not_held_again_and_releasing_names_the_pause() {
    let h = Harness::new();
    let flow = started(&h.start("ACME-1"));
    h.settled("ACME-1");
    h.engine.pause_run(flow).unwrap();
    assert!(h.engine.pause_run(flow).is_err());
    h.engine.unpause_run(flow).unwrap();
    assert!(h.engine.unpause_run(flow).is_err());
    assert_eq!(
        h.db()
            .flow(flow)
            .unwrap()
            .unwrap()
            .state
            .hold
            .map(|hold| hold.kind()),
        None::<HoldKind>
    );
}
