//! What a session restored from the last run gets back, against real `codex app-server` processes.
//!
//! A restored pane resumes through the ordinary open path, but three things it needs are facts about
//! the live registry that no unit test can see:
//!
//! **Its conversation is known from the moment it opens.** Claude does not name its conversation
//! until its first turn, and a registry entry that waits for that offers the restored conversation
//! under "Pick up where you left off" while it is on screen, and cannot match it to its pane after a
//! reload.
//!
//! **Home's claim comes back only from a Home that is running.** The window passes the claim with
//! the resume, and a claim honoured from anything else would hand Home a way to close the user's
//! work.
//!
//! **Home is told about work the quit interrupted with a handle only for what came back.**
//!
//! # No API credit is spent
//!
//! Nothing here sends a turn, so every session is an app server that opened a thread or refused one.
//! The notice for Home is built and never delivered: delivering it would start a real turn. Skips
//! without `codex`, for the reasons `agent_sessions.rs` gives.

// The same allowances `agent_sessions.rs` grants itself: a skip has to say why it skipped.
#![allow(clippy::unwrap_used, clippy::print_stderr)]

use std::sync::Arc;

use wtm_agent::session::AgentSink;
use wtm_app_lib::app::{App, SessionScope};
use wtm_app_lib::home::{self, InterruptedTarget, Update};
use wtm_config::AppPaths;
use wtm_core::model::{AgentEvent, ExitOutcome, SessionId};

struct Quiet;

impl AgentSink for Quiet {
    fn on_event(&self, _session: &SessionId, _event: &AgentEvent) {}
    fn on_exit(&self, _session: &SessionId, _outcome: &ExitOutcome) {}
    fn on_ready(&self, _session: &SessionId) {}
}

/// A thread id no rollout has: `thread/resume` refuses it, and the app server stays up.
const GONE: &str = "019a0000-0000-7000-8000-000000000000";

struct Fixture {
    dir: tempfile::TempDir,
    app: App,
    entry: &'static wtm_agent::ProviderEntry,
    sink: Arc<dyn AgentSink>,
}

fn fixture() -> Option<Fixture> {
    let dir = tempfile::tempdir().unwrap();
    let app = App::with_paths(AppPaths::rooted(dir.path())).unwrap();
    let entry = wtm_agent::entry("codex").unwrap();
    if app.agent_executable(entry).is_none() {
        eprintln!("skipping: no `codex` on the resolved PATH");
        return None;
    }
    Some(Fixture {
        dir,
        app,
        entry,
        sink: Arc::new(Quiet),
    })
}

impl Fixture {
    /// What a relaunch is to the state that outlives it: a new app over the same files. Every
    /// session of the old one is closed first, as a quit ends them.
    fn relaunch(self, sessions: &[&str]) -> Self {
        for session in sessions {
            self.app.close_agent(session);
        }
        let Self {
            dir, entry, sink, ..
        } = self;
        let app = App::with_paths(AppPaths::rooted(dir.path())).unwrap();
        Self {
            dir,
            app,
            entry,
            sink,
        }
    }

    fn open(&self, scope: SessionScope, resume: Option<&str>) -> String {
        let req = wtm_agent::SessionRequest {
            cwd: std::env::temp_dir().to_string_lossy().into_owned(),
            resume: resume.map(str::to_owned),
            ..wtm_agent::SessionRequest::default()
        };
        self.app
            .open_agent(self.entry, &req, scope, &self.sink)
            .expect("the app server should spawn")
            .as_str()
            .to_owned()
    }
}

fn worktree() -> SessionScope {
    SessionScope::worktree("/repo", "/repo-wt/feature")
}

#[test]
fn a_resumed_session_is_not_offered_for_resuming_from_the_moment_it_opens() {
    let Some(f) = fixture() else { return };
    f.app.remember_session(wtm_config::SessionRecord {
        provider: "codex".to_owned(),
        worktree: "/repo-wt/feature".to_owned(),
        provider_session: GONE.to_owned(),
        title: Some("Fix the flaky test".to_owned()),
        model: None,
        effort: None,
        updated: None,
        extra: std::collections::BTreeMap::new(),
    });
    assert_eq!(f.app.resumable("/repo-wt/feature").len(), 1);

    let session = f.open(worktree(), Some(GONE));

    // Asked at once, before the provider has said anything about the thread.
    assert_eq!(f.app.provider_session_of(&session).as_deref(), Some(GONE));
    assert!(
        f.app.resumable("/repo-wt/feature").is_empty(),
        "a conversation on screen must not be offered as one to pick up"
    );
    f.app.close_agent(&session);
}

#[test]
fn a_restored_pane_gets_homes_claim_back_only_from_a_home_that_is_running() {
    let Some(f) = fixture() else { return };
    let home = f.open(SessionScope::Home, None);
    let opened = f.open(worktree(), None);
    let other = f.open(worktree(), None);

    f.app.restore_marks(&opened, Some(&home), true);
    assert_eq!(
        f.app.home.opener_of(&opened).as_deref(),
        Some(home.as_str())
    );
    assert_eq!(
        f.app.overview_of(&opened).unwrap().user_turns,
        1,
        "the user had written to it, so Home may not close it"
    );

    // A worktree session is no Home, and a session that is not running is nobody.
    f.app.restore_marks(&other, Some(&opened), false);
    f.app.restore_marks(&other, Some("not-running"), false);
    assert_eq!(f.app.home.opener_of(&other), None);
    assert_eq!(f.app.overview_of(&other).unwrap().user_turns, 0);

    for session in [home, opened, other] {
        f.app.close_agent(&session);
    }
}

#[test]
fn home_hears_of_interrupted_work_with_a_handle_only_for_a_session_that_came_back() {
    let Some(f) = fixture() else { return };
    let home = f.open(SessionScope::Home, None);
    let back = f.open(worktree(), None);
    let target = |session: Option<&str>| InterruptedTarget {
        session: session.map(str::to_owned),
        provider: "codex".to_owned(),
        project: "/repo".to_owned(),
        worktree: "/repo-wt/feature".to_owned(),
    };
    let targets = [
        target(Some(&back)),
        target(Some("not-running")),
        target(None),
    ];

    let notices = home::interrupted_notices(&f.app, &home, &targets).unwrap();
    assert_eq!(notices.len(), 3);
    assert_eq!(notices[0].target.session, back);
    assert_eq!(notices[0].update, Update::Interrupted { live: true });
    for gone in &notices[1..] {
        assert_eq!(
            gone.target.session, "",
            "no handle for a session that is not there"
        );
        assert_eq!(gone.update, Update::Interrupted { live: false });
        assert!(
            gone.target.about.starts_with("Codex in "),
            "{}",
            gone.target.about
        );
    }

    // Only a Home is told anything.
    assert!(home::interrupted_notices(&f.app, &back, &targets).is_none());
    assert!(home::interrupted_notices(&f.app, "not-running", &targets).is_none());

    for session in [home, back] {
        f.app.close_agent(&session);
    }
}

/// Conversations, as ids their provider would know them by. Resuming one names it at once.
const HOME: &str = "019a0000-0000-7000-8000-00000000000a";
const OPENED: &str = "019a0000-0000-7000-8000-00000000000b";
const OTHER: &str = "019a0000-0000-7000-8000-00000000000c";
const UNRELATED: &str = "019a0000-0000-7000-8000-00000000000d";
const LATE: &str = "019a0000-0000-7000-8000-00000000000e";

#[test]
fn a_homes_handle_names_the_same_session_after_a_relaunch_or_names_nobody() {
    // What happened: Home opened a session and was told `s1`. After a relaunch the handle table
    // started again, `s1` went to the first session listed — an unrelated pane restored from the
    // last run — and Home's next `message_session s1` delivered its task there.
    let Some(f) = fixture() else { return };
    let home = f.open(SessionScope::Home, Some(HOME));
    let opened = f.open(worktree(), Some(OPENED));
    let other = f.open(worktree(), Some(OTHER));
    // One whose conversation its provider names only later, as Claude does on its first turn.
    let late = f.open(worktree(), None);
    assert_eq!(f.app.handle_for(&home, &opened), "s1");
    assert_eq!(f.app.handle_for(&home, &other), "s2");
    assert_eq!(f.app.handle_for(&home, &late), "s3");
    f.app.note_provider_session(&late, LATE);

    let f = f.relaunch(&[&home, &opened, &other, &late]);
    // Home resumes its conversation, and the unrelated pane is the first session it sees.
    let home = f.open(SessionScope::Home, Some(HOME));
    let unrelated = f.open(worktree(), Some(UNRELATED));
    let new = f.app.handle_for(&home, &unrelated);
    assert_eq!(
        new, "s4",
        "a number Home has used is never given to another session"
    );
    assert!(
        f.app.session_for(&home, "s1").is_err(),
        "the session s1 named has not come back, so s1 names nobody"
    );

    // The session it opened comes back, holding the same conversation, and answers to s1 again.
    let opened_again = f.open(worktree(), Some(OPENED));
    let late_again = f.open(worktree(), Some(LATE));
    assert_eq!(f.app.session_for(&home, "s1"), Ok(opened_again.clone()));
    assert_eq!(f.app.handle_for(&home, &opened_again), "s1");
    assert_eq!(f.app.session_for(&home, "s3"), Ok(late_again.clone()));

    // The one that did not come back is refused, and said to be gone rather than unknown.
    let gone = f.app.session_for(&home, "s2").unwrap_err();
    assert!(gone.contains("not running any more"), "{gone}");
    assert!(
        f.app
            .session_for(&home, "s9")
            .unwrap_err()
            .contains("there is no session")
    );
    assert_eq!(f.app.session_for(&home, &new), Ok(unrelated.clone()));

    for session in [home, unrelated, opened_again, late_again] {
        f.app.close_agent(&session);
    }
}

#[test]
fn a_different_home_conversation_after_a_relaunch_does_not_inherit_the_old_ones_handles() {
    let Some(f) = fixture() else { return };
    let home = f.open(SessionScope::Home, Some(HOME));
    let opened = f.open(worktree(), Some(OPENED));
    assert_eq!(f.app.handle_for(&home, &opened), "s1");

    let f = f.relaunch(&[&home, &opened]);
    // A new Home conversation, not the resumed one.
    let fresh = f.open(SessionScope::Home, Some(OTHER));
    let opened_again = f.open(worktree(), Some(OPENED));
    assert_eq!(
        f.app.session_for(&fresh, "s1").unwrap_err(),
        "there is no session `s1`; call `list_all_sessions` for the current handles"
    );
    assert_eq!(
        f.app.handle_for(&fresh, &opened_again),
        "s1",
        "its own numbering"
    );

    for session in [fresh, opened_again] {
        f.app.close_agent(&session);
    }
}
