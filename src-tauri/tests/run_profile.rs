//! A workflow run's agent starts with no API key and no forge credential, and refuses to start on
//! anything but a subscription sign-in.
//!
//! # What this protects
//!
//! A run is the one place the app starts an agent with nobody watching. Claude Code's `-p` mode
//! bills `ANTHROPIC_API_KEY` whenever it is set, without asking, and `claude auth status` keeps
//! saying `claude.ai` while it does. So two things have to hold, and each is proved here against
//! what it actually touches rather than against a fake:
//!
//! - the environment a run's child gets: built over an app environment full of credentials, and
//!   then read back from a real child process;
//! - the preflight: run against the real `claude` and `codex`, including Claude with a dummy key
//!   set, which is the case the preflight cannot see unless the scrub has removed it first.
//!
//! And one thing must *not* change: a pane a person opens keeps its keys and its repository's
//! `[agent.<id>.env]`, local-model `ANTHROPIC_BASE_URL` included.
//!
//! # No API credit is spent
//!
//! `claude auth status` and `codex login status` read local files. The dummy key is never sent:
//! nothing here starts a turn.
//!
//! Skips loudly for a CLI that is not installed, for the reason `agent_capability.rs` gives.

#![allow(clippy::unwrap_used, clippy::print_stderr)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use wtm_agent::run_profile::{read_claude_status, refusal};
use wtm_agent::{claude, codex};
use wtm_app_lib::app::App;
use wtm_core::model::{AuthMethod, ExitOutcome, SessionId};
use wtm_core::ports::pipe::{PipeHost, PipeSink};
use wtm_core::ports::{CancelToken, Invocation};
use wtm_exec::ResolvedPath;

/// Never a real key. Shaped like one so a CLI that checks the prefix still treats it as a key.
const DUMMY_KEY: &str = "sk-ant-wtm-test-dummy-not-a-key";

fn pairs(list: &[(&str, &str)]) -> BTreeMap<String, String> {
    list.iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

/// An app environment with every kind of credential in it: what wtm inherits when it is started
/// from a terminal that has them exported.
fn dirty_parent() -> Vec<(String, String)> {
    pairs(&[
        ("HOME", "/Users/someone"),
        ("USER", "someone"),
        ("LANG", "en_US.UTF-8"),
        ("TMPDIR", "/var/folders/xy/T/"),
        ("SSH_AUTH_SOCK", "/private/tmp/agent.sock"),
        ("GH_TOKEN", "gho_dummy"),
        ("GH_HOST", "github.example.com"),
        ("GH_CONFIG_DIR", "/Users/someone/.config/gh"),
        ("GITHUB_TOKEN", "ghp_dummy"),
        ("ANTHROPIC_API_KEY", DUMMY_KEY),
        ("ANTHROPIC_AUTH_TOKEN", "dummy"),
        ("CLAUDE_CODE_USE_BEDROCK", "1"),
        ("OPENAI_API_KEY", "sk-dummy"),
        ("CODEX_API_KEY", "sk-dummy"),
        ("CODEX_HOME", "/Users/someone/.codex"),
        ("CLAUDE_CONFIG_DIR", "/Users/someone/.claude"),
        ("JIRA_API_TOKEN", "dummy"),
        ("ATLASSIAN_API_TOKEN", "dummy"),
        ("ACLI_CONFIG_DIR", "/Users/someone/.config/acli"),
        // Stripped from every child already; here to show the scrub sits on top of that, not
        // in place of it.
        ("CARGO_MANIFEST_DIR", "/somewhere"),
    ])
    .into_iter()
    .collect()
}

/// What `AgentSession::open` layers on for a repository that points Claude at a local model.
fn repository_env() -> BTreeMap<String, String> {
    pairs(&[
        ("ANTHROPIC_BASE_URL", "http://localhost:11434"),
        ("PROJECT_FLAG", "on"),
        ("NO_COLOR", "1"),
    ])
}

fn app() -> (tempfile::TempDir, Arc<App>) {
    let dir = tempfile::tempdir().unwrap();
    let app = Arc::new(App::with_paths(wtm_config::AppPaths::rooted(dir.path())).unwrap());
    (dir, app)
}

#[test]
fn a_run_child_built_over_a_credential_filled_app_environment_gets_exactly_this() {
    let path = ResolvedPath::resolve(Some("/usr/bin:/bin"));
    let scrub = wtm_agent::run_profile::scrub(std::path::Path::new("/data/wtm/run-gh-config"));
    let inv = Invocation::new(vec!["claude".to_owned()], "/tmp", 1)
        .with_env(repository_env())
        .with_scrub(scrub);

    let env = path.env_over(dirty_parent(), &inv);

    // The whole map, so a credential added to the parent above and not to the scrub fails here
    // by name rather than passing because nobody asserted on it.
    assert_eq!(
        env,
        pairs(&[
            ("CLAUDE_CONFIG_DIR", "/Users/someone/.claude"),
            ("CODEX_HOME", "/Users/someone/.codex"),
            ("GH_CONFIG_DIR", "/data/wtm/run-gh-config"),
            ("GIT_TERMINAL_PROMPT", "0"),
            ("HOME", "/Users/someone"),
            ("LANG", "en_US.UTF-8"),
            ("LOGIN_PATH", "/usr/bin:/bin"),
            ("NO_COLOR", "1"),
            ("PATH", "/usr/bin:/bin"),
            ("PROJECT_FLAG", "on"),
            ("TMPDIR", "/var/folders/xy/T/"),
            ("USER", "someone"),
        ])
    );
}

#[test]
fn a_pane_a_person_opens_keeps_its_keys_and_its_repositorys_local_model_route() {
    // The same app environment and repository, with no scrub: today's behaviour, unchanged.
    let path = ResolvedPath::resolve(Some("/usr/bin:/bin"));
    let inv = Invocation::new(vec!["claude".to_owned()], "/tmp", 1).with_env(repository_env());

    let env = path.env_over(dirty_parent(), &inv);

    assert_eq!(
        env.get("ANTHROPIC_BASE_URL").map(String::as_str),
        Some("http://localhost:11434")
    );
    assert_eq!(
        env.get("ANTHROPIC_API_KEY").map(String::as_str),
        Some(DUMMY_KEY)
    );
    assert_eq!(
        env.get("SSH_AUTH_SOCK").map(String::as_str),
        Some("/private/tmp/agent.sock")
    );
    assert_eq!(
        env.get("GH_CONFIG_DIR").map(String::as_str),
        Some("/Users/someone/.config/gh")
    );
}

/// Every line a pipe child printed, and whether it has exited.
#[derive(Default)]
struct Lines {
    lines: Mutex<Vec<String>>,
    done: Mutex<bool>,
}

impl PipeSink for Lines {
    fn on_line(&self, _session: &SessionId, line: &str) {
        self.lines.lock().push(line.to_owned());
    }
    fn on_stderr(&self, _session: &SessionId, _line: &str) {}
    fn on_exit(&self, _session: &SessionId, _outcome: &ExitOutcome) {
        *self.done.lock() = true;
    }
}

#[test]
fn a_real_pipe_child_with_the_run_scrub_sees_no_credential_even_one_the_repository_set() {
    // Through the pipe host, because that is what starts an agent session. `env` stands in for
    // the CLI: it prints exactly what it was given.
    let host = wtm_exec::PipeHostImpl::new(ResolvedPath::resolve(None));
    let gh = tempfile::tempdir().unwrap();
    let inv = Invocation::new(vec!["env".to_owned()], std::env::temp_dir(), 10_000)
        .with_env(pairs(&[
            ("ANTHROPIC_API_KEY", DUMMY_KEY),
            ("GH_TOKEN", "gho_dummy"),
            ("OPENAI_API_KEY", "sk-dummy"),
            ("PROJECT_FLAG", "on"),
        ]))
        .with_scrub(wtm_agent::run_profile::scrub(gh.path()));
    let sink = Arc::new(Lines::default());
    host.spawn(&inv, None, Arc::clone(&sink) as Arc<dyn PipeSink>)
        .unwrap();
    for _ in 0..500 {
        if *sink.done.lock() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(*sink.done.lock(), "`env` should have exited");

    let seen: BTreeMap<String, String> = sink
        .lines
        .lock()
        .iter()
        .filter_map(|line| line.split_once('='))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
    let scrub = wtm_agent::run_profile::scrub(gh.path());
    let leaked: Vec<&String> = seen
        .keys()
        .filter(|name| scrub.removes(name) && !scrub.values().contains_key(*name))
        .collect();
    assert!(leaked.is_empty(), "these reached the child: {leaked:?}");
    assert_eq!(
        seen.get("GH_CONFIG_DIR").map(String::as_str),
        Some(gh.path().to_str().unwrap())
    );
    assert_eq!(
        seen.get("GIT_TERMINAL_PROMPT").map(String::as_str),
        Some("0")
    );
    assert_eq!(seen.get("PROJECT_FLAG").map(String::as_str), Some("on"));
    assert!(
        seen.contains_key("HOME"),
        "the CLI still needs to find its sign-in"
    );
}

#[test]
fn the_preflight_empties_the_gh_folder_a_run_will_point_gh_at() {
    let (_dir, app) = app();
    let gh = app.config.paths().run_gh_config_dir.clone();
    std::fs::create_dir_all(gh.join("nested")).unwrap();
    std::fs::write(
        gh.join("hosts.yml"),
        "github.com:\n  oauth_token: planted\n",
    )
    .unwrap();

    let (scrub, checks) = wtm_app_lib::run_profile::preflight(&app, &[]).unwrap();

    assert!(checks.is_empty());
    assert_eq!(
        std::fs::read_dir(&gh).unwrap().count(),
        0,
        "a planted login must not survive"
    );
    assert_eq!(
        scrub.values().get("GH_CONFIG_DIR").map(String::as_str),
        Some(gh.to_str().unwrap())
    );
}

#[test]
fn a_gh_folder_replaced_by_a_link_is_refused_and_what_it_points_at_is_left_alone() {
    let (_dir, app) = app();
    let gh = app.config.paths().run_gh_config_dir.clone();
    let real = tempfile::tempdir().unwrap();
    std::fs::write(real.path().join("hosts.yml"), "someone's real login").unwrap();
    std::fs::create_dir_all(gh.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(real.path(), &gh).unwrap();

    let refused = wtm_app_lib::run_profile::preflight(&app, &[]).unwrap_err();

    assert!(refused.contains("not a plain folder"), "{refused}");
    assert!(
        real.path().join("hosts.yml").exists(),
        "emptying must not follow the link"
    );
}

#[test]
fn a_run_on_a_provider_with_no_subscription_check_is_refused_by_name() {
    let (_dir, app) = app();
    let refused = wtm_app_lib::run_profile::preflight(&app, &[wtm_agent::cursor::ID]).unwrap_err();
    assert!(refused.contains("Cursor Agent"), "{refused}");
}

/// The CLI the app would start for `provider`, printing why not when there is none.
fn installed(app: &App, provider: &str) -> Option<String> {
    let entry = wtm_agent::entry(provider).unwrap();
    let Some(program) = app.agent_executable(entry) else {
        eprintln!("skipping: wtm cannot find {}'s CLI here", entry.label);
        return None;
    };
    Some(program.to_string_lossy().into_owned())
}

#[test]
fn real_claude_with_an_api_key_set_reports_the_key_and_the_run_scrub_takes_it_away() {
    let (_dir, app) = app();
    let Some(program) = installed(&app, claude::ID) else {
        return;
    };
    let ask = |inv: &Invocation| {
        let out = app
            .runner
            .run_allow_failure(inv, &CancelToken::new())
            .unwrap();
        read_claude_status(&out.stdout)
    };
    let dirty = Invocation::new(
        claude::auth_status_argv(&program),
        std::env::temp_dir(),
        10_000,
    )
    .with_env(pairs(&[("ANTHROPIC_API_KEY", DUMMY_KEY)]));

    // Without the scrub: the preflight sees the key, whatever else the machine is signed in with.
    let unscrubbed = ask(&dirty);
    assert!(
        matches!(unscrubbed, AuthMethod::ApiKey { .. }),
        "a set ANTHROPIC_API_KEY must read as a key, got {unscrubbed:?}"
    );
    assert!(refusal(claude::ID, &unscrubbed).is_some());

    // With it: the key is gone before the CLI starts, so this one is not it.
    let scrubbed = ask(&dirty.with_scrub(wtm_app_lib::run_profile::scrub(&app)));
    assert_ne!(
        scrubbed,
        AuthMethod::ApiKey {
            source: Some("ANTHROPIC_API_KEY".to_owned())
        }
    );
    assert_ne!(
        scrubbed,
        AuthMethod::Unreadable,
        "`claude auth status --json` changed shape"
    );

    // The whole path the usage view and a run take.
    let check = wtm_app_lib::run_profile::check(&app, claude::ID).unwrap();
    assert_eq!(check.method, scrubbed);
    if check.method.is_subscription() {
        assert_eq!(check.refusal, None);
        wtm_app_lib::run_profile::preflight(&app, &[claude::ID]).unwrap();
    } else {
        eprintln!(
            "skipping the subscription half: Claude Code here reports \"{}\"",
            check.summary
        );
        assert!(wtm_app_lib::run_profile::preflight(&app, &[claude::ID]).is_err());
    }
}

#[test]
fn real_codex_login_status_reads_as_a_method_and_only_chatgpt_passes() {
    let (_dir, app) = app();
    if installed(&app, codex::ID).is_none() {
        return;
    }
    let check = wtm_app_lib::run_profile::check(&app, codex::ID).unwrap();
    // The summary rather than the CLI's own line: an API-key sign-in prints part of the key.
    assert_ne!(
        check.method,
        AuthMethod::Unreadable,
        "`codex login status` changed its wording"
    );
    if check.method.is_subscription() {
        assert_eq!(check.refusal, None);
        wtm_app_lib::run_profile::preflight(&app, &[codex::ID]).unwrap();
    } else {
        eprintln!(
            "skipping the subscription half: Codex here reports \"{}\"",
            check.summary
        );
        assert!(check.refusal.is_some());
    }
}
