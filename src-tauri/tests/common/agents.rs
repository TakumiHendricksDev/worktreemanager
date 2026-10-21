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
use wtm_app_lib::flow::host::RoleHost;
use wtm_app_lib::flow::{Engine, Notice, Refusal};
use wtm_app_lib::flow_session::RoleSession;
use wtm_app_lib::handoff::{Caller, CodeCall};
use wtm_core::flow::{FlowId, InboxItem};
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
    pub(crate) cwd: Option<PathBuf>,
    pub(crate) cwd_existed: bool,
    pub(crate) brief: String,
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
    pub(crate) open_now: AtomicUsize,
    pub(crate) open_most: AtomicUsize,
    pub(crate) next: AtomicU64,
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
            open_now: AtomicUsize::new(0),
            open_most: AtomicUsize::new(0),
            next: AtomicU64::new(1),
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
            }),
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
                cwd: spec.cwd.clone(),
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
