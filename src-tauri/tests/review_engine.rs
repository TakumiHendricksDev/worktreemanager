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

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};

use parking_lot::Mutex;
use serde_json::{Value, json};
use wtm_agent::session::AgentSink;
use wtm_app_lib::app::{App, FlowRole, SessionScope};
use wtm_app_lib::flow::host::RoleHost;
use wtm_app_lib::flow::{Engine, FlowView, Notice, StartReview, Tuning};
use wtm_app_lib::flow_session::RoleSession;
use wtm_app_lib::handoff::{Caller, CodeCall};
use wtm_config::AppPaths;
use wtm_core::flow::{
    FlowId, Hold, InboxItem, InboxKind, InterruptCause, InvocationStatus, Phase, StepStatus,
    Submission,
};
use wtm_core::model::{AgentEvent, AgentSkill, ApprovalRequest, ProjectId, SessionId, Usage};
use wtm_core::ports::clock::Clock;
use wtm_testkit::{FakeClock, GitFixture};

// ═══════════════════════════════ the fake agents ═══════════════════════════════

/// What a fake session does with each turn it is sent.
#[derive(Debug, Clone)]
enum Behaviour {
    /// Submit these payloads in order, each a `flow_submit` call, then end the turn.
    Submit(Vec<Value>),
    /// Make these `flow_submit` calls, whole arguments and all, then end the turn.
    Calls(Vec<Value>),
    /// Wait until `count` sessions are open at once, then submit. Proves a stage runs together:
    /// run one at a time, the first would wait forever and its timeout would fail the test.
    Rendezvous(usize, Value),
    /// Never answer.
    Silent,
    /// End every turn without submitting.
    EndWithoutSubmitting,
    /// End the first turn without submitting, and submit when nudged.
    SubmitWhenNudged(Value),
    /// Report a usage limit instead of working.
    Limit(Option<u64>),
    /// Ask for a permission twice, then submit.
    AskPermissionThenSubmit(Value),
}

/// How a script picks a behaviour: by role and attempt (1 for the first).
type Script = Box<dyn Fn(&str, u32) -> Behaviour + Send + Sync>;

struct FakeSession {
    role: FlowRole,
    token: String,
    sink: Arc<dyn AgentSink>,
    behaviour: Behaviour,
    turns: u32,
}

/// One `open`, as the engine asked for it.
#[derive(Debug, Clone)]
struct Opened {
    role: String,
    cwd: Option<PathBuf>,
    cwd_existed: bool,
    brief: String,
}

struct FakeHost {
    me: Weak<FakeHost>,
    app: Arc<App>,
    /// The engine its agents' tool calls go to: the newest one, as after a relaunch.
    engine: Mutex<Weak<Engine>>,
    script: Mutex<Script>,
    /// Skills each provider lists when a session opens, by provider id.
    skills: Mutex<BTreeMap<String, Vec<String>>>,
    refuse_preflight: Mutex<Option<String>>,
    sessions: Mutex<BTreeMap<String, FakeSession>>,
    opened: Mutex<Vec<Opened>>,
    /// Every answer a run tool gave a fake agent, by role: `(ok, text)`.
    answers: Mutex<Vec<(String, bool, String)>>,
    turns_sent: AtomicUsize,
    open_now: AtomicUsize,
    open_most: AtomicUsize,
    next: AtomicU64,
}

impl FakeHost {
    fn new(app: Arc<App>, script: Script) -> Arc<Self> {
        Arc::new_cyclic(|me| Self {
            me: me.clone(),
            app,
            engine: Mutex::new(Weak::new()),
            script: Mutex::new(script),
            skills: Mutex::new(BTreeMap::new()),
            refuse_preflight: Mutex::new(None),
            sessions: Mutex::new(BTreeMap::new()),
            opened: Mutex::new(Vec::new()),
            answers: Mutex::new(Vec::new()),
            turns_sent: AtomicUsize::new(0),
            open_now: AtomicUsize::new(0),
            open_most: AtomicUsize::new(0),
            next: AtomicU64::new(1),
        })
    }

    fn rescript(&self, script: Script) {
        *self.script.lock() = script;
    }

    fn opened_roles(&self) -> Vec<String> {
        self.opened.lock().iter().map(|o| o.role.clone()).collect()
    }

    fn attempts_of(&self, role: &str) -> u32 {
        u32::try_from(self.opened_roles().iter().filter(|r| *r == role).count()).unwrap()
    }

    fn call(&self, session: &str, tool: &str, args: Value) -> (bool, String) {
        let (token, role) = {
            let sessions = self.sessions.lock();
            let Some(s) = sessions.get(session) else {
                return (false, "closed".to_owned());
            };
            (s.token.clone(), s.role.role.clone())
        };
        let engine = self.engine.lock().upgrade();
        let response = wtm_app_lib::flow_tools::answer_with(
            &self.app,
            engine.as_deref(),
            &token,
            &CodeCall {
                tool: tool.to_owned(),
                args,
            },
        );
        let text = response.text.or(response.error).unwrap_or_default();
        self.answers.lock().push((role, response.ok, text.clone()));
        (response.ok, text)
    }

    fn emit(&self, session: &str, event: &AgentEvent) {
        let sink = self
            .sessions
            .lock()
            .get(session)
            .map(|s| Arc::clone(&s.sink));
        if let Some(sink) = sink {
            sink.on_event(&SessionId::new(session), event);
        }
    }

    fn finish_turn(&self, session: &str) {
        self.emit(
            session,
            &AgentEvent::TurnFinished {
                turn: "t".to_owned(),
                usage: Usage::default(),
                cost_usd: None,
            },
        );
    }

    fn submit(&self, session: &str, payload: &Value) {
        self.call(
            session,
            "flow_submit",
            json!({ "kind": "findings", "payload": payload }),
        );
    }

    /// Play one turn of a session's behaviour, as the CLI would: tool calls, then the turn's end.
    fn play(&self, session: &str) {
        let (behaviour, turn) = {
            let mut sessions = self.sessions.lock();
            let Some(s) = sessions.get_mut(session) else {
                return;
            };
            s.turns += 1;
            (s.behaviour.clone(), s.turns)
        };
        match behaviour {
            Behaviour::Submit(payloads) => {
                for payload in &payloads {
                    self.submit(session, payload);
                }
                self.finish_turn(session);
            }
            Behaviour::Calls(calls) => {
                for args in calls {
                    self.call(session, "flow_submit", args);
                }
                self.finish_turn(session);
            }
            Behaviour::Rendezvous(count, payload) => {
                for _ in 0..2_000 {
                    if self.open_now.load(Ordering::SeqCst) >= count {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                self.submit(session, &payload);
                self.finish_turn(session);
            }
            Behaviour::Silent => {}
            Behaviour::EndWithoutSubmitting => self.finish_turn(session),
            Behaviour::SubmitWhenNudged(payload) => {
                if turn > 1 {
                    self.submit(session, &payload);
                }
                self.finish_turn(session);
            }
            Behaviour::Limit(resets_at) => {
                self.call(
                    session,
                    "flow_progress",
                    json!({ "text": "reading export.py" }),
                );
                self.emit(
                    session,
                    &AgentEvent::LimitReached {
                        message: "You've hit your usage limit.".to_owned(),
                        resets_at,
                    },
                );
            }
            Behaviour::AskPermissionThenSubmit(payload) => {
                for id in ["p1", "p2"] {
                    self.emit(
                        session,
                        &AgentEvent::ApprovalRequested {
                            id: id.to_owned(),
                            blocking: true,
                            request: ApprovalRequest::Command {
                                command: "git push".to_owned(),
                                cwd: None,
                                reason: None,
                            },
                        },
                    );
                }
                self.submit(session, &payload);
                self.finish_turn(session);
            }
        }
    }
}

impl RoleHost for FakeHost {
    fn preflight(&self, _providers: &[&str]) -> Result<(), String> {
        match self.refuse_preflight.lock().clone() {
            Some(refusal) => Err(refusal),
            None => Ok(()),
        }
    }

    fn open(&self, spec: &RoleSession, sink: Arc<dyn AgentSink>) -> Result<SessionId, String> {
        let attempt = self.attempts_of(&spec.role.role) + 1;
        self.opened.lock().push(Opened {
            role: spec.role.role.clone(),
            cwd: spec.cwd.clone(),
            cwd_existed: spec.cwd.as_ref().is_some_and(|cwd| cwd.is_dir()),
            brief: spec.brief.clone(),
        });
        let behaviour = (self.script.lock())(&spec.role.role, attempt);
        let id = format!("fake-{}", self.next.fetch_add(1, Ordering::SeqCst));
        let token = self.app.handoff.issue(Caller {
            scope: SessionScope::run(&spec.project, &spec.worktree, spec.role.clone()),
            provider: spec.provider.clone(),
            effort: None,
            mode: None,
            session: None,
        });
        self.app.flow_ledger.brief(&spec.role, &spec.brief);
        self.sessions.lock().insert(
            id.clone(),
            FakeSession {
                role: spec.role.clone(),
                token,
                sink: Arc::clone(&sink),
                behaviour,
                turns: 0,
            },
        );
        let now = self.open_now.fetch_add(1, Ordering::SeqCst) + 1;
        self.open_most.fetch_max(now, Ordering::SeqCst);

        // Said while opening, before the engine has the id back: the provider's own skill list,
        // which is what a CLI's `initialize` or `skills/list` reply is.
        if let Some(skills) = self.skills.lock().get(&spec.provider).cloned() {
            let session = SessionId::new(&id);
            sink.on_event(
                &session,
                &AgentEvent::SkillsListed {
                    skills: skills
                        .into_iter()
                        .map(|name| AgentSkill {
                            name,
                            description: None,
                            scope: None,
                        })
                        .collect(),
                },
            );
        }
        Ok(SessionId::new(&id))
    }

    fn send(&self, session: &SessionId, _text: &str) -> Result<(), String> {
        self.turns_sent.fetch_add(1, Ordering::SeqCst);
        let me = self.me.upgrade().ok_or("the host is gone")?;
        let id = session.as_str().to_owned();
        // On a thread of its own, as a CLI answers on its own process: the engine's role thread is
        // waiting on the session, and a turn played inline would answer before it listened.
        std::thread::spawn(move || me.play(&id));
        Ok(())
    }

    fn close(&self, session: &SessionId) {
        if self.sessions.lock().remove(session.as_str()).is_some() {
            self.open_now.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

/// Remembers what the engine announced.
#[derive(Default)]
struct Heard {
    filed: Mutex<Vec<InboxItem>>,
}

impl Notice for Heard {
    fn changed(&self, _flow: FlowId) {}
    fn filed(&self, item: &InboxItem) {
        self.filed.lock().push(item.clone());
    }
}

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
    _fixture: GitFixture,
    _data: tempfile::TempDir,
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
    assert!(view.inbox.is_empty());
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
    assert!(view.inbox.is_empty(), "a retry that worked needs nobody");
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
    assert_eq!(view.inbox.len(), 1);
    assert_eq!(view.inbox[0].kind, InboxKind::Failure);
    assert_eq!(view.inbox[0].subject, "fragile");
    assert_eq!(h.heard.filed.lock().len(), 1, "notified once");
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
            json!({ "kind": "plan", "payload": { "summary": "s", "steps": ["a"] } }),
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
    assert!(view.inbox.is_empty(), "the capacity item resolved itself");
    assert_eq!(h.heard.filed.lock().len(), 1);
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
        brief.contains("You run in place of another role"),
        "{brief}"
    );
    assert_eq!(view.inbox.len(), 1);
    assert_eq!(view.inbox[0].kind, InboxKind::Capacity);
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
    assert!(view.inbox.is_empty());
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
    h.until(flow, "correctness running", |v| {
        Harness::statuses(v, "correctness") == [InvocationStatus::Running]
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

    assert!(refused.is_err_and(|why| why.contains("API key")));
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
        .filter(|i| i.kind == InboxKind::Permission)
        .collect();
    assert_eq!(asked.len(), 1, "two prompts, one item");
    assert!(asked[0].body.contains("`git push`"));
    assert_eq!(h.heard.filed.lock().len(), 1);
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
