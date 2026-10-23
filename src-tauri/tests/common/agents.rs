//! Fake agents for the review engine's tests: a [`RoleHost`] whose sessions play a script, calling
//! the real run tools with the tokens their sessions were issued.
//!
//! Shared by `review_engine.rs` and `pull_request_reviews.rs`, which drive the same engine through
//! a review of your own work and of a teammate's pull request.

#![allow(dead_code, clippy::unwrap_used)]

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Weak};

use parking_lot::Mutex;
use serde_json::{Value, json};
use wtm_agent::session::AgentSink;
use wtm_app_lib::app::{App, FlowRole, SessionScope};
use wtm_app_lib::flow::host::{PaneStore, RoleHost, RuntimePane};
use wtm_app_lib::flow::{Engine, Notice, Refusal};
use wtm_app_lib::flow_session::RoleSession;
use wtm_app_lib::handoff::{Caller, CodeCall};
use wtm_core::flow::{FlowId, InboxItem, LimitReading, RoleProvider};
use wtm_core::model::{AgentEvent, AgentSkill, ApprovalRequest, SessionId, Usage};

// ═══════════════════════════════ the fake agents ═══════════════════════════════

/// What a fake session does with each turn it is sent.
#[derive(Debug, Clone)]
pub(crate) enum Behaviour {
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
    /// Ask the app to run these checks, by name, then submit.
    CheckThenSubmit(Vec<&'static str>, Value),
    /// Ask you this with `flow_ask`, then submit anyway, as a review role is told to.
    AskThenSubmit(Value, Value),
    /// Report progress a few times, then submit.
    ProgressThenSubmit(Value),
    /// Ask the app for something with `flow_request`, such as posting a comment, then submit.
    RequestThenSubmit(Value, Value),
    /// A writer: do these, in its working directory and through its run tools, then end the turn.
    Acts(Vec<Act>),
    /// Make these run tool calls, by name, in order; then end the turn when `end` says so. A
    /// planner that asks does not end its own turn: the engine interrupts it.
    Tools(Vec<(&'static str, Value)>, bool),
    /// Say this as the turn's reply, in words, and end the turn: a discussion turn.
    Reply(&'static str),
    /// Keep the session open, working, until the flag is set, then submit. A role still mid-turn,
    /// which is what holds a run-session place. A value with a `kind` is the whole `flow_submit`
    /// call, as a planner's plan is; anything else is a findings payload.
    Hold(Arc<AtomicBool>, Value),
    /// A designer: `prototype_prepare` with this brief, then `prototype_publish` under the id and
    /// revision it answered with, this HTML and these reuse notes; then end the turn.
    Design {
        brief: &'static str,
        html: &'static str,
        reuse: &'static str,
    },
}

/// One thing a fake writer does.
#[derive(Debug, Clone)]
pub(crate) enum Act {
    /// Write a file, relative to its working directory.
    Write(&'static str, &'static str),
    /// Put a file back as it was at `HEAD`, as a writer told to undo it would.
    Restore(&'static str),
    /// Call `flow_checkpoint` with this message.
    Checkpoint(&'static str),
    /// Call `flow_submit` with a handoff saying this.
    Handoff(&'static str),
    /// Call `flow_submit` with these findings, as a reviewer that read the worktree does.
    Findings(Value),
    /// Wait until the flag is set, as a writer still working does.
    WaitFor(Arc<AtomicBool>),
}

/// How a script picks a behaviour: by role and attempt (1 for the first).
pub(crate) type Script = Box<dyn Fn(&str, u32) -> Behaviour + Send + Sync>;

pub(crate) struct FakeSession {
    pub(crate) role: FlowRole,
    pub(crate) cwd: Option<PathBuf>,
    pub(crate) token: String,
    pub(crate) sink: Arc<dyn AgentSink>,
    pub(crate) behaviour: Behaviour,
    pub(crate) turns: u32,
}

/// One `open`, as the engine asked for it.
#[derive(Debug, Clone)]
pub(crate) struct Opened {
    pub(crate) role: String,
    /// The provider it was opened on, which is not always the role's own.
    pub(crate) provider: String,
    pub(crate) model: Option<String>,
    pub(crate) cwd: Option<PathBuf>,
    pub(crate) cwd_existed: bool,
    pub(crate) brief: String,
    /// The provider conversation the session was to continue, when it was a resume.
    pub(crate) resume: Option<String>,
    /// The run token it was issued.
    pub(crate) token: String,
    /// The turns it was sent, in order.
    pub(crate) turns: Arc<Mutex<Vec<String>>>,
    /// The pane its token was scoped to, for a runtime role.
    pub(crate) browser: Option<String>,
    /// Whether its token was a designer's, with the prototype tools.
    pub(crate) design: bool,
}

/// A runtime role's pane, as the engine asked the host for it.
#[derive(Debug, Clone)]
pub(crate) struct FakePane {
    pub(crate) id: String,
    pub(crate) url: String,
    pub(crate) closed: bool,
    /// Which roles' sessions were open when it was opened: none, if it came before the session.
    pub(crate) sessions_open: usize,
}

pub(crate) struct FakeHost {
    pub(crate) me: Weak<FakeHost>,
    pub(crate) app: Arc<App>,
    /// The engine its agents' tool calls go to: the newest one, as after a relaunch.
    pub(crate) engine: Mutex<Weak<Engine>>,
    pub(crate) script: Mutex<Script>,
    /// Skills each provider lists when a session opens, by provider id.
    pub(crate) skills: Mutex<BTreeMap<String, Vec<String>>>,
    pub(crate) refuse_preflight: Mutex<Option<String>>,
    pub(crate) sessions: Mutex<BTreeMap<String, FakeSession>>,
    pub(crate) opened: Mutex<Vec<Opened>>,
    /// Every answer a run tool gave a fake agent, by role: `(ok, text)`.
    pub(crate) answers: Mutex<Vec<(String, bool, String)>>,
    pub(crate) turns_sent: AtomicUsize,
    /// How many times the engine interrupted a turn.
    pub(crate) interrupts: AtomicUsize,
    /// Provider conversations started, numbered, as a CLI's own ids would be.
    pub(crate) conversations: AtomicU64,
    pub(crate) open_now: AtomicUsize,
    pub(crate) open_most: AtomicUsize,
    pub(crate) next: AtomicU64,
    /// The store this host says it built runtime panes on. Incognito unless a test says otherwise.
    pub(crate) pane_store: Mutex<PaneStore>,
    pub(crate) panes: Mutex<Vec<FakePane>>,
    /// What each recheck of a provider's limits answers, in order; unknown once they run out.
    pub(crate) readings: Mutex<std::collections::VecDeque<LimitReading>>,
    /// How many times the engine asked a provider for its limits.
    pub(crate) limits_asked: AtomicUsize,
    /// The model a session says it runs when it starts, by provider: what a CLI's `init` reports.
    pub(crate) ready_models: Mutex<BTreeMap<String, String>>,
}

impl FakeHost {
    pub(crate) fn new(app: Arc<App>, script: Script) -> Arc<Self> {
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
            interrupts: AtomicUsize::new(0),
            conversations: AtomicU64::new(0),
            open_now: AtomicUsize::new(0),
            open_most: AtomicUsize::new(0),
            next: AtomicU64::new(1),
            pane_store: Mutex::new(PaneStore::Incognito),
            panes: Mutex::new(Vec::new()),
            readings: Mutex::new(std::collections::VecDeque::new()),
            limits_asked: AtomicUsize::new(0),
            ready_models: Mutex::new(BTreeMap::new()),
        })
    }

    pub(crate) fn rescript(&self, script: Script) {
        *self.script.lock() = script;
    }

    pub(crate) fn opened_roles(&self) -> Vec<String> {
        self.opened.lock().iter().map(|o| o.role.clone()).collect()
    }

    pub(crate) fn attempts_of(&self, role: &str) -> u32 {
        u32::try_from(self.opened_roles().iter().filter(|r| *r == role).count()).unwrap()
    }

    pub(crate) fn call(&self, session: &str, tool: &str, args: Value) -> (bool, String) {
        let (token, role) = {
            let sessions = self.sessions.lock();
            let Some(s) = sessions.get(session) else {
                return (false, "closed".to_owned());
            };
            (s.token.clone(), s.role.role.clone())
        };
        let engine = self.engine.lock().upgrade();
        let call = CodeCall {
            tool: tool.to_owned(),
            args,
        };
        // The prototype tools go to their own handler, as the socket sends them (`Action::Design`).
        let response = if tool.starts_with(wtm_app_lib::prototype_tools::PREFIX) {
            wtm_app_lib::prototype_tools::answer_for_run(
                &self.app,
                engine.as_deref(),
                &token,
                &call,
            )
        } else {
            wtm_app_lib::flow_tools::answer_with(&self.app, engine.as_deref(), &token, &call)
        };
        let text = response.text.or(response.error).unwrap_or_default();
        self.answers.lock().push((role, response.ok, text.clone()));
        (response.ok, text)
    }

    pub(crate) fn emit(&self, session: &str, event: &AgentEvent) {
        let sink = self
            .sessions
            .lock()
            .get(session)
            .map(|s| Arc::clone(&s.sink));
        if let Some(sink) = sink {
            sink.on_event(&SessionId::new(session), event);
        }
    }

    pub(crate) fn finish_turn(&self, session: &str) {
        self.emit(
            session,
            &AgentEvent::TurnFinished {
                turn: "t".to_owned(),
                usage: Usage::default(),
                cost_usd: None,
            },
        );
    }

    pub(crate) fn submit(&self, session: &str, payload: &Value) {
        self.call(
            session,
            "flow_submit",
            json!({ "kind": "findings", "payload": payload }),
        );
    }

    /// Play one turn of a session's behaviour, as the CLI would: tool calls, then the turn's end.
    pub(crate) fn play(&self, session: &str) {
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
            Behaviour::CheckThenSubmit(names, payload) => {
                for name in names {
                    self.call(session, "flow_run_check", json!({ "name": name }));
                }
                self.submit(session, &payload);
                self.finish_turn(session);
            }
            Behaviour::AskThenSubmit(question, payload) => {
                self.call(session, "flow_ask", question);
                self.submit(session, &payload);
                self.finish_turn(session);
            }
            Behaviour::RequestThenSubmit(request, payload) => {
                self.call(session, "flow_request", request);
                self.submit(session, &payload);
                self.finish_turn(session);
            }
            Behaviour::Acts(acts) => {
                let cwd = self
                    .sessions
                    .lock()
                    .get(session)
                    .and_then(|s| s.cwd.clone())
                    .expect("a writer works in a directory");
                for act in acts {
                    match act {
                        Act::Write(path, contents) => {
                            let at = cwd.join(path);
                            std::fs::create_dir_all(at.parent().unwrap()).unwrap();
                            std::fs::write(at, contents).unwrap();
                        }
                        Act::Restore(path) => {
                            std::fs::write(cwd.join(path), at_head(&cwd, path)).unwrap();
                        }
                        Act::Checkpoint(message) => {
                            self.call(session, "flow_checkpoint", json!({ "message": message }));
                        }
                        Act::Handoff(summary) => {
                            self.call(
                                session,
                                "flow_submit",
                                json!({ "kind": "handoff", "payload": {
                                    "summary": summary,
                                    "changed": [],
                                    "checks": [],
                                    "openQuestions": [],
                                }}),
                            );
                        }
                        Act::Findings(payload) => self.submit(session, &payload),
                        Act::WaitFor(flag) => {
                            for _ in 0..4_000 {
                                if flag.load(Ordering::SeqCst) {
                                    break;
                                }
                                std::thread::sleep(std::time::Duration::from_millis(5));
                            }
                        }
                    }
                }
                self.finish_turn(session);
            }
            Behaviour::Tools(calls, end) => {
                for (tool, args) in calls {
                    self.call(session, tool, args);
                }
                if end {
                    self.finish_turn(session);
                }
            }
            Behaviour::Reply(words) => {
                self.emit(
                    session,
                    &AgentEvent::TurnStarted {
                        turn: "t".to_owned(),
                    },
                );
                self.emit(
                    session,
                    &AgentEvent::MessageDelta {
                        text: words.to_owned(),
                    },
                );
                self.finish_turn(session);
            }
            Behaviour::Hold(flag, payload) => {
                self.call(session, "flow_progress", json!({ "text": "working" }));
                for _ in 0..8_000 {
                    if flag.load(Ordering::SeqCst) {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                if payload.get("kind").is_some() {
                    self.call(session, "flow_submit", payload);
                } else {
                    self.submit(session, &payload);
                }
                self.finish_turn(session);
            }
            Behaviour::Design { brief, html, reuse } => {
                self.call(session, "flow_context", json!({}));
                let (ok, prepared) =
                    self.call(session, "prototype_prepare", json!({ "brief": brief }));
                if ok {
                    let prepared: Value = serde_json::from_str(&prepared).unwrap();
                    self.call(
                        session,
                        "prototype_publish",
                        json!({
                            "id": prepared["id"],
                            "revision": prepared["revision"],
                            "html": html,
                            "reuse": reuse,
                        }),
                    );
                }
                self.finish_turn(session);
            }
            Behaviour::ProgressThenSubmit(payload) => {
                for line in [
                    "reading export.rs",
                    "checking the empty case",
                    "writing it up",
                ] {
                    self.call(session, "flow_progress", json!({ "text": line }));
                }
                self.submit(session, &payload);
                self.finish_turn(session);
            }
        }
    }
}

impl RoleHost for FakeHost {
    fn preflight(&self, providers: &[&str]) -> Result<(), Refusal> {
        match self.refuse_preflight.lock().clone() {
            Some(message) => Err(Refusal {
                message,
                billing: providers.iter().map(|p| (*p).to_owned()).collect(),
                credits: Vec::new(),
            }),
            None => Ok(()),
        }
    }

    fn open(&self, spec: &RoleSession, sink: Arc<dyn AgentSink>) -> Result<SessionId, String> {
        let attempt = self.attempts_of(&spec.role.role) + 1;
        let id = format!("fake-{}", self.next.fetch_add(1, Ordering::SeqCst));
        let token = self.app.handoff.issue(Caller {
            scope: SessionScope::run(&spec.project, &spec.worktree, spec.role.clone()),
            provider: spec.provider.clone(),
            effort: None,
            mode: None,
            session: None,
        });
        let turns = Arc::new(Mutex::new(Vec::new()));
        self.opened.lock().push(Opened {
            role: spec.role.role.clone(),
            provider: spec.provider.clone(),
            model: spec.model.clone(),
            cwd: spec.cwd.clone(),
            cwd_existed: spec.cwd.as_ref().is_some_and(|cwd| cwd.is_dir()),
            brief: spec.brief.clone(),
            resume: spec.resume.clone(),
            token: token.clone(),
            turns: Arc::clone(&turns),
            browser: spec.role.browser.clone(),
            design: spec.role.design,
        });
        let behaviour = (self.script.lock())(&spec.role.role, attempt);
        self.app.flow_ledger.brief(&spec.role, &spec.brief);
        self.sessions.lock().insert(
            id.clone(),
            FakeSession {
                role: spec.role.clone(),
                cwd: spec.cwd.clone(),
                token,
                sink: Arc::clone(&sink),
                behaviour,
                turns: 0,
            },
        );
        let now = self.open_now.fetch_add(1, Ordering::SeqCst) + 1;
        self.open_most.fetch_max(now, Ordering::SeqCst);

        // The conversation it continues, or a new one, as a CLI reports on its handshake.
        let conversation = spec.resume.clone().unwrap_or_else(|| {
            format!(
                "conv-{}",
                self.conversations.fetch_add(1, Ordering::SeqCst) + 1
            )
        });
        sink.on_event(
            &SessionId::new(&id),
            &AgentEvent::SessionReady {
                provider_session_id: conversation,
                model: self.ready_models.lock().get(&spec.provider).cloned(),
                effort: None,
                mode: None,
                tools: Vec::new(),
            },
        );

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

    fn send(&self, session: &SessionId, text: &str) -> Result<(), String> {
        self.turns_sent.fetch_add(1, Ordering::SeqCst);
        if let Some(opened) = self.opened.lock().iter().find(|o| {
            self.sessions
                .lock()
                .get(session.as_str())
                .is_some_and(|s| s.token == o.token)
        }) {
            opened.turns.lock().push(text.to_owned());
        }
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

    /// As a CLI does when it is interrupted: the turn ends, and the conversation is kept.
    fn interrupt(&self, session: &SessionId) -> Result<(), String> {
        self.interrupts.fetch_add(1, Ordering::SeqCst);
        self.finish_turn(session.as_str());
        Ok(())
    }

    fn limits(&self, _provider: RoleProvider, _model: Option<&str>) -> LimitReading {
        self.limits_asked.fetch_add(1, Ordering::SeqCst);
        self.readings
            .lock()
            .pop_front()
            .unwrap_or(LimitReading::Unknown)
    }

    fn open_runtime_browser(
        &self,
        _project: &str,
        _worktree: &str,
        url: &str,
    ) -> Result<RuntimePane, String> {
        let id = format!("browser-fake-{}", self.next.fetch_add(1, Ordering::SeqCst));
        self.panes.lock().push(FakePane {
            id: id.clone(),
            url: url.to_owned(),
            closed: false,
            sessions_open: self.open_now.load(Ordering::SeqCst),
        });
        Ok(RuntimePane {
            id,
            url: url.to_owned(),
            store: *self.pane_store.lock(),
        })
    }

    fn close_runtime_browser(&self, pane: &str) {
        if let Some(found) = self.panes.lock().iter_mut().find(|p| p.id == pane) {
            found.closed = true;
        }
    }

    fn screenshot(&self, pane: &str) -> Result<(Vec<u8>, String), String> {
        let url = self
            .panes
            .lock()
            .iter()
            .find(|p| p.id == pane && !p.closed)
            .map(|p| p.url.clone())
            .ok_or("that pane is closed")?;
        // Not a picture anyone looks at here: the engine keeps the bytes it is given.
        Ok((b"\x89PNG fake".to_vec(), url))
    }
}

/// `path` as `HEAD` has it in `cwd`: what a writer told to undo a change puts back.
fn at_head(cwd: &std::path::Path, path: &str) -> Vec<u8> {
    // A test is a sanctioned place to spawn directly: this is the fake agent's own shell.
    #[allow(clippy::disallowed_methods)]
    let out = std::process::Command::new("git")
        .args(["show", &format!("HEAD:{path}")])
        .current_dir(cwd)
        .output()
        .unwrap();
    assert!(out.status.success(), "{path} is not at HEAD");
    out.stdout
}

/// Remembers what the engine announced.
#[derive(Default)]
pub(crate) struct Heard {
    pub(crate) filed: Mutex<Vec<InboxItem>>,
}

impl Notice for Heard {
    fn changed(&self, _flow: FlowId) {}
    fn filed(&self, item: &InboxItem) {
        self.filed.lock().push(item.clone());
    }
}
