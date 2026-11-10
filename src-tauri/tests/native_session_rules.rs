//! A native session's own rules, run against a real `App` with a fake pipe and no webview.
//!
//! `native_deliveries.rs` proves the journal; this proves what a session does with it: which
//! provider frames are WTM's to consume, when a new instruction may start, which hooks belong to
//! the pane, and how an uncertain delivery is settled. None of that was under `just check` before,
//! because a session needed a `tauri::AppHandle` and a test binary cannot build one. Sessions now
//! report through the `Events` seam, and a recorder stands in for the webview here.
//!
//! The worktree is a throwaway `git init` (`GitFixture`), because a hook's workspace check and an
//! Open Design record both resolve a real worktree. No provider CLI is launched: the pipe is
//! `FakePipe`, and replies are fed in exactly as the pipe reader would.

// Polling a fake pipe for a frame written by another thread; there is no completion to await.
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde_json::{Value, json};
use wtm_agent::native::Status;
use wtm_app_lib::app::{App, SessionScope};
use wtm_app_lib::handoff::Caller;
use wtm_app_lib::native_sessions::delivery::{DeliveryStatus, Resolution};
use wtm_app_lib::native_sessions::hooks::{acknowledge, hook_input};
use wtm_app_lib::native_sessions::{Events, Parts, Session, Snapshot, serve_hook};
use wtm_config::AppPaths;
use wtm_core::model::SessionId;
use wtm_core::ports::pipe::PipeHost;
use wtm_testkit::{FakePipe, GitFixture};

const HTML: &str = "<!doctype html><html><body><button class='btn btn-primary project-action'>Export</button></body></html>";
const PANE: &str = "pane-1";

#[derive(Default)]
struct Recorder {
    changes: Mutex<Vec<Snapshot>>,
    plugins: Mutex<Vec<(String, Value)>>,
}

impl Events for Recorder {
    fn changed(&self, snapshot: &Snapshot) {
        self.changes.lock().push(snapshot.clone());
    }
    fn plugin(&self, name: &'static str, payload: Value, _worktree: String) {
        self.plugins.lock().push((name.to_owned(), payload));
    }
}

struct Harness {
    app: Arc<App>,
    project: String,
    worktree: String,
    root: PathBuf,
    _fixture: GitFixture,
    _config: tempfile::TempDir,
}

struct Native {
    session: Arc<Session>,
    token: String,
    pipe: Arc<FakePipe>,
    events: Arc<Recorder>,
}

impl Harness {
    fn new() -> Self {
        let fixture = GitFixture::new();
        let config = tempfile::tempdir().unwrap();
        let app = Arc::new(App::with_paths(AppPaths::rooted(config.path())).unwrap());
        let root = app.register(fixture.root()).unwrap();
        let project = app.project(&root.to_string_lossy()).unwrap();
        let worktree = app
            .worktrees(&project)
            .unwrap()
            .into_iter()
            .find(|w| w.is_main)
            .unwrap();
        Self {
            root: Path::new(&worktree.path).canonicalize().unwrap(),
            project: project.root.to_string_lossy().into_owned(),
            worktree: worktree.id,
            app,
            _fixture: fixture,
            _config: config,
        }
    }

    /// A registered native session, as `open` leaves one once its terminal is running.
    fn session(&self, provider: &str, pane: &str, reply_timeout: Duration) -> Native {
        let token = self.app.handoff.issue(Caller {
            scope: SessionScope::worktree(&self.project, &self.worktree),
            provider: provider.into(),
            effort: None,
            mode: None,
            session: None,
        });
        let pipe = Arc::new(FakePipe::new());
        let events = Arc::new(Recorder::default());
        let session = Session::new(
            &self.app,
            Parts {
                token: token.clone(),
                project: self.project.clone(),
                worktree: self.worktree.clone(),
                provider: provider.into(),
                conversation: String::new(),
                root: self.root.clone(),
                version: "test".into(),
                tools: vec![],
                model: None,
                instructions: "WTM context".into(),
                events: Arc::clone(&events) as Arc<dyn Events>,
                pipe: Arc::clone(&pipe) as Arc<dyn PipeHost>,
                reply_timeout,
                directory: None,
            },
        );
        self.app.native_sessions.register(Arc::clone(&session));
        session.bind(pane);
        self.app.handoff.bind_session(&token, pane);
        session.attach_backend(SessionId::new("fake-pipe-1"));
        Native {
            session,
            token,
            pipe,
            events,
        }
    }

    fn hook(&self, native: &Native, event: Value) -> Value {
        let mut frame = json!({"nativeHook": true, "token": native.token});
        frame["event"] = event;
        serve_hook(&self.app, &frame)
    }

    fn claude_event(&self, name: &str, conversation: &str) -> Value {
        json!({
            "hook_event_name": name,
            "session_id": conversation,
            "cwd": self.root,
            "permission_mode": "default",
        })
    }

    /// A Claude session in `pane` whose `SessionStart` has registered `conversation`.
    fn claude(&self, pane: &str, conversation: &str) -> Native {
        let native = self.session("claude", pane, Duration::from_secs(2));
        let started = self.hook(&native, self.claude_event("SessionStart", conversation));
        assert!(started.get("error").is_none(), "{started}");
        native
    }

    /// A Codex session in `pane` whose TUI has opened `thread` and been told it is idle.
    fn codex(&self, pane: &str, thread: &str, reply_timeout: Duration) -> Native {
        let native = self.session("codex", pane, reply_timeout);
        native
            .session
            .client_message(json!({"id": 1, "method": "thread/start", "params": {}}))
            .unwrap();
        let opened =
            json!({"id": 1, "result": {"thread": {"id": thread, "status": {"type": "idle"}}}});
        assert!(!native.session.server_message(&opened));
        assert_eq!(native.session.snapshot().status, Status::Idle);
        native
    }

    /// A published, opened Open Design review owned by the pane.
    fn design(&self) -> (String, String) {
        let mut design = wtm_app_lib::prototype_commands::prepare(
            &self.app,
            PANE,
            "An export button",
            None,
            None,
        )
        .unwrap();
        let revision = design.revision.clone();
        design = wtm_app_lib::prototype_commands::publish(
            &self.app,
            PANE,
            &design.id,
            &revision,
            HTML,
            "Uses the project's button",
        )
        .unwrap();
        design.review.as_mut().unwrap().opened = true;
        wtm_app_lib::prototypes::save(&design).unwrap();
        (design.id, revision)
    }
}

impl Native {
    /// The last frame written to the app-server with this method, waiting for it if a send on
    /// another thread has not written it yet.
    fn written(&self, method: &str) -> Value {
        for _ in 0..500 {
            let found = self
                .pipe
                .written()
                .iter()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .rfind(|frame| frame["method"] == method);
            if let Some(frame) = found {
                return frame;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("{method} was never written");
    }

    fn send_in_background(
        &self,
        text: &'static str,
        review: Option<(&'static str, &'static str)>,
    ) -> std::thread::JoinHandle<Result<wtm_app_lib::native_sessions::Receipt, String>> {
        let session = Arc::clone(&self.session);
        std::thread::spawn(move || session.send("Implement", text, review))
    }
}

#[test]
fn a_wtm_reply_is_consumed_and_settles_its_receipt_while_provider_frames_reach_the_terminal() {
    let harness = Harness::new();
    let native = harness.codex(PANE, "thread-1", Duration::from_secs(5));
    let sending = native.send_in_background("exact", Some(("design", "review")));
    let turn = native.written("turn/start");
    assert_eq!(turn["params"]["threadId"], "thread-1");
    assert_eq!(turn["params"]["input"][0]["text"], "exact");
    let id = turn["id"].as_str().unwrap().to_owned();
    // A provider request that happens to carry WTM's id is still the provider's.
    let request = json!({"id": id, "method": "item/commandExecution/requestApproval", "params": {"threadId": "thread-1"}});
    assert!(!native.session.server_message(&request));
    assert!(
        native
            .session
            .server_message(&json!({"id": id, "result": {"turn": {"id": "t"}}}))
    );
    let receipt = sending.join().unwrap().unwrap();
    assert_eq!(receipt.status, DeliveryStatus::Delivered);
    assert_eq!(turn["params"]["clientUserMessageId"], receipt.id.as_str());
    let notification =
        json!({"method": "turn/started", "params": {"threadId": "thread-1", "turn": {"id": "t"}}});
    assert!(!native.session.server_message(&notification));
}

#[test]
fn an_unanswered_instruction_stays_unknown_and_a_late_reply_still_settles_it_either_way() {
    let harness = Harness::new();
    let native = harness.codex(PANE, "thread-1", Duration::from_millis(100));
    let receipt = native.session.send("Implement", "first", None).unwrap();
    assert_eq!(receipt.status, DeliveryStatus::Unknown);
    assert!(native.session.snapshot().error.is_some());
    let first = native.written("turn/start");
    assert!(
        native
            .session
            .server_message(&json!({"id": first["id"], "result": {}}))
    );
    let settled = harness
        .app
        .native_sessions
        .deliveries
        .read(&receipt.id)
        .unwrap();
    assert_eq!(settled.status, DeliveryStatus::Delivered);
    assert!(native.session.snapshot().error.is_none());

    let finished = json!({"method": "turn/completed", "params": {"threadId": "thread-1", "turn": {"id": "t", "status": "completed"}}});
    native.session.server_message(&finished);
    let second = native.session.send("Implement", "second", None).unwrap();
    assert_eq!(second.status, DeliveryStatus::Unknown);
    let frame = native.written("turn/start");
    assert_ne!(frame["id"], first["id"]);
    let refused = json!({"id": frame["id"], "error": {"message": "busy"}});
    assert!(native.session.server_message(&refused));
    let rejected = harness
        .app
        .native_sessions
        .deliveries
        .read(&second.id)
        .unwrap();
    assert_eq!(rejected.status, DeliveryStatus::Rejected);
    assert_eq!(rejected.error.as_deref(), Some("busy"));
    assert_eq!(native.session.snapshot().status, Status::Idle);
    let starts = native
        .pipe
        .written()
        .iter()
        .filter(|l| l.contains("\"turn/start\""))
        .count();
    assert_eq!(starts, 2, "nothing was replayed");
}

#[test]
fn a_busy_conversation_refuses_new_instructions_but_answers_an_approved_review_with_its_receipt() {
    let harness = Harness::new();
    let native = harness.codex(PANE, "thread-1", Duration::from_millis(100));
    let sent = native
        .session
        .send("Implement", "approved", Some(("design", "review")))
        .unwrap();
    assert_eq!(sent.status, DeliveryStatus::Unknown);
    assert_eq!(native.session.snapshot().status, Status::Working);
    let again = native
        .session
        .send("Implement", "changed text", Some(("design", "review")))
        .unwrap();
    assert_eq!(again.id, sent.id);
    let refused = native
        .session
        .send("Implement", "new", Some(("design", "next")));
    assert!(refused.unwrap_err().contains("idle"));
    let starts = native
        .pipe
        .written()
        .iter()
        .filter(|l| l.contains("\"turn/start\""))
        .count();
    assert_eq!(starts, 1);
}

#[test]
fn hooks_are_refused_for_another_provider_workspace_or_conversation_but_a_subdirectory_is_home() {
    let harness = Harness::new();
    let early = harness.session("claude", "pane-early", Duration::from_secs(2));
    let before = harness.hook(&early, harness.claude_event("PreToolUse", "conv-1"));
    assert!(before["error"].as_str().unwrap().contains("SessionStart"));
    drop(early);

    let native = harness.claude(PANE, "conv-1");
    assert_eq!(native.session.snapshot().conversation, "conv-1");
    assert_eq!(native.session.snapshot().status, Status::Idle);

    let nested = harness.root.join("src").join("deeper");
    std::fs::create_dir_all(&nested).unwrap();
    let mut moved = harness.claude_event("PreToolUse", "conv-1");
    moved["cwd"] = json!(nested);
    assert!(harness.hook(&native, moved).get("error").is_none());

    let outside = tempfile::tempdir().unwrap();
    let mut elsewhere = harness.claude_event("PreToolUse", "conv-1");
    elsewhere["cwd"] = json!(outside.path());
    assert!(
        harness.hook(&native, elsewhere)["error"]
            .as_str()
            .unwrap()
            .contains("workspace")
    );
    let sibling = harness.claude_event("PreToolUse", "another-conversation");
    assert!(
        harness.hook(&native, sibling)["error"]
            .as_str()
            .unwrap()
            .contains("conversation")
    );

    let codex = harness.session("codex", "pane-codex", Duration::from_secs(2));
    let foreign = harness.hook(&codex, harness.claude_event("SessionStart", "conv-9"));
    assert!(foreign["error"].as_str().unwrap().contains("Claude"));
    let unknown = serve_hook(
        &harness.app,
        &json!({"nativeHook": true, "token": "forged", "event": harness.claude_event("Stop", "conv-1")}),
    );
    assert!(unknown.get("error").is_some());
}

#[test]
fn session_start_switches_the_conversation_and_keeps_earlier_uncertain_receipts_visible() {
    let harness = Harness::new();
    let native = harness.claude(PANE, "conv-1");
    let pending = native.session.send("Implement", "pending", None).unwrap();
    assert_eq!(pending.status, DeliveryStatus::Pending);
    let cleared = harness.hook(&native, harness.claude_event("SessionStart", "conv-2"));
    assert!(cleared.get("error").is_none());
    let snapshot = native.session.snapshot();
    assert_eq!(snapshot.conversation, "conv-2");
    let [listed] = snapshot.deliveries.as_slice() else {
        panic!(
            "the earlier receipt should still be listed: {:?}",
            snapshot.deliveries
        );
    };
    assert_eq!(listed.id, pending.id);
    assert!(listed.earlier);
    assert_eq!(listed.conversation, "conv-1");
    // Its conversation is no longer this one, so the new prompt does not collect it.
    let prompt = harness.hook(&native, harness.claude_event("UserPromptSubmit", "conv-2"));
    assert_eq!(prompt["hookSpecificOutput"]["additionalContext"], "");
    let withdrawn = native
        .session
        .resolve(&pending.id, Resolution::Withdrawn)
        .unwrap();
    assert_eq!(withdrawn.status, DeliveryStatus::Rejected);
    assert!(native.session.snapshot().deliveries.is_empty());
    assert!(
        native
            .events
            .plugins
            .lock()
            .iter()
            .any(|(name, payload)| name == "chat.event" && payload["renderer"] == "native"),
        "provider events reach plugins through the events seam"
    );
}

#[test]
fn a_stale_or_unreadable_review_is_rejected_at_collection_and_the_next_prompt_still_works() {
    let harness = Harness::new();
    let native = harness.claude(PANE, "conv-1");
    let missing = native
        .session
        .send(
            "Implement",
            "gone",
            Some(("00000000-0000-4000-8000-000000000000", "review")),
        )
        .unwrap();
    let prompt = harness.hook(&native, harness.claude_event("UserPromptSubmit", "conv-1"));
    assert!(prompt.get("error").is_none(), "{prompt}");
    let unreadable = harness
        .app
        .native_sessions
        .deliveries
        .read(&missing.id)
        .unwrap();
    assert_eq!(unreadable.status, DeliveryStatus::Rejected);
    assert!(unreadable.error.unwrap().contains("could not be read"));
    harness.hook(&native, harness.claude_event("Stop", "conv-1"));

    let (design, review) = harness.design();
    let old = native
        .session
        .send(
            "Implement",
            "old",
            Some((design.as_str(), "an-older-review")),
        )
        .unwrap();
    let prompt = harness.hook(&native, harness.claude_event("UserPromptSubmit", "conv-1"));
    assert_eq!(prompt["hookSpecificOutput"]["additionalContext"], "");
    let stale = harness
        .app
        .native_sessions
        .deliveries
        .read(&old.id)
        .unwrap();
    assert_eq!(stale.status, DeliveryStatus::Rejected);
    assert!(stale.error.unwrap().contains("changed"));
    harness.hook(&native, harness.claude_event("Stop", "conv-1"));

    let current = native
        .session
        .send(
            "Implement",
            "the current design",
            Some((design.as_str(), review.as_str())),
        )
        .unwrap();
    let prompt = harness.hook(&native, harness.claude_event("UserPromptSubmit", "conv-1"));
    let context = prompt["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(context.ends_with("the current design"));
    assert_eq!(
        harness
            .app
            .native_sessions
            .deliveries
            .read(&current.id)
            .unwrap()
            .status,
        DeliveryStatus::Unknown
    );
}

#[test]
fn the_acknowledge_line_comes_first_and_only_the_collecting_conversation_can_acknowledge_it() {
    let harness = Harness::new();
    let native = harness.claude(PANE, "conv-1");
    let payload = "x".repeat(200 * 1024);
    let session = Arc::clone(&native.session);
    let receipt = session.send("Implement", &payload, None).unwrap();
    assert!(
        acknowledge(&native.session, &receipt.id).is_err(),
        "nothing collected it yet"
    );
    let prompt = harness.hook(&native, harness.claude_event("UserPromptSubmit", "conv-1"));
    let context = prompt["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    let first = context.lines().next().unwrap();
    assert!(first.contains("mcp__wtm__native_acknowledge"));
    assert!(first.contains(&receipt.id));

    let other = harness.claude("pane-other", "conv-other");
    assert!(acknowledge(&other.session, &receipt.id).is_err());
    acknowledge(&native.session, &receipt.id).unwrap();
    assert_eq!(
        harness
            .app
            .native_sessions
            .deliveries
            .read(&receipt.id)
            .unwrap()
            .status,
        DeliveryStatus::Delivered
    );
    assert!(native.session.snapshot().tools_connected);
    acknowledge(&native.session, &receipt.id).unwrap();
}

#[test]
fn claude_says_its_history_cannot_be_checked_while_codex_reads_its_own() {
    let harness = Harness::new();
    let claude = harness.claude(PANE, "conv-1");
    let nothing = claude.session.check_delivery().unwrap();
    assert!(!nothing.checked);
    assert!(nothing.reason.unwrap().contains("No delivery"));
    let receipt = claude.session.send("Implement", "collected", None).unwrap();
    harness.hook(&claude, harness.claude_event("UserPromptSubmit", "conv-1"));
    let check = claude.session.check_delivery().unwrap();
    assert!(!check.checked && !check.changed);
    assert!(check.reason.unwrap().contains("Claude"));
    let resolved = claude
        .session
        .resolve(&receipt.id, Resolution::NotDelivered)
        .unwrap();
    assert_eq!(resolved.resolution, Some(Resolution::NotDelivered));
    harness.hook(&claude, harness.claude_event("Stop", "conv-1"));
    assert!(claude.session.send("Implement", "next", None).is_ok());

    let codex = harness.codex("pane-codex", "thread-1", Duration::from_secs(1));
    let lost = codex.session.send("Implement", "lost", None).unwrap();
    assert_eq!(lost.status, DeliveryStatus::Unknown);
    let session = Arc::clone(&codex.session);
    let checking = std::thread::spawn(move || session.check_delivery());
    let read = codex.written("thread/read");
    assert_eq!(read["params"]["threadId"], "thread-1");
    let history = json!({"id": read["id"], "result": {"thread": {"id": "thread-1", "turns": [{"items": [
        {"type": "userMessage", "id": "m", "clientId": lost.id, "content": [{"type": "text", "text": "lost"}]}
    ]}]}}});
    assert!(codex.session.server_message(&history));
    let check = checking.join().unwrap().unwrap();
    assert!(check.checked && check.changed);
    assert_eq!(
        harness
            .app
            .native_sessions
            .deliveries
            .read(&lost.id)
            .unwrap()
            .status,
        DeliveryStatus::Delivered
    );
}

#[test]
fn a_rejected_native_delivery_offers_the_design_for_implementation_again() {
    let harness = Harness::new();
    let native = harness.claude(PANE, "conv-1");
    let (design, review) = harness.design();
    let receipt = native
        .session
        .send(
            "Implement",
            "design",
            Some((design.as_str(), review.as_str())),
        )
        .unwrap();
    let mut record = wtm_app_lib::prototype_commands::record(&harness.app, PANE, &design).unwrap();
    record.implemented_review = Some(review.clone());
    wtm_app_lib::prototypes::save(&record).unwrap();
    let state = wtm_app_lib::prototype_commands::state(&harness.app, record.clone());
    assert_eq!(
        state.prototype.implemented_review.as_deref(),
        Some(review.as_str())
    );
    assert_eq!(state.delivery.unwrap().status, DeliveryStatus::Pending);

    native
        .session
        .resolve(&receipt.id, Resolution::Withdrawn)
        .unwrap();
    let state = wtm_app_lib::prototype_commands::state(&harness.app, record);
    assert_eq!(state.prototype.implemented_review, None);
    let delivery = state.delivery.unwrap();
    assert_eq!(delivery.status, DeliveryStatus::Rejected);
    assert_eq!(delivery.resolution, Some(Resolution::Withdrawn));
}

#[test]
fn an_ended_session_leaves_every_listing_and_takes_its_token_with_it() {
    let harness = Harness::new();
    let native = harness.claude(PANE, "conv-1");
    assert_eq!(harness.app.native_sessions.list().len(), 1);
    harness.app.native_sessions.close(PANE);
    assert!(harness.app.native_sessions.list().is_empty());
    assert!(harness.app.native_sessions.get(PANE).is_none());
    assert!(
        harness
            .app
            .native_sessions
            .by_token(&native.token)
            .is_none()
    );
    assert!(harness.app.handoff.resolve(&native.token).is_none());
    assert!(
        harness
            .app
            .conversation_sessions()
            .iter()
            .all(|s| s.session != PANE)
    );
    let last = native.events.changes.lock().last().cloned().unwrap();
    assert_eq!(last.status, Status::Ended, "the pane is told it ended");
    assert!(native.pipe.was_killed());
}

#[test]
fn a_lost_connection_stays_disconnected_whatever_the_provider_reports_next() {
    let harness = Harness::new();
    let native = harness.codex(PANE, "thread-1", Duration::from_millis(100));
    native
        .session
        .disconnect("The native terminal stopped reading");
    let finished = json!({"method": "turn/completed", "params": {"threadId": "thread-1", "turn": {"id": "t", "status": "completed"}}});
    native.session.server_message(&finished);
    let snapshot = native.session.snapshot();
    assert_eq!(snapshot.status, Status::Disconnected);
    assert!(native.session.send("Implement", "text", None).is_err());
    let check = native.session.check_delivery().unwrap();
    assert!(!check.checked);
}

#[test]
fn hook_input_keeps_only_what_wtm_reads_however_large_the_tool_response() {
    let huge = "z".repeat(3 * 1024 * 1024);
    let input = json!({
        "hook_event_name": "PostToolUse",
        "session_id": "conv-1",
        "cwd": "/tmp",
        "tool_name": "Read",
        "tool_use_id": "tool-1",
        "tool_input": {"file_path": "/tmp/big"},
        "tool_response": {"content": huge},
    })
    .to_string();
    let reduced = hook_input(input.as_bytes()).unwrap();
    assert_eq!(reduced["tool_use_id"], "tool-1");
    assert!(reduced.get("tool_response").is_none());
    assert!(reduced.get("tool_input").is_none());
    assert!(reduced.to_string().len() < 1024);

    let prompt = "é".repeat(100 * 1024);
    let submitted = json!({"hook_event_name": "UserPromptSubmit", "prompt": prompt}).to_string();
    let reduced = hook_input(submitted.as_bytes()).unwrap();
    let kept = reduced["prompt"].as_str().unwrap();
    assert!(kept.len() <= 64 * 1024 && kept.starts_with('é'));
}
